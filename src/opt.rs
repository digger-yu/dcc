// src/opt.rs - typed IR 优化 passes
// 第一性原理: 优化在 IR 层做数据流/常量传播，后端只负责指令选择。

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::ast::{BinOp, Block, BlockItem, Decl, Expr, FuncDecl, Program, Stmt};
use crate::const_eval;
use crate::diagnostics::Span;
use crate::ir::Module;

// ---------------------------------------------------------------------------
// Inline policy constants
//
// These govern the inliner's behaviour at each optimisation level.
// The Linux kernel relies on `__always_inline` / `__attribute__((always_inline))`
// being honoured unconditionally, and on `__attribute__((noinline))` preventing
// inlining.  Getting these wrong can cause functional correctness issues
// (e.g. percpu access macros, bit-op inline functions).
// ---------------------------------------------------------------------------

/// Maximum call-chain depth for speculative (non-forced) inlining.
/// `always_inline` / `flatten` bypass this limit.
const INLINE_MAX_DEPTH: u32 = 8;

// ---------------------------------------------------------------------------
// Inline decision tracing
//
// Set DCC_INLINE_TRACE=1 in the environment to emit one line per inline
// decision to stderr.  Useful for debugging kernel builds where
// __always_inline / noinline correctness is critical.
// ---------------------------------------------------------------------------

static INLINE_TRACE_ENABLED: OnceLock<bool> = OnceLock::new();

fn inline_trace_enabled() -> bool {
    *INLINE_TRACE_ENABLED.get_or_init(|| {
        std::env::var("DCC_INLINE_TRACE")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    })
}

/// Emit a single trace line for an inline decision.
/// Format: `dcc-inline: <decision> <callee> [reason]`
fn trace_inline_decision(callee: &str, decision: &str, reason: &str) {
    if inline_trace_enabled() {
        eprintln!(
            "dcc-inline: {} {} ({})",
            decision, callee, reason,
        );
    }
}

/// 按 -O 级别运行 IR 优化。
///
/// 优化语义约束:
/// - `-fno-strict-aliasing` (默认): 不做类型双关假设 (TBAA). 当前别名分析
///   (P4 Steensgaard) 是类型不敏感的, 不区分不同指针类型的别名关系, 已
///   保守处理. 未来若添加 TBAA 需检查 target.strict_aliasing.
/// - `-fno-strict-overflow` (默认): 不假设有符号溢出为未定义行为. 常量折
///   叠使用 wrapping 算术, 循环优化不做基于无溢出的范围推断. 未来若添
///   加基于有符号溢出 UB 的优化需检查 target.strict_overflow.
/// - 常量折叠 (fold_constants): 编译期常量使用 wrapping_add/sub/mul,
///   与 C 标准整数语义一致 (模算术).
///
/// 内联策略:
/// - `-O0`: 仅内联 `always_inline` / `flatten` 标记的函数 (内核正确性
///   依赖, 如 percpu 宏). 不受尺寸限制.
/// - `-O1-O2`: 仅 `always_inline` / `flatten` (未来计划加入成本模型
///   驱动的 `static inline` 内联).
/// - `-O3`: 额外启发式内联小型 `static` 零参函数 (cost ≤ 20 AST nodes).
/// - `noinline` 属性始终被尊重 (除非与 `always_inline` 冲突, 此时
///   `always_inline` 优先, 与 GCC 行为一致).
/// -  speculative 内联深度上限 `INLINE_MAX_DEPTH` (8). 递归函数
///   通过 callee-name 检测阻止直接递归内联.
pub fn optimize(module: &mut Module, level: u8) {
    // 内联: O0-O2 仅 always_inline/flatten; O3 额外启发式
    function_inline(module, level);
    if level == 0 {
        return;
    }
    fold_constants(module);
    if level >= 2 {
        prune_dead_stores(module);
        // Note: dead asm elimination is intentionally NOT performed.
        // GCC treats all inline asm as potentially having unknown side effects,
        // so DCE preserves asm statements even without outputs or clobbers.
        // We follow this convention for compatibility.
        fold_constant_branches(module);
        eliminate_unreachable(module);
        crate::ssa_gvn::run(module, level);
        crate::loop_opt::run(module, level);
        // M16: 跨函数 DCE (call graph reachability)
        cross_function_dce(module);
    }
    if level >= 2 {
        // M16: SSA pass pipeline (strength reduction, IVOP, TCO, etc.)
        run_ssa_passes(module, level);
    }
}

/// M16: 把 AST -> SSA, 跑 ssa_passes::run, 再 SSA -> AST 重建。
/// 含 goto / label / switch 的函数自动跳过 (走 is_structured 检测)。
/// 含循环 (while/for/do) 的函数也跳过: SSA builder 当前不支持 phi 节点,
/// 循环变量版本无法在回边合并。
fn run_ssa_passes(module: &mut Module, level: u8) {
    let prog = module.source_mut();
    let mut modified = false;
    for decl in &mut prog.decls {
        if let Decl::Func(f) = decl {
            // 跳过含循环的函数 (SSA builder 无 phi 节点支持)
            if func_has_loop(f) {
                continue;
            }
            // 跳过含数组/取址/指针解引用的函数: SSA→AST 重建对这些模式
            // 尚未完全正确, 会产生错误的代码 (数组地址丢失, 栈指针被覆盖等).
            if func_has_unsafe_ssa_pattern(f) {
                continue;
            }
            if let Some(mut ssa) = crate::ssa::build_from_func(f) {
                crate::ssa_passes::run(&mut ssa, level);
                *f.body.as_mut().unwrap() = crate::ssa::rebuild_block(&ssa);
                modified = true;
            }
        }
    }
    let _ = modified;
}

/// 检测函数体是否包含循环 (while/for/do-while)。
fn func_has_loop(f: &FuncDecl) -> bool {
    f.body.as_ref().is_some_and(block_has_loop)
}

fn block_has_loop(b: &Block) -> bool {
    b.items.iter().any(|item| match item {
        BlockItem::Stmt(s) => stmt_has_loop(s),
        _ => false,
    })
}

fn stmt_has_loop(s: &Stmt) -> bool {
    match s {
        Stmt::While { .. } | Stmt::For { .. } | Stmt::Do { .. } => true,
        Stmt::Block { b, .. } => block_has_loop(b),
        Stmt::If { then_, else_, .. } => {
            stmt_has_loop(then_) || else_.as_ref().is_some_and(|e| stmt_has_loop(e))
        }
        _ => false,
    }
}

/// 检测函数是否包含 SSA→AST 重建当前无法正确处理的模式:
///   - 数组类型声明 (int a[4])
///   - 取址操作 (&x)
///   - 指针解引用 (*p)
///   - 数组索引 (a[i])
///
/// 这些模式涉及地址计算, SSA 重建可能产生错误的地址/栈操作.
fn func_has_unsafe_ssa_pattern(f: &FuncDecl) -> bool {
    f.body.as_ref().is_some_and(block_has_unsafe_ssa_pattern)
}

fn block_has_unsafe_ssa_pattern(b: &Block) -> bool {
    b.items.iter().any(|item| match item {
        BlockItem::Decl(Decl::Var(v)) => {
            // 数组类型声明
            matches!(v.ty, crate::ast::Type::Array(_, _))
                || v.init.as_ref().is_some_and(expr_has_unsafe_ssa_pattern)
        }
        BlockItem::Stmt(s) => stmt_has_unsafe_ssa_pattern(s),
        _ => false,
    })
}

fn stmt_has_unsafe_ssa_pattern(s: &Stmt) -> bool {
    match s {
        Stmt::Expr { e, .. } | Stmt::Return { val: Some(e), .. } => expr_has_unsafe_ssa_pattern(e),
        // asm goto 和 asm 表达式包含无法 SSA 化的副作用
        Stmt::AsmGoto { .. } => true,
        Stmt::If {
            cond, then_, else_, ..
        } => {
            expr_has_unsafe_ssa_pattern(cond)
                || stmt_has_unsafe_ssa_pattern(then_)
                || else_
                    .as_ref()
                    .is_some_and(|e| stmt_has_unsafe_ssa_pattern(e))
        }
        Stmt::Block { b, .. } => block_has_unsafe_ssa_pattern(b),
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
            expr_has_unsafe_ssa_pattern(cond) || stmt_has_unsafe_ssa_pattern(body)
        }
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            init.as_ref()
                .is_some_and(|i| stmt_has_unsafe_ssa_pattern(i))
                || cond.as_ref().is_some_and(expr_has_unsafe_ssa_pattern)
                || step.as_ref().is_some_and(expr_has_unsafe_ssa_pattern)
                || stmt_has_unsafe_ssa_pattern(body)
        }
        Stmt::Switch { expr, cases, .. } => {
            expr_has_unsafe_ssa_pattern(expr)
                || cases.iter().any(|c| block_has_unsafe_ssa_pattern(&c.body))
        }
        _ => false,
    }
}

