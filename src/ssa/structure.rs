// src/ssa/structure.rs - 进 SSA 之前的 AST 侧政策与降级
//
//   - 结构化控制流判据 is_structured*: 含 asm-goto/switch 的函数不进 SSA, 降级回 AST pass。
//     goto/label/computed-goto **从 2026-10 第三轮起放行**：它们不再被拒，而是由
//     `has_unstructured_flow` 标记为「非结构化」，在 `opt::run_ssa_passes` 里走
//     analysis-only 路径（借 SSA 算活性/家，函数体本身不重建）。
//   - switch -> if-chain 降级: lower_switches_in_func{,_block,_stmt}/lower_switch_stmt/
//     SwitchArm/arm_ends_with_exit/has_switch_bound_*/bound_flow/expr_has_loop_control/
//     is_dense_switch/expr_ty。
//   - 不放: SSA -> AST 的重建 (那是 rebuild.rs / expr_rebuild.rs)。

use crate::ast::{BinOp, Block, BlockItem, Case, Decl, Expr, FuncDecl, Stmt, Type};
use crate::diagnostics::Span;

/// 探测 `FuncDecl::body` 是否含 asm-goto/switch（结构化重建表达不了这两类）。
///
/// goto/label/computed-goto 不在判据里：`build.rs` 已经能把它们建成 CFG
/// （label BB + `Terminator::Dispatch`），限制已经从「进不了 SSA」改成「不重建」
/// —— 后者由 [`has_unstructured_flow`] 单独回答。
pub(super) fn is_structured(body: &Block) -> bool {
    structured_reject(body).is_none()
}

/// `is_structured` 的「为什么」版本：返回第一处无法在 IR 里忠实例化的形态。
///
/// 只服务于 `DCC_SSA_BAIL=1` 的诊断：`is_structured` 返回 false 时函数会**默默**
/// 留在 AST 通道上（没有 bail 文本），而「没进 SSA」与「进了但没优化到」在产物上
/// 长得一模一样。两者只靠读代码区分不是办法。
pub fn structured_reject(body: &Block) -> Option<String> {
    body.items.iter().find_map(|item| match item {
        BlockItem::Stmt(s) => stmt_reject(s),
        BlockItem::Decl(Decl::Func(f)) => {
            f.body.as_ref().and_then(structured_reject)
        },
        _ => None,
    })
}

fn stmt_reject(s: &Stmt) -> Option<String> {
    match s {
        // asm-goto 与（没能降级的）switch 仍用 AST 层 pass：前者是无法在 IR 里
        // 表达的标签集合，后者是真正的 fallthrough/continue 语义。
        // goto/label/computed-goto 见文件头：已从准入判据里移出。
        Stmt::AsmGoto { .. } => Some("asm goto".to_string()),
        Stmt::Switch { .. } => Some("switch（含 fallthrough / 区间 case / 绑定 continue，未能降级）".to_string()),
        Stmt::If { then_, else_, .. } => {
            stmt_reject(then_).or_else(|| else_.as_ref().and_then(|e| stmt_reject(e)))
        },
        Stmt::While { body, .. } | Stmt::Do { body, .. } => stmt_reject(body),
        Stmt::For { body, .. } => stmt_reject(body),
        Stmt::Block { b, .. } => structured_reject(b),
        _ => None,
    }
}

/// 函数体里有没有 goto / label / computed-goto —— 也就是「SSA→AST 结构化重建
/// 无法忠实表达」的控制流。
///
/// 这条判据决定 SSA 通道的**落地方式**，不再决定准入：
///   * 有 ⇒ analysis-only：建 IR、算活性与家，把家写回**原封不动的 AST**，不重建；
///   * 无 ⇒ 照旧：跑 pass、重建、交付。
///
/// 为什么不直接把非结构化 CFG 重建出来：重建器的 `while`/`if` 恢复算法以结构化
/// CFG 为前提（`rebuild_cycle` 按循环头展开路径）。把不可约 CFG 塞进去会得到一条
/// 「能编译、跑得动、但语义已经变了」的产物 —— 比 `luaV_execute` 拿不到寄存器更糟。
pub fn has_unstructured_flow(body: &Block) -> bool {
    body.items.iter().any(|item| match item {
        BlockItem::Stmt(s) => stmt_has_unstructured_flow(s),
        BlockItem::Decl(Decl::Func(f)) => {
            f.body.as_ref().is_some_and(|b| has_unstructured_flow(b))
        },
        _ => false,
    })
}

