// src/ssa/structure.rs - 进 SSA 之前的 AST 侧政策与降级
//
//   - 结构化控制流判据 is_structured*: 含 asm-goto/switch 的函数不进 SSA, 降级回 AST pass。
//     goto/label/computed-goto **从 2026-10 第三轮起放行**：它们不再被拒，而是由
//     `has_unstructured_flow` 标记为「非结构化」，在 `opt::run_ssa_passes` 里走
//     analysis-only 路径（借 SSA 算活性/家，函数体本身不重建）。
//   - **两个问题两个主人（搞混就会错值，本轮评审又撞上一次）**：
//       * 「这个函数能不能**进** SSA」由 `src/opt.rs::func_has_unsafe_ssa_pattern` 决定
//         （它看的是危险形态：位域 RMW、可写取址、vararg…；第二参数 `allow_static_locals`
//         就是 analysis-only 通道对静态局部的豁免）；
//       * 「进了之后能不能**落地重建**」由本文件的 [`has_unstructured_flow`] 决定
//         （不可约 CFG 不能走 `rebuild_cycle`）。
//     即：准入与本文件无关，本文件只回答「能不能交还给 AST」。
//   - switch -> if-chain 降级: lower_switches_in_func{,_block,_stmt}/lower_switch_stmt/
//     SwitchArm/arm_ends_with_exit/has_switch_bound_*/bound_flow/expr_has_loop_control/
//     is_dense_switch/expr_ty。
//   - 不放: SSA -> AST 的重建 (那是 rebuild.rs / expr_rebuild.rs)。

use crate::ast::{
    ArraySize, BinOp, Block, BlockItem, Case, Decl, Expr, FuncDecl, Ident, InitItem, QualType,
    Stmt, Type,
};
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
    body.items.iter().any(item_has_unstructured_flow)
}

