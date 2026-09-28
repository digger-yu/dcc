// src/opt.rs - typed IR 优化 passes
// 第一性原理: 优化在 IR 层做数据流/常量传播，后端只负责指令选择。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;

use crate::ast::{
    AddrSpace, ArraySize, BinOp, Block, BlockItem, Decl, Expr, FuncDecl, Ident, InitItem, Program,
    QualifierFlags, Stmt, VarDecl,
};
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
        eprintln!("dcc-inline: {} {} ({})", decision, callee, reason,);
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
/// - speculative 内联深度上限 `INLINE_MAX_DEPTH` (8). 递归函数
///   通过 callee-name 检测阻止直接递归内联.
pub fn optimize(module: &mut Module, level: u8) {
    optimize_with_target(module, level, false, 0);
}

/// 带目标信息的优化入口。
///
/// `has_vnni`: 目标是否支持 VNNI 指令
/// `vec_bits`: 向量寄存器位宽 (256/512)
pub fn optimize_with_target(module: &mut Module, level: u8, has_vnni: bool, vec_bits: u32) {
    // Default: strict_aliasing=true (C standard semantics)
    optimize_with_full_target(module, level, has_vnni, vec_bits, true);
}

/// 带完整目标信息的优化入口。
///
/// `strict_aliasing`: -fno-strict-aliasing 时为 false, 别名分析合并所有指针
pub fn optimize_with_full_target(
    module: &mut Module,
    level: u8,
    has_vnni: bool,
    vec_bits: u32,
    strict_aliasing: bool,
) {
    // 内联: O0-O2 仅 always_inline/flatten; O3 额外启发式
    function_inline(module, level);
    if level == 0 {
        return;
    }
    fold_constants(module);
    if level >= 2 {
        prune_dead_stores(module);
        // Dead asm elimination: the SSA DCE pass removes pure
        // (non-volatile, no outputs, no memory clobber) asm when
        // its results are unused, matching GCC behavior. Volatile
        // asm and asm with side effects are always preserved.
        fold_constant_branches(module);
        eliminate_unreachable(module);
        crate::ssa_gvn::run(module, level);
        crate::loop_opt::run_with_target(module, level, has_vnni, vec_bits);
        // M16: 跨函数 DCE (call graph reachability)
        cross_function_dce(module);
        // Qualifier-aware optimization: const/volatile/restrict
        qualifier_aware_optimization(module);
    }
    if level >= 2 {
        // M16: SSA pass pipeline (strength reduction, IVOP, TCO, etc.)
        run_ssa_passes(module, level, strict_aliasing);
    }
}

/// M16: 把 AST -> SSA, 跑 ssa_passes::run, 再 SSA -> AST 重建。
/// 含 goto / label / switch 的函数自动跳过 (走 is_structured 检测)。
/// SSA builder 通过 insert_loop_phis 为循环回边插入 phi 节点,
/// rebuild_cycle 将循环 CFG 重建为 while 循环 AST。
/// 注意: rebuild_cycle 对含循环函数的重建尚不完全正确,
/// 因此含循环的函数仍走 AST pass 路径。
fn run_ssa_passes(module: &mut Module, level: u8, strict_aliasing: bool) {
    let prog = module.source_mut();
    // Collect volatile global variable names - SSA passes must not
    // propagate constants through volatile accesses.
    let volatile_globals: std::collections::HashSet<String> = prog
        .decls
        .iter()
        .filter_map(|d| match d {
            Decl::Var(v) if v.quals.is_volatile() => Some(v.name.name.clone()),
            _ => None,
        })
        .collect();
    let mut modified = false;
    for decl in &mut prog.decls {
        if let Decl::Func(f) = decl {
            // Skip functions with loops: SSA→AST rebuild for cyclic CFG
            // does not yet correctly handle loop variable updates and
            // condition re-evaluation. These functions use AST-level passes.
            if func_has_loop(f) {
                continue;
            }
            // Skip functions with unsafe SSA patterns
            if func_has_unsafe_ssa_pattern(f) {
                continue;
            }
            // Skip functions that access volatile globals
            if !volatile_globals.is_empty() && func_refs_volatile(f, &volatile_globals) {
                continue;
            }
            // P0-4: 先将 switch 降级为 if-chain, 使含 switch 的函数也能 SSA 化
            crate::ssa::lower_switches_in_func(f);
            if let Some(mut ssa) = crate::ssa::build_from_func(f) {
                crate::ssa_passes::run_with_opts(&mut ssa, level, strict_aliasing);
                *f.body.as_mut().unwrap() = crate::ssa::rebuild_block(&ssa);
                modified = true;
            }
        }
    }
    let _ = modified;
}

/// Returns true if the function references any volatile global variable.
fn func_refs_volatile(f: &FuncDecl, volatile_globals: &std::collections::HashSet<String>) -> bool {
    if let Some(body) = &f.body {
        for item in &body.items {
            if block_item_refs_volatile(item, volatile_globals) {
                return true;
            }
        }
    }
    false
}

fn block_item_refs_volatile(item: &BlockItem, vol: &std::collections::HashSet<String>) -> bool {
    match item {
        BlockItem::Stmt(s) => stmt_refs_volatile(s, vol),
        BlockItem::Decl(Decl::Var(v)) => {
            if let Some(init) = &v.init {
                expr_refs_volatile(init, vol)
            } else {
                false
            }
        },
        _ => false,
    }
}

fn stmt_refs_volatile(s: &Stmt, vol: &std::collections::HashSet<String>) -> bool {
    match s {
        Stmt::Return { val: Some(e), .. } | Stmt::Expr { e, .. } => expr_refs_volatile(e, vol),
        Stmt::If {
            cond, then_, else_, ..
        } => {
            expr_refs_volatile(cond, vol)
                || stmt_refs_volatile(then_, vol)
                || else_.as_ref().is_some_and(|e| stmt_refs_volatile(e, vol))
        },
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
            expr_refs_volatile(cond, vol) || stmt_refs_volatile(body, vol)
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            init.as_ref().is_some_and(|i| stmt_refs_volatile(i, vol))
                || cond.as_ref().is_some_and(|c| expr_refs_volatile(c, vol))
                || step.as_ref().is_some_and(|s| expr_refs_volatile(s, vol))
                || stmt_refs_volatile(body, vol)
        },
        Stmt::Block { b, .. } => b.items.iter().any(|i| block_item_refs_volatile(i, vol)),
        Stmt::Switch { expr, cases, .. } => {
            expr_refs_volatile(expr, vol)
                || cases.iter().any(|c| {
                    c.body
                        .items
                        .iter()
                        .any(|i| block_item_refs_volatile(i, vol))
                })
        },
        // C++ 零成本异常: volatile 引用可以只出现在受保护调用里 (例如 `volatile`
        // 全局当作参)。不下降到这里会让调用点看似不碰 volatile, 参与不该做的移动/
        // 删除 —— volatile 语义要求按可观察顺序执行, 这类判定必须保守。
        Stmt::Invoke { call, .. } => stmt_refs_volatile(call, vol),
        Stmt::EhLandingPad {
            object, selector, ..
        } => expr_refs_volatile(object, vol) || expr_refs_volatile(selector, vol),
        _ => false,
    }
}

