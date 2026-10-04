// src/ssa/rebuild.rs - SSA→AST 的控制流重建 (M16 优化 pass 基础设施)
//
// 放什么: rebuild_block / RebuildCtx / rebuild_bb / rebuild_cycle / push_inst_stmt /
//   rebuild_eh_blocks / emit_rebuild_inst / walk_to_join / fold_join / loop_entry_join /
//   branch_join / phi_name / is_lower_ir_temp_base / is_pass_hint_inst /
//   inst_has_side_effect / is_loop_exit_pure_return。
// 不放什么: 单条指令→表达式的翻译与类型推导在 expr_rebuild.rs, 不在本文件。
//   出口边的归属判据在 loop_exit.rs, 本文件的发射点必须与它同源。

use std::collections::{HashMap, HashSet};

use crate::ast::{Block, BlockItem, Case, Decl, Expr, Stmt, Type};

use crate::ssa::cfg::{
    compute_dominators, compute_ipdoms, is_back_edge, is_cycle_entry, ordered_block_names,
    predecessor_counts,
};
use crate::ssa::expr_rebuild::{inst_to_expr_resolved, resolve_operand};
use crate::ssa::inline_temps::{is_synthetic_temp_base, plan_inlined_temps, InlinePlan};
use crate::ssa::ir::{Function, Inst, InstKind, Operand, OrderedNameSet, SsaVar, Terminator};
use crate::ssa::loop_exit::{loop_exit_owner, loop_exit_plan, loop_shape, LoopExitOwner};

/// 把 SSA Function 重建为 AST Block (codegen 仍走 AST 路径)。
///
/// 策略:
///   1. 只声明原始局部变量 (来自 func.original_locals) 与**未被内联**的 SSA 临时变量。
///   2. 单次使用且顺序安全的 SSA 临时变量在其使用处内联展开, 避免多余栈槽。
///   3. phi 节点在 merge BB 用对原变量赋值处理。
pub fn rebuild_block(func: &Function) -> Block {
    let mut items: Vec<BlockItem> = Vec::new();
    let entry = func.entry.clone();
    let mut visited: HashSet<String> = HashSet::new();
    let mut env: HashMap<String, SsaVar> = HashMap::new();

    // 0. 内联计划 (必须先于局部声明, 见 `plan_inlined_temps` 注释)
    let InlinePlan {
        subst,
        kept_temp_bases,
    } = plan_inlined_temps(func);
    let _ = &kept_temp_bases;

    // 1. 只声明原始局部变量 (不声明 SSA 临时变量)
    let param_names: HashSet<String> = func.params.iter().map(|p| p.base.clone()).collect();
    // 收集已通过 PreservedDecl 保留原始声明的变量名
    let preserved_vars: HashSet<String> = func
        .blocks
        .values()
        .flat_map(|bb| bb.insts.iter())
        .filter_map(|inst| match &inst.kind {
            InstKind::PreservedDecl { var } => Some(var.name.name.clone()),
            _ => None,
        })
        .collect();

    // 收集所有 SSA 定义中的基础变量名及其类型
    // 用于为不在 original_locals 中但有多次赋值的变量生成声明
    //
    // 必须按 `ordered_block_names` (创建序) 而不是 `blocks.values()` (HashMap 随机序)
    // 遍历: 同一 base 的多个 SSA 版本可能携带不同 `ty`, `or_insert_with` 的
    // "首个胜出" 规则会因遍历序而选到不同类型; 而且这里得到的名字序会被下面
    // 直接拼进 `decl_names`, 决定局部变量声明序 → 决定栈槽偏移。
    let mut all_def_types: HashMap<String, Type> = HashMap::new();
    let mut all_def_order = OrderedNameSet::new();
    for bb_name in ordered_block_names(func) {
        let Some(bb) = func.blocks.get(&bb_name) else {
            continue;
        };
        for inst in &bb.insts {
            if let Some(d) = &inst.dest {
                if all_def_order.insert(d.base.clone()) {
                    all_def_types.insert(d.base.clone(), d.ty.clone());
                }
            }
        }
        for phi in &bb.phis {
            if all_def_order.insert(phi.dest.base.clone()) {
                all_def_types.insert(phi.dest.base.clone(), phi.dest.ty.clone());
            }
        }
    }

    // phi 的 dest 永远不会由指令赋值: rebuild_bb 只把它映射到 env 里的某个
    // 已有 SSA 值。若在这里把它当成局部变量声明, codegen 会为它分配栈槽但
    // 没有任何写入, 读取时拿到未初始化值 —— `int z = a && b;` 之类经
    // lower_logical_and 生成 phi 的表达式会静默出错。因此 phi dest 一律不声明。
    let phi_dest_names: HashSet<String> = func
        .blocks
        .values()
        .flat_map(|bb| bb.phis.iter())
        .map(|p| phi_name(&p.dest))
        .collect();

    let mut decl_names: Vec<String> = func.original_locals.iter().cloned().collect();
    // 追加: 非 _ 开头、非参数、非 original_locals、非 preserved 的变量
    // 这些是 lower_ir 生成的非临时变量（如 fresh_var 生成的非临时名），
    // 需要声明否则 codegen 会按隐式 int 处理导致类型错误
    // 注意: 这里遵循 `all_def_order`(首次定义序) 而非 HashMap 随机序。
    for name in all_def_order.iter() {
        if phi_dest_names.contains(name) {
            continue;
        }
        // 合成临时值默认**不**声明 —— 它们通常已被内联成表达式。
        // 只有内联被判定为顺序不安全 (见 `plan_inlined_temps`) 因而会发射成一条
        // 对它的真实赋值时, 才必须给它声明, 否则后端按隐式 int 处理。
        // 用户自己的 `_s` 已在 `original_locals` 里, 不走这条跳过分支。
        if is_synthetic_temp_base(func, name) && !kept_temp_bases.contains(name) {
            continue;
        }
        if param_names.contains(name) {
            continue;
        }
        if func.original_locals.contains(name) {
            continue;
        }
        if preserved_vars.contains(name) {
            continue;
        }
        decl_names.push(name.clone());
    }

    // D3 / Phase 3.4：分裂出的真局部也必须声明（它们不在 `original_locals` 里）。
    // 表由 `regalloc` 算一次存进 `func.local_splits`，重建与交付读同一个答案；
    // 今天这张表恒空（同基名的版本同一个家），所以产物逐字节不变。
    for (ast_name, _base, _) in func.local_splits.new_locals() {
        if !decl_names.contains(ast_name) {
            decl_names.push(ast_name.clone());
        }
    }

    for name in &decl_names {
        if param_names.contains(name) {
            continue;
        }
        // 已通过 PreservedDecl 保留原始声明, 跳过自动声明
        if preserved_vars.contains(name) {
            continue;
        }
        // Use the stored type from local_types if available, otherwise try to infer from instructions
        // 分裂出的局部优先从 `local_splits` 拿自己的类型：下面的两条退路都只认基名，
        // 拿不到就会退成 `Type::Int`（按 4 字节分配栈槽）—— 静默宽度错。
        let ty = func.local_splits.type_of_local(name).unwrap_or_else(|| {
            func.local_types.get(name).cloned().unwrap_or_else(|| {
                all_def_types.get(name).cloned().unwrap_or_else(|| {
                    func.blocks
                        .values()
                        .flat_map(|bb| bb.insts.iter())
                        .find_map(|inst| {
                            inst.dest
                                .as_ref()
                                .filter(|d| &d.base == name)
                                .map(|d| d.ty.clone())
                        })
                        .unwrap_or(Type::Int)
                })
            })
        });
        // 保留原始类型限定符 (volatile/const/restrict),
        // 防止 volatile 变量的读写被优化器错误移除
        let quals = func
            .local_quals
            .get(name)
            .copied()
            .unwrap_or(crate::ast::QualifierFlags::NONE);
        items.push(BlockItem::Decl(Decl::Var(crate::ast::VarDecl {
            ty,
            name: crate::ast::Ident {
                name: name.clone(),
                span: func.span.clone(),
            },
            init: None,
            span: func.span.clone(),
            is_extern: false,
            is_static: false,
            section: None,
            alignment: None,
            is_weak: false,
            visibility: None,
            is_unused: false,
            register_name: None,
            // 家不由重建写：位置表在 `rebuild_block` **之后**才由 regalloc 建好
            // （它的准入判据要扫重建后的体），所以唯一写入点是 `apply_bindings`。
            home: None,
            cleanup_fn: None,
            is_thread_local: false,
            no_sanitize: vec![],
            alias_name: None,
            copy_fn: None,
            is_retain: false,
            is_used_attr: false,
            quals,
            addr_space: crate::ast::AddrSpace::None,
            tls_model: None,
            asm_label: None,
        })));
    }

    let doms = compute_dominators(func);
    let mut cx = RebuildCtx {
        func,
        doms: &doms,
        subst: &subst,
        visited: &mut visited,
        env: &mut env,
    };
    rebuild_bb(&entry, &mut cx, &mut items, None, None);
    // C++ 零成本异常: 落地垫区不属于正常 CFG (只由 unwinder 按地址区间进入),
    // 上面的结构化重建从 entry 出发走不到它们。在函数体末尾以
    // `Label + 平铺语句 + Goto` 的形式补发射。
    rebuild_eh_blocks(func, &mut cx, &mut items);
    Block {
        items,
        span: func.span.clone(),
    }
}