/// 单个 `BlockItem` 是否携带非结构化流。存在理由：`Stmt::Switch` 的 prelude 是一条
/// `Vec<BlockItem>`而不是一个 `Block`，没这个入口就得为它造一个临时 `Block`。
fn item_has_unstructured_flow(item: &BlockItem) -> bool {
    match item {
        BlockItem::Stmt(s) => stmt_has_unstructured_flow(s),
        BlockItem::Decl(Decl::Func(f)) => f.body.as_ref().is_some_and(has_unstructured_flow),
        _ => false,
    }
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
        Stmt::Switch {
            cases, prelude, ..
        } => {
            // prelude（第一个 `case` 之前的语句与标签）也是 switch 体的一部分：那里出现的
            // `foo:` / `goto foo` 同样让本函数不是结构化的。以前只看 cases，「唯一的标签
            // 恰好在 prelude 里」的函数会被误判成结构化并进重建通道。
            cases.iter().any(|c| has_unstructured_flow(&c.body))
                || prelude.iter().any(item_has_unstructured_flow)
        },
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
        span,
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
    // prelude 非空就不降级（与上面跳表那条同一个判据，`switch_jt_plan` 也拒）：C 语义里
    // 第一个 `case` 之前的语句**不会被顺序执行**（控制流直接跳到匹配的子句），而降级只能
    // 把它拼成「链之前的顺序语句」，等于让它在每次进入 switch 时都跑一遍。
    // 实测 `int s(int x){ switch (x) { side = 99; case 1: return 10; default: return 20; } }`
    // 在 -O2/-O3 把 side 写成 99，而 -O0 与 gcc 都是 0（gcc 还会给 `-Wswitch-unreachable`）。
    // 发射器那条路把 prelude 排在分派链**之后**（只能被 fall-through/标签命中），
    // 那是唯一对的形状，所以这里只能放弃降级。
    if !prelude.is_empty() {
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
    // 3'. 有副作用的 selector 必须先求值一次。下面这条链是把条件表达式**逐臂 clone**
    //     出去的（`sel.clone()` 那两处），于是 `switch (i++)` 会按「臂位 + 1」次自增，
    //     并且每个臂比较的是不同的值 ⇒ 选错臂。实测 `tests/fixtures/opt/
    //     switch_selector_effects.c` 在 -O2 得 `pick=-1 i=5`（gcc 是 `pick=300 i=3`），
    //     而 -O0 走 AST 的 `Stmt::Switch` 臂只求值一次所以是对的 —— 静默错值只在
    //     开了降级的档位上露，而且值落在链首时连 -O2 都看不出来（短路替后面的臂
    //     省掉了重复求值），所以夹具里特意放了一条例在末环的 `sw_lastarm`。
    //     这里的处置：把 selector 存进一个临时局部，链里只 clone 那个名字（纯 lvalue，
    //     clone 多少次都无害）。名字按源码位置取 ⇒ 同一 TU 内唯一，且不引入全局
    //     计数器（产物字节必须可复现，`check_reproducible_output.sh` 在卡这件事）。
    //     **无副作用的 selector 保持原形态**，因而绝大多数 switch 零产物漂移。
    //     残留：`switch (volatile 对象)` 仍会逐臂重读一次。含 volatile 的函数本来就被
    //     `opt::func_refs_volatile` 整函数挡在 SSA 外，这条不在本判据的射程内。
    let (sel, sel_decl): (Expr, Option<BlockItem>) = if crate::semantics::expr_has_side_effects(
        expr
    ) {
        let name = format!("__sw_sel_{}_{}_{}", span.line, span.col, span.len);
        let decl = BlockItem::Decl(Decl::Var(crate::opt::plain_var_decl(
            expr_ty(expr),
            name.clone(),
            Some(expr.clone()),
            span,
        )));
        (
            Expr::Ident {
                name: Ident {
                    name,
                    span: span.clone(),
                },
                span: span.clone(),
                ty: expr_ty(expr),
            },
            Some(decl),
        )
    } else {
        (expr.clone(), None)
    };
    // 4. 从后往前拼 if-else 链。
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
                l: Box::new(sel.clone()),
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
                        l: Box::new(sel.clone()),
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
    // 5. selector 临时量必须排在链之前：C 里控制表达式先于任何臂体求值一次。
    //    有 prelude（`switch(x){ foo: case 1: … }`）时也仍在 prelude 之前：那些标签
    //    本来就是「跳过控制表达式直接进臂体」的目标，把临时量放在它们后面会留
    //    下一个未初始化值，放在前面顶多少走一次初始化（与今天的重复求值相比仍是对的）。
    let body_stmt = match sel_decl {
        Some(d) => Stmt::Block {
            b: Block {
                items: vec![d, BlockItem::Stmt(body_stmt)],
                span: span.clone(),
            },
            span: span.clone(),
        },
        None => body_stmt,
    };
    // prelude 已在函数开头拒掉（见上面那条），这里不可能遇到。
    let _ = prelude;
    Some(body_stmt)
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

// ===========================================================================
// 阶扵6 批1：稠密 switch → 跳表分派（只给「已经在 AST 通道上」的非结构化函数）
// ===========================================================================
//
// 为什么不做到结构化函数上：结构化函数的 switch 会被上面的 `lower_switches_in_func`
// 降级成 if 链交给 SSA 重建；这里若塞进 label / goto / 静态局部表，重建端要能忠实
// 还原它们（那正是 §0 判死的那件事）。非结构化函数（`opt::run_ssa_passes` 里
// `unstructured == true`）不重建，AST 改写成什么样就发什么样 ⇒ `&&label` + 静态局部
// 表 + `goto *` 就是 Lua 解释器每天在走的那一条通路，两个通道（ELF 与 .s 文本）与
// 并行发射都不需要新增任何机制。反过来说，MIPS 那个 `lower_switch_with_jump_table`
// 是 `_cases` 未用的 TODO 空壳（且 `shared/control_flow.rs` 全仓零个
// `impl ControlFlowBackend`），不能「接回」。
//
// 开关 `DCC_SWITCHTABLE`：**默认开**。这个默认值只由实测决定（批0c/批1）：
// 链/表两臂在同一轮内交替跑 15 轮（`scripts/ab_pair.sh`，被测是 sqlite3.c 的 20 万行负载），
// median(r) = 0.7245、95%CI = [0.713, 0.755]、15/15 轮同向，同二进制 dup 臂
// 与表臂只差 0.3% ⇒ 这是 -27.5% 的真实收益，不是布局运气（同批实测：纯布局
// 就能让 SQLite 动 10%，所以必须同轮配对）。要关掉只写 `DCC_SWITCHTABLE=0`：
// 链那一臂保留作为回退定位的把手，不是默认产物。

fn jumptable_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| match std::env::var("DCC_SWITCHTABLE") {
        Ok(v) => {
            let t = v.trim().to_string();
            t.is_empty() || t != "0"
        },
        Err(_) => true,
    })
}