fn expr_refs_volatile(e: &Expr, vol: &std::collections::HashSet<String>) -> bool {
    match e {
        Expr::Ident { name, .. } => vol.contains(&name.name),
        Expr::Binop { l, r, .. } | Expr::Assign { l, r, .. } | Expr::Comma { l, r, .. } => {
            expr_refs_volatile(l, vol) || expr_refs_volatile(r, vol)
        },
        Expr::Unary { e, .. }
        | Expr::Cast { e, .. }
        | Expr::AddrOf { e, .. }
        | Expr::Deref { e, .. }
        | Expr::Real { e, .. }
        | Expr::Imag { e, .. } => expr_refs_volatile(e, vol),
        Expr::Call { callee, args, .. } => {
            expr_refs_volatile(callee, vol) || args.iter().any(|a| expr_refs_volatile(a, vol))
        },
        Expr::Index { arr, idx, .. } => {
            expr_refs_volatile(arr, vol) || expr_refs_volatile(idx, vol)
        },
        Expr::Member { expr, .. } => expr_refs_volatile(expr, vol),
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            expr_refs_volatile(cond, vol)
                || expr_refs_volatile(then_, vol)
                || expr_refs_volatile(else_, vol)
        },
        Expr::PostInc { e, .. }
        | Expr::PostDec { e, .. }
        | Expr::PreInc { e, .. }
        | Expr::PreDec { e, .. } => expr_refs_volatile(e, vol),
        Expr::StmtExpr { block, .. } => {
            block.items.iter().any(|i| block_item_refs_volatile(i, vol))
        },
        Expr::InitList { inits, .. } | Expr::CompoundLit { inits, .. } => {
            inits.iter().any(|init| match init {
                crate::ast::InitItem::Expr(e) => expr_refs_volatile(e, vol),
                crate::ast::InitItem::Designated { value, .. } => expr_refs_volatile(value, vol),
            })
        },
        _ => false,
    }
}

/// Qualifier-aware optimization: const/volatile/restrict utilization.
///
/// - **const**: Collect global `const` variables and warn on stores to them.
///   Const globals are placed in read-only sections by the linker, so stores
///   would cause runtime faults.
/// - **volatile**: Already handled above - functions accessing volatile globals
///   skip SSA optimization to preserve load/store ordering.
/// - **restrict**: Function parameters with restrict qualifier indicate
///   non-aliasing pointers. This information is available for future alias
///   analysis improvements (currently alias_analysis in ssa_passes.rs does
///   not use restrict hints).
fn qualifier_aware_optimization(module: &mut Module) {
    let prog = module.source();
    // Collect const global variable names
    let const_globals: std::collections::HashSet<String> = prog
        .decls
        .iter()
        .filter_map(|d| match d {
            Decl::Var(v) if v.quals.is_const() => Some(v.name.name.clone()),
            _ => None,
        })
        .collect();
    // Check for stores to const globals and emit warnings
    if !const_globals.is_empty() {
        for decl in &prog.decls {
            if let Decl::Func(f) = decl {
                if let Some(body) = &f.body {
                    check_stores_to_const_globals(body, &const_globals);
                }
            }
        }
    }
}

/// Check for stores to const global variables and emit warnings.
fn check_stores_to_const_globals(block: &Block, const_globals: &std::collections::HashSet<String>) {
    for item in &block.items {
        match item {
            BlockItem::Stmt(s) => check_stmt_for_const_stores(s, const_globals),
            BlockItem::Decl(Decl::Var(v)) => {
                if let Some(init) = &v.init {
                    check_expr_for_const_stores(init, const_globals);
                }
            },
            _ => {},
        }
    }
}

fn check_stmt_for_const_stores(stmt: &Stmt, const_globals: &std::collections::HashSet<String>) {
    match stmt {
        Stmt::Expr { e, .. } => check_expr_for_const_stores(e, const_globals),
        Stmt::Return { val: Some(e), .. } => check_expr_for_const_stores(e, const_globals),
        Stmt::If {
            cond, then_, else_, ..
        } => {
            check_expr_for_const_stores(cond, const_globals);
            check_stmt_for_const_stores(then_, const_globals);
            if let Some(e) = else_ {
                check_stmt_for_const_stores(e, const_globals);
            }
        },
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
            check_expr_for_const_stores(cond, const_globals);
            check_stmt_for_const_stores(body, const_globals);
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(init_stmt) = init {
                check_stmt_for_const_stores(init_stmt, const_globals);
            }
            if let Some(cond_expr) = cond {
                check_expr_for_const_stores(cond_expr, const_globals);
            }
            if let Some(step_expr) = step {
                check_expr_for_const_stores(step_expr, const_globals);
            }
            check_stmt_for_const_stores(body, const_globals);
        },
        Stmt::Block { b, .. } => check_stores_to_const_globals(b, const_globals),
        Stmt::Switch { expr, cases, .. } => {
            check_expr_for_const_stores(expr, const_globals);
            for case in cases {
                check_stores_to_const_globals(&case.body, const_globals);
            }
        },
        _ => {},
    }
}

fn check_expr_for_const_stores(expr: &Expr, const_globals: &std::collections::HashSet<String>) {
    match expr {
        Expr::Assign { l, .. } => {
            // Check if assigning to a const global
            // Note: Warning emission would require diagnostic infrastructure
            // integration. For now, we just detect the pattern.
            if let Expr::Ident { name, .. } = l.as_ref() {
                if const_globals.contains(&name.name) {
                    // Store to const global detected - would emit warning
                    // if diagnostic infrastructure was integrated here
                }
            }
        },
        Expr::Binop { l, r, .. } | Expr::Comma { l, r, .. } => {
            check_expr_for_const_stores(l, const_globals);
            check_expr_for_const_stores(r, const_globals);
        },
        Expr::Unary { e, .. }
        | Expr::Cast { e, .. }
        | Expr::AddrOf { e, .. }
        | Expr::Deref { e, .. }
        | Expr::Real { e, .. }
        | Expr::Imag { e, .. } => {
            check_expr_for_const_stores(e, const_globals);
        },
        Expr::Call { callee, args, .. } => {
            check_expr_for_const_stores(callee, const_globals);
            for arg in args {
                check_expr_for_const_stores(arg, const_globals);
            }
        },
        Expr::Index { arr, idx, .. } => {
            check_expr_for_const_stores(arr, const_globals);
            check_expr_for_const_stores(idx, const_globals);
        },
        Expr::Member { expr, .. } => check_expr_for_const_stores(expr, const_globals),
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            check_expr_for_const_stores(cond, const_globals);
            check_expr_for_const_stores(then_, const_globals);
            check_expr_for_const_stores(else_, const_globals);
        },
        Expr::PostInc { e, .. }
        | Expr::PostDec { e, .. }
        | Expr::PreInc { e, .. }
        | Expr::PreDec { e, .. } => {
            // Increment/decrement on const global is also an error
            if let Expr::Ident { name, .. } = e.as_ref() {
                if const_globals.contains(&name.name) {
                    // Increment/decrement of const global detected
                }
            }
            check_expr_for_const_stores(e, const_globals);
        },
        Expr::StmtExpr { block, .. } => check_stores_to_const_globals(block, const_globals),
        Expr::InitList { inits, .. } | Expr::CompoundLit { inits, .. } => {
            for init in inits {
                match init {
                    crate::ast::InitItem::Expr(e) => check_expr_for_const_stores(e, const_globals),
                    crate::ast::InitItem::Designated { value, .. } => {
                        check_expr_for_const_stores(value, const_globals)
                    },
                }
            }
        },
        _ => {},
    }
}