/// 把一条 SSA 指令翻译成 AST 语句并追加到 `out`。
///
/// `rebuild_bb` (结构化重建) 与 `rebuild_eh_blocks` (落地垫平铺发射) 共用同一份
/// 翻译规则: 否则受保护调用与落地垫序言在两条路径上的语义会漂移 —— 实测只
/// 在其中一处特殊处理, `Stmt::Invoke` 的包装会时有时无, LSDA 登记的地址区间
/// 因此指向不存在的调用指令 (抛异常时找不到帧)。
fn push_inst_stmt(inst: &Inst, bb_name: &str, cx: &mut RebuildCtx<'_>, out: &mut Vec<BlockItem>) {
    let func = cx.func;
    let subst = cx.subst;
    // C++ 零成本异常: 落地垫序言必须是一条**专用语句** (后端要在标签后的第一
    // 条指令把 %rax/%rdx 溢出到槽位), 不能退化成一次赋值表达式。
    if let InstKind::EhLandingPad { selector } = &inst.kind {
        let Some(obj) = inst.dest.as_ref() else {
            return;
        };
        let sel = selector;
        cx.env.insert(obj.name(), obj.clone());
        cx.env.insert(sel.name(), sel.clone());
        let mk_ident = |v: &SsaVar| Expr::Ident {
            name: crate::ast::Ident {
                name: func.local_splits.ast_name(v),
                span: v.span.clone(),
            },
            span: v.span.clone(),
            ty: v.ty.clone(),
        };
        out.push(BlockItem::Stmt(Stmt::EhLandingPad {
            label: crate::ast::Ident {
                name: bb_name.to_string(),
                span: func.span.clone(),
            },
            object: Box::new(mk_ident(obj)),
            selector: Box::new(mk_ident(sel)),
            span: func.span.clone(),
        }));
        return;
    }
    // 受保护调用: 先按普通调用生成一条语句, 再用 `Stmt::Invoke` 把那条语句整体
    // 包起来 —— 后端以被包装语句的地址区间作为 LSDA call-site 区间。
    let eh_wrap: Option<(Option<String>, Vec<crate::ast::EhAction>)> = match &inst.kind {
        InstKind::Invoke {
            unwind, actions, ..
        } => Some((unwind.clone(), actions.clone())),
        _ => None,
    };
    if let Some(d) = &inst.dest {
        cx.env.insert(d.name(), d.clone());
        let ssa_name = d.name();
        if let Some(Some(_)) = subst.get(&ssa_name) {
            return;
        }
        let rhs = inst_to_expr_resolved(&inst.kind, subst, &inst.span, func, Some(&d.ty));
        let assign = Expr::Assign {
            op: crate::ast::BinOp::Add,
            l: Box::new(Expr::Ident {
                name: crate::ast::Ident {
                    // 赋值左端用版本自己的局部名（否则读写会落到不同变量上）
                    name: func.local_splits.ast_name(d),
                    span: d.span.clone(),
                },
                span: d.span.clone(),
                ty: d.ty.clone(),
            }),
            r: Box::new(rhs),
            span: inst.span.clone(),
            ty: d.ty.clone(),
        };
        out.push(BlockItem::Stmt(Stmt::Expr {
            e: assign,
            span: inst.span.clone(),
        }));
    } else {
        let e = inst_to_expr_resolved(&inst.kind, subst, &inst.span, func, None);
        out.push(BlockItem::Stmt(Stmt::Expr {
            e,
            span: inst.span.clone(),
        }));
    }
    if let Some((lp, actions)) = eh_wrap {
        // 上面必定刚追入了一条语句; 取回它并包装。若拿不到 (不是语句形态)
        // 则保持原样推回, 不牺牲已有代码。
        let Some(BlockItem::Stmt(call)) = out.pop() else {
            return;
        };
        out.push(BlockItem::Stmt(Stmt::Invoke {
            call: Box::new(call),
            landing_pad: lp.map(|name| crate::ast::Ident {
                name,
                span: func.span.clone(),
            }),
            actions,
            span: func.span.clone(),
        }));
    }
}

/// 在函数体末尾平铺发射落地垫区。
///
/// 发射顺序 = `eh_unwind_blocks` 的登记序 (由 lowering 保证确定), 因为该顺序
/// 直接决定落地垫在 `.text` 中的位置与 LSDA call-site 表里的 landing pad 偏移,
/// 必须是产物可复现的单一真源。区内**不**做结构化循环/条件恢复: 每条边都用
/// 显式 `goto`, 因此一个块只会发射一次, 且跳转目标可以是任意块
/// (其他落地垫块, 或带 `eh_reentry_labels` 标签的正常规块)。
fn rebuild_eh_blocks(func: &Function, cx: &mut RebuildCtx<'_>, out: &mut Vec<BlockItem>) {
    if func.eh_unwind_blocks.is_empty() {
        return;
    }
    // 兜底: 仍有回入点没被发射过就不能留悬空 `goto`。
    //
    // 正常情况下的 try 汇合块一定在入口可达的条带上 (lowering 把 `current_bb`
    // 切到它继续下探)。但整块 try 处于死代码 (如 `if (0) { try{...}catch{...} }`)
    // 时, 结构化重建从 entry 走不到它, 而落地垫区仍会发 catch 体里的
    // `goto try_end` —— 链接期就是 undefined reference。这里按平铺模式补发射
    // 未贴标签的回入点 (`rebuild_bb` 自己负责贴标签, 并已访问的后继直接为空),
    // 而不是只报错: 死代码不该让编译失败。
    let missing: Vec<String> = func
        .eh_reentry_labels
        .iter()
        .filter(|name| {
            func.blocks.contains_key(*name) && !cx.visited.contains(&format!("eh_label#{}", name))
        })
        .cloned()
        .collect();
    for name in missing {
        rebuild_bb(&name, cx, out, None, None);
    }
    for name in &func.eh_unwind_blocks {
        let Some(bb) = func.blocks.get(name) else {
            continue;
        };
        let bb = bb.clone();
        // 同一个落地垫块可能被多个调用点共享, 只发射一次。
        if !cx.visited.insert(name.clone()) {
            continue;
        }
        let mut items: Vec<BlockItem> = Vec::new();
        for phi in &bb.phis {
            if let Some((src, _)) = phi.incoming.first() {
                cx.env.insert(phi_name(&phi.dest), src.clone());
            }
        }
        for inst in &bb.insts {
            if is_pass_hint_inst(&inst.kind) {
                continue;
            }
            if let InstKind::PreservedDecl { var } = &inst.kind {
                items.push(BlockItem::Decl(Decl::Var(var.clone())));
                continue;
            }
            push_inst_stmt(inst, name, cx, &mut items);
        }
        match &bb.term {
            Terminator::Ret { val } => {
                let e = val
                    .as_ref()
                    .map(|o| resolve_operand(o, cx.env, cx.subst, func));
                items.push(BlockItem::Stmt(Stmt::Return {
                    val: e,
                    span: func.span.clone(),
                }));
            },
            Terminator::Jump(t) => items.push(BlockItem::Stmt(Stmt::Goto {
                target: crate::ast::Ident {
                    name: t.clone(),
                    span: func.span.clone(),
                },
                span: func.span.clone(),
            })),
            Terminator::Branch { cond, t, f } => {
                let cond_e = resolve_operand(cond, cx.env, cx.subst, func);
                let mk_goto = |n: &str| Stmt::Goto {
                    target: crate::ast::Ident {
                        name: n.to_string(),
                        span: func.span.clone(),
                    },
                    span: func.span.clone(),
                };
                items.push(BlockItem::Stmt(Stmt::If {
                    cond: cond_e,
                    then_: Box::new(mk_goto(t)),
                    else_: Some(Box::new(mk_goto(f))),
                    span: func.span.clone(),
                }));
            },
            // 落地垫区内的 `Unreachable` = 正常永远走不到 (如 `_Unwind_Resume`
            // 后的填充); 不发任何跳转, 自然落入下一段。
            // `SwitchTable` 不出现在落地垫区 (分派只按 selector 比较)。
            Terminator::Unreachable | Terminator::SwitchTable { .. } => {},
        }
        out.push(BlockItem::Stmt(Stmt::Label {
            name: crate::ast::Ident {
                name: name.clone(),
                span: func.span.clone(),
            },
            stmt: Box::new(Stmt::Block {
                b: Block {
                    items,
                    span: func.span.clone(),
                },
                span: func.span.clone(),
            }),
            span: func.span.clone(),
        }));
    }
}