/// 一个能忠实换成跳表的 switch 的解算结果。
struct JtPlan {
    /// 表以 `值 - min` 为下标，所以只需要 `count` 个槽。
    min: i128,
    count: usize,
    /// 每个槽跳去的子句下标；`usize::MAX` = 该槽是洞，去 default（无 default 则去出口）。
    slots: Vec<usize>,
    has_default: bool,
}

/// 能不能上表。判据一条一条给，因为每一条后面都要被反面夹具钉住：
/// 区间 case、非整型 selector、带 prelude（`switch(x){ foo: case 1: }` 的 `foo:`）的
/// 一律不上 —— prelude 那些标签的语义是「跳过控制表达式直接进臂体」，表分派绕过它。
fn switch_jt_plan(expr: &Expr, cases: &[Case], prelude: &[BlockItem]) -> Option<JtPlan> {
    if !prelude.is_empty() {
        return None;
    }
    match expr.ty() {
        Type::Char
        | Type::SChar
        | Type::UChar
        | Type::Bool
        | Type::Short
        | Type::UShort
        | Type::Int
        | Type::UInt
        | Type::Long
        | Type::ULong
        | Type::LongLong
        | Type::ULongLong => {},
        // Enum 的底层型另判；__int128 / 指针 / 聚合都不能做表下标。
        Type::Enum(_, _, Some(inner)) => match inner.base {
            Type::Int | Type::UInt | Type::Long | Type::ULong => {},
            _ => return None,
        },
        _ => return None,
    }
    let mut named = 0usize;
    let mut has_default = false;
    let mut min = i128::MAX;
    let mut max = i128::MIN;
    let mut vals: Vec<(i128, usize)> = Vec::new();
    for (i, c) in cases.iter().enumerate() {
        match &c.val {
            None => has_default = true,
            Some(v) => {
                if c.range_end.is_some() {
                    return None;
                }
                let k = crate::const_eval::eval(v).ok()? as i128;
                named += 1;
                vals.push((k, i));
                if k < min {
                    min = k;
                }
                if k > max {
                    max = k;
                }
            },
        }
    }
    if named < 8 {
        return None;
    }
    let span = max - min + 1;
    if span > 4096 {
        return None;
    }
    // 密度阈值沿用 `is_dense_switch` 那个式子（以前是个从未被调用的死函数），
    // 现在它在真正会用到的位置上生效：洞比具名臂多到 3 倍就不上表，否则表把
    // 比较链「换成更多的 cache 行」而不是减少扫描。
    if (span as usize) > 4 * (named + 1) {
        return None;
    }
    let count = span as usize;
    let mut slots = vec![usize::MAX; count];
    for (k, i) in &vals {
        slots[(*k - min) as usize] = *i;
    }
    Some(JtPlan { min, count, slots, has_default })
}