fn stmt_has_unstructured_flow(s: &Stmt) -> bool {
    match s {
        Stmt::Goto { .. } | Stmt::GotoIndirect { .. } | Stmt::Label { .. } => true,
        Stmt::If { then_, else_, .. } => {
            stmt_has_unstructured_flow(then_)
                || else_.as_ref().is_some_and(|e| stmt_has_unstructured_flow(e))
        },
        Stmt::While { body, .. } | Stmt::Do { body, .. } => stmt_has_unstructured_flow(body),
        Stmt::For { init, body, .. } => {
            init.as_ref().is_some_and(|i| stmt_has_unstructured_flow(i))
                || stmt_has_unstructured_flow(body)
        },
        Stmt::Block { b, .. } => has_unstructured_flow(b),
        Stmt::Switch { cases, .. } => cases
            .iter()
            .any(|c| has_unstructured_flow(&c.body)),
        Stmt::Invoke { call, .. } => stmt_has_unstructured_flow(call),
        _ => false,
    }
}

// ============================================================
// Switch -> If-chain 降级 pass
// ============================================================

/// 将函数体中的 switch 语句转换为 if-else 链.
/// 密集 case 值的 switch 保留原样, 由 SSA Builder 生成 SwitchTable 终结符.
/// 这样含 switch 的函数也能进入 SSA 化路径.
pub fn lower_switches_in_func(f: &mut FuncDecl) -> bool {
    if let Some(body) = &mut f.body {
        lower_switches_in_block(body)
    } else {
        false
    }
}

/// 递归扫描 block，将所有 switch 语句替换为 if-else 链.
fn lower_switches_in_block(block: &mut Block) -> bool {
    let mut modified = false;
    let mut new_items = Vec::with_capacity(block.items.len());
    for mut item in block.items.drain(..) {
        match &mut item {
            BlockItem::Stmt(s) => {
                if let Some(new_stmt) = lower_switch_stmt(s) {
                    *s = new_stmt;
                    modified = true;
                }
                // 递归处理嵌套 block
                lower_switches_in_stmt(s);
            },
            BlockItem::Decl(d) => {
                if let Decl::Func(f) = d {
                    if let Some(body) = &mut f.body {
                        if lower_switches_in_block(body) {
                            modified = true;
                        }
                    }
                }
            },
        }
        new_items.push(item);
    }
    block.items = new_items;
    modified
}

/// 递归处理语句中的嵌套 block.
fn lower_switches_in_stmt(s: &mut Stmt) {
    match s {
        Stmt::If { then_, else_, .. } => {
            lower_switches_in_stmt(then_);
            if let Some(e) = else_ {
                lower_switches_in_stmt(e);
            }
        },
        Stmt::While { body, .. } | Stmt::Do { body, .. } => {
            lower_switches_in_stmt(body);
        },
        Stmt::For { body, .. } => {
            lower_switches_in_stmt(body);
        },
        Stmt::Block { b, .. } => {
            lower_switches_in_block(b);
        },
        // 嵌套 switch: 外层降级完成后留下的内层 switch 同样要处理，否则一个
        // 「switch 嵌在 case 体里」的函数会整块失去 SSA 化 (`is_structured` 见
        // Switch 即拒)，而它的内层 switch 本身是完全可以忠实降级的。
        Stmt::Switch { cases, .. } => {
            for c in cases.iter_mut() {
                lower_switches_in_block(&mut c.body);
            }
        },
        Stmt::Label { stmt, .. } => {
            lower_switches_in_stmt(stmt);
        },
        _ => {},
    }
}

/// 一个 if-chain 分支: 「命中这些 case 值 (或 default)」 + 「分支体」。
struct SwitchArm {
    /// `None` 含义上的默认分支由 `is_default` 表达; 有 `is_default` 时 `vals` 无用。
    vals: Vec<Expr>,
    is_default: bool,
    body: Vec<BlockItem>,
    span: Span,
}