fn expr_has_unsafe_ssa_pattern(e: &Expr) -> bool {
    match e {
        Expr::AddrOf { .. } | Expr::Deref { .. } | Expr::Index { .. } => true,
        // Inline asm 已显式表示为 SSA 节点。调度器使用其 clobber
        // 元数据判断哪些指令可以跨越 asm。
        Expr::AsmExpr { .. } => false,
        Expr::Binop { l, r, .. } => {
            expr_has_unsafe_ssa_pattern(l) || expr_has_unsafe_ssa_pattern(r)
        }
        Expr::Unary { e, .. } | Expr::Cast { e, .. } => expr_has_unsafe_ssa_pattern(e),
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            expr_has_unsafe_ssa_pattern(cond)
                || expr_has_unsafe_ssa_pattern(then_)
                || expr_has_unsafe_ssa_pattern(else_)
        }
        Expr::Call { callee, args, .. } => {
            expr_has_unsafe_ssa_pattern(callee) || args.iter().any(expr_has_unsafe_ssa_pattern)
        }
        Expr::Assign { l, r, .. } => {
            expr_has_unsafe_ssa_pattern(l) || expr_has_unsafe_ssa_pattern(r)
        }
        Expr::Comma { l, r, .. } => {
            expr_has_unsafe_ssa_pattern(l) || expr_has_unsafe_ssa_pattern(r)
        }
        _ => false,
    }
}

/// M16: 跨函数 DCE - 基于 call graph 标记不可达 internal static 函数
fn cross_function_dce(module: &mut Module) {
    let prog = module.source_mut();
    let mut funcs: Vec<FuncDecl> = Vec::new();
    let mut others: Vec<Decl> = Vec::new();
    let mut extra_roots = Vec::new();
    for d in prog.decls.drain(..) {
        match d {
            Decl::Func(f) => funcs.push(f),
            Decl::Var(v) => {
                if let Some(init) = &v.init {
                    collect_global_func_roots(init, &mut extra_roots);
                }
                others.push(Decl::Var(v));
            }
            Decl::GlobalAsm {
                ref outputs,
                ref inputs,
                ..
            } => {
                for e in outputs.iter().chain(inputs.iter()) {
                    collect_global_func_roots(e, &mut extra_roots);
                }
                others.push(d);
            }
            other => others.push(other),
        }
    }
    crate::ssa_passes::cross_function_dce_with_roots(&mut funcs, true, &extra_roots);
    let mut all: Vec<Decl> = others;
    for f in funcs {
        all.push(Decl::Func(f));
    }
    prog.decls = all;
}

fn collect_global_func_roots(expr: &Expr, out: &mut Vec<String>) {
    match expr {
        Expr::Ident { name, ty, .. } => {
            if matches!(ty, crate::ast::Type::Func(_, _)) {
                out.push(name.name.clone());
            }
        }
        Expr::AddrOf { e, .. } => collect_global_func_roots(e, out),
        Expr::Cast { e, .. }
        | Expr::Unary { e, .. }
        | Expr::SizeofExpr { e, .. }
        | Expr::AlignofExpr { e, .. }
        | Expr::Deref { e, .. }
        | Expr::PostInc { e, .. }
        | Expr::PostDec { e, .. }
        | Expr::PreInc { e, .. }
        | Expr::PreDec { e, .. }
        | Expr::Real { e, .. }
        | Expr::Imag { e, .. }
        | Expr::VaArg { ap: e, .. } => collect_global_func_roots(e, out),
        Expr::Binop { l, r, .. } | Expr::Assign { l, r, .. } | Expr::Comma { l, r, .. } => {
            collect_global_func_roots(l, out);
            collect_global_func_roots(r, out);
        }
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            collect_global_func_roots(cond, out);
            collect_global_func_roots(then_, out);
            collect_global_func_roots(else_, out);
        }
        Expr::Index { arr, idx, .. } => {
            collect_global_func_roots(arr, out);
            collect_global_func_roots(idx, out);
        }
        Expr::Member { expr, .. } => collect_global_func_roots(expr, out),
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            for init in inits {
                match init {
                    crate::ast::InitItem::Expr(e) => collect_global_func_roots(e, out),
                    crate::ast::InitItem::Designated { value, .. } => {
                        collect_global_func_roots(value, out)
                    }
                }
            }
        }
        Expr::AsmExpr {
            outputs, inputs, ..
        } => {
            for e in outputs.iter().chain(inputs.iter()) {
                collect_global_func_roots(e, out);
            }
        }
        _ => {}
    }
}

fn fold_constants(module: &mut Module) {
    let prog = module.source_mut();
    // FENV_ACCESS ON 时浮点运算依赖运行时舍入模式, 编译期折叠会改变语义
    // (C99 F.8): 折叠器跳过浮点二元运算折叠.
    let fenv_access = prog.fenv_access;
    fold_program(prog, fenv_access);
}

fn fold_program(prog: &mut Program, fenv_access: bool) {
    for decl in &mut prog.decls {
        if let Decl::Func(f) = decl {
            if let Some(body) = &mut f.body {
                fold_block(&mut body.items, fenv_access);
            }
        }
    }
}

fn fold_block(items: &mut [BlockItem], fenv_access: bool) {
    for item in items.iter_mut() {
        match item {
            BlockItem::Stmt(s) => fold_stmt(s, fenv_access),
            BlockItem::Decl(Decl::Var(v)) => {
                if let Some(init) = &mut v.init {
                    *init = fold_expr(init.clone(), fenv_access);
                }
            }
            _ => {}
        }
    }
}

fn fold_stmt(s: &mut Stmt, fenv_access: bool) {
    match s {
        Stmt::If {
            cond, then_, else_, ..
        } => {
            *cond = fold_expr(cond.clone(), fenv_access);
            fold_stmt(then_, fenv_access);
            if let Some(e) = else_ {
                fold_stmt(e, fenv_access);
            }
        }
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
            *cond = fold_expr(cond.clone(), fenv_access);
            fold_stmt(body, fenv_access);
        }
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(c) = cond {
                *c = fold_expr(c.clone(), fenv_access);
            }
            if let Some(st) = step {
                *st = fold_expr(st.clone(), fenv_access);
            }
            if let Some(i) = init {
                fold_stmt(i, fenv_access);
            }
            fold_stmt(body, fenv_access);
        }
        Stmt::Return { val: Some(e), .. } => {
            *e = fold_expr(e.clone(), fenv_access);
        }
        Stmt::Expr { e, .. } => *e = fold_expr(e.clone(), fenv_access),
        Stmt::Block { b, .. } => fold_block(&mut b.items, fenv_access),
        _ => {}
    }
}

fn fold_expr(e: Expr, fenv_access: bool) -> Expr {
    let e = match e {
        Expr::Binop { op, l, r, span, ty } => {
            let l = Box::new(fold_expr(*l, fenv_access));
            let r = Box::new(fold_expr(*r, fenv_access));
            if let (Expr::Int { v: lv, .. }, Expr::Int { v: rv, .. }) = (l.as_ref(), r.as_ref()) {
                if let Some(v) = eval_binop(op, *lv, *rv) {
                    return Expr::Int { v, span, ty };
                }
            }
            // FENV_ACCESS ON: 不折叠浮点常量运算 (舍入模式可能被 fesetround
            // 在运行时改变, 编译期折叠将按默认舍入求值, 语义错误).
            if !fenv_access {
                if let (Expr::Float { v: lv, .. }, Expr::Float { v: rv, .. }) =
                    (l.as_ref(), r.as_ref())
                {
                    if let Some(v) = eval_float_binop(op, *lv, *rv) {
                        return Expr::Float { v, span, ty };
                    }
                }
            }
            Expr::Binop { op, l, r, span, ty }
        }
        Expr::Unary { op, e, span, ty } => {
            let e = Box::new(fold_expr(*e, fenv_access));
            if let Expr::Int { v, .. } = e.as_ref() {
                if let Some(v) = eval_unary(op, *v) {
                    return Expr::Int { v, span, ty };
                }
            }
            Expr::Unary { op, e, span, ty }
        }
        Expr::Call {
            callee,
            args,
            span,
            ty,
        } => {
            if let Expr::Ident { name, .. } = callee.as_ref() {
                if name.name == "__builtin_constant_p" && args.len() == 1 {
                    return Expr::Int {
                        v: const_eval::is_constant(&args[0]) as i128,
                        span,
                        ty,
                    };
                }
            }
            Expr::Call {
                callee: Box::new(fold_expr(*callee, fenv_access)),
                args: args
                    .into_iter()
                    .map(|a| fold_expr(a, fenv_access))
                    .collect(),
                span,
                ty,
            }
        }
        other => other,
    };
    if let Ok(v) = const_eval::eval(&e) {
        if let Expr::Int { span, ty, .. } = &e {
            return Expr::Int {
                v,
                span: span.clone(),
                ty: ty.clone(),
            };
        }
    }
    e
}