/// 把单个 `Stmt::Switch` 改写成「静态局部表 + 界检 + goto *表[下标]」。
/// 返回 `None` = 不上表，保持原来的 `Stmt::Switch`（发链）。
/// `hoisted_table`：表声明递出去，由调用方拼到函数体顶层（原因见下面写入处的注释）。
/// `seq`：本函数内的改写序号。**名字不能按源码位置取**：sqlite3.c 里 111 个不同
/// 的 switch 共享同一个 `Span`（实测 `__swt_78929_3` 在 `sqlite3Select` 一个函数里
/// 就被 hoist 了 8 次），而标签名只到函数为止隔离（发射器加 `.Ll_<func>_`），
/// 所以同函数内同名 ⇒ `self.labels` 后写覆盖前写 ⇒ 表项跳到错的臂体
/// （实测链到 sqlite3 里一跑就是 SIGSEGV，倒在 `finalizeAggFunctions`）。序号由
/// 本函数单线程递经一遍定序的 `Vec` 走完 ⇒ 可复现（`check_reproducible_output.sh` 卡）。
fn lower_switch_to_jumptable(
    s: &Stmt,
    hoisted_table: &mut Option<BlockItem>,
    seq: &mut u32,
) -> Option<Stmt> {
    let Stmt::Switch { expr, cases, prelude, span } = s else {
        return None;
    };
    let plan = switch_jt_plan(expr, cases, prelude)?;
    // `continue` 绑到本 switch 的（`switch { case 1: continue; }`）不改写：
    // 上面 if-chain 那条路本来就直接放弃（`has_switch_bound_continue`），这里跟进同一行为。
    for c in cases {
        if has_switch_bound_continue(&c.body.items) {
            return None;
        }
    }
    let sp = span.clone();
    let uid = format!("n{seq}");
    *seq += 1;
    let tbl = format!("__swt_{}", uid);
    let selv = format!("__swv_{}", uid);
    let idxv = format!("__swi_{}", uid);
    let endlab = format!("__swe_{}", uid);
    let deflab = format!("__swd_{}", uid);
    let clab = |i: usize| format!("__swc_{}_{}", uid, i);
    let id = |name: &str, ty: Type| Expr::Ident {
        name: Ident {
            name: name.to_string(),
            span: sp.clone(),
        },
        span: sp.clone(),
        ty,
    };
    let labid = |name: &str| Ident {
        name: name.to_string(),
        span: sp.clone(),
    };

    // 1) 表：`static const void *const tbl[count] = { &&L, &&L, ... }`。
    //    走的是 Lua `disptab` 那条已跑通的静态局部通道（编译期进 data，
    //    槽里是 `.text` 标号地址；不是运行时逐元素初始化）。
    let void_ptr = Type::Ptr(Box::new(QualType::new(Type::Void)));
    let elem = QualType::new(void_ptr.clone());
    let tbl_ty = Type::Array(Box::new(elem), ArraySize::Const(plan.count));
    let inits: Vec<InitItem> = plan
        .slots
        .iter()
        .map(|&si| {
            let target = if si == usize::MAX {
                if plan.has_default {
                    deflab.clone()
                } else {
                    endlab.clone()
                }
            } else {
                clab(si)
            };
            InitItem::Expr(Expr::LabelAddr {
                label: labid(&target),
                span: sp.clone(),
                ty: void_ptr.clone(),
            })
        })
        .collect();
    let mut tbl_decl = crate::opt::plain_var_decl(tbl_ty.clone(), tbl.clone(), None, &sp);
    tbl_decl.is_static = true;
    tbl_decl.init = Some(Expr::InitList {
        inits,
        span: sp.clone(),
        ty: tbl_ty.clone(),
    });

    // 2) selector 只求值一次（类型用它自己的）。
    let sel_decl = crate::opt::plain_var_decl(expr.ty(), selv.clone(), Some(expr.clone()), &sp);

    // 3) 下标的初值：`(unsigned long)__swv - (unsigned long)min`。先减再界检，那么负数与
    //    越界同成一个无符号「往回绕」的大值，一条 `ja`/`jbe` 就能一起挡掉。
    //    min==0（VdbeExec 就是这种）时整个减法不发：否则每步多一条 `sub rax,0`。
    let sel_u = Expr::Cast {
        ty: Type::ULong,
        e: Box::new(id(&selv, expr.ty())),
        span: sp.clone(),
    };
    let idx_init = if plan.min == 0 {
        sel_u
    } else {
        let min_cast = Expr::Cast {
            ty: Type::ULong,
            e: Box::new(Expr::Int {
                v: plan.min,
                span: sp.clone(),
                ty: Type::Long,
            }),
            span: sp.clone(),
        };
        Expr::Binop {
            op: BinOp::Sub,
            l: Box::new(sel_u),
            r: Box::new(min_cast),
            span: sp.clone(),
            ty: Type::ULong,
        }
    };
    let idx_decl = crate::opt::plain_var_decl(Type::ULong, idxv.clone(), Some(idx_init), &sp);

    // 4) 界检：`if (idx > count-1) goto <default 或 出口>;`
    let bound = Expr::Binop {
        op: BinOp::Gt,
        l: Box::new(id(&idxv, Type::ULong)),
        r: Box::new(Expr::Int {
            v: (plan.count - 1) as i128,
            span: sp.clone(),
            ty: Type::ULong,
        }),
        span: sp.clone(),
        ty: Type::Int,
    };
    let out_target = if plan.has_default { deflab.clone() } else { endlab.clone() };
    let check = Stmt::If {
        cond: bound,
        then_: Box::new(Stmt::Goto {
            target: labid(&out_target),
            span: sp.clone(),
        }),
        else_: None,
        span: sp.clone(),
    };

    // 5) 分派：`goto *tbl[idx];`（与 Lua 的 `vmdispatch` 同形）。
    let dispatch = Stmt::GotoIndirect {
        expr: Box::new(Expr::Index {
            arr: Box::new(id(&tbl, tbl_ty.clone())),
            idx: Box::new(id(&idxv, Type::ULong)),
            span: sp.clone(),
            ty: void_ptr.clone(),
        }),
        span: sp.clone(),
    };

    // 6) 臂体：按**源码物理序**紧跟标签发射（与发射器那条链同一条纪律：物理相邻
    //    == C 的 fall-through；把 default 排到末尾会让上一个臂误落入它）。
    let mut items: Vec<BlockItem> = Vec::new();
    for (i, c) in cases.iter().enumerate() {
        let name = match &c.val {
            Some(_) => clab(i),
            None => deflab.clone(),
        };
        items.push(BlockItem::Stmt(Stmt::Label {
            name: labid(&name),
            stmt: Box::new(Stmt::Block {
                b: Block {
                    items: c.body.items.clone(),
                    span: c.span.clone(),
                },
                span: c.span.clone(),
            }),
            span: c.span.clone(),
        }));
    }
    items.push(BlockItem::Stmt(Stmt::Label {
        name: labid(&endlab),
        stmt: Box::new(Stmt::Block {
            b: Block {
                items: Vec::new(),
                span: sp.clone(),
            },
            span: sp.clone(),
        }),
        span: sp.clone(),
    }));

    // 表与两个临时量必须在臂体之前；`break` 绑本 switch 的靠 `do { … } while (0)`
    // 兜住（与 if-chain 那条路同一个处置，否则 `break` 会跳到外层循环去）。
    let mut head = vec![
        BlockItem::Decl(Decl::Var(sel_decl)),
        BlockItem::Decl(Decl::Var(idx_decl)),
        BlockItem::Stmt(check),
        BlockItem::Stmt(dispatch),
    ];
    // 表必须递到**函数体顶层**，不能留在原块里：静态局部的 data blob 由语义阶段填的
    // `static_local_map` 预先登记（`pre_register_static_locals`），而本函数是优化阶段
    // 才造出这个声明的——嵌在循环体里时它拿不到登记，发射出来就是一句
    // `lea r11,[rip+0x0]` 且位移永不回填（实测 VdbeExec 上直接 SIGSEGV）。
    // 函数体顶层是 Lua `disptab` 每天在用的位置，那条通路是通的。
    *hoisted_table = Some(BlockItem::Decl(Decl::Var(tbl_decl)));
    head.append(&mut items);
    let mut body = Stmt::Block {
        b: Block {
            items: head,
            span: sp.clone(),
        },
        span: sp.clone(),
    };
    if cases.iter().any(|c| has_switch_bound_break(&c.body.items)) {
        body = Stmt::Do {
            body: Box::new(body),
            cond: Expr::Int {
                v: 0,
                span: sp.clone(),
                ty: Type::Int,
            },
            span: sp.clone(),
        };
    }
    Some(body)
}