fn phi_name(v: &SsaVar) -> String {
    v.name()
}

/// 识别 lower_ir `fresh_syn` 产出的合成临时值 base 名: `dcc_t<序号>_<后缀>`.
///
/// 这些名字由编译器生成、全局唯一、从不与用户局部变量重名 (`fresh_syn` 会跳过
/// 重名候选), 因此可以和 `_` 前缀名一样参与「单次使用即内联」的重建: 内联后进
/// 循环条件的表达式会在每次迭代重算, 语义正确.
pub(super) fn is_lower_ir_temp_base(base: &str) -> bool {
    let rest = match base.strip_prefix("dcc_t") {
        Some(r) => r,
        None => return false,
    };
    match rest.find('_') {
        Some(idx) => {
            let digits = &rest[..idx];
            !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
        },
        None => false,
    }
}

/// 判断某条 SSA 指令是否为优化 pass 插入的 hint/metadata (不对应真实计算).
/// 这些指令以 Store 到 `__dcc_` 前缀符号的形式存在, 例如:
///   - register_allocation: `__dcc_regalloc_done`
///   - auto_parallelization: `__dcc_parallel_hint`
///   - pgo: `__dcc_hot_hint` / `__dcc_cold_hint`
///
/// 在 SSA→AST 重建时必须丢弃: 它们向**不存在的符号**发内存写, 送到后端会被按
/// 「未声明名字」回退成本帧栈槽, 于是产出一条 向未初始化地址的写 → 运行期 SEGV
/// (旧 P11 的 `__dcc_section_hot/cold` 就是这么崩的, 该 pass 已删)。
/// 因此丢弃规则不分例外: `__dcc_` 前缀的 Store 一律是 pass 内部元数据。
pub(super) fn is_pass_hint_inst(kind: &InstKind) -> bool {
    if let InstKind::Store {
        e: Operand::SymAddr(name, _),
        ..
    } = kind
    {
        name.starts_with("__dcc_")
    } else {
        false
    }
}

/// 指令是否带有不可消除的副作用 (Call / Store / volatile InlineAsm 等)。
/// SSA→AST 重建时, 即使其结果未被使用也不能整条丢弃。
pub(super) fn inst_has_side_effect(kind: &InstKind) -> bool {
    match kind {
        InstKind::Call { .. }
        | InstKind::Invoke { .. }
        | InstKind::EhLandingPad { .. }
        | InstKind::Store { .. }
        | InstKind::IndexStore { .. }
        | InstKind::MemberStore { .. } => true,
        // volatile asm 始终保留；非 volatile asm 带输出也保留
        InstKind::InlineAsm {
            volatile, outputs, ..
        } => *volatile || !outputs.is_empty(),
        _ => false,
    }
}

/// 循环体重建时判断「自然循环之外、但只能整体返回」的块。
///
/// `find_loop_body` 只收集能沿前驱走到 latch 的块 (自然循环定义), 因此**循环体内
/// 直接 `return`** 的块 (terminator 是 `Ret`, 没有任何后继) 不在 body_set 里。
/// 旧实现在这种目标上一律发 `Stmt::Break`, 于是
/// `while (n > 0) { if (n == 3) return 42; n = n - 1; } return 0;`
/// 被重建成 `if (n==3) break;` —— **返回值 42 静默消失**, 函数返回循环之后的值
/// (C++ 后端经 `rebuild_block` 走这条路, C 前端直接建 AST 故不受影响)。
///
/// 只有当该块 ① 无 phi、② 仅有单一前驱 (即它不是多条路径的汇聚出口, 不存在
/// 需要按路径合并的值) 时, 才可以原地展开成 `Stmt::Return`; 其余情形仍按
/// `break` 处理, 保持循环出口汇聚块 (带 phi 的 `while_end`) 的既有正确行为。
pub(super) fn is_loop_exit_pure_return(target: &str, func: &Function) -> bool {
    let Some(bb) = func.blocks.get(target) else {
        return false;
    };
    if !matches!(bb.term, Terminator::Ret { .. }) {
        return false;
    }
    if !bb.phis.is_empty() {
        return false;
    }
    let mut uniq: HashSet<&str> = HashSet::new();
    for p in &bb.preds {
        uniq.insert(p.as_str());
    }
    uniq.len() == 1
}

/// SSA→AST 结构化重建 (`rebuild_bb` / `rebuild_cycle`) 的共享上下文。
///
/// 把一路沿递归透传的 5 个参数 (`func` / `doms` / `subst` / `visited` /
/// `env`) 聚成一个结构体, 让两条重建入口的签名不再超长。其中前三个是
/// 只读视角, 后两个是随重建推进的可变状态。
///
/// 分支 / `switch` 的每条路径需要各自独立的 `env`/`subst` 副本, 因此在
/// 递归调用处就地派生子上下文 (`RebuildCtx { env: &mut path_env, .. }`),
/// 语义与原先逐参数克隆传递完全一致。
struct RebuildCtx<'a> {
    /// 正在重建的 SSA 函数: 块表 / block_order / span 等只读信息
    func: &'a Function,
    /// 支配树 (块名 → 支配它的块名集合): 回边与循环识别用, 只读
    doms: &'a HashMap<String, HashSet<String>>,
    /// SSA 变量名 → 可内联表达式 (`None` = 已定义但不可内联), 只读
    subst: &'a HashMap<String, Option<Expr>>,
    /// 已整体发射过的块; 仅直线条带区 (`loop_ctx.is_none()`) 生效
    visited: &'a mut HashSet<String>,
    /// SSA 变量名 → 当前活跃的 SSA 版本, 随 phi / 指令赋值推进
    env: &'a mut HashMap<String, SsaVar>,
}