fn eval_binop(op: BinOp, l: i128, r: i128) -> Option<i128> {
    Some(match op {
        BinOp::Add => l.checked_add(r)?,
        BinOp::Sub => l.checked_sub(r)?,
        BinOp::Mul => l.checked_mul(r)?,
        BinOp::Div => {
            if r == 0 {
                return None;
            } else {
                l / r
            }
        }
        BinOp::Mod => {
            if r == 0 {
                return None;
            } else {
                l % r
            }
        }
        BinOp::And => l & r,
        BinOp::Or => l | r,
        BinOp::Xor => l ^ r,
        BinOp::Shl => l << (r as u32),
        BinOp::Shr => l >> (r as u32),
        _ => return None,
    })
}

fn eval_unary(op: crate::ast::UnOp, v: i128) -> Option<i128> {
    Some(match op {
        crate::ast::UnOp::Neg => v.checked_neg()?,
        crate::ast::UnOp::Not => {
            if v == 0 {
                1
            } else {
                0
            }
        }
        crate::ast::UnOp::BitNot => !v,
        _ => return None,
    })
}

fn eval_float_binop(op: BinOp, l: f64, r: f64) -> Option<f64> {
    Some(match op {
        BinOp::Add => l + r,
        BinOp::Sub => l - r,
        BinOp::Mul => l * r,
        BinOp::Div => {
            if r == 0.0 {
                return None;
            } else {
                l / r
            }
        }
        _ => return None,
    })
}

fn prune_dead_stores(module: &mut Module) {
    let prog = module.source_mut();
    for decl in &mut prog.decls {
        if let Decl::Func(f) = decl {
            if let Some(body) = &mut f.body {
                prune_block(&mut body.items);
            }
        }
    }
}

fn prune_block(items: &mut Vec<BlockItem>) {
    items.retain(
        |item| !matches!(item, BlockItem::Stmt(Stmt::Expr { e, .. }) if is_side_effect_free(e)),
    );
    for item in items.iter_mut() {
        match item {
            BlockItem::Stmt(s) => prune_stmt(s),
            BlockItem::Decl(Decl::Func(f)) => {
                if let Some(body) = &mut f.body {
                    prune_block(&mut body.items);
                }
            }
            _ => {}
        }
    }
}

fn prune_stmt(s: &mut Stmt) {
    match s {
        Stmt::If { then_, else_, .. } => {
            prune_stmt(then_);
            if let Some(e) = else_ {
                prune_stmt(e);
            }
        }
        Stmt::While { body, .. } | Stmt::Do { body, .. } => prune_stmt(body),
        Stmt::For { init, body, .. } => {
            if let Some(i) = init {
                prune_stmt(i);
            }
            prune_stmt(body);
        }
        Stmt::Block { b, .. } => prune_block(&mut b.items),
        _ => {}
    }
}

/// -O2: 常量条件分支折叠（DCE 的一种）。
fn fold_constant_branches(module: &mut Module) {
    let prog = module.source_mut();
    let fenv_access = prog.fenv_access;
    for decl in &mut prog.decls {
        if let Decl::Func(f) = decl {
            if let Some(body) = &mut f.body {
                fold_branches_block(&mut body.items, fenv_access);
            }
        }
    }
}

/// 语句（递归）内是否含 Label。goto 可跳入常量死分支中的标签,
/// 折叠时若丢弃标签会导致 codegen "unresolved label" 崩溃。
fn stmt_has_label(s: &Stmt) -> bool {
    match s {
        Stmt::Label { .. } => true,
        Stmt::Block { b, .. } => block_has_label(b),
        Stmt::If { then_, else_, .. } => {
            stmt_has_label(then_) || else_.as_ref().map(|e| stmt_has_label(e)).unwrap_or(false)
        }
        Stmt::While { body, .. } | Stmt::Do { body, .. } | Stmt::For { body, .. } => {
            stmt_has_label(body)
        }
        Stmt::Switch { cases, .. } => cases.iter().any(|c| block_has_label(&c.body)),
        _ => false,
    }
}

fn block_has_label(b: &Block) -> bool {
    b.items.iter().any(|item| match item {
        BlockItem::Stmt(s) => stmt_has_label(s),
        BlockItem::Decl(Decl::Func(f)) => f.body.as_ref().map(block_has_label).unwrap_or(false),
        _ => false,
    })
}

fn fold_branches_block(items: &mut Vec<BlockItem>, fenv_access: bool) {
    let mut i = 0;
    while i < items.len() {
        let fold_if = matches!(
            items.get(i),
            Some(BlockItem::Stmt(Stmt::If { .. })) if const_eval::eval(
                match items.get(i) {
                    Some(BlockItem::Stmt(Stmt::If { cond, .. })) => cond,
                    _ => unreachable!(),
                }
            ).is_ok()
        );
        if fold_if {
            let item = items.remove(i);
            if let BlockItem::Stmt(Stmt::If {
                cond,
                then_,
                else_,
                span,
                ..
            }) = item
            {
                let v = const_eval::eval(&cond).unwrap_or(0);
                // 被丢弃的分支内含 Label 时不能折叠: goto 仍可跳入, 删除会丢标签
                let has_label = if v != 0 {
                    else_.as_ref().map(|e| stmt_has_label(e)).unwrap_or(false)
                } else {
                    stmt_has_label(&then_)
                };
                if has_label {
                    items.insert(
                        i,
                        BlockItem::Stmt(Stmt::If {
                            cond,
                            then_,
                            else_,
                            span,
                        }),
                    );
                    i += 1;
                    continue;
                }
                let taken = if v != 0 {
                    *then_
                } else {
                    else_.map(|e| *e).unwrap_or(Stmt::Block {
                        b: Block {
                            items: vec![],
                            span: span.clone(),
                        },
                        span: span.clone(),
                    })
                };
                match taken {
                    Stmt::Block { mut b, .. } => {
                        fold_branches_block(&mut b.items, fenv_access);
                        for it in b.items.into_iter().rev() {
                            items.insert(i, it);
                        }
                        continue;
                    }
                    other => items.insert(i, BlockItem::Stmt(other)),
                }
            }
        } else if let BlockItem::Stmt(s) = &mut items[i] {
            fold_branches_stmt(s, fenv_access);
        }
        i += 1;
    }
}

fn fold_branches_stmt(s: &mut Stmt, fenv_access: bool) {
    match s {
        Stmt::If {
            cond, then_, else_, ..
        } => {
            *cond = fold_expr(cond.clone(), fenv_access);
            fold_branches_stmt(then_, fenv_access);
            if let Some(e) = else_ {
                fold_branches_stmt(e, fenv_access);
            }
        }
        Stmt::While {
            cond, body, span, ..
        } => {
            *cond = fold_expr(cond.clone(), fenv_access);
            if let Ok(v) = const_eval::eval(cond) {
                if v == 0 && !stmt_has_label(body) {
                    *s = Stmt::Block {
                        b: Block {
                            items: vec![],
                            span: span.clone(),
                        },
                        span: span.clone(),
                    };
                    return;
                }
            }
            fold_branches_stmt(body, fenv_access);
        }
        Stmt::Block { b, .. } => fold_branches_block(&mut b.items, fenv_access),
        Stmt::For {
            init,
            cond,
            body,
            span,
            ..
        } => {
            if let Some(c) = cond {
                *c = fold_expr(c.clone(), fenv_access);
                if let Ok(v) = const_eval::eval(c) {
                    if v == 0 && !stmt_has_label(body) {
                        if let Some(i) = init.take() {
                            *s = Stmt::Expr {
                                e: stmt_to_void_expr(*i),
                                span: span.clone(),
                            };
                        } else {
                            *s = Stmt::Block {
                                b: Block {
                                    items: vec![],
                                    span: span.clone(),
                                },
                                span: span.clone(),
                            };
                        }
                        return;
                    }
                }
            }
            if let Some(i) = init {
                fold_branches_stmt(i, fenv_access);
            }
            fold_branches_stmt(body, fenv_access);
        }
        _ => {}
    }
}