/// 入口：只在调用方已经确定本函数**不走 SSA 重建**（`unstructured`）时被叫到。
pub fn lower_switches_to_jumptable_in_func(f: &mut FuncDecl) -> bool {
    if !jumptable_enabled() {
        return false;
    }
    let Some(body) = f.body.as_mut() else {
        return false;
    };
    let mut hoisted: Vec<BlockItem> = Vec::new();
    let mut seq: u32 = 0;
    let changed = lower_jt_in_block(body, &mut hoisted, &mut seq);
    // 诊断窗口：表名、数量与函数名一起打。上面那些「同一函数出现两张同名表」的现象
    // 只能靠这个看，靠读代码推不出来（推了两次都推错）。
    if std::env::var("DCC_SWITCHTABLE_DUMP").is_ok() && !hoisted.is_empty() {
        for h in &hoisted {
            if let BlockItem::Decl(Decl::Var(v)) = h {
                eprintln!("dcc: switch 跳表 hoist：func={} table={}", f.name.name, v.name.name);
            }
        }
    }
    // 表声明全部拼到函数体最前（静态局部在顶层才能拿到 `pre_register_static_locals`
    // 的登记）。多个 switch 时各自一张表，位置相邻即可。
    if !hoisted.is_empty() {
        body.items.splice(0..0, hoisted);
    }
    changed
}