fn rebuild_bb(
    bb_name: &str,
    cx: &mut RebuildCtx<'_>,
    out: &mut Vec<BlockItem>,
    loop_ctx: Option<(&str, &HashSet<String>)>,
    // 直线条带区里的**汇聚点**: 走到它就立即返回, 由调用方 (产生分支的那个
    // `Terminator::Branch`) 在 If **之后**发射一次。仅在 `loop_ctx.is_none()`
    // 时有意义 (循环体内两条路径本来各自以 continue/break 结束)。
    stop_at: Option<&str>,
) {
    // 从上下文取出只读视角: 都是 Copy 的共享引用, 取出后与 `cx` 的可变借用
    // 互不冲突 (就地派生子上下文时仍可继续使用)。
    let func = cx.func;
    let doms = cx.doms;
    let subst = cx.subst;
    // C++ 零成本异常：落地垫块**不得被结构化重建消费**。它只由 unwinder 按 `.text`
    // 地址区间、以及其他块的 `goto` 进入，正文与标签由 `rebuild_eh_blocks` 在主体之后
    // 统一发一次。这里沿正常控制流递归进来就会把本块记进 `visited`，于是尾巴跳过发射
    // —— 「语句已发射」被当成「标签已发射」，其他回入点的 `goto` 全部悬空（实测
    // `eh_loop_and_scope_exit.cpp`：`.Ll_main_eh_resume.8` 被 3 条 `goto` 引用却零个定义，
    // 链接前 E1102）。正确发法：此处只发 `goto`，正文留给尾巴。
    //
    // 位置必须在下面那个 `visited.insert` **之前** —— 那个 insert 就是「已发射」的唯一
    // 真源，在它后面拦等于先记账再拦，尾巴照样不发（本轮实测过：插在 `blocks.get` 之后
    // 仍然 E1102）。
    if func.eh_unwind_blocks.iter().any(|p| p == bb_name) {
        out.push(BlockItem::Stmt(Stmt::Goto {
            target: crate::ast::Ident {
                name: bb_name.to_string(),
                span: func.span.clone(),
            },
            span: func.span.clone(),
        }));
        return;
    }
    // In loop-internal mode, we rebuild blocks per-path so that join points
    // (e.g. for_inc reached from both branches of an if) are not skipped.
    // Each *straight-line* path in a structured loop ends with continue or
    // break; a nested loop header reached inside the body is a cycle entry
    // whose back edge would otherwise be followed as a normal edge and
    // recurse forever (header -> body -> latch -> header -> ...). That case
    // is converged by the `is_cycle_entry` branch below (nested
    // `rebuild_cycle`), which is also why the check must run in loop-internal
    // mode, not only at top level.
    if loop_ctx.is_none() {
        // 根因修复: 以前这里直接把汇聚块内联进**第一条**被重建的路径
        // (`Jump(join)` 递归下去) 并用共享的 `visited` 把它标记成已处理,
        // 于是另一条路径重建时它返回空 (`else_: None`), If **之后**的所有
        // 语句只会在条件为真时执行 —— 静默错值/跳初始化后的全部代码.
        // 典型现场: `if (__cxa_guard_acquire(g)) { init; release; } 后续代码`
        // 使函数局部 static 只在首次调用时执行函数体 (其余调用直接返回),
        // `if (c) {...} ...` 同理 (C 路径不受影响, 它不走这个重建)。
        if let Some(s) = stop_at {
            if bb_name == s {
                return;
            }
        }
        if !cx.visited.insert(bb_name.to_string()) {
            return;
        }
    }
    let bb = match func.blocks.get(bb_name) {
        Some(b) => b,
        None => return,
    };
    // In loop-internal mode, reaching the loop header means continue.
    if let Some((lh, _)) = loop_ctx {
        if bb_name == lh {
            out.push(BlockItem::Stmt(Stmt::Continue {
                span: func.span.clone(),
            }));
            return;
        }
    }
    // Detect if this block is a cycle entry (target of a back edge).
    // If so, emit a while loop to represent the cyclic CFG.
    //
    // 这一检查在循环体内同样必须做: 到达当前循环 header 的路径在上面的
    // `bb_name == lh` 分支已经变成 `continue` 提前返回, 所以循环内模式走到
    // 这里的块若仍是回边目标, 它必然是**嵌套内层循环**的 header。以前只在
    // `loop_ctx.is_none()` 时识别环, 内层回边 (latch -> inner header) 被当作
    // 普通边继续递归展开, `header -> body -> latch -> header -> ...` 无限互
    // 递归 —— C++ 后端任意两层 for/while 嵌套都栈溢出 (根因修复)。
    // C++ 零成本 EH: 落地垫 / catch 区用 `goto` 回入正常规块, 这些块因此必须留下标签。
    //
    // 标签挂在**空块**上: 后端发射 `label:` 后紧跟空块不产生任何指令, 所以正常
    // 执行流照常落到本块的第一条语句, `goto label` 也恰好落到同一位置 —— 两者
    // 共用同一个入口点, 不需要把整个块的内容包进一层嵌套 (那会改变块内变量的
    // 作用域)。
    //
    // 重复标签 = 汇编器错误, 所以单独用一个带命名空间前缀的 `visited` key 记录
    // 「已贴过标签」, 而不是只处理直线条带区:
    //   * 直线条带区本来就由上面的 `visited` 保证每个块只展开一次, 于是这里也只会
    //     贴一次标签。
    //   * 循环体内 (`loop_ctx.is_some()`) 由 `rebuild_cycle` 按**路径**分别重建 ——
    //     这是为了让循环里的汇聚点不被某条路径吃掉, 代价是同一个汇聚块会被展开多次。
    //     以前只在这种模式下不贴标签, 于是「循环体内的 try 正常走完」的回入点成为
    //     悬空 `goto` (链接期 undefined reference)。现在按第一次贴标签并就地展开,
    //     后续路径改成 `goto label`: 两条路径本就是同一个汇聚块, 落到同一份后续代码
    //     正是它们应有的语义。
    //
    // 本检查必须在 `is_cycle_entry` 之前: 回入点恰好是循环头 (如 `while (true)` 里
    // 最后一句是 try) 时, 标签仍要发在循环之前, `goto` 才能正确重入循环。
    if func.eh_reentry_labels.iter().any(|n| n == bb_name) {
        let label_key = format!("eh_label#{}", bb_name);
        if cx.visited.insert(label_key) {
            out.push(BlockItem::Stmt(Stmt::Label {
                name: crate::ast::Ident {
                    name: bb_name.to_string(),
                    span: func.span.clone(),
                },
                stmt: Box::new(Stmt::Block {
                    b: Block {
                        items: Vec::new(),
                        span: func.span.clone(),
                    },
                    span: func.span.clone(),
                }),
                span: func.span.clone(),
            }));
        } else if loop_ctx.is_some() {
            out.push(BlockItem::Stmt(Stmt::Goto {
                target: crate::ast::Ident {
                    name: bb_name.to_string(),
                    span: func.span.clone(),
                },
                span: func.span.clone(),
            }));
            return;
        }
    }
    if is_cycle_entry(bb_name, func, doms) {
        if std::env::var("DCC_LOOPDBG").is_ok() {
            eprintln!(
                "[loopdbg] enter-cycle {bb_name} stop_at={stop_at:?} outer_loop={:?}",
                loop_ctx.map(|(h, _)| h)
            );
        }
        rebuild_cycle(bb_name, cx, out, stop_at, loop_ctx);
        return;
    }
    let bb = bb.clone();
    // 0. phi 处理: 将前驱的 SSA 值映射到 phi dest 的基础名
    for phi in &bb.phis {
        for (src, pred) in &phi.incoming {
            if pred == bb_name || cx.env.contains_key(&phi_name(&phi.dest)) {
                cx.env.insert(phi_name(&phi.dest), src.clone());
            }
        }
        if let Some((src, _)) = bb.phis.first().and_then(|p| p.incoming.first()) {
            cx.env.insert(phi_name(&phi.dest), src.clone());
        }
    }
    // 1. 翻译指令为 AST 赋值语句
    for inst in &bb.insts {
        // 跳过 SSA pass 插入的 hint/metadata 指令
        if is_pass_hint_inst(&inst.kind) {
            continue;
        }
        // PreservedDecl: 直接输出原始变量声明
        if let InstKind::PreservedDecl { var } = &inst.kind {
            out.push(BlockItem::Decl(Decl::Var(var.clone())));
            continue;
        }
        push_inst_stmt(inst, bb_name, cx, out);
    }
    // 2. 翻译 terminator -> AST 后, 继续后继
    match &bb.term {
        Terminator::Ret { val } => {
            let e = val
                .as_ref()
                .map(|o| resolve_operand(o, cx.env, subst, func));
            out.push(BlockItem::Stmt(Stmt::Return {
                val: e,
                span: func.span.clone(),
            }));
        },
        Terminator::Jump(t) => {
            if let Some((lh, body_set)) = loop_ctx {
                if t == lh {
                    out.push(BlockItem::Stmt(Stmt::Continue {
                        span: func.span.clone(),
                    }));
                } else if body_set.contains(t) {
                    rebuild_bb(t, cx, out, loop_ctx, stop_at);
                } else if matches!(
                    loop_exit_owner(t, func, lh, body_set, doms),
                    LoopExitOwner::PureReturn | LoopExitOwner::InlineOnEdge
                ) {
                    // 出口边守卫 (5 个发射点的唯一真源, 见 [`loop_exit_owner`]):
                    // 体内 `return` 块与「先做事再跳出循环」的 break 路径块都在**到达边**
                    // 展开成对应语句; 续体 (`Continuation`) 与无路可发的保守回退
                    // (`Unsupported`) 只发 `break`, 由 `rebuild_cycle` 在循环之后发一次。
                    rebuild_bb(t, cx, out, loop_ctx, stop_at);
                } else {
                    out.push(BlockItem::Stmt(Stmt::Break {
                        span: func.span.clone(),
                    }));
                }
            } else {
                rebuild_bb(t, cx, out, loop_ctx, stop_at);
            }
        },
        Terminator::Branch { cond, t, f } => {
            // 两条路径的**公共汇聚点** (沿「唯一前驱 + 无条件跳转」链向前走的
            // 第一个多入边块)。发现后: 两条路径都走到它即返回, If 之后把它作为
            // 续体发射一次 —— 否则它会被内联进第一条路径, 另一条路径直接跳过
            // (见函数开头 `stop_at` 处的根因说明)。
            //
            // 只在直线条带区做: 循环体内由 rebuild_cycle 自己处理收敛。
            let join = if loop_ctx.is_none() {
                let bj = branch_join(bb_name, t, f, func);
                let raw = if bj
                    .as_deref()
                    .is_some_and(|j| func.blocks.contains_key(j))
                {
                    bj
                } else {
                    // 虚拟后支配者占位名 (`compute_ipdoms` 的 `\u{1}[postdom-exit]`) 不是块:
                    // 拿它当汇聚点会让两条路径都**不发**汇聚块, 而 If 之后的发射又因
                    // 块不存在而空转 —— 体内有 `return` 时 (一条路径不经过任何后续块就出
                    // 函数) `from` 根本没有后支配者, 正是这种形态。此时先试「循环入口边」
                    // 那种汇聚点 (见 [`loop_entry_join`]), 再把原来的值留作兼容回退。
                    loop_entry_join(t, f, func, doms).or(bj)
                };
                // **口径对齐** (本工作包实测新查出的根因): 汇聚点必须与循环续体走同一条
                // 折叠链。`branch_join` 给的是**未折叠**的汇聚块 (如 `for.end`: 零语句桩,
                // `Jump if.end`), 而续体由 `loop_exit_plan` 给的是**折叠后**的块 (`if.end`)。
                // 两者不等时 `path_stop` 就管不住循环尾部那次续体发射: 续体被内联进**第一
                // 个**到达它的臂, 兄弟臂因共享 `visited` 拿到空 (实测 -O2 下
                // `if(k==1){for(...)}else{while(k-->0)...}` 的 else 臂丢掉整个累加并落到
                // 函数尾部返回 0)。折叠条件与 [`resolve_loop_exit_stub`] 完全同一套。
                raw
                    .filter(|j| func.blocks.contains_key(j.as_str()) && j.as_str() != bb_name)
                    .map(|j| fold_join(&j, func, stop_at))
            } else {
                None
            };
            let path_stop = join.as_deref().or(stop_at);
            if std::env::var("DCC_LOOPDBG").is_ok() {
                eprintln!(
                    "[loopdbg] branch bb={bb_name} t={t} f={f} join={join:?} stop_at={stop_at:?} outer_loop={:?}",
                    loop_ctx.map(|(h, _)| h)
                );
            }
            let cond_e = resolve_operand(cond, cx.env, subst, func);
            let mut then_items: Vec<BlockItem> = Vec::new();
            let mut then_env = cx.env.clone();
            let then_subst = subst.clone();
            match loop_ctx {
                Some((lh, _)) if t == lh => {
                    then_items.push(BlockItem::Stmt(Stmt::Continue {
                        span: func.span.clone(),
                    }));
                },
                Some((lh, body_set)) if !body_set.contains(t) => {
                    if matches!(
                        loop_exit_owner(t, func, lh, body_set, doms),
                        LoopExitOwner::PureReturn | LoopExitOwner::InlineOnEdge
                    ) {
                        rebuild_bb(
                            t,
                            &mut RebuildCtx {
                                func,
                                doms,
                                subst: &then_subst,
                                visited: &mut *cx.visited,
                                env: &mut then_env,
                            },
                            &mut then_items,
                            loop_ctx,
                            path_stop,
                        );
                    } else {
                        then_items.push(BlockItem::Stmt(Stmt::Break {
                            span: func.span.clone(),
                        }));
                    }
                },
                _ => {
                    rebuild_bb(
                        t,
                        &mut RebuildCtx {
                            func,
                            doms,
                            subst: &then_subst,
                            visited: &mut *cx.visited,
                            env: &mut then_env,
                        },
                        &mut then_items,
                        loop_ctx,
                        path_stop,
                    );
                },
            }
            let mut else_items: Vec<BlockItem> = Vec::new();
            let mut else_env = cx.env.clone();
            let else_subst = subst.clone();
            match loop_ctx {
                Some((lh, _)) if f == lh => {
                    else_items.push(BlockItem::Stmt(Stmt::Continue {
                        span: func.span.clone(),
                    }));
                },
                Some((lh, body_set)) if !body_set.contains(f) => {
                    if matches!(
                        loop_exit_owner(f, func, lh, body_set, doms),
                        LoopExitOwner::PureReturn | LoopExitOwner::InlineOnEdge
                    ) {
                        rebuild_bb(
                            f,
                            &mut RebuildCtx {
                                func,
                                doms,
                                subst: &else_subst,
                                visited: &mut *cx.visited,
                                env: &mut else_env,
                            },
                            &mut else_items,
                            loop_ctx,
                            path_stop,
                        );
                    } else {
                        else_items.push(BlockItem::Stmt(Stmt::Break {
                            span: func.span.clone(),
                        }));
                    }
                },
                _ => {
                    rebuild_bb(
                        f,
                        &mut RebuildCtx {
                            func,
                            doms,
                            subst: &else_subst,
                            visited: &mut *cx.visited,
                            env: &mut else_env,
                        },
                        &mut else_items,
                        loop_ctx,
                        path_stop,
                    );
                },
            }
            out.push(BlockItem::Stmt(Stmt::If {
                cond: cond_e,
                then_: Box::new(Stmt::Block {
                    b: Block {
                        items: then_items,
                        span: func.span.clone(),
                    },
                    span: func.span.clone(),
                }),
                else_: if else_items.is_empty() {
                    None
                } else {
                    Some(Box::new(Stmt::Block {
                        b: Block {
                            items: else_items,
                            span: func.span.clone(),
                        },
                        span: func.span.clone(),
                    }))
                },
                span: func.span.clone(),
            }));
            // 汇聚点作为 If **之后**的续体发射 (只发一次: `stop_at` 已经阻止任何
            // 一条路径把它内联进去)。若它就是外层区域的停止点, 则交给外层发射。
            if let Some(j) = join {
                if Some(j.as_str()) != stop_at {
                    rebuild_bb(&j, cx, out, loop_ctx, stop_at);
                }
            }
        },
        Terminator::Unreachable => {},
        Terminator::SwitchTable {
            idx,
            values,
            targets,
            default,
        } => {
            let idx_e = resolve_operand(idx, cx.env, subst, func);
            // 出口边发射点 (第 6 个, 本轮实测新发现): 旧代码对每个 case 目标与 default
            // **无条件**递归展开, 于是在「循环内 switch + case 落到循环外的合并块」上
            // 会把同一个续体**发两份** (一份在 case 体内, 一份在 `rebuild_cycle` 的循环
            // 之后), 而体内 `return` 块被当成续体时返回值又整个丢掉。现在与其余 5 个
            // 点走同一个 [`loop_exit_owner`] (判据与发射器的唯一真源)。
            let switch_edge_blocked = |target: &str| -> bool {
                match loop_ctx {
                    Some((lh, body_set)) => {
                        target != lh
                            && !body_set.contains(target)
                            && !matches!(
                                loop_exit_owner(target, func, lh, body_set, doms),
                                LoopExitOwner::PureReturn | LoopExitOwner::InlineOnEdge
                            )
                    },
                    None => false,
                }
            };
            let mut cases = Vec::new();
            // Each case branch rebuilds its path independently with its own
            // visited set, so that shared downstream blocks (e.g. switch_end)
            // are processed for each case rather than skipped on subsequent
            // cases due to the first case marking them visited.
            for (v, target) in values.iter().zip(targets.iter()) {
                let mut target_items: Vec<BlockItem> = Vec::new();
                let mut target_env = cx.env.clone();
                let target_subst = subst.clone();
                let mut case_visited = cx.visited.clone();
                // `None`: 每条 case 自带 visited 副本, 链上的多入边块在各条
                // case 路径里各自展开 (与旧行为一致), 因此不走 `stop_at` 机制。
                if switch_edge_blocked(target) {
                    target_items.push(BlockItem::Stmt(Stmt::Break {
                        span: func.span.clone(),
                    }));
                } else {
                    rebuild_bb(
                        target,
                        &mut RebuildCtx {
                            func,
                            doms,
                            subst: &target_subst,
                            visited: &mut case_visited,
                            env: &mut target_env,
                        },
                        &mut target_items,
                        loop_ctx,
                        None,
                    );
                }
                cases.push(Case {
                    val: Some(Expr::Int {
                        v: *v,
                        span: func.span.clone(),
                        ty: Type::Int,
                    }),
                    range_end: None,
                    body: Block {
                        items: target_items,
                        span: func.span.clone(),
                    },
                    span: func.span.clone(),
                });
            }
            // default case
            let mut def_items: Vec<BlockItem> = Vec::new();
            let mut def_env = cx.env.clone();
            let def_subst = subst.clone();
            let mut def_visited = cx.visited.clone();
            if switch_edge_blocked(default) {
                def_items.push(BlockItem::Stmt(Stmt::Break {
                    span: func.span.clone(),
                }));
            } else {
                rebuild_bb(
                    default,
                    &mut RebuildCtx {
                        func,
                        doms,
                        subst: &def_subst,
                        visited: &mut def_visited,
                        env: &mut def_env,
                    },
                    &mut def_items,
                    loop_ctx,
                    None,
                );
            }
            cases.push(Case {
                val: None,
                range_end: None,
                body: Block {
                    items: def_items,
                    span: func.span.clone(),
                },
                span: func.span.clone(),
            });
            out.push(BlockItem::Stmt(Stmt::Switch {
                expr: idx_e,
                cases,
                span: func.span.clone(),
            }));
        },
    }
}