fn items_clear_for_stmt(s: &mut Stmt) {
    let span = s.span().clone();
    *s = Stmt::Block {
        b: Block {
            items: vec![],
            span: span.clone(),
        },
        span,
    };
}

fn stmt_to_void_expr(s: Stmt) -> Expr {
    match s {
        Stmt::Expr { e, .. } => e,
        Stmt::Block { .. } => Expr::Int {
            v: 0,
            span: Span::point("", 0, 0),
            ty: crate::ast::Type::Int,
        },
        _ => Expr::Int {
            v: 0,
            span: Span::point("", 0, 0),
            ty: crate::ast::Type::Int,
        },
    }
}

/// -O2: return 后不可达语句删除。
fn eliminate_unreachable(module: &mut Module) {
    let prog = module.source_mut();
    for decl in &mut prog.decls {
        if let Decl::Func(f) = decl {
            if let Some(body) = &mut f.body {
                dce_block(&mut body.items);
            }
        }
    }
}

fn dce_block(items: &mut Vec<BlockItem>) {
    let mut stopped = false;
    items.retain(|item| {
        if stopped {
            // goto 仍可跳入 return 之后的标签，不能当不可达代码删除
            if matches!(item, BlockItem::Stmt(Stmt::Label { .. })) {
                return true;
            }
            return false;
        }
        if let BlockItem::Stmt(Stmt::Return { .. }) = item {
            stopped = true;
        }
        true
    });
    for item in items.iter_mut() {
        if let BlockItem::Stmt(s) = item {
            dce_stmt(s);
        }
    }
}

fn dce_stmt(s: &mut Stmt) {
    match s {
        Stmt::If { then_, else_, .. } => {
            dce_block_in_stmt(then_);
            if let Some(e) = else_ {
                dce_block_in_stmt(e);
            }
        }
        Stmt::While { body, .. } | Stmt::Do { body, .. } | Stmt::For { body, .. } => {
            dce_block_in_stmt(body);
        }
        Stmt::Block { b, .. } => dce_block(&mut b.items),
        _ => {}
    }
}

fn dce_block_in_stmt(s: &mut Stmt) {
    if let Stmt::Block { b, .. } = s {
        dce_block(&mut b.items);
    } else {
        dce_stmt(s);
    }
}

/// 内联入口: 收集模块内所有函数定义, 然后逐函数内联.
///
/// `level` 控制内联激进程度:
///   - O0: 仅 `always_inline` / `flatten`
///   - O1-O2: 同 O0 (未来计划加入成本模型驱动)
///   - O3: 额外启发式小型 static 零参函数 (cost ≤ 20 AST nodes)
fn function_inline(module: &mut Module, level: u8) {
    let prog = module.source_mut();
    let fenv_access = prog.fenv_access;
    let funcs: HashMap<String, FuncDecl> = prog
        .decls
        .iter()
        .filter_map(|d| match d {
            Decl::Func(f) if f.body.is_some() => Some((f.name.name.clone(), f.clone())),
            _ => None,
        })
        .collect();
    if funcs.is_empty() {
        return;
    }
    for decl in &mut prog.decls {
        if let Decl::Func(f) = decl {
            let caller_flatten = f.is_flatten;
            if let Some(body) = &mut f.body {
                inline_calls_in_block(
                    body, &funcs, level, caller_flatten, fenv_access, 0,
                );
            }
        }
    }
}


// ---------------------------------------------------------------------------
// Inline cost model
//
// Estimates the code-size impact of inlining a function call.
// Used by the O3 heuristic path and reserved for future O1/O2 cost-driven
// inlining.
//
// The model is intentionally simple (AST node counting) since dcc operates
// on the pre-lowering AST, not a CFG/SSA.  The goal is to prevent
// code-size blow-up from inlining large or complex callees while still
// capturing the common kernel pattern of small accessor/macro functions.
// ---------------------------------------------------------------------------

/// Maximum cost threshold for speculative inlining at O3.
/// Computed as: body_expr_nodes + param_count * avg_arg_nodes.
const INLINE_COST_THRESHOLD: usize = 20;

/// Count AST nodes in an expression (recursive).
fn expr_node_count(e: &Expr) -> usize {
    match e {
        Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::Char { .. }
        | Expr::String { .. }
        | Expr::Ident { .. }
        | Expr::SizeofTy { .. }
        | Expr::SizeofExpr { .. }
        | Expr::AlignofTy { .. }
        | Expr::AlignofExpr { .. }
        | Expr::Offsetof { .. }
        | Expr::ComplexLit { .. }
        | Expr::LabelAddr { .. } => 1,
        Expr::Unary { e, .. }
        | Expr::Cast { e, .. }
        | Expr::Real { e, .. }
        | Expr::Imag { e, .. }
        | Expr::Deref { e, .. }
        | Expr::AddrOf { e, .. }
        | Expr::PostInc { e, .. }
        | Expr::PostDec { e, .. }
        | Expr::PreInc { e, .. }
        | Expr::PreDec { e, .. } => 1 + expr_node_count(e),
        Expr::Binop { l, r, .. }
        | Expr::Assign { l, r, .. }
        | Expr::Index { arr: l, idx: r, .. }
        | Expr::Comma { l, r, .. } => {
            1 + expr_node_count(l) + expr_node_count(r)
        }
        Expr::Cond {
            cond, then_, else_, ..
        } => 1 + expr_node_count(cond) + expr_node_count(then_) + expr_node_count(else_),
        Expr::Call { callee, args, .. } => {
            1 + expr_node_count(callee)
                + args.iter().map(expr_node_count).sum::<usize>()
        }
        Expr::Member { expr, .. } => 1 + expr_node_count(expr),
        Expr::VaArg { ap, .. } => 1 + expr_node_count(ap),
        Expr::StmtExpr { block, .. } => {
            1 + block.items.iter().map(item_node_count).sum::<usize>()
        }
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            1 + inits.iter().map(init_node_count).sum::<usize>()
        }
        Expr::AsmExpr {
            outputs,
            inputs,
            ..
        } => {
            1 + outputs.iter().chain(inputs.iter()).map(expr_node_count).sum::<usize>()
        }
    }
}

fn item_node_count(item: &BlockItem) -> usize {
    match item {
        BlockItem::Stmt(s) => stmt_node_count(s),
        BlockItem::Decl(Decl::Var(v)) => {
            1 + v.init.as_ref().map_or(0, expr_node_count)
        }
        BlockItem::Decl(Decl::Func(f)) => {
            1 + f.body.as_ref().map_or(0, |b| {
                b.items.iter().map(item_node_count).sum::<usize>()
            })
        }
        _ => 1,
    }
}

fn stmt_node_count(s: &Stmt) -> usize {
    match s {
        Stmt::Return { val, .. } => {
            1 + val.as_ref().map_or(0, expr_node_count)
        }
        Stmt::Expr { e, .. } => 1 + expr_node_count(e),
        Stmt::If {
            cond, then_, else_, ..
        } => {
            1 + expr_node_count(cond) + stmt_node_count(then_)
                + else_.as_ref().map_or(0, |e| stmt_node_count(e))
        }
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
            1 + expr_node_count(cond) + stmt_node_count(body)
        }
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            1 + init.as_ref().map_or(0, |s| stmt_node_count(s))
                + cond.as_ref().map_or(0, expr_node_count)
                + step.as_ref().map_or(0, expr_node_count)
                + stmt_node_count(body)
        }
        Stmt::Block { b, .. } => {
            1 + b.items.iter().map(item_node_count).sum::<usize>()
        }
        Stmt::Switch { expr, cases, .. } => {
            1 + expr_node_count(expr)
                + cases
                    .iter()
                    .map(|c| {
                        c.body
                            .items
                            .iter()
                            .map(item_node_count)
                            .sum::<usize>()
                    })
                    .sum::<usize>()
        }
        _ => 1,
    }
}

