// src/ssa/lower.rs - Builder 调用的降级辅助与 phi 插入
//
// 放什么: `lower_lvalue` / `collect_local_decls{,_stmt}` / `insert_loop_phis` /
//   `bump_version` / `insert_label_phis` —— 被 `Builder` 与 `build_ssa` 调用的辅助。
// 不放什么: 降级政策本体只在 `build.rs`, 本文件只放被它调用的辅助;
//   AST 侧政策与入 SSA 前的整形在 `structure.rs`, SSA→AST 的重建在 `rebuild.rs`。

use std::collections::{HashMap, HashSet};

use crate::ast::{BinOp, BlockItem, Decl, Expr, QualType, Stmt, Type};
use crate::diagnostics::Span;

use crate::ssa::build::Builder;
use crate::ssa::cfg::{
    collect_all_preds, compute_dominators, find_loop_body, ordered_block_names,
};
use crate::ssa::ir::{
    rename_uses_in, Function, Inst, InstKind, Operand, OrderedNameSet, Phi, SsaVar, Terminator,
};

pub(super) fn lower_lvalue(
    l: &Expr,
    _op: &BinOp,
    src: Operand,
    span: &Span,
    ty: &Type,
    b: &mut Builder,
) -> Option<SsaVar> {
    if let Expr::Ident {
        name, ty: var_ty, ..
    } = l
    {
        // 与 `Expr::Assign` 分支用同一条判据: 文件作用域对象走内存写, 不开 SSA 版本。
        // 当前唯一调用点 (`Expr::Assign`) 已在进入本函数前拦下 Ident 左值, 保留该分支
        // 是为了让「以名字为左值的写」只有一个真源, 避免今后新增调用者重新造出
        // 「用全局名当指令 dest」这条静默遮蔽路径。
        if !b.is_local_object(&name.name) {
            b.store_to_named_object(&name.name, var_ty, src.clone(), span, false);
            return None;
        }
        // 被取址的标量局部: 与赋值分支用同一条判据, 写内存、不开版本。
        if b.is_mem_only_object(&name.name, var_ty) {
            let is_volatile = b
                .func
                .local_quals
                .get(name.name.as_str())
                .map(|q| q.is_volatile())
                .unwrap_or(false);
            b.store_to_named_object(&name.name, var_ty, src.clone(), span, is_volatile);
            return None;
        }
        let v = SsaVar {
            base: name.name.clone(),
            version: b.next_version(&name.name),
            ty: var_ty.clone(),
            span: span.clone(),
        };
        // Emit a copy instruction so rebuild_block connects the
        // computed value to the SSA variable name.
        // Use lower_expr_into if src is a simple operand to avoid
        // creating extra temporaries.
        let v_clone = v.clone();
        match &src {
            Operand::Var(_src_var) => {
                // Source is already a variable - emit direct copy
                b.emit(Inst {
                    dest: Some(v_clone.clone()),
                    kind: InstKind::Bin {
                        op: BinOp::Add,
                        l: src.clone(),
                        r: Operand::Int(0),
                    },
                    span: span.clone(),
                });
            },
            _ => {
                // Source is a constant - emit as immediate assignment
                b.emit(Inst {
                    dest: Some(v_clone.clone()),
                    kind: InstKind::Bin {
                        op: BinOp::Add,
                        l: src.clone(),
                        r: Operand::Int(0),
                    },
                    span: span.clone(),
                });
            },
        }
        b.define(v.clone());
        return Some(v);
    }
    if let Expr::Deref { e: inner, .. } = l {
        let addr = b.lower_expr(inner);
        let store_volatile = match inner.ty() {
            Type::Ptr(qt) => qt.is_volatile(),
            _ => false,
        };
        b.emit(Inst {
            dest: None,
            kind: InstKind::Store {
                e: addr,
                src,
                is_volatile: store_volatile,
            },
            span: span.clone(),
        });
        let dest = b.fresh("store", ty.clone(), span);
        return Some(dest);
    }
    if let Expr::Index { arr, idx, ty, .. } = l {
        // 基址必须取**地址形式**，不能取右值形式：`m[i][j] = v` 的 `m[i]` 是**数组类型**
        // 的左值，`lower_expr` 会把它当值降级（`IndexLoad{ty: Array}` ⇒ 重建时是个住在
        // 自己帧临时里的行副本），写进去的是副本而不是数组本体。-O3 展开后两个下标都
        // 成常量时实测就是这个形状：`small7x3[3][0]`/`[6][0]` 根本没被写（读到 0），
        // 同一行的 `[i][1]`/`[i][2]` 却正常写进数组 —— 因为那两条的内层基址刚好被内联
        // 成了「行地址」而这一条没有。基名是 `lower_lvalue_base_addr`：数组行→`IndexAddr`，
        // 指针/具名数组/解引用基址→原衰减分支本就给出地址。写入仍按本条指令自己的
        // 元素类型 `ty` 定标（`IndexStore{ty: Int}` ⇒ 行首地址 + j*4），不会按行步长算错。
        let a = b.lower_lvalue_base_addr(arr);
        let i = b.lower_expr(idx);
        let store_volatile = match arr.ty() {
            Type::Array(elem_qt, _) => elem_qt.is_volatile(),
            _ => false,
        };
        b.emit(Inst {
            dest: None,
            kind: InstKind::IndexStore {
                arr: a,
                idx: i,
                src,
                ty: ty.clone(),
                is_volatile: store_volatile,
            },
            span: span.clone(),
        });
        let dest = b.fresh("idxstore", ty.clone(), span);
        return Some(dest);
    }
    if let Expr::Member {
        expr,
        field,
        is_arrow,
        ..
    } = l
    {
        let obj_ty = expr.ty();
        let obj = b.lower_expr(expr);
        let member_volatile = match (&obj_ty, expr.as_ref()) {
            (Type::Ptr(qt), _) if *is_arrow => qt.is_volatile(),
            (_, Expr::Ident { name, .. }) if !*is_arrow => b
                .func
                .local_quals
                .get(name.name.as_str())
                .map(|q| q.is_volatile())
                .unwrap_or(false),
            _ => false,
        };
        if *is_arrow {
            let deref_ty = match &obj_ty {
                Type::Ptr(inner) => inner.base.clone(),
                other => other.clone(),
            };
            let inner = b.fresh("deref", Type::Ptr(Box::new(QualType::new(ty.clone()))), span);
            b.emit(Inst {
                dest: Some(inner.clone()),
                kind: InstKind::Load {
                    e: obj,
                    ty: ty.clone(),
                    is_volatile: member_volatile,
                },
                span: span.clone(),
            });
            b.emit(Inst {
                dest: None,
                kind: InstKind::MemberStore {
                    obj: Operand::Var(inner),
                    obj_ty: deref_ty,
                    field: field.name.clone(),
                    src,
                    ty: ty.clone(),
                    is_volatile: member_volatile,
                    is_arrow: false,
                },
                span: span.clone(),
            });
        } else {
            b.emit(Inst {
                dest: None,
                kind: InstKind::MemberStore {
                    obj,
                    obj_ty: obj_ty.clone(),
                    field: field.name.clone(),
                    src,
                    ty: ty.clone(),
                    is_volatile: member_volatile,
                    is_arrow: false,
                },
                span: span.clone(),
            });
        }
        let dest = b.fresh("memstore", ty.clone(), span);
        return Some(dest);
    }
    None
}