/// 将单个 switch 语句转换为 if-else 链。
///
/// 返回 `None` = 本 switch 不能用 if-chain 忠实表达 ⇒ **放弃降级**，函数带着
/// `Stmt::Switch` 走 AST 路径 (`is_structured` 对 Switch 返回 false，因此不进 SSA；
/// 这比降级错语义好)。放弃的形态只有两类：真正的 fallthrough (含 `[[fallthrough]]`
/// / GNU `case lo ... hi:` 区间)，以及绑定到本 switch 的 `continue`。
///
/// 为什么必须判这两类、又为什么必须包一层 `do { … } while (0)` —— 旧实现直接
/// 把 `case.body` 拼成 `if (e == v) { body } else …`，有两处静默误编译：
///
///   1. **break 改绑**：`break` 原本跳出 switch，摊成 if-chain 后它跳出的是
///      **外层循环**。实测 `for(i<3){switch(i){case 0:break;case 1:s+=1;break;
///      default:s+=2;break;}}` 在 -O2 返回 0 而不是 3：第一圈 `i==0` 的 `break`
///      直接把循环于掉了。这条只在「循环里含 switch」时才发作，而循环此前被
///      `func_has_loop` 闸门挡在 SSA 外，所以长期没被观测到。
///      ⇒ 现在把整条链包进 `do { … } while (0)`：`break` 仍然「跳出整个 switch」。
///   2. **fallthrough 丢失**：C 的 `case 1: x; case 2: y;` 会连着执行，if-chain
///      只会执行 `x`。空体标签 (`case 1: case 2: …`) 是合法的「共享分支」，按值
///      合并即可；非空体且没自带退出语义的就是真 fallthrough ⇒ 放弃降级。
fn lower_switch_stmt(s: &Stmt) -> Option<Stmt> {
    let Stmt::Switch {
        expr,
        cases,
        prelude,
        span: _,
    } = s
    else {
        return None;
    };
    if cases.is_empty() {
        return None;
    }
    // GNU 区间 case 无法用 `==` 表达。
    if cases.iter().any(|c| c.range_end.is_some()) {
        return None;
    }
    // 1. 按源码顺序合并「空体标签」: `case 1: case 2: body` ⇒ 一个分支两个值。
    let mut arms: Vec<SwitchArm> = Vec::new();
    let mut pending_vals: Vec<Expr> = Vec::new();
    let mut pending_default = false;
    for c in cases {
        match &c.val {
            Some(v) => pending_vals.push(v.clone()),
            None => pending_default = true,
        }
        if c.body.items.is_empty() {
            continue;
        }
        arms.push(SwitchArm {
            vals: std::mem::take(&mut pending_vals),
            is_default: std::mem::replace(&mut pending_default, false),
            body: c.body.items.clone(),
            span: c.span.clone(),
        });
    }
    if arms.is_empty() {
        // 整个 switch 只有空体标签 ⇒ 什不发生 (default 不存在时就是无操作)。
        return Some(Stmt::Block {
            b: Block {
                items: Vec::new(),
                span: cases[0].span.clone(),
            },
            span: cases[0].span.clone(),
        });
    }
    // 2. fallthrough / switch-bound continue 判定。
    for (i, arm) in arms.iter().enumerate() {
        if has_switch_bound_continue(&arm.body) {
            return None;
        }
        let is_last_chain_arm = i + 1 == arms.len();
        if !is_last_chain_arm && !arm_ends_with_exit(&arm.body) {
            return None;
        }
        // 没有 default 时，最后一个分支“落空”后还会继续 switch 后面的代码 —— 与
        // if-chain 末尾无 else 同构，无需额外判定；但有 default 时 default 必须是
        // 链的最后一环，否则它前面的分支「落空」会跑到错误的目标。
        if arm.is_default && !is_last_chain_arm {
            return None;
        }
    }
    let need_guard = arms.iter().any(|a| has_switch_bound_break(&a.body));
    // 3. 从后往前拼 if-else 链。
    let mut current_else: Option<Box<Stmt>> = None;
    for arm in arms.iter().rev() {
        let body = Stmt::Block {
            b: Block {
                items: arm.body.clone(),
                span: arm.span.clone(),
            },
            span: arm.span.clone(),
        };
        current_else = Some(Box::new(if arm.is_default || arm.vals.is_empty() {
            body
        } else {
            let mut cond = Expr::Binop {
                op: BinOp::EqEq,
                l: Box::new(expr.clone()),
                r: Box::new(arm.vals[0].clone()),
                span: arm.span.clone(),
                ty: expr_ty(expr),
            };
            for v in &arm.vals[1..] {
                cond = Expr::Binop {
                    op: BinOp::Or,
                    l: Box::new(cond),
                    r: Box::new(Expr::Binop {
                        op: BinOp::EqEq,
                        l: Box::new(expr.clone()),
                        r: Box::new(v.clone()),
                        span: arm.span.clone(),
                        ty: expr_ty(expr),
                    }),
                    span: arm.span.clone(),
                    ty: Type::Int,
                };
            }
            Stmt::If {
                cond,
                then_: Box::new(body),
                else_: current_else.take(),
                span: arm.span.clone(),
            }
        }));
    }
    let chain = current_else.map(|s| *s)?;
    // 前驱段 (`switch(x){ foo: case 1: ... }` 中 foo 等) 必须原样保留在链之前,
    // 否则 if-chain 化后标签被吞掉 ⇒ goto 悬空 (E1102)。
    let body_stmt = if !need_guard {
        chain
    } else {
        // 4. `do { 链 } while (0)`: 把绑定到 switch 的 `break` 留在「跳出整个 switch」
        //    这个位置上。否则 `break` 在 if-chain 里会绑到外层循环 (上面根因 1)。
        let zero = Expr::Int {
            v: 0,
            span: cases[0].span.clone(),
            ty: Type::Int,
        };
        Stmt::Do {
            body: Box::new(Stmt::Block {
                b: Block {
                    items: vec![BlockItem::Stmt(chain)],
                    span: cases[0].span.clone(),
                },
                span: cases[0].span.clone(),
            }),
            cond: zero,
            span: cases[0].span.clone(),
        }
    };
    if prelude.is_empty() {
        Some(body_stmt)
    } else {
        let span = cases[0].span.clone();
        Some(Stmt::Block {
            b: Block {
                items: [prelude.clone(), vec![BlockItem::Stmt(body_stmt)]].concat(),
                span: span.clone(),
            },
            span,
        })
    }
}