fn init_node_count(init: &crate::ast::InitItem) -> usize {
    match init {
        crate::ast::InitItem::Expr(e) => expr_node_count(e),
        crate::ast::InitItem::Designated { value, .. } => expr_node_count(value),
    }
}

/// Estimate the cost of inlining function `f` called with `args`.
///
/// Returns the estimated node count of the inlined result.
/// Lower is better; values above `INLINE_COST_THRESHOLD` indicate
/// the inlining would likely increase code size.
fn estimate_inline_cost(f: &FuncDecl, body: &Block, args: &[Expr]) -> usize {
    // Base cost: all nodes in the function body
    let body_cost: usize = body.items.iter().map(item_node_count).sum();
    // Argument duplication cost: each argument is substituted for every
    // reference to the corresponding parameter in the body.
    // We approximate by counting arg nodes * param references (bounded
    // by body_cost as an upper heuristic).
    let arg_cost: usize = args.iter().map(|a| expr_node_count(a)).sum::<usize>();
    // Total: body + arg duplication (arg cost scaled by param usage ratio)
    body_cost.saturating_add(arg_cost)
}

fn inline_calls_in_block(
    body: &mut Block,
    funcs: &HashMap<String, FuncDecl>,
    level: u8,
    caller_flatten: bool,
    fenv_access: bool,
    depth: u32,
) {
    for item in &mut body.items {
        match item {
            BlockItem::Stmt(s) => inline_calls_in_stmt(
                s, funcs, level, caller_flatten, fenv_access, depth,
            ),
            BlockItem::Decl(Decl::Func(f)) => {
                if let Some(b) = &mut f.body {
                    inline_calls_in_block(
                        b, funcs, level, caller_flatten, fenv_access, depth,
                    );
                }
            }
            _ => {}
        }
    }
}

fn inline_calls_in_stmt(
    s: &mut Stmt,
    funcs: &HashMap<String, FuncDecl>,
    level: u8,
    caller_flatten: bool,
    fenv_access: bool,
    depth: u32,
) {
    match s {
        Stmt::Return { val: Some(e), .. } => {
            *e = inline_expr(e.clone(), funcs, level, caller_flatten, fenv_access, depth);
        }
        Stmt::Expr { e, .. } => {
            *e = inline_expr(e.clone(), funcs, level, caller_flatten, fenv_access, depth)
        }
        Stmt::If {
            cond, then_, else_, ..
        } => {
            *cond = inline_expr(cond.clone(), funcs, level, caller_flatten, fenv_access, depth);
            inline_calls_in_stmt(then_, funcs, level, caller_flatten, fenv_access, depth);
            if let Some(e) = else_ {
                inline_calls_in_stmt(e, funcs, level, caller_flatten, fenv_access, depth);
            }
        }
        Stmt::Block { b, .. } => {
            inline_calls_in_block(b, funcs, level, caller_flatten, fenv_access, depth)
        }
        Stmt::While { body, .. } | Stmt::Do { body, .. } => {
            inline_calls_in_stmt(body, funcs, level, caller_flatten, fenv_access, depth)
        }
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(c) = cond {
                *c = inline_expr(c.clone(), funcs, level, caller_flatten, fenv_access, depth);
            }
            if let Some(st) = step {
                *st = inline_expr(st.clone(), funcs, level, caller_flatten, fenv_access, depth);
            }
            if let Some(i) = init {
                inline_calls_in_stmt(i, funcs, level, caller_flatten, fenv_access, depth);
            }
            inline_calls_in_stmt(body, funcs, level, caller_flatten, fenv_access, depth);
        }
        _ => {}
    }
}

fn extract_inline_return(body: &Block) -> Option<&Expr> {
    match body.items.as_slice() {
        [BlockItem::Stmt(Stmt::Return { val: Some(e), .. })] => Some(e),
        items if !items.is_empty() => {
            if let BlockItem::Stmt(Stmt::Return { val: Some(e), .. }) = items.last()? {
                if items.iter().all(|it| {
                    matches!(
                        it,
                        BlockItem::Decl(_) | BlockItem::Stmt(Stmt::Return { .. })
                    )
                }) {
                    Some(e)
                } else {
                    None
                }
            } else {
                None
            }
        }
        _ => None,
    }
}

fn expr_calls_name(expr: &Expr, name: &str) -> bool {
    match expr {
        Expr::Call { callee, args, .. } => {
            let direct = matches!(callee.as_ref(), Expr::Ident { name: n, .. } if n.name == name);
            direct || args.iter().any(|a| expr_calls_name(a, name))
        }
        Expr::Binop { l, r, .. } => expr_calls_name(l, name) || expr_calls_name(r, name),
        Expr::Unary { e, .. } => expr_calls_name(e, name),
        Expr::Cast { e, .. } => expr_calls_name(e, name),
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            expr_calls_name(cond, name)
                || expr_calls_name(then_, name)
                || expr_calls_name(else_, name)
        }
        Expr::Comma { l, r, .. } => expr_calls_name(l, name) || expr_calls_name(r, name),
        _ => false,
    }
}

fn substitute_params(expr: Expr, params: &[(String, Expr)]) -> Expr {
    match expr {
        Expr::Ident { name, span, ty } => {
            if let Some((_, arg)) = params.iter().find(|(n, _)| n == &name.name) {
                arg.clone()
            } else {
                Expr::Ident { name, span, ty }
            }
        }
        Expr::Binop { op, l, r, span, ty } => Expr::Binop {
            op,
            l: Box::new(substitute_params(*l, params)),
            r: Box::new(substitute_params(*r, params)),
            span,
            ty,
        },
        Expr::Unary { op, e, span, ty } => Expr::Unary {
            op,
            e: Box::new(substitute_params(*e, params)),
            span,
            ty,
        },
        Expr::Cast { ty, e, span } => Expr::Cast {
            ty,
            e: Box::new(substitute_params(*e, params)),
            span,
        },
        Expr::Cond {
            cond,
            then_,
            else_,
            span,
            ty,
        } => Expr::Cond {
            cond: Box::new(substitute_params(*cond, params)),
            then_: Box::new(substitute_params(*then_, params)),
            else_: Box::new(substitute_params(*else_, params)),
            span,
            ty,
        },
        Expr::Comma { l, r, span, ty } => Expr::Comma {
            l: Box::new(substitute_params(*l, params)),
            r: Box::new(substitute_params(*r, params)),
            span,
            ty,
        },
        other => other,
    }
}