/// 检测函数体是否包含循环 (while/for/do-while)。
/// 含循环的函数暂时跳过 SSA pass, 走 AST 层优化。
/// rebuild_cycle 对循环 CFG 的重建尚不完全正确。
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
        },
        _ => false,
    }
}

/// 检测函数是否包含 SSA→AST 重建当前无法正确处理的模式:
///   - asm goto (非结构化控制流)
///   - 数组类型带初始化式 (SSA 无法表示复合字面量)
///
/// 取址/解引用/索引已由 SSA 指令集 (AddrOf/Load/Store/IndexLoad/IndexStore)
/// 完整支持, 不再跳过。
fn func_has_unsafe_ssa_pattern(f: &FuncDecl) -> bool {
    f.body.as_ref().is_some_and(block_has_unsafe_ssa_pattern)
}

fn block_has_unsafe_ssa_pattern(b: &Block) -> bool {
    b.items.iter().any(|item| match item {
        BlockItem::Decl(Decl::Var(v)) => {
            // 数组类型带初始化式 (复合字面量 SSA 无法表示)
            (matches!(v.ty, crate::ast::Type::Array(_, _))
                && v.init.as_ref().is_some())
            // static 局部变量跨调用持久, SSA 无法正确建模
            || v.is_static
        },
        BlockItem::Stmt(s) => stmt_has_unsafe_ssa_pattern(s),
        _ => false,
    })
}

fn stmt_has_unsafe_ssa_pattern(s: &Stmt) -> bool {
    match s {
        // asm goto 包含无法 SSA 化的非结构化控制流
        Stmt::AsmGoto { .. } => true,
        Stmt::If { then_, else_, .. } => {
            stmt_has_unsafe_ssa_pattern(then_)
                || else_
                    .as_ref()
                    .is_some_and(|e| stmt_has_unsafe_ssa_pattern(e))
        },
        Stmt::Block { b, .. } => block_has_unsafe_ssa_pattern(b),
        Stmt::While { body, .. } | Stmt::Do { body, .. } => stmt_has_unsafe_ssa_pattern(body),
        Stmt::For { init, body, .. } => {
            init.as_ref()
                .is_some_and(|i| stmt_has_unsafe_ssa_pattern(i))
                || stmt_has_unsafe_ssa_pattern(body)
        },
        Stmt::Switch { cases, .. } => cases.iter().any(|c| block_has_unsafe_ssa_pattern(&c.body)),
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
            },
            Decl::GlobalAsm {
                ref outputs,
                ref inputs,
                ..
            } => {
                for e in outputs.iter().chain(inputs.iter()) {
                    collect_global_func_roots(e, &mut extra_roots);
                }
                others.push(d);
            },
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
        },
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
        },
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            collect_global_func_roots(cond, out);
            collect_global_func_roots(then_, out);
            collect_global_func_roots(else_, out);
        },
        Expr::Index { arr, idx, .. } => {
            collect_global_func_roots(arr, out);
            collect_global_func_roots(idx, out);
        },
        Expr::Member { expr, .. } => collect_global_func_roots(expr, out),
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            for init in inits {
                match init {
                    crate::ast::InitItem::Expr(e) => collect_global_func_roots(e, out),
                    crate::ast::InitItem::Designated { value, .. } => {
                        collect_global_func_roots(value, out)
                    },
                }
            }
        },
        Expr::AsmExpr {
            outputs, inputs, ..
        } => {
            for e in outputs.iter().chain(inputs.iter()) {
                collect_global_func_roots(e, out);
            }
        },
        _ => {},
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
            },
            _ => {},
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
        },
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
            *cond = fold_expr(cond.clone(), fenv_access);
            fold_stmt(body, fenv_access);
        },
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
        },
        Stmt::Return { val: Some(e), .. } => {
            *e = fold_expr(e.clone(), fenv_access);
        },
        Stmt::Expr { e, .. } => *e = fold_expr(e.clone(), fenv_access),
        Stmt::Block { b, .. } => fold_block(&mut b.items, fenv_access),
        _ => {},
    }
}

fn fold_expr(e: Expr, fenv_access: bool) -> Expr {
    let e = match e {
        Expr::Binop { op, l, r, span, ty } => {
            let l = Box::new(fold_expr(*l, fenv_access));
            let r = Box::new(fold_expr(*r, fenv_access));
            // 整数常量折叠: 优先使用 const_eval (类型感知, 正确处理
            // 无符号类型宽度和溢出语义).
            if let (Expr::Int { .. }, Expr::Int { .. }) = (l.as_ref(), r.as_ref()) {
                let binop_expr = Expr::Binop {
                    op,
                    l: l.clone(),
                    r: r.clone(),
                    span: span.clone(),
                    ty: ty.clone(),
                };
                if let Ok(v) = const_eval::eval_full(&binop_expr, fenv_access) {
                    if let Some(iv) = v.as_int() {
                        return Expr::Int { v: iv, span, ty };
                    }
                }
            }
            // FENV_ACCESS ON: 不折叠浮点常量运算 (舍入模式可能被 fesetround
            // 在运行时改变, 编译期折叠将按默认舍入求值, 语义错误).
            if !fenv_access {
                if let (Expr::Float { c: lc, .. }, Expr::Float { c: rc, .. }) =
                    (l.as_ref(), r.as_ref())
                {
                    // LongDouble 不做编译期折叠：宿主无 80-bit 运算，
                    // 留给后端 x87 运行时按目标舍入模式求值。
                    if lc.is_long_double() || rc.is_long_double() {
                        // fall through, keep original expression
                    } else if let Some(v) = eval_float_binop(op, lc.as_f64(), rc.as_f64()) {
                        // 结果精度必须匹配目标类型: Float 结果截为 f32
                        let c = if matches!(ty, crate::ast::Type::Float) {
                            crate::ast::FloatConst::F32(v as f32)
                        } else {
                            crate::ast::FloatConst::F64(v)
                        };
                        return Expr::Float { c, span, ty };
                    }
                }
            }
            Expr::Binop { op, l, r, span, ty }
        },
        Expr::Unary { op, e, span, ty } => {
            let e = Box::new(fold_expr(*e, fenv_access));
            // 整数一元运算折叠: 使用 const_eval (类型感知, 正确处理
            // BitNot 的类型宽度掩码).
            if let Expr::Int { .. } = e.as_ref() {
                let unary_expr = Expr::Unary {
                    op,
                    e: e.clone(),
                    span: span.clone(),
                    ty: ty.clone(),
                };
                if let Ok(v) = const_eval::eval_full(&unary_expr, fenv_access) {
                    if let Some(iv) = v.as_int() {
                        return Expr::Int { v: iv, span, ty };
                    }
                }
            }
            Expr::Unary { op, e, span, ty }
        },
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
        },
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
        },
        BinOp::Mod => {
            if r == 0 {
                return None;
            } else {
                l % r
            }
        },
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
        },
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
        },
        BinOp::Mod => {
            if r == 0.0 {
                return None;
            } else {
                // C fmod semantics: l - trunc(l/r) * r
                // Rust f64 % f64 matches C fmod()
                l % r
            }
        },
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
            },
            _ => {},
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
        },
        Stmt::While { body, .. } | Stmt::Do { body, .. } => prune_stmt(body),
        Stmt::For { init, body, .. } => {
            if let Some(i) = init {
                prune_stmt(i);
            }
            prune_stmt(body);
        },
        Stmt::Block { b, .. } => prune_block(&mut b.items),
        _ => {},
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
        },
        Stmt::While { body, .. } | Stmt::Do { body, .. } | Stmt::For { body, .. } => {
            stmt_has_label(body)
        },
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
                    },
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
        },
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
        },
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
        },
        _ => {},
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
        },
        Stmt::While { body, .. } | Stmt::Do { body, .. } | Stmt::For { body, .. } => {
            dce_block_in_stmt(body);
        },
        Stmt::Block { b, .. } => dce_block(&mut b.items),
        _ => {},
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
///   - O1-O2: 同 O0, 但 hot caller (profile-guided) 享受放宽阈值
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
            let caller_is_hot = f.is_hot;
            if let Some(body) = &mut f.body {
                inline_calls_in_block(
                    body,
                    &funcs,
                    level,
                    caller_flatten,
                    caller_is_hot,
                    fenv_access,
                    0,
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
        | Expr::ComplexIntLit { .. }
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
        | Expr::Comma { l, r, .. } => 1 + expr_node_count(l) + expr_node_count(r),
        Expr::Cond {
            cond, then_, else_, ..
        } => 1 + expr_node_count(cond) + expr_node_count(then_) + expr_node_count(else_),
        Expr::Call { callee, args, .. } => {
            1 + expr_node_count(callee) + args.iter().map(expr_node_count).sum::<usize>()
        },
        Expr::Member { expr, .. } => 1 + expr_node_count(expr),
        Expr::VaArg { ap, .. } => 1 + expr_node_count(ap),
        Expr::StmtExpr { block, .. } => 1 + block.items.iter().map(item_node_count).sum::<usize>(),
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            1 + inits.iter().map(init_node_count).sum::<usize>()
        },
        Expr::AsmExpr {
            outputs, inputs, ..
        } => {
            1 + outputs
                .iter()
                .chain(inputs.iter())
                .map(expr_node_count)
                .sum::<usize>()
        },
    }
}