/// Emit a while loop for a cyclic CFG region.
///
/// Strategy:
/// 1. Find the latch block (source of the back edge to header).
/// 2. Collect loop body blocks via reverse DFS from latch (not going through header).
/// 3. Header instructions are NOT emitted outside the loop; the loop condition
///    is resolved via `resolve_operand` which inlines temporary variables.
/// 4. Body blocks are emitted in order; back edges become `continue`, exits become `break`.
///
/// `outer_ctx` 是调用方所处的**外层**循环上下文 (`loop_ctx`, 顶层为 `None`):
/// 重建嵌套循环时, 循环出口的续体必须回到外层上下文 (外层循环体里的
/// 后继块仍要按外层循环展开, 落到外层 header 时发 `continue`), 而不是
/// 当作顶层直线条带。
fn rebuild_cycle(
    header: &str,
    cx: &mut RebuildCtx<'_>,
    out: &mut Vec<BlockItem>,
    stop_at: Option<&str>,
    outer_ctx: Option<(&str, &HashSet<String>)>,
) {
    // 同 `rebuild_bb`: 只读视角取出为局部, 与下面的可变上下文互不借用。
    let func = cx.func;
    let doms = cx.doms;
    let subst = cx.subst;
    let header_bb = match func.blocks.get(header) {
        Some(b) => b,
        None => return,
    };
    // Mark header as visited so we don't re-enter it via rebuild_bb
    cx.visited.insert(header.to_string());

    // Find all back edges that target the header (latch -> header).
    //
    // latch 与循环体由 [`loop_shape`] 给出 —— 与判据 `has_multi_exit_cycle` 共用**同一份**
    // 结构事实。两边各算一次就会因 `preds`/`succs` 缓存过期而看到不同的循环,
    // 那正是「判据放行但发射器无路可发」的另一半根源 (单 latch 会漏掉 continue 类
    // latch 所在的分支区域, 见 `find_loop_body_multi` 注释)。
    let (latches, body_incl_header) = match loop_shape(func, header, doms) {
        Some(v) => v,
        // 找不到回边 —— 对 cycle entry 不该发生; 退化为按普通块重建 (与旧行为一致)。
        None => return,
    };
    let body_bbs_set: HashSet<String> = body_incl_header
        .iter()
        .filter(|b| b.as_str() != header)
        .cloned()
        .collect();
    // 出口所有权计划：续体唯一化 + 提升许可 + 5 个发射点共用的同一真源。
    let plan = loop_exit_plan(func, header, &body_bbs_set, &latches, doms);

    // Handle header phi nodes: map phi dest to the entry (first incoming) version.
    // In the AST representation, there is only one variable per base name,
    // so SSA phi semantics are captured by mutable variable assignments in the body.
    for phi in &header_bb.phis {
        if let Some((entry_src, _)) = phi.incoming.first() {
            cx.env.insert(phi_name(&phi.dest), entry_src.clone());
        }
    }

    // Determine loop condition from header's branch terminator.
    // For a while-loop, the header is the condition block and we extract
    // the condition expression.  For a do-while (header is a body block
    // whose terminator is a Jump), the condition lives inside the loop
    // body and is expressed as `if (!cond) break;`.
    //
    // 提取出 `cond_expr` 的形态会**整体丢弃**节点头块的普通指令 (见下方
    // `if cond_expr.is_none()`), 因为它的答案已按 `subst` 内联进条件表达式。
    // 只有当每条指令都确实「只是被内联掉的合成临时值」时这个丢弃才不丢语义:
    // 头块里写入**具名对象**的指令一旦被丢掉就是静默错值/死循环 ——
    // `while (k-- > 0) s += k;` 的 `k_2 = k_1 - 1` 正落在节点头, 丢掉后 `k`
    // 永不递减 (实测 -O2 忙等)。
    let header_insts_droppable = header_bb.insts.iter().all(|inst| {
        is_pass_hint_inst(&inst.kind)
            || inst
                .dest
                .as_ref()
                .is_some_and(|d| matches!(subst.get(&d.name()), Some(Some(_))))
    });
    let cond_expr_raw: Option<Expr> = match &header_bb.term {
        Terminator::Branch { cond, t, f } => {
            let t_in_body = body_bbs_set.contains(t);
            let f_in_body = body_bbs_set.contains(f);
            let t_back = is_back_edge(header, t, doms);
            let f_back = is_back_edge(header, f, doms);
            let t_continues = t_in_body || t_back;
            let f_continues = f_in_body || f_back;
            if t_continues && !f_continues {
                Some(resolve_operand(cond, cx.env, subst, func))
            } else if f_continues && !t_continues {
                // Negate condition
                let cond_e = resolve_operand(cond, cx.env, subst, func);
                Some(Expr::Unary {
                    op: crate::ast::UnOp::Not,
                    e: Box::new(cond_e),
                    span: func.span.clone(),
                    ty: Type::Int,
                })
            } else {
                None
            }
        },
        _ => None, // infinite loop or non-branch header (e.g. do-while)
    };
    let cond_expr: Option<Expr> = if header_insts_droppable && plan.allow_header_lift {
        cond_expr_raw
    } else {
        None
    };

    // Determine loop body entry block(s) from header terminator.
    // For while-loop: header is cond block, body entry = the successor that's
    // inside the loop. For do-while/infinite: header is a body block, we start
    // recursing from it.
    //
    // 出口块集合**不再在这里手工收集**: 续体由 [`loop_exit_plan`] 唯一确定。
    // 旧做法 `exit_blocks.into_iter().next()` 把头块出边排最前, 而头块那条体外出边
    // 常常正好就是体内 `return` 块 ⇒ 循环之后发的是那条 `return`, 于是「出口边发射点
    // 缺守卫」与「续体取法随意」互相掩盖 (§7.32 的核心认识)。
    let mut body_entries: Vec<String> = Vec::new();
    match &header_bb.term {
        Terminator::Branch { t, f, .. } => {
            let t_in_body = body_bbs_set.contains(t);
            let f_in_body = body_bbs_set.contains(f);
            let t_back = is_back_edge(header, t, doms);
            let f_back = is_back_edge(header, f, doms);
            if t_in_body && !t_back {
                body_entries.push(t.clone());
            }
            if f_in_body && !f_back {
                body_entries.push(f.clone());
            }
        },
        Terminator::Jump(t) => {
            if body_bbs_set.contains(t) && !is_back_edge(header, t, doms) {
                body_entries.push(t.clone());
            }
        },
        Terminator::SwitchTable {
            targets, default, ..
        } => {
            for t in targets {
                if body_bbs_set.contains(t) && !is_back_edge(header, t, doms) {
                    body_entries.push(t.clone());
                }
            }
            if body_bbs_set.contains(default) && !is_back_edge(header, default, doms) {
                body_entries.push(default.clone());
            }
        },
        _ => {},
    }
    let exit_succ: Option<String> = plan.cont.clone();

    // Emit loop body using rebuild_bb in loop-internal mode.
    // Start with the header instructions if the header is a body block
    // (do-while, infinite loop). Then recurse from each body entry block;
    // rebuild_bb handles all control flow recursively, converting jumps to
    // the loop header into `continue` and jumps out of the loop into `break`.
    let mut body_items: Vec<BlockItem> = Vec::new();
    // header 自己就是一个**分支点**时 (无限循环 / 旋转后的 do-while 形态, 即
    // `cond_expr.is_none()`), 必须把它的 Branch 发成 `if (c) {…} else {…}`。
    // 旧做法是把 header 的两个 in-body 后继都归进 `body_entries` 再**顺序**展开 ——
    // 「选哪条边」的条件仍丢掉: `for (...) { if (i==1) continue; s+=i; }` 被重建为
    // 「先 i++ 再测 i<n」的两段直线代码, 循环第一圈就 return (实测 `cont(4)` 返回 0)。
    let mut branched_from_header = false;
    // 边是否还在循环内 (含头块自身 —— 回到头块就是 `continue`)。
    let side_in_loop = |target: &str| -> bool { target == header || body_bbs_set.contains(target) };
    // 出口边发射点 (5 个中的第 4/5 个): 旧做法在头块分支那里**直接发裸 `Stmt::Break`**,
    // 不过出口边守卫 —— 在「节点头本身就是分支点」的循环 (旋转后的 do-while /
    // 无限循环) 上体内 `return` 的返回值会被整个丢掉 (§7.32 未修项 1)。现在与
    // `rebuild_bb` 那三处走同一个 [`loop_exit_owner`] (判据与发射器的唯一真源)。
    let expand_on_edge = |target: &str| -> bool {
        !side_in_loop(target)
            && matches!(
                loop_exit_owner(target, func, header, &body_bbs_set, doms),
                LoopExitOwner::PureReturn | LoopExitOwner::InlineOnEdge
            )
    };
    if cond_expr.is_none() {
        for inst in &header_bb.insts {
            if is_pass_hint_inst(&inst.kind) {
                continue;
            }
            emit_rebuild_inst(inst, &mut body_items, cx.env, subst, func);
        }
        if let Terminator::Branch { cond, t, f } = &header_bb.term {
            let both_in_body = body_bbs_set.contains(t) && body_bbs_set.contains(f);
            let neither_is_header = t != header && f != header;
            // 头块指令不可丢弃时 (`while (k-- > 0)` 这种「条件里带自增」的形态)
            // 必须连 header 的 Branch 一起发成 `if (c) {…} else {…}`: 那种情况下
            // 退出判据无法被提到 `while (...)` 里 (提上去就要丢掉头块的写)。
            // 第三个触发条件 `!plan.allow_header_lift`: 提升被计划否掉时 (头块那条出口边
            // **不是**续体), 必须把 `Branch` 当语句发出去, 那条出口边才有发射点 —— 否则
            // 体内 `return` 随提升整个丢掉 (§7.32 那 14 条错值的根因)。
            let has_exit_side = !(side_in_loop(t) && side_in_loop(f));
            if (both_in_body && neither_is_header)
                || !header_insts_droppable
                || (!plan.allow_header_lift && has_exit_side)
            {
                branched_from_header = true;
                let cond_e = resolve_operand(cond, cx.env, subst, func);
                let mut then_items: Vec<BlockItem> = Vec::new();
                let mut else_items: Vec<BlockItem> = Vec::new();
                let mut then_env = cx.env.clone();
                let mut else_env = cx.env.clone();
                let then_subst = subst.clone();
                let else_subst = subst.clone();
                let lctx = Some((header, &body_bbs_set));
                if side_in_loop(t) {
                    rebuild_bb(
                        t,
                        &mut RebuildCtx {
                            func,
                            doms,
                            subst: &then_subst,
                            visited: &mut *cx.visited,
                            env: &mut then_env,
                        },
                        &mut then_items,
                        lctx,
                        stop_at,
                    );
                } else if expand_on_edge(t) {
                    rebuild_bb(
                        t,
                        &mut RebuildCtx {
                            func,
                            doms,
                            subst: &then_subst,
                            visited: &mut *cx.visited,
                            env: &mut then_env,
                        },
                        &mut then_items,
                        lctx,
                        stop_at,
                    );
                } else {
                    then_items.push(BlockItem::Stmt(Stmt::Break {
                        span: func.span.clone(),
                    }));
                }
                if side_in_loop(f) {
                    rebuild_bb(
                        f,
                        &mut RebuildCtx {
                            func,
                            doms,
                            subst: &else_subst,
                            visited: &mut *cx.visited,
                            env: &mut else_env,
                        },
                        &mut else_items,
                        lctx,
                        stop_at,
                    );
                } else if expand_on_edge(f) {
                    rebuild_bb(
                        f,
                        &mut RebuildCtx {
                            func,
                            doms,
                            subst: &else_subst,
                            visited: &mut *cx.visited,
                            env: &mut else_env,
                        },
                        &mut else_items,
                        lctx,
                        stop_at,
                    );
                } else {
                    else_items.push(BlockItem::Stmt(Stmt::Break {
                        span: func.span.clone(),
                    }));
                }
                body_items.push(BlockItem::Stmt(Stmt::If {
                    cond: cond_e,
                    then_: Box::new(Stmt::Block {
                        b: Block {
                            items: then_items,
                            span: func.span.clone(),
                        },
                        span: func.span.clone(),
                    }),
                    else_: if else_items.is_empty() {
                        None
                    } else {
                        Some(Box::new(Stmt::Block {
                            b: Block {
                                items: else_items,
                                span: func.span.clone(),
                            },
                            span: func.span.clone(),
                        }))
                    },
                    span: func.span.clone(),
                }));
            }
        }
    }
    if !branched_from_header {
        for entry in &body_entries {
            let mut entry_env = cx.env.clone();
            let entry_subst = subst.clone();
            rebuild_bb(
                entry,
                &mut RebuildCtx {
                    func,
                    doms,
                    subst: &entry_subst,
                    visited: &mut *cx.visited,
                    env: &mut entry_env,
                },
                &mut body_items,
                Some((header, &body_bbs_set)),
                stop_at,
            );
        }
    }
    // Emit the while loop
    let cond = cond_expr.unwrap_or(Expr::Int {
        v: 1,
        span: func.span.clone(),
        ty: Type::Int,
    });
    out.push(BlockItem::Stmt(Stmt::While {
        cond,
        body: Box::new(Stmt::Block {
            b: Block {
                items: body_items,
                span: func.span.clone(),
            },
            span: func.span.clone(),
        }),
        span: func.span.clone(),
    }));
    // Continue from exit block (回到外层循环上下文, 顶层调用时 outer_ctx 为 None)
    if std::env::var("DCC_LOOPDBG").is_ok() {
        eprintln!(
            "[loopdbg] cycle header={header} cont={exit_succ:?} stop_at={stop_at:?} outer_loop={:?}",
            outer_ctx.map(|(h, _)| h)
        );
    }
    if let Some(exit) = exit_succ {
        rebuild_bb(&exit, cx, out, outer_ctx, stop_at);
    }
}