/// 递归收集 block 中所有 Decl::Var 的变量名
pub(super) fn collect_local_decls(
    items: &[BlockItem],
    out: &mut OrderedNameSet,
    types: &mut HashMap<String, Type>,
    quals: &mut HashMap<String, crate::ast::QualifierFlags>,
) {
    for item in items {
        match item {
            BlockItem::Decl(Decl::Var(v)) => {
                // extern 声明引用全局符号, 不作为局部变量收集
                if !v.is_extern {
                    out.insert(v.name.name.clone());
                    types.insert(v.name.name.clone(), v.ty.clone());
                    quals.insert(v.name.name.clone(), v.quals);
                }
            },
            BlockItem::Stmt(s) => collect_local_decls_stmt(s, out, types, quals),
            _ => {},
        }
    }
}

fn collect_local_decls_stmt(
    s: &Stmt,
    out: &mut OrderedNameSet,
    types: &mut HashMap<String, Type>,
    quals: &mut HashMap<String, crate::ast::QualifierFlags>,
) {
    // 本函数必须是**穷举**的: `is_local_object` 以 `original_locals` 为「局部」的唯一真源,
    // 漏掉任何一个可以携带声明的语句形态, 就会把该形态里声明的局部变量误判为文件
    // 作用域对象 (写它时变成 `*(&name) = v`, 后端找不到该符号即报 E1103)。
    // 故这里不写 `_ => {}` 兼容分支: 新增 `Stmt` 形态时本函数会编译失败, 强制同步。
    match s {
        Stmt::Block { b, .. } => collect_local_decls(&b.items, out, types, quals),
        Stmt::If { then_, else_, .. } => {
            collect_local_decls_stmt(then_, out, types, quals);
            if let Some(e) = else_ {
                collect_local_decls_stmt(e, out, types, quals);
            }
        },
        // `for (int i = 0; ...)` 的声明藏在 init 语句里, 与 body 同等重要。
        Stmt::For { init, body, .. } => {
            if let Some(i) = init {
                collect_local_decls_stmt(i, out, types, quals);
            }
            collect_local_decls_stmt(body, out, types, quals);
        },
        Stmt::While { body, .. } | Stmt::Do { body, .. } => {
            collect_local_decls_stmt(body, out, types, quals);
        },
        Stmt::Switch { cases, .. } => {
            for c in cases {
                collect_local_decls(&c.body.items, out, types, quals);
            }
        },
        Stmt::Label { stmt, .. } => {
            collect_local_decls_stmt(stmt, out, types, quals);
        },
        // 以下形态不可能包含声明: 只读取值 / 控制流边 / 本身不嵌入子语句。
        Stmt::Expr { .. }
        | Stmt::Return { .. }
        | Stmt::Break { .. }
        | Stmt::Continue { .. }
        | Stmt::Fallthrough { .. }
        | Stmt::Goto { .. }
        | Stmt::GotoIndirect { .. }
        | Stmt::AsmGoto { .. } => {},
        // C++ 零成本异常: `Invoke` 的 `call` 是一次受保护调用, `EhLandingPad` 是两条
        // 寄存器交接声明 —— 两者都不携带块级声明, 且含它们的函数走不到本路径
        // (`is_structured` 已将 EH 形态旁路), 仍显式列出以保持穷举。
        Stmt::Invoke { .. } | Stmt::EhLandingPad { .. } => {},
    }
}