fn item_node_count(item: &BlockItem) -> usize {
    match item {
        BlockItem::Stmt(s) => stmt_node_count(s),
        BlockItem::Decl(Decl::Var(v)) => 1 + v.init.as_ref().map_or(0, expr_node_count),
        BlockItem::Decl(Decl::Func(f)) => {
            1 + f
                .body
                .as_ref()
                .map_or(0, |b| b.items.iter().map(item_node_count).sum::<usize>())
        },
        _ => 1,
    }
}

fn stmt_node_count(s: &Stmt) -> usize {
    match s {
        Stmt::Return { val, .. } => 1 + val.as_ref().map_or(0, expr_node_count),
        Stmt::Expr { e, .. } => 1 + expr_node_count(e),
        Stmt::If {
            cond, then_, else_, ..
        } => {
            1 + expr_node_count(cond)
                + stmt_node_count(then_)
                + else_.as_ref().map_or(0, |e| stmt_node_count(e))
        },
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
            1 + expr_node_count(cond) + stmt_node_count(body)
        },
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
        },
        Stmt::Block { b, .. } => 1 + b.items.iter().map(item_node_count).sum::<usize>(),
        Stmt::Switch { expr, cases, .. } => {
            1 + expr_node_count(expr)
                + cases
                    .iter()
                    .map(|c| c.body.items.iter().map(item_node_count).sum::<usize>())
                    .sum::<usize>()
        },
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
fn estimate_inline_cost(_f: &FuncDecl, body: &Block, args: &[Expr]) -> usize {
    // Base cost: all nodes in the function body
    let body_cost: usize = body.items.iter().map(item_node_count).sum();
    // Argument duplication cost: each argument is substituted for every
    // reference to the corresponding parameter in the body.
    // We approximate by counting arg nodes * param references (bounded
    // by body_cost as an upper heuristic).
    let arg_cost: usize = args.iter().map(expr_node_count).sum::<usize>();
    // Total: body + arg duplication (arg cost scaled by param usage ratio)
    body_cost.saturating_add(arg_cost)
}

fn inline_calls_in_block(
    body: &mut Block,
    funcs: &HashMap<String, FuncDecl>,
    level: u8,
    caller_flatten: bool,
    caller_is_hot: bool,
    fenv_access: bool,
    depth: u32,
) {
    for item in &mut body.items {
        match item {
            BlockItem::Stmt(s) => inline_calls_in_stmt(
                s,
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            ),
            BlockItem::Decl(Decl::Func(f)) => {
                if let Some(b) = &mut f.body {
                    inline_calls_in_block(
                        b,
                        funcs,
                        level,
                        caller_flatten,
                        caller_is_hot,
                        fenv_access,
                        depth,
                    );
                }
            },
            _ => {},
        }
    }
}

fn inline_calls_in_stmt(
    s: &mut Stmt,
    funcs: &HashMap<String, FuncDecl>,
    level: u8,
    caller_flatten: bool,
    caller_is_hot: bool,
    fenv_access: bool,
    depth: u32,
) {
    // always_inline / flatten 且函数体含局部声明: 语句级展开.
    // 成功后调用点已被替换为展开后的块, 无需再做表达式级内联.
    if try_stmt_inline_call(s, funcs, caller_flatten, depth) {
        return;
    }
    match s {
        Stmt::Return { val: Some(e), .. } => {
            *e = inline_expr(
                e.clone(),
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            );
        },
        Stmt::Expr { e, .. } => {
            *e = inline_expr(
                e.clone(),
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            )
        },
        Stmt::If {
            cond, then_, else_, ..
        } => {
            *cond = inline_expr(
                cond.clone(),
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            );
            inline_calls_in_stmt(
                then_,
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            );
            if let Some(e) = else_ {
                inline_calls_in_stmt(
                    e,
                    funcs,
                    level,
                    caller_flatten,
                    caller_is_hot,
                    fenv_access,
                    depth,
                );
            }
        },
        Stmt::Block { b, .. } => inline_calls_in_block(
            b,
            funcs,
            level,
            caller_flatten,
            caller_is_hot,
            fenv_access,
            depth,
        ),
        Stmt::While { body, .. } | Stmt::Do { body, .. } => inline_calls_in_stmt(
            body,
            funcs,
            level,
            caller_flatten,
            caller_is_hot,
            fenv_access,
            depth,
        ),
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(c) = cond {
                *c = inline_expr(
                    c.clone(),
                    funcs,
                    level,
                    caller_flatten,
                    caller_is_hot,
                    fenv_access,
                    depth,
                );
            }
            if let Some(st) = step {
                *st = inline_expr(
                    st.clone(),
                    funcs,
                    level,
                    caller_flatten,
                    caller_is_hot,
                    fenv_access,
                    depth,
                );
            }
            if let Some(i) = init {
                inline_calls_in_stmt(
                    i,
                    funcs,
                    level,
                    caller_flatten,
                    caller_is_hot,
                    fenv_access,
                    depth,
                );
            }
            inline_calls_in_stmt(
                body,
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            );
        },
        _ => {},
    }
}