/// 分支体是否 自带「离开 switch」的语义 (fallthrough 判定)。
fn arm_ends_with_exit(items: &[BlockItem]) -> bool {
    let Some(last) = items.last() else {
        return false;
    };
    matches!(
        last,
        BlockItem::Stmt(
            Stmt::Break { .. }
                | Stmt::Return { .. }
                | Stmt::Goto { .. }
                | Stmt::GotoIndirect { .. }
                | Stmt::AsmGoto { .. }
        )
    )
}

/// 体内是否存在「绑定到本 switch」的 `break` (更内层的循环/switch 会屏蔽它)。
fn has_switch_bound_break(items: &[BlockItem]) -> bool {
    items.iter().any(|it| match it {
        BlockItem::Stmt(Stmt::Break { .. }) => true,
        BlockItem::Stmt(s) => bound_flow(s, false),
        BlockItem::Decl(_) => false,
        _ => true,
    })
}

fn has_switch_bound_continue(items: &[BlockItem]) -> bool {
    items.iter().any(|it| match it {
        BlockItem::Stmt(s) => bound_flow(s, true),
        _ => false,
    })
}

/// `want_continue` = 找 `continue` (屏蔽层同样不计算在内); 否则找 `break`。
///
/// 更内层的 `for`/`while`/`do` 体会自已的 `break`/`continue`，`switch` 体会自已
/// `break` (但 `continue` 仍穿透它绑定到更外层循环)，所以屏蔽位必须分开。
fn bound_flow(s: &Stmt, want_continue: bool) -> bool {
    let stmt = |st: &Stmt| bound_flow(st, want_continue);
    match s {
        Stmt::Break { .. } => !want_continue,
        Stmt::Continue { .. } => want_continue,
        Stmt::Block { b, .. } => b.items.iter().any(|i| match i {
            BlockItem::Stmt(st) => stmt(st),
            _ => false,
        }),
        Stmt::If {
            cond, then_, else_, ..
        } => {
            expr_has_loop_control(cond, want_continue)
                || stmt(then_)
                || else_.as_ref().map_or(false, |e| stmt(e))
        },
        Stmt::For { init, .. } => {
            // 内层循环屏蔽 break; continue 在 for-init (声明) 里不可能出现。
            if want_continue {
                init.as_ref().map_or(false, |i| stmt(i))
            } else {
                false
            }
        },
        Stmt::While { body, .. } | Stmt::Do { body, .. } => {
            if want_continue {
                false
            } else {
                bound_flow(body, false)
            }
        },
        Stmt::Switch { cases, expr, .. } => {
            expr_has_loop_control(expr, want_continue)
                || cases.iter().any(|c| {
                    c.body.items.iter().any(|i| match i {
                        BlockItem::Stmt(st) => bound_flow(st, want_continue),
                        _ => false,
                    })
                })
        },
        _ => false,
    }
}