fn lower_jt_in_block(block: &mut Block, hoisted: &mut Vec<BlockItem>, seq: &mut u32) -> bool {
    let mut changed = false;
    for i in 0..block.items.len() {
        if let BlockItem::Stmt(Stmt::Switch { .. }) = &block.items[i] {
            // clone 而不是 take：不上表时原样留着，不能把语句挖空。一个 switch
            // 节点的体积相比整个函数体可以忽略，不值得为此引入占位变体。
            let BlockItem::Stmt(s) = block.items[i].clone() else {
                unreachable!()
            };
            let mut tbl: Option<BlockItem> = None;
            if let Some(repl) = lower_switch_to_jumptable(&s, &mut tbl, seq) {
                block.items[i] = BlockItem::Stmt(repl);
                if let Some(t) = tbl {
                    hoisted.push(t);
                }
                changed = true;
            }
        }
    }
    for item in &mut block.items {
        match item {
            BlockItem::Stmt(s) => lower_jt_in_stmt(s, hoisted, seq),
            BlockItem::Decl(Decl::Func(f)) => {
                if let Some(b) = f.body.as_mut() {
                    lower_jt_in_block(b, hoisted, seq);
                }
            },
            _ => {},
        }
    }
    changed
}

fn lower_jt_in_stmt(s: &mut Stmt, hoisted: &mut Vec<BlockItem>, seq: &mut u32) {
    match s {
        Stmt::Block { b, .. } => {
            lower_jt_in_block(b, hoisted, seq);
        },
        Stmt::If { then_, else_, .. } => {
            lower_jt_in_stmt(then_, hoisted, seq);
            if let Some(e) = else_ {
                lower_jt_in_stmt(e, hoisted, seq);
            }
        },
        Stmt::While { body, .. } | Stmt::Do { body, .. } | Stmt::For { body, .. } => {
            lower_jt_in_stmt(body, hoisted, seq);
        },
        Stmt::Label { stmt, .. } => lower_jt_in_stmt(stmt, hoisted, seq),
        Stmt::Switch { cases, .. } => {
            for c in cases.iter_mut() {
                lower_jt_in_block(&mut c.body, hoisted, seq);
            }
        },
        _ => {},
    }
}