/// Insert phi nodes at loop headers for variables modified in loop bodies.
///
/// When the SSA builder processes loops (while/for/do), it creates a CFG
/// with back edges. Variables modified in the loop body get new SSA versions,
/// but without phi nodes at the loop header, SSA optimization passes cannot
/// correctly reason about loop-carried values.
///
/// This pass:
/// 1. Identifies back edges (latch → header) using dominator analysis
/// 2. For each loop header, collects variables defined in the loop body
/// 3. Inserts phi nodes merging the pre-loop and post-body versions
/// 4. **把循环体内对该变量的引用重命名到 phi dest**, 并给 phi 记上真正的 preheader。
///
/// 第 4 步与「从整个循环体而不是只看 header 收集使用」是本函数的核心修正: 旧实现
/// 只看 header 块里的指令, 而 `while` 的 header 常常只有 cond (`n > 0`), 循环携带变量
/// `s` 的读发生在体内那条 `s += n` 上 ⇒ 根本不生成 phi, IR 不在真正的 SSA 形式上;
/// 即使生成了 phi, 体内引用仍指向进循环前的版本, 于是「每轮读上一轮的值」这件事
/// 在 IR 里不存在。两者叠加使重建结果只在「基名塌缩」巧合下才对, 任何常量传播或
/// 值编号一碰就错。
pub(super) fn insert_loop_phis(func: &mut Function) {
    // Compute dominator tree for correct back-edge detection.
    // A back edge is an edge A→B where B dominates A.
    let doms = compute_dominators(func);
    let mut back_edges: Vec<(String, String)> = Vec::new(); // (latch, header)
                                                            // 必须用 `ordered_block_names` 而非 `&func.blocks`: back_edges 的处理顺序
                                                            // 决定同一 header 上 phi 的排列序, 而 phi 序会经 rebuild 影响产物字节。
    for bb_name in ordered_block_names(func) {
        let Some(bb) = func.blocks.get(&bb_name) else {
            continue;
        };
        let successors = match &bb.term {
            Terminator::Jump(t) => vec![t.clone()],
            Terminator::Branch { t, f, .. } => vec![t.clone(), f.clone()],
            Terminator::SwitchTable {
                targets, default, ..
            } => {
                let mut s = targets.clone();
                if !s.contains(default) {
                    s.push(default.clone());
                }
                s
            },
            _ => vec![],
        };
        let from_doms = match doms.get(&bb_name) {
            Some(d) => d,
            None => continue,
        };
        for succ in successors {
            // Back edge: target dominates source
            if from_doms.contains(&succ) {
                back_edges.push((bb_name.clone(), succ));
            }
        }
    }
    if back_edges.is_empty() {
        return;
    }
    // For each back edge (latch → header), find variables defined in the
    // loop body and insert phi nodes at the header.
    let all_preds = collect_all_preds(func);
    for (latch, header) in back_edges.clone() {
        // Collect loop body blocks: all blocks that can reach the latch
        // without going through the header (simplified: blocks dominated
        // by the header that can reach the latch).
        let loop_bbs = find_loop_body(func, &header, &latch);
        let in_body: HashSet<String> = loop_bbs.iter().cloned().collect();
        // 体内基名 → 体内最后一个定义 (回边带回来的值); 体内外的定义一起扫,
        // 体外取「循环前最后一次定义」= preheader 可用的那个版本。
        let mut body_defs: HashMap<String, SsaVar> = HashMap::new();
        let mut outside_defs: HashMap<String, SsaVar> = HashMap::new();
        let mut max_version: HashMap<String, u32> = HashMap::new();
        for p in &func.params {
            max_version
                .entry(p.base.clone())
                .and_modify(|v| {
                    if p.version > *v {
                        *v = p.version;
                    }
                })
                .or_insert(p.version);
            outside_defs.entry(p.base.clone()).or_insert_with(|| p.clone());
        }
        for bb_name in ordered_block_names(func) {
            let Some(bb) = func.blocks.get(&bb_name) else {
                continue;
            };
            let inside = in_body.contains(&bb_name);
            for inst in &bb.insts {
                if let Some(d) = &inst.dest {
                    bump_version(&mut max_version, &d.base, d.version);
                    if inside {
                        body_defs.insert(d.base.clone(), d.clone());
                    } else {
                        outside_defs.insert(d.base.clone(), d.clone());
                    }
                }
            }
            for phi in &bb.phis {
                bump_version(&mut max_version, &phi.dest.base, phi.dest.version);
                if inside {
                    body_defs.insert(phi.dest.base.clone(), phi.dest.clone());
                }
            }
        }
        // header 已有的 phi (多回边/内层循环重复处理时) 不重复创建。
        let existing_phi: HashSet<String> = func
            .blocks
            .get(&header)
            .map(|b| b.phis.iter().map(|p| p.dest.base.clone()).collect())
            .unwrap_or_default();
        // 真正的 preheader: header 的前驱中不在循环体内者。旧实现写魔法串
        // `"__entry"` (全仓无消费者), phi 的「从哪条边进来」这个信息因此丢失。
        let preheader = all_preds
            .get(&header)
            .map(|ps| {
                let mut outside: Vec<&String> =
                    ps.iter().filter(|p| !in_body.contains(p.as_str())).collect();
                outside.sort();
                outside.first().map(|p| (*p).clone())
            })
            .flatten();
        let mut new_phis: Vec<Phi> = Vec::new();
        let mut renames: HashMap<String, SsaVar> = HashMap::new();
        // body_defs 是 HashMap, 直接迭代会把随机序写进 bb.phis; 按名字排序保证可复现。
        let mut body_def_names: Vec<&String> = body_defs.keys().collect();
        body_def_names.sort();
        for base in body_def_names {
            if existing_phi.contains(base.as_str()) {
                continue;
            }
            let body_var = &body_defs[base];
            let Some(entry_var) = outside_defs.get(base.as_str()) else {
                continue;
            };
            if entry_var.name() == body_var.name() {
                continue;
            }
            let phi_dest = SsaVar {
                base: base.clone(),
                version: max_version.get(base.as_str()).copied().unwrap_or(0) + 1,
                ty: body_var.ty.clone(),
                span: body_var.span.clone(),
            };
            new_phis.push(Phi {
                dest: phi_dest.clone(),
                incoming: vec![
                    (
                        entry_var.clone(),
                        preheader.clone().unwrap_or_else(|| "__entry".into()),
                    ),
                    (body_var.clone(), latch.clone()),
                ],
            });
            // 关键一步: 循环体内所有对该变量的读改写都指向 phi dest。不做这步,
            // 「每轮读上一轮的值」在 IR 里根本不存在, 任何常量传播/值编号都会
            // 把循环携带变量在进循环前的初值传进体里。
            renames.insert(entry_var.name(), phi_dest);
        }
        if let Some(bb) = func.blocks.get_mut(&header) {
            bb.phis.extend(new_phis);
        }
        rename_uses_in(func, &in_body, &renames);
    }
}