/// 表达式里的 `break`/`continue` 只能来自语句表达式 `({...})` —— 保守报「有」。
fn expr_has_loop_control(e: &Expr, _want_continue: bool) -> bool {
    match e {
        Expr::StmtExpr { .. } => true,
        _ => false,
    }
}

/// 判断 switch 的 case 值是否密集 (适合跳转表优化).
/// 密集条件: max - min < 4 * count
fn is_dense_switch(cases: &[Case]) -> bool {
    let mut vals: Vec<i128> = Vec::new();
    for c in cases {
        if let Some(Expr::Int { v, .. }) = &c.val {
            vals.push(*v);
        }
    }
    if vals.len() < 2 {
        return false;
    }
    let min_val = *vals.iter().min().unwrap();
    let max_val = *vals.iter().max().unwrap();
    (max_val - min_val) < 4 * vals.len() as i128
}

/// 获取表达式的类型 (简化版).
fn expr_ty(e: &Expr) -> Type {
    match e {
        Expr::Int { ty, .. } => ty.clone(),
        Expr::Float { ty, .. } => ty.clone(),
        Expr::String { ty, .. } => ty.clone(),
        Expr::Char { ty, .. } => ty.clone(),
        Expr::Ident { ty, .. } => ty.clone(),
        Expr::Binop { ty, .. } => ty.clone(),
        Expr::Unary { ty, .. } => ty.clone(),
        Expr::Call { ty, .. } => ty.clone(),
        Expr::Assign { ty, .. } => ty.clone(),
        Expr::Cond { ty, .. } => ty.clone(),
        Expr::Cast { ty, .. } => ty.clone(),
        Expr::SizeofTy { ty, .. } => ty.clone(),
        Expr::SizeofExpr { .. } => Type::ULong,
        Expr::Index { ty, .. } => ty.clone(),
        Expr::Member { ty, .. } => ty.clone(),
        Expr::AddrOf { ty, .. } => ty.clone(),
        Expr::Deref { ty, .. } => ty.clone(),
        Expr::CompoundLit { ty, .. } => ty.clone(),
        Expr::InitList { ty, .. } => ty.clone(),
        Expr::StmtExpr { ty, .. } => ty.clone(),
        Expr::Comma { ty, .. } => ty.clone(),
        Expr::ComplexLit { ty, .. } | Expr::ComplexIntLit { ty, .. } => ty.clone(),
        Expr::Real { ty, .. } => ty.clone(),
        Expr::Imag { ty, .. } => ty.clone(),
        Expr::VaArg { ty, .. } => ty.clone(),
        Expr::Offsetof { result_ty, .. } => result_ty.clone(),
        Expr::AlignofTy { .. } => Type::ULong,
        Expr::AlignofExpr { .. } => Type::ULong,
        Expr::PostInc { ty, .. }
        | Expr::PostDec { ty, .. }
        | Expr::PreInc { ty, .. }
        | Expr::PreDec { ty, .. } => ty.clone(),
        Expr::AsmExpr { ty, .. } => ty.clone(),
        Expr::LabelAddr { ty, .. } => ty.clone(),
        _ => Type::Int,
    }
}