/// 内联决策: 判断函数 `name` 是否可被内联, 并返回替换后的表达式.
///
/// 决策规则 (按优先级):
///   1. `noinline` 且非 `always_inline`: 拒绝 (noinline 被尊重)
///   2. `always_inline` 与 `noinline` 同时存在: `always_inline` 优先 (GCC 兼容)
///   3. `always_inline` / `flatten`: 无条件接受 (不受尺寸/深度限制)
///   4. O0 级别: 拒绝所有非强制内联
///   5. 深度超限: 拒绝
///   6. 直接递归: 拒绝 (callee 名与函数名相同)
///   7. O3 启发式: static + 零参 + cost ≤ INLINE_COST_THRESHOLD
///   8. 其他: 拒绝
///
/// 当 DCC_INLINE_TRACE=1 时, 每个决策均输出到 stderr.
fn try_inline_call(
    name: &str,
    args: &[Expr],
    funcs: &HashMap<String, FuncDecl>,
    level: u8,
    caller_flatten: bool,
    depth: u32,
) -> Option<Expr> {
    let f = funcs.get(name)?;
    let body = f.body.as_ref()?;
    if f.is_variadic || args.len() != f.params.len() {
        trace_inline_decision(name, "SKIP", "variadic/arity mismatch");
        return None;
    }

    // --- 属性优先级: noinline vs always_inline ---
    // GCC 行为: 两者同时标记时 always_inline 优先.
    if f.is_noinline && !f.is_always_inline {
        trace_inline_decision(name, "REJECT", "noinline attribute");
        return None;
    }

    // --- 强制内联: always_inline / flatten ---
    // 不受尺寸限制, 不受深度限制. 内核 __always_inline 依赖此行为.
    let forced = f.is_always_inline || caller_flatten;
    if forced {
        let ret = extract_inline_return(body)?;
        if expr_calls_name(ret, name) {
            trace_inline_decision(name, "REJECT", "forced but direct recursion");
            return None; // 直接递归保护
        }
        let reason = if f.is_always_inline {
            "always_inline"
        } else {
            "caller flatten"
        };
        trace_inline_decision(name, "INLINE", reason);
        return Some(substitute_call(f, ret, args));
    }

    // --- O0: 非强制内联一律拒绝 ---
    if level == 0 {
        trace_inline_decision(name, "REJECT", "O0 non-forced");
        return None;
    }

    // --- 深度限制: 防止嵌套内联爆炸 ---
    if depth >= INLINE_MAX_DEPTH {
        trace_inline_decision(
            name, "REJECT",
            &format!("depth {} >= {}", depth, INLINE_MAX_DEPTH),
        );
        return None;
    }

    // --- O3 启发式: 小型 static 零参函数, 使用成本模型 ---
    let cost = estimate_inline_cost(f, body, args);
    if !(f.is_static && f.params.is_empty() && level >= 3
        && cost <= INLINE_COST_THRESHOLD)
    {
        trace_inline_decision(
            name, "REJECT",
            &format!("cost {} > {} or not O3-static", cost, INLINE_COST_THRESHOLD),
        );
        return None;
    }

    let ret = extract_inline_return(body)?;
    if expr_calls_name(ret, name) {
        trace_inline_decision(name, "REJECT", "O3 heuristic but direct recursion");
        return None; // 直接递归保护
    }
    trace_inline_decision(
        name, "INLINE",
        &format!("O3 heuristic, cost={}", cost),
    );
    Some(substitute_call(f, ret, args))
}

/// 参数替换: 将函数体返回表达式中的形参替换为实参.
fn substitute_call(f: &FuncDecl, ret: &Expr, args: &[Expr]) -> Expr {
    let bindings: Vec<(String, Expr)> = f
        .params
        .iter()
        .zip(args.iter())
        .filter_map(|(p, a)| p.name.as_ref().map(|id| (id.name.clone(), a.clone())))
        .collect();
    if bindings.len() != f.params.len() {
        return ret.clone();
    }
    substitute_params(ret.clone(), &bindings)
}

fn inline_expr(
    e: Expr,
    funcs: &HashMap<String, FuncDecl>,
    level: u8,
    caller_flatten: bool,
    fenv_access: bool,
    depth: u32,
) -> Expr {
    let e = match e {
        Expr::Call {
            callee,
            args,
            span,
            ty,
        } => {
            if let Expr::Ident { name, .. } = callee.as_ref() {
                if let Some(inlined) = try_inline_call(
                    &name.name, &args, funcs, level, caller_flatten, depth,
                ) {
                    // 内联后继续递归内联替换结果中的嵌套调用
                    return inline_expr(
                        inlined, funcs, level, caller_flatten, fenv_access,
                        depth + 1,
                    );
                }
            }
            Expr::Call {
                callee: Box::new(inline_expr(
                    *callee, funcs, level, caller_flatten, fenv_access, depth,
                )),
                args: args
                    .into_iter()
                    .map(|a| {
                        inline_expr(a, funcs, level, caller_flatten, fenv_access, depth)
                    })
                    .collect(),
                span,
                ty,
            }
        }
        Expr::Binop { op, l, r, span, ty } => Expr::Binop {
            op,
            l: Box::new(inline_expr(*l, funcs, level, caller_flatten, fenv_access, depth)),
            r: Box::new(inline_expr(*r, funcs, level, caller_flatten, fenv_access, depth)),
            span,
            ty,
        },
        Expr::Unary { op, e, span, ty } => Expr::Unary {
            op,
            e: Box::new(inline_expr(*e, funcs, level, caller_flatten, fenv_access, depth)),
            span,
            ty,
        },
        Expr::Cond {
            cond,
            then_,
            else_,
            span,
            ty,
        } => Expr::Cond {
            cond: Box::new(inline_expr(
                *cond, funcs, level, caller_flatten, fenv_access, depth,
            )),
            then_: Box::new(inline_expr(
                *then_, funcs, level, caller_flatten, fenv_access, depth,
            )),
            else_: Box::new(inline_expr(
                *else_, funcs, level, caller_flatten, fenv_access, depth,
            )),
            span,
            ty,
        },
        other => other,
    };
    fold_expr(e, fenv_access)
}

fn is_side_effect_free(e: &Expr) -> bool {
    matches!(
        e,
        Expr::Int { .. }
            | Expr::SizeofTy { .. }
            | Expr::SizeofExpr { .. }
            | Expr::AlignofTy { .. }
            | Expr::AlignofExpr { .. }
            | Expr::Offsetof { .. }
    )
}

trait StmtSpan {
    fn span(&self) -> &Span;
}