/// 从 `start` 沿「唯一前驱 + 无条件跳转」链向前走, 返回第一个多条入边的块。
///
/// 链上没有汇聚点 (碰到 return / 二分支 / 缺失块) 或回到已访问块时返回 `None`。
fn walk_to_join(
    start: &str,
    func: &Function,
    preds: &HashMap<String, usize>,
    seen: &mut HashSet<String>,
) -> Option<String> {
    let mut cur = start.to_string();
    loop {
        if !seen.insert(cur.clone()) {
            return None;
        }
        if preds.get(&cur).copied().unwrap_or(0) > 1 {
            return Some(cur);
        }
        let bb = func.blocks.get(&cur)?;
        match &bb.term {
            Terminator::Jump(s) => cur = s.clone(),
            _ => return None,
        }
    }
}

/// 汇聚点的折叠: 沿「无 phi + 零真实语句 + 无条件 `Jump`」链向前走, 遇到**外层停止点**
/// `stop_at`、块不存在、有 phi/有真实语句、或多目标终结符即停。
///
/// 与 [`resolve_loop_exit_stub`] 同一套条件 (那里额外要求不折回循环体内, 因为它的对象
/// 是出口边; 汇聚链在循环外, 没有 header/body 可比), 目的是让「这条边到底谁发」与
/// 「落点发几次」用**同一个块名**说话 —— 只要两边口径不一致, `stop_at` 就管不住
/// 循环尾部那次续体发射 (见 `rebuild_bb` 的 `join` 计算)。
fn fold_join(target: &str, func: &Function, stop_at: Option<&str>) -> String {
    let mut cur = target.to_string();
    let mut seen: HashSet<String> = HashSet::new();
    loop {
        if !seen.insert(cur.clone()) {
            return cur;
        }
        if Some(cur.as_str()) == stop_at {
            return cur;
        }
        let Some(bb) = func.blocks.get(&cur) else {
            return cur;
        };
        if !bb.phis.is_empty() || !bb.insts.iter().all(|i| is_pass_hint_inst(&i.kind)) {
            return cur;
        }
        let Terminator::Jump(n) = &bb.term else {
            return cur;
        };
        cur = n.clone();
    }
}