fn extract_inline_return(body: &Block) -> Option<&Expr> {
    match body.items.as_slice() {
        [BlockItem::Stmt(Stmt::Return { val: Some(e), .. })] => Some(e),
        items if !items.is_empty() => {
            if let BlockItem::Stmt(Stmt::Return { val: Some(e), .. }) = items.last()? {
                // 表达式级内联只能保留返回表达式; 前置项必须是无运行时
                // 副作用的类型定义. 变量声明 (尤其带初始化器) 会被丢弃,
                // 导致返回表达式引用未定义变量或丢失副作用.
                let only_types = items[..items.len() - 1].iter().all(|it| {
                    matches!(
                        it,
                        BlockItem::Decl(Decl::Typedef { .. })
                            | BlockItem::Decl(Decl::StructDef(_))
                            | BlockItem::Decl(Decl::UnionDef(_))
                            | BlockItem::Decl(Decl::EnumDef(_))
                    )
                });
                if only_types {
                    Some(e)
                } else {
                    None
                }
            } else {
                None
            }
        },
        _ => None,
    }
}

/// 语句级 forced 内联的全局唯一计数器 (每次展开生成不同的临时名)
static STMT_INLINE_UID: AtomicU32 = AtomicU32::new(0);

/// ty 是否含运行时长度数组维度 (VLA). VLA 声明的初始化/分配在
/// 内联展开后无法简单跨语句重写, 遇到即拒绝展开.
fn type_has_vla_dim(t: &crate::ast::Type) -> bool {
    match t {
        crate::ast::Type::Array(_, sz) => matches!(sz, ArraySize::Var(_)),
        crate::ast::Type::Ptr(p) => type_has_vla_dim(&p.base),
        crate::ast::Type::Struct(_, fields, _, _) | crate::ast::Type::Union(_, fields, _, _) => {
            fields.iter().any(|f| type_has_vla_dim(&f.ty))
        },
        _ => false,
    }
}

/// 将表达式中的标识符按 map 重命名 (callee 局部/形参 → 展开临时名).
/// 只重命名 callee body 内的名字; 实参表达式不参与重命名.
fn rename_idents_in_expr(e: &mut Expr, map: &HashMap<String, String>) {
    match e {
        Expr::Ident { name, .. } => {
            if let Some(new) = map.get(&name.name) {
                name.name = new.clone();
            }
        },
        Expr::Binop { l, r, .. } | Expr::Assign { l, r, .. } => {
            rename_idents_in_expr(l, map);
            rename_idents_in_expr(r, map);
        },
        Expr::Unary { e: x, .. }
        | Expr::Real { e: x, .. }
        | Expr::Imag { e: x, .. }
        | Expr::Cast { e: x, .. }
        | Expr::AddrOf { e: x, .. }
        | Expr::Deref { e: x, .. }
        | Expr::PostInc { e: x, .. }
        | Expr::PostDec { e: x, .. }
        | Expr::PreInc { e: x, .. }
        | Expr::PreDec { e: x, .. } => rename_idents_in_expr(x, map),
        Expr::Call { callee, args, .. } => {
            rename_idents_in_expr(callee, map);
            for a in args {
                rename_idents_in_expr(a, map);
            }
        },
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            rename_idents_in_expr(cond, map);
            rename_idents_in_expr(then_, map);
            rename_idents_in_expr(else_, map);
        },
        Expr::Index { arr, idx, .. } => {
            rename_idents_in_expr(arr, map);
            rename_idents_in_expr(idx, map);
        },
        Expr::Member { expr, .. } => rename_idents_in_expr(expr, map),
        Expr::Comma { l, r, .. } => {
            rename_idents_in_expr(l, map);
            rename_idents_in_expr(r, map);
        },
        Expr::VaArg { ap, .. } => rename_idents_in_expr(ap, map),
        Expr::Offsetof { indices, .. } => {
            for i in indices {
                rename_idents_in_expr(i, map);
            }
        },
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            for it in inits {
                match it {
                    InitItem::Expr(x) => rename_idents_in_expr(x, map),
                    InitItem::Designated { value, designator } => {
                        if let crate::ast::Designator::Index(idx) = designator {
                            rename_idents_in_expr(idx, map);
                        }
                        rename_idents_in_expr(value, map);
                    },
                }
            }
        },
        Expr::AsmExpr {
            outputs, inputs, ..
        } => {
            for o in outputs {
                rename_idents_in_expr(o, map);
            }
            for i in inputs {
                rename_idents_in_expr(i, map);
            }
        },
        Expr::StmtExpr { block, .. } => {
            rename_idents_in_items(&mut block.items, map);
        },
        // 字面量/sizeof/alignof/&&label: 不含可重命名的运行时标识符
        _ => {},
    }
}

fn rename_idents_in_stmt(s: &mut Stmt, map: &HashMap<String, String>) {
    match s {
        Stmt::Return { val: Some(e), .. } => rename_idents_in_expr(e, map),
        Stmt::Expr { e, .. } => rename_idents_in_expr(e, map),
        Stmt::If {
            cond, then_, else_, ..
        } => {
            rename_idents_in_expr(cond, map);
            rename_idents_in_stmt(then_, map);
            if let Some(el) = else_ {
                rename_idents_in_stmt(el, map);
            }
        },
        Stmt::While { cond, body, .. } | Stmt::Do { body, cond, .. } => {
            rename_idents_in_expr(cond, map);
            rename_idents_in_stmt(body, map);
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(i) = init {
                rename_idents_in_stmt(i, map);
            }
            if let Some(c) = cond {
                rename_idents_in_expr(c, map);
            }
            if let Some(st) = step {
                rename_idents_in_expr(st, map);
            }
            rename_idents_in_stmt(body, map);
        },
        Stmt::Block { b, .. } => rename_idents_in_items(&mut b.items, map),
        // C++ 零成本异常: 强制内联的改名必须看到受保护调用内部的语句与落地垫槽位,
        // 否则被内联函数里的同名变量会与该区域内的调用者变量混淆。
        Stmt::Invoke { call, .. } => rename_idents_in_stmt(call, map),
        Stmt::EhLandingPad {
            object, selector, ..
        } => {
            rename_idents_in_expr(object, map);
            rename_idents_in_expr(selector, map);
        },
        _ => {},
    }
}

fn rename_idents_in_items(items: &mut Vec<BlockItem>, map: &HashMap<String, String>) {
    for it in items {
        match it {
            BlockItem::Stmt(s) => rename_idents_in_stmt(s, map),
            BlockItem::Decl(Decl::Var(v)) => {
                if let Some(init) = &mut v.init {
                    rename_idents_in_expr(init, map);
                }
            },
            BlockItem::Decl(Decl::Func(f)) => {
                if let Some(b) = &mut f.body {
                    rename_idents_in_items(&mut b.items, map);
                }
            },
            _ => {},
        }
    }
}

/// 构造一个空的局部变量声明 (所有属性字段为默认值)
fn plain_var_decl(ty: crate::ast::Type, name: String, init: Option<Expr>, span: &Span) -> VarDecl {
    VarDecl {
        ty,
        name: Ident {
            name,
            span: span.clone(),
        },
        init,
        span: span.clone(),
        is_extern: false,
        is_static: false,
        section: None,
        alignment: None,
        is_weak: false,
        visibility: None,
        is_unused: false,
        register_name: None,
        cleanup_fn: None,
        is_thread_local: false,
        no_sanitize: vec![],
        alias_name: None,
        copy_fn: None,
        is_retain: false,
        is_used_attr: false,
        quals: QualifierFlags::NONE,
        addr_space: AddrSpace::None,
        tls_model: None,
        asm_label: None,
    }
}