impl StmtSpan for Stmt {
    fn span(&self) -> &Span {
        match self {
            Stmt::If { span, .. }
            | Stmt::While { span, .. }
            | Stmt::Do { span, .. }
            | Stmt::For { span, .. }
            | Stmt::Return { span, .. }
            | Stmt::Expr { span, .. }
            | Stmt::Block { span, .. }
            | Stmt::Break { span, .. }
            | Stmt::Continue { span, .. }
            | Stmt::Goto { span, .. }
            | Stmt::GotoIndirect { span, .. }
            | Stmt::Label { span, .. }
            | Stmt::Switch { span, .. }
            | Stmt::AsmGoto { span, .. } => span,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Ident, Type};
    use crate::diagnostics::Span;
    use crate::ir;

    fn int_ty() -> Type {
        Type::Int
    }

    #[test]
    fn o2_folds_constant_if() {
        let prog = Program {
            decls: vec![Decl::Func(FuncDecl {
                ret_ty: int_ty(),
                name: Ident {
                    name: "main".into(),
                    span: Span::point("t.c", 1, 1),
                },
                params: vec![],
                body: Some(Block {
                    items: vec![BlockItem::Stmt(Stmt::If {
                        cond: Expr::Int {
                            v: 1,
                            span: Span::point("t.c", 2, 1),
                            ty: int_ty(),
                        },
                        then_: Box::new(Stmt::Return {
                            val: Some(Expr::Int {
                                v: 7,
                                span: Span::point("t.c", 3, 1),
                                ty: int_ty(),
                            }),
                            span: Span::point("t.c", 3, 1),
                        }),
                        else_: None,
                        span: Span::point("t.c", 2, 1),
                    })],
                    span: Span::point("t.c", 1, 1),
                }),
                span: Span::point("t.c", 1, 1),
                is_static: false,
                is_extern: false,
                is_variadic: false,
                is_weak: false,
                is_constructor: false,
                is_destructor: false,
                is_nested: false,
                is_cold: false,
                is_hot: false,
                section: None,
                alignment: None,
                visibility: None,
                alias_name: None,
                upvars: vec![],
                is_unused: false,
                is_noinline: false,
                is_always_inline: false,
                is_old_style: false,
                format_kind: None,
                format_string_idx: None,
                format_first_arg: None,
                no_sanitize: vec![],
                target_clones: vec![],
                returns_nonnull: false,
                ctor_priority: None,
                dtor_priority: None,
                is_retain: false,
                is_used_attr: false,
                is_flatten: false,
                error_msg: None,
                warning_msg: None,
                deprecated_msg: None,
                ifunc_resolver: None,
                asm_output_template: None,
                is_global: false,
                is_device: false,
                is_host: false,
            is_naked: false,
            })],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
        let (mut module, _) = ir::lower(prog).expect("lower");
        optimize(&mut module, 2);
        let body = &module.source().decls[0];
        if let Decl::Func(f) = body {
            assert_eq!(f.body.as_ref().unwrap().items.len(), 1);
            assert!(matches!(
                f.body.as_ref().unwrap().items[0],
                BlockItem::Stmt(Stmt::Return { .. })
            ));
        }
    }

    #[test]
    fn o2_float_fold_respects_fenv_access() {
        let float_expr = || Expr::Binop {
            op: BinOp::Add,
            l: Box::new(Expr::Float {
                v: 1.5,
                span: Span::point("t.c", 1, 1),
                ty: Type::Double,
            }),
            r: Box::new(Expr::Float {
                v: 2.25,
                span: Span::point("t.c", 1, 1),
                ty: Type::Double,
            }),
            span: Span::point("t.c", 1, 1),
            ty: Type::Double,
        };
        // 默认 (FENV_ACCESS OFF): 浮点常量运算被折叠
        let folded = fold_expr(float_expr(), false);
        assert!(
            matches!(folded, Expr::Float { v, .. } if (v - 3.75).abs() < 1e-12),
            "float binop should fold without FENV_ACCESS, got {folded:?}"
        );
        // FENV_ACCESS ON: 保留运行时二元运算 (舍入模式可能在运行时改变)
        let kept = fold_expr(float_expr(), true);
        assert!(
            matches!(kept, Expr::Binop { .. }),
            "float binop must not fold under FENV_ACCESS, got {kept:?}"
        );
    }

    #[test]
    fn o2_preserves_goto_label_after_return() {
        let prog = Program {
            decls: vec![Decl::Func(FuncDecl {
                ret_ty: int_ty(),
                name: Ident {
                    name: "f".into(),
                    span: Span::point("t.c", 1, 1),
                },
                params: vec![],
                body: Some(Block {
                    items: vec![
                        BlockItem::Stmt(Stmt::If {
                            cond: Expr::Ident {
                                name: Ident {
                                    name: "x".into(),
                                    span: Span::point("t.c", 2, 1),
                                },
                                span: Span::point("t.c", 2, 1),
                                ty: int_ty(),
                            },
                            then_: Box::new(Stmt::Goto {
                                target: Ident {
                                    name: "found".into(),
                                    span: Span::point("t.c", 3, 1),
                                },
                                span: Span::point("t.c", 3, 1),
                            }),
                            else_: None,
                            span: Span::point("t.c", 2, 1),
                        }),
                        BlockItem::Stmt(Stmt::Return {
                            val: Some(Expr::Int {
                                v: 0,
                                span: Span::point("t.c", 4, 1),
                                ty: int_ty(),
                            }),
                            span: Span::point("t.c", 4, 1),
                        }),
                        BlockItem::Stmt(Stmt::Label {
                            name: Ident {
                                name: "found".into(),
                                span: Span::point("t.c", 5, 1),
                            },
                            stmt: Box::new(Stmt::Return {
                                val: Some(Expr::Int {
                                    v: 1,
                                    span: Span::point("t.c", 6, 1),
                                    ty: int_ty(),
                                }),
                                span: Span::point("t.c", 6, 1),
                            }),
                            span: Span::point("t.c", 5, 1),
                        }),
                    ],
                    span: Span::point("t.c", 1, 1),
                }),
                span: Span::point("t.c", 1, 1),
                is_static: false,
                is_extern: false,
                is_variadic: false,
                is_weak: false,
                is_constructor: false,
                is_destructor: false,
                is_nested: false,
                is_cold: false,
                is_hot: false,
                section: None,
                alignment: None,
                visibility: None,
                alias_name: None,
                upvars: vec![],
                is_unused: false,
                is_noinline: false,
                is_always_inline: false,
                is_old_style: false,
                format_kind: None,
                format_string_idx: None,
                format_first_arg: None,
                no_sanitize: vec![],
                target_clones: vec![],
                returns_nonnull: false,
                ctor_priority: None,
                dtor_priority: None,
                is_retain: false,
                is_used_attr: false,
                is_flatten: false,
                error_msg: None,
                warning_msg: None,
                deprecated_msg: None,
                ifunc_resolver: None,
                asm_output_template: None,
                is_global: false,
                is_device: false,
                is_host: false,
            is_naked: false,
            })],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
        let (mut module, _) = ir::lower(prog).expect("lower");
        optimize(&mut module, 2);
        let body = &module.source().decls[0];
        if let Decl::Func(f) = body {
            assert_eq!(
                f.body.as_ref().unwrap().items.len(),
                3,
                "label after return must survive DCE"
            );
            assert!(matches!(
                f.body.as_ref().unwrap().items[2],
                BlockItem::Stmt(Stmt::Label { .. })
            ));
        }
    }

    #[test]
    fn o1_inlines_always_inline_with_params() {
        let span = Span::point("t.c", 1, 1);
        let int = int_ty();
        let x_ident = || Expr::Ident {
            name: Ident {
                name: "x".into(),
                span: span.clone(),
            },
            span: span.clone(),
            ty: int.clone(),
        };
        let sq = Decl::Func(FuncDecl {
            ret_ty: int.clone(),
            name: Ident {
                name: "sq".into(),
                span: span.clone(),
            },
            params: vec![crate::ast::Param {
                ty: int.clone(),
                name: Some(Ident {
                    name: "x".into(),
                    span: span.clone(),
                }),
                span: span.clone(),
            }],
            body: Some(Block {
                items: vec![BlockItem::Stmt(Stmt::Return {
                    val: Some(Expr::Binop {
                        op: BinOp::Mul,
                        l: Box::new(x_ident()),
                        r: Box::new(x_ident()),
                        span: span.clone(),
                        ty: int.clone(),
                    }),
                    span: span.clone(),
                })],
                span: span.clone(),
            }),
            span: span.clone(),
            is_static: true,
            is_extern: false,
            is_variadic: false,
            is_weak: false,
            is_constructor: false,
            is_destructor: false,
            is_nested: false,
            is_cold: false,
            is_hot: false,
            section: None,
            alignment: None,
            visibility: None,
            alias_name: None,
            upvars: vec![],
            is_unused: false,
            is_noinline: false,
            is_always_inline: true,
            is_old_style: false,
            format_kind: None,
            format_string_idx: None,
            format_first_arg: None,
            no_sanitize: vec![],
            target_clones: vec![],
            returns_nonnull: false,
            ctor_priority: None,
            dtor_priority: None,
            is_retain: false,
            is_used_attr: false,
            is_flatten: false,
            error_msg: None,
            warning_msg: None,
            deprecated_msg: None,
            ifunc_resolver: None,
            asm_output_template: None,
            is_global: false,
            is_device: false,
            is_host: false,
            is_naked: false,
        });
        let main_fn = Decl::Func(FuncDecl {
            ret_ty: int.clone(),
            name: Ident {
                name: "main".into(),
                span: span.clone(),
            },
            params: vec![],
            body: Some(Block {
                items: vec![BlockItem::Stmt(Stmt::Return {
                    val: Some(Expr::Call {
                        callee: Box::new(Expr::Ident {
                            name: Ident {
                                name: "sq".into(),
                                span: span.clone(),
                            },
                            span: span.clone(),
                            ty: int.clone(),
                        }),
                        args: vec![Expr::Int {
                            v: 5,
                            span: span.clone(),
                            ty: int.clone(),
                        }],
                        span: span.clone(),
                        ty: int.clone(),
                    }),
                    span: span.clone(),
                })],
                span: span.clone(),
            }),
            span: span.clone(),
            is_static: false,
            is_extern: false,
            is_variadic: false,
            is_weak: false,
            is_constructor: false,
            is_destructor: false,
            is_nested: false,
            is_cold: false,
            is_hot: false,
            section: None,
            alignment: None,
            visibility: None,
            alias_name: None,
            upvars: vec![],
            is_unused: false,
            is_noinline: false,
            is_always_inline: false,
            is_old_style: false,
            format_kind: None,
            format_string_idx: None,
            format_first_arg: None,
            no_sanitize: vec![],
            target_clones: vec![],
            returns_nonnull: false,
            ctor_priority: None,
            dtor_priority: None,
            is_retain: false,
            is_used_attr: false,
            is_flatten: false,
            error_msg: None,
            warning_msg: None,
            deprecated_msg: None,
            ifunc_resolver: None,
            asm_output_template: None,
            is_global: false,
            is_device: false,
            is_host: false,
            is_naked: false,
        });
        let prog = Program {
            decls: vec![sq, main_fn],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
        let (mut module, _) = ir::lower(prog).expect("lower");
        optimize(&mut module, 1);
        let main_body = match &module.source().decls[1] {
            Decl::Func(f) => f.body.as_ref().unwrap(),
            _ => panic!("expected main"),
        };
        match &main_body.items[0] {
            BlockItem::Stmt(Stmt::Return {
                val: Some(Expr::Int { v: 25, .. }),
                ..
            }) => {}
            other => panic!("expected inlined constant 25, got {other:?}"),
        }
    }

    #[test]
    fn o2_preserves_label_in_dead_if_branch() {
        // `if (0) { lbl: ... }` 中的标签可被 goto 跳入,
        // 常量分支折叠不能丢弃它 (否则 codegen unresolved label 崩溃)。
        let span = Span::point("t.c", 1, 1);
        let int = int_ty();
        let func = Decl::Func(FuncDecl {
            ret_ty: int.clone(),
            name: Ident {
                name: "f".into(),
                span: span.clone(),
            },
            params: vec![],
            body: Some(Block {
                items: vec![BlockItem::Stmt(Stmt::If {
                    cond: Expr::Int {
                        v: 0,
                        span: span.clone(),
                        ty: int.clone(),
                    },
                    then_: Box::new(Stmt::Block {
                        b: Block {
                            items: vec![BlockItem::Stmt(Stmt::Label {
                                name: Ident {
                                    name: "undo".into(),
                                    span: span.clone(),
                                },
                                stmt: Box::new(Stmt::Expr {
                                    e: Expr::Int {
                                        v: 1,
                                        span: span.clone(),
                                        ty: int.clone(),
                                    },
                                    span: span.clone(),
                                }),
                                span: span.clone(),
                            })],
                            span: span.clone(),
                        },
                        span: span.clone(),
                    }),
                    else_: None,
                    span: span.clone(),
                })],
                span: span.clone(),
            }),
            span: span.clone(),
            is_static: false,
            is_extern: false,
            is_variadic: false,
            is_weak: false,
            is_constructor: false,
            is_destructor: false,
            is_nested: false,
            is_cold: false,
            is_hot: false,
            section: None,
            alignment: None,
            visibility: None,
            alias_name: None,
            upvars: vec![],
            is_unused: false,
            is_noinline: false,
            is_always_inline: false,
            is_old_style: false,
            format_kind: None,
            format_string_idx: None,
            format_first_arg: None,
            no_sanitize: vec![],
            target_clones: vec![],
            returns_nonnull: false,
            ctor_priority: None,
            dtor_priority: None,
            is_retain: false,
            is_used_attr: false,
            is_flatten: false,
            error_msg: None,
            warning_msg: None,
            deprecated_msg: None,
            ifunc_resolver: None,
            asm_output_template: None,
            is_global: false,
            is_device: false,
            is_host: false,
            is_naked: false,
        });
        let prog = Program {
            decls: vec![func],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
        let (mut module, _) = ir::lower(prog).expect("lower");
        optimize(&mut module, 2);
        let body = &module.source().decls[0];
        if let Decl::Func(f) = body {
            let items = f.body.as_ref().unwrap().items.clone();
            let has_label = items.iter().any(|item| match item {
                BlockItem::Stmt(Stmt::If { then_, .. }) => stmt_has_label(then_),
                BlockItem::Stmt(s) => stmt_has_label(s),
                _ => false,
            });
            assert!(has_label, "label in dead if branch must survive -O2 fold");
        }
    }

    // -------------------------------------------------------------------
    // Inline policy tests (parser-driven for conciseness)
    // -------------------------------------------------------------------

    /// Helper: parse source, lower, optimize at given level, return Program.
    fn optimize_src(src: &[u8], level: u8) -> Program {
        let (prog, diags) = crate::parser::parse(src, "inline-test.c");
        assert!(diags.iter().all(|d| !matches!(d.sev, crate::diagnostics::Severity::Error)),
            "parse errors: {:?}", diags);
        let (mut module, _) = ir::lower(prog).expect("lower");
        optimize(&mut module, level);
        module.source().clone()
    }

    /// Check if main() body contains a Call expression (i.e. not inlined).
    fn main_has_call(prog: &Program) -> bool {
        for d in &prog.decls {
            if let Decl::Func(f) = d {
                if f.name.name == "main" {
                    if let Some(b) = &f.body {
                        return block_has_call(b);
                    }
                }
            }
        }
        false
    }

    fn block_has_call(b: &Block) -> bool {
        b.items.iter().any(|it| match it {
            BlockItem::Stmt(s) => stmt_has_call(s),
            _ => false,
        })
    }

    fn stmt_has_call(s: &Stmt) -> bool {
        match s {
            Stmt::Return { val: Some(e), .. } | Stmt::Expr { e, .. } => expr_has_call(e),
            Stmt::Block { b, .. } => block_has_call(b),
            Stmt::If { cond, then_, else_, .. } => {
                expr_has_call(cond) || stmt_has_call(then_)
                    || else_.as_ref().is_some_and(|e| stmt_has_call(e))
            }
            _ => false,
        }
    }

    fn expr_has_call(e: &Expr) -> bool {
        match e {
            Expr::Call { .. } => true,
            Expr::Binop { l, r, .. } => expr_has_call(l) || expr_has_call(r),
            Expr::Unary { e, .. } | Expr::Cast { e, .. } => expr_has_call(e),
            Expr::Cond { cond, then_, else_, .. } => {
                expr_has_call(cond) || expr_has_call(then_) || expr_has_call(else_)
            }
            _ => false,
        }
    }

    #[test]
    fn noinline_is_respected_at_o1() {
        // noinline function must NOT be inlined even if small
        let prog = optimize_src(
            b"__attribute__((noinline)) static int f(int x) { return x + 1; }\n\
              int main(void) { return f(5); }\n",
            1,
        );
        assert!(main_has_call(&prog), "noinline function should not be inlined");
    }

    #[test]
    fn always_inline_large_body_inlined_at_o0() {
        // always_inline with body > 8 items must still be inlined at O0
        let prog = optimize_src(
            b"__attribute__((always_inline)) static inline int big(int x) {\n\
              int a = x+1; int b = a+2; int c = b+3; int d = c+4;\n\
              int e = d+5; int f = e+6; int g = f+7; int h = g+8;\n\
              int i = h+9; return i;\n\
              }\n\
              int main(void) { return big(0); }\n",
            0,
        );
        assert!(!main_has_call(&prog),
            "always_inline must be inlined at O0 regardless of body size");
    }

    #[test]
    fn always_inline_plus_noinline_always_inline_wins() {
        // When both always_inline and noinline are set, always_inline wins (GCC)
        let prog = optimize_src(
            b"__attribute__((always_inline, noinline)) static inline int f(int x) {\n\
              return x * 2;\n\
              }\n\
              int main(void) { return f(3); }\n",
            1,
        );
        assert!(!main_has_call(&prog),
            "always_inline should win over noinline");
    }

    #[test]
    fn o0_does_not_inline_regular_static_inline() {
        // Regular static inline must NOT be inlined at O0
        let prog = optimize_src(
            b"static inline int f(int x) { return x + 1; }\n\
              int main(void) { return f(5); }\n",
            0,
        );
        assert!(main_has_call(&prog),
            "regular static inline should not be inlined at O0");
    }

    #[test]
    fn expr_node_count_simple() {
        // 5 + 3 => Binop(5, 3) = 3 nodes
        let e = Expr::Binop {
            op: BinOp::Add,
            l: Box::new(Expr::Int { v: 5, span: Span::point("t", 1, 1), ty: int_ty() }),
            r: Box::new(Expr::Int { v: 3, span: Span::point("t", 1, 1), ty: int_ty() }),
            span: Span::point("t", 1, 1),
            ty: int_ty(),
        };
        assert_eq!(expr_node_count(&e), 3);
    }

    #[test]
    fn expr_node_count_ident() {
        let e = Expr::Ident {
            name: Ident { name: "x".into(), span: Span::point("t", 1, 1) },
            span: Span::point("t", 1, 1),
            ty: int_ty(),
        };
        assert_eq!(expr_node_count(&e), 1);
    }

    #[test]
    fn estimate_inline_cost_small_func() {
        // A simple return x+1 function should have low cost
        let (prog, _) = crate::parser::parse(
            b"static int f(int x) { return x + 1; }\n",
            "cost-test.c",
        );
        if let Decl::Func(f) = &prog.decls[0] {
            let body = f.body.as_ref().unwrap();
            let args = vec![Expr::Int {
                v: 5,
                span: Span::point("t", 1, 1),
                ty: int_ty(),
            }];
            let cost = estimate_inline_cost(f, body, &args);
            assert!(cost <= INLINE_COST_THRESHOLD,
                "small function cost {} should be <= {}", cost, INLINE_COST_THRESHOLD);
        } else {
            panic!("expected FuncDecl");
        }
    }
}