/// 二路分支的**循环入口形态**的汇聚点: 一侧是循环头, 另一侧正是那个循环的续体。
///
/// 为什么 [`branch_join`] 判不出它: 两条路径的 `walk_to_join` 各自立刻返回不同的块
/// (循环头本身入边数就 >1: preheader + latch), 于是回落到 `from` 的后支配者; 而体内有
/// `return` 时 `from` 根本没有真正的后支配者 (一条路径直接从 `return` 出函数), 拿到的是
/// 虚拟 EXIT 占位名。不补这一条, 循环的续体就会被**内联进进入循环的那条路径** (发在 `if`
/// 里面), 另一条路径什么也没有: 实测 `for(int i=0;i<n;i++){if(i==2)return 7;} return 1;`
/// 在 `n=0` 时返回 0 而不是 1 (落出路径后面空无一物, 函数直接落到序言)。
///
/// 续体由 [`loop_exit_plan`] 唯一给出 —— 与本工作包的所有权模型同一个真源: 「续体只能发
/// 一次」在入口边上就等价于「续体就是汇聚点」。
fn loop_entry_join(
    t: &str,
    f: &str,
    func: &Function,
    doms: &HashMap<String, HashSet<String>>,
) -> Option<String> {
    for (entry, other) in [(t, f), (f, t)] {
        if !is_cycle_entry(entry, func, doms) {
            continue;
        }
        let Some((latches, body)) = loop_shape(func, entry, doms) else {
            continue;
        };
        let body_excl: HashSet<String> = body
            .iter()
            .filter(|b| b.as_str() != entry)
            .cloned()
            .collect();
        let plan = loop_exit_plan(func, entry, &body_excl, &latches, doms);
        if plan.cont.as_deref() == Some(other) {
            return Some(other.to_string());
        }
    }
    None
}