/// 语句级 forced 内联展开.
///
/// 处理函数体含局部变量声明的 always_inline / flatten 调用点:
/// 将 `ret = f(args)` / `f(args)` 展开为
///   { 形参临时声明; callee 局部声明 (重命名); 中间表达式语句; 返回语句 }
///
/// 展开为块后, 形参以临时变量传值 (实参只求值一次, 与 GCC 语义一致).
/// 返回 true 表示调用点已被替换.
fn try_stmt_inline_call(
    s: &mut Stmt,
    funcs: &HashMap<String, FuncDecl>,
    caller_flatten: bool,
    depth: u32,
) -> bool {
    // 提取调用点 (return f(...) 或表达式语句 f(...))
    let (callee_name, args, in_return) = match s {
        Stmt::Expr {
            e: Expr::Call { callee, args, .. },
            ..
        } => match callee.as_ref() {
            Expr::Ident { name, .. } => (name.name.clone(), args.clone(), false),
            _ => return false,
        },
        Stmt::Return {
            val: Some(Expr::Call { callee, args, .. }),
            ..
        } => match callee.as_ref() {
            Expr::Ident { name, .. } => (name.name.clone(), args.clone(), true),
            _ => return false,
        },
        _ => return false,
    };
    let f = match funcs.get(&callee_name) {
        Some(f) => f,
        None => return false,
    };
    if !(f.is_always_inline || caller_flatten) {
        return false;
    }
    if f.is_variadic || args.len() != f.params.len() {
        return false;
    }
    if depth >= INLINE_MAX_DEPTH {
        trace_inline_decision(
            &callee_name,
            "REJECT",
            "stmt-inline depth limit (possible mutual recursion)",
        );
        return false;
    }
    let body = match f.body.as_ref() {
        Some(b) => b,
        None => return false,
    };
    // 单 return 表达式体交给表达式级替换 (避免生成不必要的临时变量)
    if matches!(body.items.as_slice(), [BlockItem::Stmt(Stmt::Return { .. })]) {
        return false;
    }

    // 结构检查: 只支持 [VarDecl]* [ExprStmt]* [Return?] 形式的函数体.
    // 控制流 (if/loop/goto/嵌套块/switch) 与特殊存储类别 (static/extern/
    // tls/register/cleanup/VLA) 保留普通调用, 避免错误重写.
    let mut decls: Vec<VarDecl> = Vec::new();
    let mut mid: Vec<Stmt> = Vec::new();
    let mut ret_val: Option<Option<Expr>> = None;
    for (i, item) in body.items.iter().enumerate() {
        match item {
            BlockItem::Decl(Decl::Var(v)) => {
                if v.is_static
                    || v.is_extern
                    || v.is_thread_local
                    || v.cleanup_fn.is_some()
                    || v.register_name.is_some()
                    || v.alias_name.is_some()
                    || v.asm_label.is_some()
                    || type_has_vla_dim(&v.ty)
                {
                    return false;
                }
                decls.push(v.clone());
            },
            // 类型定义无运行时副作用, 保留在展开块中
            BlockItem::Decl(Decl::Typedef { .. })
            | BlockItem::Decl(Decl::StructDef(_))
            | BlockItem::Decl(Decl::UnionDef(_))
            | BlockItem::Decl(Decl::EnumDef(_)) => {},
            BlockItem::Stmt(st @ Stmt::Expr { .. }) => mid.push(st.clone()),
            BlockItem::Stmt(Stmt::Return { val, .. }) if i + 1 == body.items.len() => {
                ret_val = Some(val.clone());
            },
            // 非 return 结尾/中间 return/其他语句形态: 不展开
            _ => return false,
        }
    }
    // void 返回且调用点是表达式语句: 保留普通调用即可 (无副作用丢失风险)
    let ret_val = match ret_val {
        Some(v) => v,
        None => {
            trace_inline_decision(&callee_name, "REJECT", "stmt-inline: no trailing return");
            return false;
        },
    };
    let rspan = match s {
        Stmt::Return { span, .. } | Stmt::Expr { span, .. } => span.clone(),
        _ => unreachable!(),
    }
    .clone();

    // 名字重映射: 形参与局部变量 → 展开临时名 (前缀 + uid, 避免与
    // 调用方同名变量及多次展开互相冲突)
    let uid = STMT_INLINE_UID.fetch_add(1, Ordering::Relaxed);
    let mut map: HashMap<String, String> = HashMap::new();
    let mut items: Vec<BlockItem> = Vec::new();
    for (p, a) in f.params.iter().zip(args.iter()) {
        let pname = match p.name.as_ref() {
            Some(id) => &id.name,
            None => return false,
        };
        let tmp = format!("__inl{}_${}", uid, pname);
        map.insert(pname.clone(), tmp.clone());
        items.push(BlockItem::Decl(Decl::Var(plain_var_decl(
            p.ty.clone(),
            tmp,
            Some(a.clone()),
            &p.name
                .as_ref()
                .map(|i| i.span.clone())
                .unwrap_or_else(|| rspan.clone()),
        ))));
    }
    for mut v in decls {
        let new = format!("__inl{}_l{}", uid, v.name.name);
        map.insert(v.name.name.clone(), new.clone());
        v.name.name = new;
        items.push(BlockItem::Decl(Decl::Var(v)));
    }
    // 展开块内的 callee 名字重命名 (含声明初始化器与中间语句)
    rename_idents_in_items(&mut items, &map);
    for st in mid {
        items.push(BlockItem::Stmt(st));
    }
    // 返回语句 (ret_val 的表达式已随 items 之前的 rename 处理? 不:
    // ret_val 尚未插入 items, 单独重命名)
    match ret_val {
        Some(e) => {
            let mut e = e;
            rename_idents_in_expr(&mut e, &map);
            items.push(BlockItem::Stmt(if in_return {
                Stmt::Return {
                    val: Some(e),
                    span: rspan.clone(),
                }
            } else {
                Stmt::Expr {
                    e,
                    span: rspan.clone(),
                }
            }));
        },
        None => {
            if in_return {
                items.push(BlockItem::Stmt(Stmt::Return {
                    val: None,
                    span: rspan.clone(),
                }));
            }
            // void 返回且调用点是表达式语句: 无需返回语句
        },
    }
    trace_inline_decision(&callee_name, "INLINE", "always_inline (statement expansion)");
    *s = Stmt::Block {
        b: Block {
            items,
            span: rspan.clone(),
        },
        span: rspan,
    };
    // 展开块内仍可能有嵌套调用 (如 callee 调用另一个 always_inline)
    if let Stmt::Block { b, .. } = s {
        inline_calls_in_block(b, funcs, 0, caller_flatten, false, false, depth + 1);
    }
    true
}