fn bump_version(map: &mut HashMap<String, u32>, base: &str, v: u32) {
    map.entry(base.to_string())
        .and_modify(|cur| {
            if v > *cur {
                *cur = v;
            }
        })
        .or_insert(v);
}

/// P0-4 Phase 2: 在 label BB 入口插入 phi 节点.
///
/// 当 goto 跳转到 label 时，变量版本可能与 label 正常入口不同.
/// 为每个被 goto 跨越的变量插入 phi 节点合并不同版本.
pub(super) fn insert_label_phis(
    func: &mut Function,
    label_bbs: &HashMap<String, String>,
    _label_var_snapshots: &HashMap<String, HashMap<String, SsaVar>>,
) {
    if label_bbs.is_empty() {
        return;
    }
    // 对每个 label BB，检查是否有多个前驱且前驱的变量版本不同
    // 先把待处理 BB 按名字排序: 不同 label 彼此独立, 但显式定序可防未来改成
    // 全局副作用型逻辑时重新引入 HashMap 迭代序泄漏。
    let mut label_bbs_sorted: Vec<&String> = label_bbs.values().collect();
    label_bbs_sorted.sort();
    for bb_name in label_bbs_sorted {
        let bb = match func.blocks.get(bb_name) {
            Some(b) => b,
            None => continue,
        };
        if bb.preds.len() <= 1 {
            continue; // 只有一个前驱，不需要 phi
        }
        // 收集每个前驱 BB 最后定义的变量版本
        let mut pred_vars: Vec<(String, HashMap<String, SsaVar>)> = Vec::new();
        for pred_name in &bb.preds {
            let pred_bb = match func.blocks.get(pred_name) {
                Some(b) => b,
                None => continue,
            };
            let mut vars: HashMap<String, SsaVar> = HashMap::new();
            for inst in &pred_bb.insts {
                if let Some(d) = &inst.dest {
                    vars.insert(d.base.clone(), d.clone());
                }
            }
            pred_vars.push((pred_name.clone(), vars));
        }
        if pred_vars.len() < 2 {
            continue;
        }
        // 找出在所有前驱中都有定义但版本不同的变量
        let first_vars = &pred_vars[0].1;
        let mut needs_phi: Vec<String> = Vec::new();
        // first_vars 是 HashMap, 必须先排序再收集, 否则 needs_phi 的序 (→ phi 在
        // BB 内的排列序) 会随进程随机种子变化。
        let mut var_names: Vec<&String> = first_vars.keys().collect();
        var_names.sort();
        for base in var_names {
            let var = &first_vars[base];
            let mut all_same = true;
            for (_, other_vars) in &pred_vars[1..] {
                if let Some(other_var) = other_vars.get(base) {
                    if other_var.name() != var.name() {
                        all_same = false;
                        break;
                    }
                }
            }
            if !all_same {
                needs_phi.push(base.clone());
            }
        }
        // 为需要 phi 的变量插入 phi 节点
        if !needs_phi.is_empty() {
            let bb = match func.blocks.get_mut(bb_name) {
                Some(b) => b,
                None => continue,
            };
            for base in &needs_phi {
                // 找到最高版本号 + 1 作为 phi 目标版本
                let max_version = pred_vars
                    .iter()
                    .filter_map(|(_, vars)| vars.get(base))
                    .map(|v| v.version)
                    .max()
                    .unwrap_or(0);
                let ty = pred_vars
                    .iter()
                    .filter_map(|(_, vars)| vars.get(base))
                    .next()
                    .map(|v| v.ty.clone())
                    .unwrap_or(Type::Int);
                let span = pred_vars
                    .iter()
                    .filter_map(|(_, vars)| vars.get(base))
                    .next()
                    .map(|v| v.span.clone())
                    .unwrap_or_else(|| func.span.clone());
                let phi_dest = SsaVar {
                    base: base.clone(),
                    version: max_version + 1,
                    ty,
                    span,
                };
                let incoming: Vec<(SsaVar, String)> = pred_vars
                    .iter()
                    .filter_map(|(pred_name, vars)| {
                        vars.get(base).map(|v| (v.clone(), pred_name.clone()))
                    })
                    .collect();
                if incoming.len() >= 2 {
                    bb.phis.push(Phi {
                        dest: phi_dest,
                        incoming,
                    });
                }
            }
        }
    }
}