/// 二路分支的公共汇聚块: 先走「唯一前驱 + 无条件跳转」快路径, 不行再取
/// 分支块的**直接后支配者** (见 [`compute_ipdoms`])。
///
/// `from` 是发出这个 `Terminator::Branch` 的块名。
pub(super) fn branch_join(from: &str, t: &str, f: &str, func: &Function) -> Option<String> {
    if t == f {
        return Some(t.to_string());
    }
    let preds = predecessor_counts(func);
    let jt = walk_to_join(t, func, &preds, &mut HashSet::new());
    let jf = walk_to_join(f, func, &preds, &mut HashSet::new());
    if let (Some(a), Some(b)) = (&jt, &jf) {
        if a == b {
            return Some(a.clone());
        }
    }
    // 快路径判不出凝聚点 (典型: 某一条路径自己又是一个分支 —— 嵌套三元
    // `a ? b : c ? d : e`, 或三元分支里的 `&&`/`||` 短路链), 用后支配者这个
    // 正确判据。它比启发式宽, 因此必须同时要求凝聚点不是分支块自己。
    let ipdom = compute_ipdoms(func);
    let j = ipdom.get(from)?;
    if j == from {
        return None;
    }
    Some(j.clone())
}

/// Emit a single SSA instruction as an AST statement during rebuild.
fn emit_rebuild_inst(
    inst: &Inst,
    out: &mut Vec<BlockItem>,
    env: &mut HashMap<String, SsaVar>,
    subst: &HashMap<String, Option<Expr>>,
    func: &Function,
) {
    // PreservedDecl: 直接输出原始变量声明 (跳过 SSA 化)
    if let InstKind::PreservedDecl { var } = &inst.kind {
        out.push(BlockItem::Decl(Decl::Var(var.clone())));
        return;
    }
    if let Some(d) = &inst.dest {
        env.insert(d.name(), d.clone());
        let ssa_name = d.name();
        if let Some(Some(_)) = subst.get(&ssa_name) {
            return;
        }
        let rhs = inst_to_expr_resolved(&inst.kind, subst, &inst.span, func, Some(&d.ty));
        let assign = Expr::Assign {
            op: crate::ast::BinOp::Add,
            l: Box::new(Expr::Ident {
                name: crate::ast::Ident {
                    // 赋值左端用版本自己的局部名（否则读写会落到不同变量上）
                    name: func.local_splits.ast_name(d),
                    span: d.span.clone(),
                },
                span: d.span.clone(),
                ty: d.ty.clone(),
            }),
            r: Box::new(rhs),
            span: inst.span.clone(),
            ty: d.ty.clone(),
        };
        out.push(BlockItem::Stmt(Stmt::Expr {
            e: assign,
            span: inst.span.clone(),
        }));
    } else {
        let e = inst_to_expr_resolved(&inst.kind, subst, &inst.span, func, None);
        out.push(BlockItem::Stmt(Stmt::Expr {
            e,
            span: inst.span.clone(),
        }));
    }
}