fn expr_calls_name(expr: &Expr, name: &str) -> bool {
    match expr {
        Expr::Call { callee, args, .. } => {
            let direct = matches!(callee.as_ref(), Expr::Ident { name: n, .. } if n.name == name);
            direct || args.iter().any(|a| expr_calls_name(a, name))
        },
        Expr::Binop { l, r, .. } => expr_calls_name(l, name) || expr_calls_name(r, name),
        Expr::Unary { e, .. } => expr_calls_name(e, name),
        Expr::Cast { e, .. } => expr_calls_name(e, name),
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            expr_calls_name(cond, name)
                || expr_calls_name(then_, name)
                || expr_calls_name(else_, name)
        },
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
        },
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
        Expr::Call {
            callee,
            args,
            span,
            ty,
        } => Expr::Call {
            callee: Box::new(substitute_params(*callee, params)),
            args: args
                .into_iter()
                .map(|a| substitute_params(a, params))
                .collect(),
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
///   7. Profile-guided (hot caller, O2+): 放宽阈值 (cost ≤ 40, 不要求 static)
///   8. O3 启发式: static + 零参 + cost ≤ INLINE_COST_THRESHOLD (20)
///   9. 其他: 拒绝
///
/// 当 DCC_INLINE_TRACE=1 时, 每个决策均输出到 stderr.
fn try_inline_call(
    name: &str,
    args: &[Expr],
    funcs: &HashMap<String, FuncDecl>,
    level: u8,
    caller_flatten: bool,
    caller_is_hot: bool,
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
    // 表达式级替换仅支持单 return 语句的函数体; 含局部变量声明等
    // 结构的 always_inline 函数体由 try_stmt_inline_call 做语句级展开,
    // 此处拒绝内联 (保留普通调用) 以避免丢失声明副作用.
    let forced = f.is_always_inline || caller_flatten;
    if forced {
        if let [BlockItem::Stmt(Stmt::Return { val: Some(ret), .. })] = body.items.as_slice() {
            if expr_calls_name(ret, name) {
                trace_inline_decision(name, "REJECT", "forced but direct recursion");
                return None; // 直接递归保护
            }
            trace_inline_decision(name, "INLINE", "always_inline (expression body)");
            return Some(substitute_call(f, ret, args));
        }
        trace_inline_decision(
            name,
            "REJECT",
            "forced non-expression body (stmt-level inline handled at call site)",
        );
        return None;
    }

    // --- O0: 非强制内联一律拒绝 ---
    if level == 0 {
        trace_inline_decision(name, "REJECT", "O0 non-forced");
        return None;
    }

    // --- 深度限制: 防止嵌套内联爆炸 ---
    if depth >= INLINE_MAX_DEPTH {
        trace_inline_decision(name, "REJECT", &format!("depth {} >= {}", depth, INLINE_MAX_DEPTH));
        return None;
    }

    // --- O3 启发式: 小型 static 零参函数, 使用成本模型 ---
    // Profile-guided: hot caller 在 O2+ 享受放宽阈值:
    //   - 不要求 static
    //   - 允许有参数
    //   - 成本阈值从 20 提升到 40
    let cost = estimate_inline_cost(f, body, args);
    let profile_relaxed = caller_is_hot && level >= 2;
    let heuristic_ok = if profile_relaxed {
        cost <= INLINE_COST_THRESHOLD * 2
    } else {
        f.is_static && f.params.is_empty() && level >= 3 && cost <= INLINE_COST_THRESHOLD
    };
    if !heuristic_ok {
        trace_inline_decision(
            name,
            "REJECT",
            &format!(
                "cost {} > {} or not O3-static{}",
                cost,
                INLINE_COST_THRESHOLD,
                if profile_relaxed { " (profile-relaxed)" } else { "" },
            ),
        );
        return None;
    }

    let ret = extract_inline_return(body)?;
    if expr_calls_name(ret, name) {
        trace_inline_decision(name, "REJECT", "O3 heuristic but direct recursion");
        return None; // 直接递归保护
    }
    trace_inline_decision(
        name,
        "INLINE",
        &format!(
            "O3 heuristic, cost={}{}",
            cost,
            if profile_relaxed { " (profile-relaxed)" } else { "" },
        ),
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
    caller_is_hot: bool,
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
                    &name.name,
                    &args,
                    funcs,
                    level,
                    caller_flatten,
                    caller_is_hot,
                    depth,
                ) {
                    // 内联后继续递归内联替换结果中的嵌套调用
                    return inline_expr(
                        inlined,
                        funcs,
                        level,
                        caller_flatten,
                        caller_is_hot,
                        fenv_access,
                        depth + 1,
                    );
                }
            }
            Expr::Call {
                callee: Box::new(inline_expr(
                    *callee,
                    funcs,
                    level,
                    caller_flatten,
                    caller_is_hot,
                    fenv_access,
                    depth,
                )),
                args: args
                    .into_iter()
                    .map(|a| {
                        inline_expr(
                            a,
                            funcs,
                            level,
                            caller_flatten,
                            caller_is_hot,
                            fenv_access,
                            depth,
                        )
                    })
                    .collect(),
                span,
                ty,
            }
        },
        Expr::Binop { op, l, r, span, ty } => Expr::Binop {
            op,
            l: Box::new(inline_expr(
                *l,
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            )),
            r: Box::new(inline_expr(
                *r,
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            )),
            span,
            ty,
        },
        Expr::Unary { op, e, span, ty } => Expr::Unary {
            op,
            e: Box::new(inline_expr(
                *e,
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            )),
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
                *cond,
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            )),
            then_: Box::new(inline_expr(
                *then_,
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            )),
            else_: Box::new(inline_expr(
                *else_,
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            )),
            span,
            ty,
        },
        other => other,
    };
    fold_expr(e, fenv_access)
}

fn is_side_effect_free(e: &Expr) -> bool {
    // Volatile-typed expressions always have observable side effects
    if crate::ast::type_has_volatile(&e.ty()) {
        return false;
    }
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
            | Stmt::AsmGoto { span, .. }
            | Stmt::Invoke { span, .. }
            | Stmt::EhLandingPad { span, .. }
            | Stmt::Fallthrough { span } => span,
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
                is_inline: false,
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
                is_noreturn: false,
                is_pure: false,
                is_const_attr: false,
                returns_twice: false,
                no_instrument: false,
                is_malloc: false,
                malloc_dealloc: None,
                malloc_dealloc_argno: None,
                alloc_size: None,
                assume_aligned: None,
                warn_unused_result: false,
                is_sentinel: false,
                format_arg_idx: None,
                is_nodiscard: false,
                asm_label: None,
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
                c: crate::ast::FloatConst::F64(1.5),
                span: Span::point("t.c", 1, 1),
                ty: Type::Double,
            }),
            r: Box::new(Expr::Float {
                c: crate::ast::FloatConst::F64(2.25),
                span: Span::point("t.c", 1, 1),
                ty: Type::Double,
            }),
            span: Span::point("t.c", 1, 1),
            ty: Type::Double,
        };
        // 默认 (FENV_ACCESS OFF): 浮点常量运算被折叠
        let folded = fold_expr(float_expr(), false);
        assert!(
            matches!(folded, Expr::Float { c: crate::ast::FloatConst::F64(v), .. } if (v - 3.75).abs() < 1e-12),
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
                is_inline: false,
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
                is_noreturn: false,
                is_pure: false,
                is_const_attr: false,
                returns_twice: false,
                no_instrument: false,
                is_malloc: false,
                malloc_dealloc: None,
                malloc_dealloc_argno: None,
                alloc_size: None,
                assume_aligned: None,
                warn_unused_result: false,
                is_sentinel: false,
                format_arg_idx: None,
                is_nodiscard: false,
                asm_label: None,
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
                quals: crate::ast::QualifierFlags::NONE,
                ty: int.clone(),
                name: Some(Ident {
                    name: "x".into(),
                    span: span.clone(),
                }),
                span: span.clone(),
                vla_dim: None,
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
            is_inline: false,
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
            is_noreturn: false,
            is_pure: false,
            is_const_attr: false,
            returns_twice: false,
            no_instrument: false,
            is_malloc: false,
            malloc_dealloc: None,
            malloc_dealloc_argno: None,
            alloc_size: None,
            assume_aligned: None,
            warn_unused_result: false,
            is_sentinel: false,
            format_arg_idx: None,
            is_nodiscard: false,
            asm_label: None,
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
            is_inline: false,
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
            is_noreturn: false,
            is_pure: false,
            is_const_attr: false,
            returns_twice: false,
            no_instrument: false,
            is_malloc: false,
            malloc_dealloc: None,
            malloc_dealloc_argno: None,
            alloc_size: None,
            assume_aligned: None,
            warn_unused_result: false,
            is_sentinel: false,
            format_arg_idx: None,
            is_nodiscard: false,
            asm_label: None,
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
            }) => {},
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
            is_inline: false,
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
            is_noreturn: false,
            is_pure: false,
            is_const_attr: false,
            returns_twice: false,
            no_instrument: false,
            is_malloc: false,
            malloc_dealloc: None,
            malloc_dealloc_argno: None,
            alloc_size: None,
            assume_aligned: None,
            warn_unused_result: false,
            is_sentinel: false,
            format_arg_idx: None,
            is_nodiscard: false,
            asm_label: None,
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
        assert!(
            diags
                .iter()
                .all(|d| !matches!(d.sev, crate::diagnostics::Severity::Error)),
            "parse errors: {:?}",
            diags
        );
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
            Stmt::If {
                cond, then_, else_, ..
            } => {
                expr_has_call(cond)
                    || stmt_has_call(then_)
                    || else_.as_ref().is_some_and(|e| stmt_has_call(e))
            },
            _ => false,
        }
    }

    fn expr_has_call(e: &Expr) -> bool {
        match e {
            Expr::Call { .. } => true,
            Expr::Binop { l, r, .. } => expr_has_call(l) || expr_has_call(r),
            Expr::Unary { e, .. } | Expr::Cast { e, .. } => expr_has_call(e),
            Expr::Cond {
                cond, then_, else_, ..
            } => expr_has_call(cond) || expr_has_call(then_) || expr_has_call(else_),
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
        assert!(
            !main_has_call(&prog),
            "always_inline must be inlined at O0 regardless of body size"
        );
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
        assert!(!main_has_call(&prog), "always_inline should win over noinline");
    }

    #[test]
    fn o0_does_not_inline_regular_static_inline() {
        // Regular static inline must NOT be inlined at O0
        let prog = optimize_src(
            b"static inline int f(int x) { return x + 1; }\n\
              int main(void) { return f(5); }\n",
            0,
        );
        assert!(main_has_call(&prog), "regular static inline should not be inlined at O0");
    }

    #[test]
    fn expr_node_count_simple() {
        // 5 + 3 => Binop(5, 3) = 3 nodes
        let e = Expr::Binop {
            op: BinOp::Add,
            l: Box::new(Expr::Int {
                v: 5,
                span: Span::point("t", 1, 1),
                ty: int_ty(),
            }),
            r: Box::new(Expr::Int {
                v: 3,
                span: Span::point("t", 1, 1),
                ty: int_ty(),
            }),
            span: Span::point("t", 1, 1),
            ty: int_ty(),
        };
        assert_eq!(expr_node_count(&e), 3);
    }

    #[test]
    fn expr_node_count_ident() {
        let e = Expr::Ident {
            name: Ident {
                name: "x".into(),
                span: Span::point("t", 1, 1),
            },
            span: Span::point("t", 1, 1),
            ty: int_ty(),
        };
        assert_eq!(expr_node_count(&e), 1);
    }

    #[test]
    fn estimate_inline_cost_small_func() {
        // A simple return x+1 function should have low cost
        let (prog, _) =
            crate::parser::parse(b"static int f(int x) { return x + 1; }\n", "cost-test.c");
        if let Decl::Func(f) = &prog.decls[0] {
            let body = f.body.as_ref().unwrap();
            let args = vec![Expr::Int {
                v: 5,
                span: Span::point("t", 1, 1),
                ty: int_ty(),
            }];
            let cost = estimate_inline_cost(f, body, &args);
            assert!(
                cost <= INLINE_COST_THRESHOLD,
                "small function cost {} should be <= {}",
                cost,
                INLINE_COST_THRESHOLD
            );
        } else {
            panic!("expected FuncDecl");
        }
    }

    /// Profile-guided inline: hot caller at O2 should relax the
    /// heuristic (allow non-static, higher cost threshold).
    #[test]
    fn profile_guided_inline_hot_caller_relaxed_threshold() {
        let sp = Span::point("t.c", 1, 1);
        // callee: non-static, 2 params, body cost = 3 (return x+y)
        let callee = FuncDecl {
            ret_ty: int_ty(),
            name: Ident {
                name: "add".into(),
                span: sp.clone(),
            },
            params: vec![
                crate::ast::Param {
                    quals: crate::ast::QualifierFlags::NONE,
                    name: Some(Ident {
                        name: "x".into(),
                        span: sp.clone(),
                    }),
                    ty: int_ty(),
                    span: sp.clone(),
                    vla_dim: None,
                },
                crate::ast::Param {
                    quals: crate::ast::QualifierFlags::NONE,
                    name: Some(Ident {
                        name: "y".into(),
                        span: sp.clone(),
                    }),
                    ty: int_ty(),
                    span: sp.clone(),
                    vla_dim: None,
                },
            ],
            body: Some(Block {
                items: vec![BlockItem::Stmt(Stmt::Return {
                    val: Some(Expr::Binop {
                        op: BinOp::Add,
                        l: Box::new(Expr::Ident {
                            name: Ident {
                                name: "x".into(),
                                span: sp.clone(),
                            },
                            span: sp.clone(),
                            ty: int_ty(),
                        }),
                        r: Box::new(Expr::Ident {
                            name: Ident {
                                name: "y".into(),
                                span: sp.clone(),
                            },
                            span: sp.clone(),
                            ty: int_ty(),
                        }),
                        span: sp.clone(),
                        ty: int_ty(),
                    }),
                    span: sp.clone(),
                })],
                span: sp.clone(),
            }),
            span: sp.clone(),
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
            is_inline: false,
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
            is_noreturn: false,
            is_pure: false,
            is_const_attr: false,
            returns_twice: false,
            no_instrument: false,
            is_malloc: false,
            malloc_dealloc: None,
            malloc_dealloc_argno: None,
            alloc_size: None,
            assume_aligned: None,
            warn_unused_result: false,
            is_sentinel: false,
            format_arg_idx: None,
            is_nodiscard: false,
            asm_label: None,
        };

        let mut funcs = HashMap::new();
        funcs.insert("add".into(), callee.clone());
        let args = vec![
            Expr::Int {
                v: 1,
                span: sp.clone(),
                ty: int_ty(),
            },
            Expr::Int {
                v: 2,
                span: sp.clone(),
                ty: int_ty(),
            },
        ];

        // O2 + caller_is_hot=false: non-static with params should REJECT
        let result_cold = try_inline_call("add", &args, &funcs, 2, false, false, 0);
        assert!(
            result_cold.is_none(),
            "O2 cold caller should reject non-static callee with params"
        );

        // O2 + caller_is_hot=true: profile-relaxed should ACCEPT
        let result_hot = try_inline_call("add", &args, &funcs, 2, false, true, 0);
        assert!(
            result_hot.is_some(),
            "O2 hot caller should inline non-static callee (profile-relaxed)"
        );
    }
}
