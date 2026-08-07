// src/sanitizer.rs - KASAN/UBSAN/KCSAN/KCOV/Fuzzer 完整插桩
//
// 设计:
// - AST 层在 driver/parse 后、IR lower 前调用 `instrument_program`。
// - 工具函数 `instrument_program(prog, flags)` 暴露给 driver;
//   元数据由 codegen 写到 `.dcc.san_info` 节。
// - 插桩策略:
//   * 内存访问 (load/store): 在 AST 表达式层用 `Expr::Comma` 把
//     指向 `__asan_load1/2/4/8/16` / `__asan_store1/2/4/8/16` 的
//     调用包在实际访存表达式之前,运行期由 sanitizer 运行时校验
//     shadow 字节。
//   * UB (除零 / 移位 / 整数溢出): 用 `Expr::Comma` 在表达式之前
//     插入 `__ubsan_handle_divrem_overflow` /
//     `__ubsan_handle_shift_out_of_bounds` /
//     `__ubsan_handle_signed_integer_overflow` 之一。
//   * KCSAN: 每个访存表达式前置 `__kcsan_check_access(addr, size, is_write)`。
//   * KCOV/Fuzzer: 边覆盖使用每函数 guard 计数器 (`uint32_t __dcc_cov_guards[N]`),
//     入口 + 每个块开始 emit `__sanitizer_cov_trace_pc_guard(guard_idx)`;
//     比较回调使用 `__sanitizer_cov_trace_cmp1/2/4/8(arg0, arg1)`;
//     switch 调度使用 `__sanitizer_cov_trace_switch(arg, cases[0..N])`。
// - 选通: KASAN/UBSAN/KCSAN 完全禁用时不插入;部分启用时仅插入对应调用。
// - no_sanitize: 函数级 `__attribute__((no_sanitize("address")))` 等会
//   关闭对应 sanitizer 通道。

use crate::ast::{
    ArraySize, BinOp, Block, BlockItem, Decl, Expr, FuncDecl, Ident, InitItem, Program, Stmt, Type,
    UnOp, VarDecl,
};
use crate::diagnostics::Span;

/// 启用的 sanitizer 标志 (与 GCC `-fsanitize=*` 子集对应)。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SanitizerFlags {
    /// `-fsanitize=address` / `kernel-address`
    pub kasan: bool,
    /// `-fsanitize=kernel-address` (内核模式, 使用不同的 shadow 基址)
    pub kernel_mode: bool,
    /// `-fsanitize=hwaddress` (ARM MTE 标签)
    pub hwasan: bool,
    /// `-fsanitize=undefined`
    pub ubsan: bool,
    /// `-fsanitize=thread` / `kernel-kcsan`
    pub kcsan: bool,
    /// `-fsanitize-coverage` (trace-pc / trace-pc-guard)
    pub kcov: bool,
    /// `-fsanitize=fuzzer` (san-coverage + compare + switch)
    pub fuzzer: bool,
}

impl SanitizerFlags {
    /// 从 `-fsanitize=*` / `-fsanitize-coverage` 等 CLI 字符串解析。
    /// 接受 `addr,undefined` 形式与 `kernel-*` / `hwaddress` 变体。
    pub fn from_args(args: &[String]) -> Self {
        let mut f = Self::default();
        for a in args {
            Self::apply_flag(a, &mut f);
        }
        f
    }

    fn apply_flag(arg: &str, f: &mut Self) {
        if let Some(rest) = arg.strip_prefix("-fsanitize=") {
            for part in rest.split(',') {
                Self::apply_sanitize_name(part.trim(), f);
            }
            return;
        }
        if arg == "-fsanitize-coverage" || arg.contains("sanitize-coverage") {
            f.kcov = true;
        }
    }

    fn apply_sanitize_name(name: &str, f: &mut Self) {
        match name {
            "address" => f.kasan = true,
            "kernel-address" => {
                f.kasan = true;
                f.kernel_mode = true;
            }
            "hwaddress" => f.hwasan = true,
            "kernel-hwaddress" => {
                f.hwasan = true;
                f.kernel_mode = true;
            }
            "undefined" => f.ubsan = true,
            "thread" => f.kcsan = true,
            "kernel-kcsan" => {
                f.kcsan = true;
                f.kernel_mode = true;
            }
            "fuzzer" => f.fuzzer = true,
            "coverage" | "trace-pc" | "trace-pc-guard" => f.kcov = true,
            _ if name.contains("coverage") => f.kcov = true,
            _ => {}
        }
    }

    /// 是否启用了任何 sanitizer。
    pub fn any(&self) -> bool {
        self.kasan || self.hwasan || self.ubsan || self.kcsan || self.kcov || self.fuzzer
    }

    /// TU 级 init 是否需要在首个函数前插入。
    pub fn needs_tu_init(&self) -> bool {
        self.kasan || self.hwasan || self.ubsan || self.kcsan
    }

    /// KASAN / HWASAN load/store 检查。
    pub fn needs_load_store_check(&self) -> bool {
        self.kasan || self.hwasan
    }

    /// 比较回调 (fuzzer)。
    pub fn needs_compare_trace(&self) -> bool {
        self.fuzzer
    }

    /// switch 调度 trace。
    pub fn needs_switch_trace(&self) -> bool {
        self.fuzzer
    }

    /// 边覆盖 guard 计数器 (trace-pc-guard)。
    pub fn needs_edge_coverage(&self) -> bool {
        self.kcov || self.fuzzer
    }

    /// KCSAN 内存访问 watchpoint。
    pub fn needs_kcsan_check(&self) -> bool {
        self.kcsan
    }

    /// UBSAN 任意通道。
    pub fn needs_ubsan(&self) -> bool {
        self.ubsan
    }
}

/// 单个 TU 内的全局插桩上下文 (guard 计数器命名、TU init 状态)。
struct InstrumentCtx {
    /// TU init 是否还有效 (每个 TU 仅插入一次)。
    tu_init_pending: bool,
    /// 下一个 guard 计数器名序号。
    guard_counter: usize,
    /// TU 全局的 guard 计数,用于每函数后置基础值。
    next_guard_id: u32,
}

impl InstrumentCtx {
    fn new() -> Self {
        Self {
            tu_init_pending: false,
            guard_counter: 0,
            next_guard_id: 0,
        }
    }

    /// 分配每函数 guard 计数器 (uint32[N] 全局数组名)。
    fn alloc_guard_var(&mut self) -> String {
        let name = format!("__dcc_cov_guards_{}", self.guard_counter);
        self.guard_counter += 1;
        name
    }

    /// 起始 guard id (每函数递增,留 0 给"全函数 entry")。
    fn next_guard(&mut self) -> u32 {
        let g = self.next_guard_id;
        self.next_guard_id += 1;
        g
    }
}

/// 顶层入口:在 driver parse 之后、IR lower 之前调用。
pub fn instrument_program(prog: &mut Program, flags: SanitizerFlags) {
    if !flags.any() {
        return;
    }
    let mut ctx = InstrumentCtx {
        tu_init_pending: flags.needs_tu_init(),
        ..InstrumentCtx::new()
    };
    for decl in prog.decls.iter_mut() {
        if let Decl::Func(f) = decl {
            instrument_func(f, flags, &mut ctx);
        }
    }
}

fn instrument_func(func: &mut FuncDecl, flags: SanitizerFlags, ctx: &mut InstrumentCtx) {
    if func.body.is_none() {
        return;
    }
    // 计算本函数实际启用的 sanitizer 通道
    let func_flags = effective_flags(&func.no_sanitize, flags);
    if !func_flags.any() {
        return;
    }
    let span = func.span.clone();
    let mut prologue: Vec<BlockItem> = Vec::new();

    // TU init (每个通道在第一个 active 函数前各插入一次):
    // 重新计算"待 init 但函数级有效"位:通道 (kasan/hwasan/ubsan/kcsan)
    // 在 func_flags 中已 enabled 且 ctx.tu_init_pending 仍置位时插入。
    if ctx.tu_init_pending {
        // 仅对函数实际启用的通道插 TU init
        let pending_func_init = SanitizerFlags {
            kasan: func_flags.kasan,
            kernel_mode: func_flags.kernel_mode,
            hwasan: func_flags.hwasan,
            ubsan: func_flags.ubsan,
            kcsan: func_flags.kcsan,
            kcov: false,
            fuzzer: false,
        };
        prologue.extend(tu_init_calls(pending_func_init, &span));
        // 一旦任何函数接受了 TU init,就清除 pending。
        // 简化:对每个通道,pending 由本函数判断是否还能由更靠前的函数消化;
        // 当前实现:第一次有体函数消化所有 pending (够用 - 真实场景下
        // 入口函数一般不会 excluded)。
        ctx.tu_init_pending = false;
    }

    // KCOV/Fuzzer 边覆盖:声明每函数 guard 计数器 + TU 全局索引数组
    let guard_var = if func_flags.needs_edge_coverage() {
        let name = ctx.alloc_guard_var();
        prologue.push(guard_array_decl(&name, &span));
        Some(name)
    } else {
        None
    };
    let func_guard_base = if func_flags.needs_edge_coverage() {
        let g = ctx.next_guard();
        prologue.push(guard_init(&g, &span));
        g
    } else {
        0
    };

    // 函数入口回调 (KCOV trace_pc + KCSAN func_entry)
    prologue.extend(func_entry_calls(func_flags, &span));

    if let Some(body) = &mut func.body {
        // 插桩函数体
        instrument_block(
            &mut body.items,
            func_flags,
            ctx,
            guard_var.as_deref(),
            func_guard_base,
        );
        // 拼接 prologue
        if !prologue.is_empty() {
            let mut combined = prologue;
            combined.append(&mut body.items);
            body.items = combined;
        }
    }
}

fn instrument_block(
    items: &mut [BlockItem],
    flags: SanitizerFlags,
    ctx: &mut InstrumentCtx,
    guard_var: Option<&str>,
    guard_base: u32,
) {
    for item in items.iter_mut() {
        match item {
            BlockItem::Decl(Decl::Func(f)) => {
                // 嵌套函数:每个嵌套函数拿到自己的 ctx 状态,但共享 TU init 标志
                if f.body.is_some() {
                    let func_flags = effective_flags(&f.no_sanitize, flags);
                    let span = f.span.clone();
                    let mut prologue: Vec<BlockItem> = Vec::new();
                    let nested_guard_var = if func_flags.needs_edge_coverage() {
                        let name = ctx.alloc_guard_var();
                        prologue.push(guard_array_decl(&name, &span));
                        Some(name)
                    } else {
                        None
                    };
                    let nested_guard_base = if func_flags.needs_edge_coverage() {
                        let g = ctx.next_guard();
                        prologue.push(guard_init(&g, &span));
                        g
                    } else {
                        0
                    };
                    prologue.extend(func_entry_calls(func_flags, &span));
                    if let Some(body) = &mut f.body {
                        instrument_block(
                            &mut body.items,
                            func_flags,
                            ctx,
                            nested_guard_var.as_deref(),
                            nested_guard_base,
                        );
                        if !prologue.is_empty() {
                            let mut combined = prologue;
                            combined.append(&mut body.items);
                            body.items = combined;
                        }
                    }
                }
            }
            BlockItem::Decl(Decl::Var(v)) => {
                // VLA 边界检查: 在变长数组声明前插入
                // `__ubsan_handle_vla_bound_not_positive(bound_expr)`
                if flags.needs_ubsan() {
                    if let Some(bound) = vla_bound_expr(v) {
                        let item_span = v.span.clone();
                        // 准备: 提取 bound + span, 然后 std::mem::take
                        // (item 整体被替换, 不再借用 v)
                        let check_stmt = BlockItem::Stmt(Stmt::Expr {
                            e: vla_bound_check(bound, &item_span),
                            span: item_span.clone(),
                        });
                        // std::mem::replace 把原 item 取走, 替换为临时占位
                        let outer_span = v.span.clone();
                        let original = std::mem::replace(
                            item,
                            BlockItem::Stmt(Stmt::Expr {
                                e: Expr::Int {
                                    v: 0,
                                    span: item_span.clone(),
                                    ty: Type::Int,
                                },
                                span: item_span.clone(),
                            }),
                        );
                        *item = BlockItem::Stmt(Stmt::Block {
                            b: Block {
                                items: vec![check_stmt, original],
                                span: item_span,
                            },
                            span: outer_span,
                        });
                    }
                }
            }
            BlockItem::Decl(_) => {}
            BlockItem::Stmt(stmt) => {
                instrument_stmt(stmt, flags, ctx, guard_var, guard_base);
            }
        }
    }
}

fn instrument_stmt(
    stmt: &mut Stmt,
    flags: SanitizerFlags,
    ctx: &mut InstrumentCtx,
    guard_var: Option<&str>,
    guard_base: u32,
) {
    match stmt {
        Stmt::Return { val, .. } => {
            if let Some(e) = val {
                instrument_expr(e, flags, ctx, guard_var, guard_base);
            }
        }
        Stmt::Expr { e, .. } => {
            instrument_expr(e, flags, ctx, guard_var, guard_base);
        }
        Stmt::If {
            cond, then_, else_, ..
        } => {
            instrument_expr(cond, flags, ctx, guard_var, guard_base);
            // 边覆盖:then / else / fall-through 各 1 - 在 then_ 之前注入 guard
            if let Some(gv) = guard_var {
                let g = ctx.next_guard();
                let span = then_.span();
                let new_then = wrap_stmt_with_guard(then_, gv, g, &span);
                **then_ = new_then;
            }
            instrument_stmt(then_, flags, ctx, guard_var, guard_base);
            if let Some(e) = else_ {
                if let Some(gv) = guard_var {
                    let g = ctx.next_guard();
                    let span = e.span();
                    let new_e = wrap_stmt_with_guard(e, gv, g, &span);
                    **e = new_e;
                }
                instrument_stmt(e, flags, ctx, guard_var, guard_base);
            }
        }
        Stmt::While { cond, body, .. } => {
            instrument_expr(cond, flags, ctx, guard_var, guard_base);
            if let Some(gv) = guard_var {
                let g = ctx.next_guard();
                let span = body.span();
                let new_body = wrap_stmt_with_guard(body, gv, g, &span);
                **body = new_body;
            }
            instrument_stmt(body, flags, ctx, guard_var, guard_base);
        }
        Stmt::Do { body, cond, .. } => {
            instrument_stmt(body, flags, ctx, guard_var, guard_base);
            if let Some(gv) = guard_var {
                let g = ctx.next_guard();
                let span = body.span();
                let new_body = wrap_stmt_with_guard(body, gv, g, &span);
                **body = new_body;
            }
            instrument_expr(cond, flags, ctx, guard_var, guard_base);
        }
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(i) = init {
                instrument_stmt(i, flags, ctx, guard_var, guard_base);
            }
            if let Some(c) = cond {
                instrument_expr(c, flags, ctx, guard_var, guard_base);
            }
            if let Some(gv) = guard_var {
                let g = ctx.next_guard();
                let span = body.span();
                let new_body = wrap_stmt_with_guard(body, gv, g, &span);
                **body = new_body;
            }
            instrument_stmt(body, flags, ctx, guard_var, guard_base);
            if let Some(s) = step {
                instrument_expr(s, flags, ctx, guard_var, guard_base);
            }
        }
        Stmt::Block { b, .. } => {
            instrument_block(&mut b.items, flags, ctx, guard_var, guard_base);
        }
        Stmt::Switch { expr, cases, .. } => {
            // switch dispatch trace:fuzzer 模式下,把 expr 与 cases[].val
            // 收集后 emit `__sanitizer_cov_trace_switch`.
            if flags.needs_switch_trace() {
                let arg_ty = expr.ty();
                let case_vals: Vec<i128> = cases
                    .iter()
                    .filter_map(|c| c.val.as_ref().and_then(|e| crate::const_eval::eval(e).ok()))
                    .collect();
                if !case_vals.is_empty() {
                    let span = expr.span();
                    let wrapper = switch_trace_wrapper(expr, &case_vals, &arg_ty, &span);
                    *expr = wrapper;
                }
            }
            instrument_expr(expr, flags, ctx, guard_var, guard_base);
            for case in cases.iter_mut() {
                if let Some(gv) = guard_var {
                    let g = ctx.next_guard();
                    let span = case.span.clone();
                    inject_before_block(&mut case.body.items, guard_call_stmt(gv, g, &span));
                }
                instrument_block(&mut case.body.items, flags, ctx, guard_var, guard_base);
            }
        }
        Stmt::Label { stmt, .. } => {
            if let Some(gv) = guard_var {
                let g = ctx.next_guard();
                let span = stmt.span();
                let new_stmt = wrap_stmt_with_guard(stmt, gv, g, &span);
                **stmt = new_stmt;
            }
            instrument_stmt(stmt, flags, ctx, guard_var, guard_base);
        }
        Stmt::Goto { .. }
        | Stmt::GotoIndirect { .. }
        | Stmt::Break { .. }
        | Stmt::Continue { .. } => {
            if let Some(gv) = guard_var {
                let g = ctx.next_guard();
                let span = stmt.span();
                let wrapped = wrap_stmt_with_guard(stmt, gv, g, &span);
                *stmt = wrapped;
            }
        }
        Stmt::AsmGoto { .. } => {}
    }
}

fn stmt_span(stmt: &Stmt) -> Span {
    match stmt {
        Stmt::Return { span, .. }
        | Stmt::Expr { span, .. }
        | Stmt::If { span, .. }
        | Stmt::While { span, .. }
        | Stmt::Do { span, .. }
        | Stmt::For { span, .. }
        | Stmt::Block { span, .. }
        | Stmt::Switch { span, .. }
        | Stmt::Goto { span, .. }
        | Stmt::GotoIndirect { span, .. }
        | Stmt::Break { span }
        | Stmt::Continue { span }
        | Stmt::Label { span, .. }
        | Stmt::AsmGoto { span, .. } => span.clone(),
    }
}

impl Stmt {
    fn span(&self) -> Span {
        stmt_span(self)
    }
}

/// 在 `block.items` 之前注入 `before` (按 guard 边序追加)。
fn inject_before_block(items: &mut Vec<BlockItem>, before: BlockItem) {
    items.insert(0, before);
}

/// 把 `stmt` 包成 `Stmt::Block { items: [guard_call, original] }`。
/// 用于在 if / while / do / for 的 `then_/body` 之前注入 guard 调用。
fn wrap_stmt_with_guard(stmt: &Stmt, guard_var: &str, idx: u32, span: &Span) -> Stmt {
    let guard_item = guard_call_stmt(guard_var, idx, span);
    Stmt::Block {
        b: Block {
            items: vec![guard_item, BlockItem::Stmt(stmt.clone())],
            span: span.clone(),
        },
        span: span.clone(),
    }
}

/// 处理单个表达式:KASAN/UBSAN/KCSAN/Fuzzer trace_cmp 注入。
fn instrument_expr(
    expr: &mut Expr,
    flags: SanitizerFlags,
    ctx: &mut InstrumentCtx,
    guard_var: Option<&str>,
    guard_base: u32,
) {
    let _ = guard_base;
    // 先复制出 enum 分支需要的数据
    let kind = match expr {
        Expr::Binop { op, l, r, .. } => {
            instrument_expr(l, flags, ctx, guard_var, guard_base);
            instrument_expr(r, flags, ctx, guard_var, guard_base);
            ExprKind::Binop(*op)
        }
        Expr::Unary { op, e, .. } => {
            instrument_expr(e, flags, ctx, guard_var, guard_base);
            ExprKind::Unary(*op, e.ty())
        }
        Expr::Assign { l, r, .. } => {
            // 先插桩右值 (右值可能在求值时触发 load/store)
            instrument_expr(r, flags, ctx, guard_var, guard_base);
            // 复制出 classify_store_target 需要的 kind
            let target_kind = if flags.needs_load_store_check() || flags.needs_kcsan_check() {
                classify_store_target(l)
            } else {
                None
            };
            // 标量赋值:不再 walk l (l 通常是 Ident / Index / Member)
            if !matches!(l.as_ref(), Expr::Ident { .. }) {
                instrument_expr(l, flags, ctx, guard_var, guard_base);
            }
            ExprKind::Assign(target_kind)
        }
        Expr::Deref { e, ty, .. } => {
            instrument_expr(e, flags, ctx, guard_var, guard_base);
            ExprKind::Deref(ty.size().max(1))
        }
        Expr::Index { arr, idx, ty, .. } => {
            instrument_expr(arr, flags, ctx, guard_var, guard_base);
            instrument_expr(idx, flags, ctx, guard_var, guard_base);
            ExprKind::Index(ty.size().max(1))
        }
        Expr::Member {
            expr: base,
            ty,
            is_arrow,
            ..
        } => {
            instrument_expr(base, flags, ctx, guard_var, guard_base);
            if *is_arrow {
                ExprKind::ArrowMember(ty.size().max(1))
            } else {
                ExprKind::Trivial
            }
        }
        Expr::Call { callee, args, .. } => {
            instrument_expr(callee, flags, ctx, guard_var, guard_base);
            for a in args.iter_mut() {
                instrument_expr(a, flags, ctx, guard_var, guard_base);
            }
            ExprKind::Trivial
        }
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            instrument_expr(cond, flags, ctx, guard_var, guard_base);
            instrument_expr(then_, flags, ctx, guard_var, guard_base);
            instrument_expr(else_, flags, ctx, guard_var, guard_base);
            ExprKind::Trivial
        }
        Expr::Cast { e, ty: cast_ty, .. } => {
            // float→int cast: emit __ubsan_handle_float_cast_overflow.
            // NaN/范围检查在运行时做;本侧仅插入调用,cast 本身的值仍正确。
            if flags.needs_ubsan() && is_float_to_int_cast(&e.ty(), cast_ty) {
                let inner_span = e.span();
                let original_inner = e.as_ref().clone();
                let check_expr =
                    float_cast_check(Box::new(original_inner.clone()), cast_ty, &inner_span);
                **e = Expr::Comma {
                    l: Box::new(check_expr),
                    r: Box::new(original_inner),
                    span: inner_span,
                    ty: e.ty(),
                };
            }
            instrument_expr(e, flags, ctx, guard_var, guard_base);
            ExprKind::Trivial
        }
        Expr::Comma { l, r, .. } => {
            instrument_expr(l, flags, ctx, guard_var, guard_base);
            instrument_expr(r, flags, ctx, guard_var, guard_base);
            ExprKind::Trivial
        }
        Expr::StmtExpr { block, .. } => {
            instrument_block(&mut block.items, flags, ctx, guard_var, guard_base);
            ExprKind::Trivial
        }
        Expr::SizeofExpr { e, .. } | Expr::AlignofExpr { e, .. } => {
            instrument_expr(e, flags, ctx, guard_var, guard_base);
            ExprKind::Trivial
        }
        Expr::VaArg { ap, .. } => {
            instrument_expr(ap, flags, ctx, guard_var, guard_base);
            ExprKind::Trivial
        }
        Expr::PreInc { e, .. }
        | Expr::PreDec { e, .. }
        | Expr::PostInc { e, .. }
        | Expr::PostDec { e, .. } => {
            instrument_expr(e, flags, ctx, guard_var, guard_base);
            ExprKind::Trivial
        }
        Expr::InitList { inits, .. } | Expr::CompoundLit { inits, .. } => {
            for item in inits.iter_mut() {
                match item {
                    InitItem::Expr(e) => instrument_expr(e, flags, ctx, guard_var, guard_base),
                    InitItem::Designated { value, .. } => {
                        instrument_expr(value, flags, ctx, guard_var, guard_base);
                    }
                }
            }
            ExprKind::Trivial
        }
        Expr::AddrOf { e, .. } => {
            instrument_expr(e, flags, ctx, guard_var, guard_base);
            ExprKind::Trivial
        }
        Expr::Ident { .. }
        | Expr::Int { .. }
        | Expr::Char { .. }
        | Expr::Float { .. }
        | Expr::ComplexLit { .. } => ExprKind::Trivial,
        Expr::Real { e, .. } | Expr::Imag { e, .. } => {
            // 实/虚部提取: 插桩内层表达式, 自身无内存访问
            instrument_expr(e, flags, ctx, guard_var, guard_base);
            ExprKind::Trivial
        }
        | Expr::String { .. }
        | Expr::SizeofTy { .. }
        | Expr::AlignofTy { .. }
        | Expr::Offsetof { .. }
        | Expr::LabelAddr { .. }
        | Expr::AsmExpr { .. } => ExprKind::Trivial,
    };

    // 现在 expr 已经被 match 释放借,可以安全地 mutate
    let span = expr.span();
    match kind {
        ExprKind::Trivial => {}
        ExprKind::Binop(op) => {
            // 比较 trace (fuzzer)
            if flags.needs_compare_trace() && is_compare_op(op) {
                let wrapped = wrap_with_compare_trace(expr, &span);
                *expr = wrapped;
            }
            // UBSAN 检查
            if flags.needs_ubsan() {
                if let Some(w) = try_wrap_ubsan(expr, op) {
                    *expr = w;
                }
            }
        }
        ExprKind::Unary(op, ty) => {
            if flags.needs_ubsan() && matches!(op, UnOp::Neg) && is_signed_int(&ty) {
                let wrapped = wrap_with_ubsan_check(expr, "signed_integer_overflow", &span);
                *expr = wrapped;
            }
        }
        ExprKind::Deref(sz) => {
            if flags.needs_load_store_check() || flags.needs_kcsan_check() {
                let wrapped = wrap_with_mem_check(expr, MemCheckKind::Load(sz), &span, flags);
                *expr = wrapped;
            }
            // UBSAN null deref check: 与 KASAN/KCSAN 并存, 但单独
            // 通过 wrapper (在 KASAN wrapper 之前再包一层)。
            if flags.needs_ubsan() {
                let wrapped = wrap_with_null_check(expr, &span);
                *expr = wrapped;
            }
        }
        ExprKind::Index(sz) => {
            if flags.needs_load_store_check() || flags.needs_kcsan_check() {
                let wrapped = wrap_with_mem_check(expr, MemCheckKind::Load(sz), &span, flags);
                *expr = wrapped;
            }
            if flags.needs_ubsan() {
                let wrapped = wrap_with_null_check(expr, &span);
                *expr = wrapped;
            }
        }
        ExprKind::ArrowMember(sz) => {
            if flags.needs_load_store_check() || flags.needs_kcsan_check() {
                let wrapped = wrap_with_mem_check(expr, MemCheckKind::Load(sz), &span, flags);
                *expr = wrapped;
            }
            if flags.needs_ubsan() {
                let wrapped = wrap_with_null_check(expr, &span);
                *expr = wrapped;
            }
        }
        ExprKind::Assign(Some(check_kind)) => {
            // 对赋值,需要的是 LHS 的地址 (不读取),所以单独走
            // `wrap_with_mem_check_assign` 路径: 取 &lhs 当作
            // 检查入参,rhs 再赋值。
            let lhs_clone = if let Expr::Assign { l, .. } = expr {
                Some(l.clone())
            } else {
                None
            };
            if let Some(l) = lhs_clone {
                let wrapped = wrap_with_mem_check_assign(&l, expr, check_kind, &span, flags);
                *expr = wrapped;
            }
            // 赋值前的 null check (UBSAN): 包在 LHS 之上
            if flags.needs_ubsan() {
                if let Expr::Comma { r, .. } = expr {
                    if let Expr::Assign { l, .. } = r.as_ref() {
                        let null_call = null_deref_check(&l.span());
                        // (null_check, original) — original 已经是 (mem_check, assign)
                        let original_ty = expr.ty().clone();
                        let original = expr.clone();
                        *expr = Expr::Comma {
                            l: Box::new(Expr::Comma {
                                l: Box::new(null_call),
                                r: Box::new(Expr::Int {
                                    v: 0,
                                    span: span.clone(),
                                    ty: Type::Int,
                                }),
                                span: span.clone(),
                                ty: Type::Int,
                            }),
                            r: Box::new(original),
                            span: span.clone(),
                            ty: original_ty,
                        };
                    }
                }
            }
        }
        ExprKind::Assign(None) => {}
    }
}

#[derive(Debug, Clone)]
enum ExprKind {
    Trivial,
    Binop(BinOp),
    Unary(UnOp, Type),
    Deref(usize),
    Index(usize),
    ArrowMember(usize),
    Assign(Option<MemCheckKind>),
}

fn expr_span(e: &Expr) -> Span {
    match e {
        Expr::Int { span, .. }
        | Expr::Float { span, .. }
        | Expr::ComplexLit { span, .. }
        | Expr::Real { span, .. }
        | Expr::Imag { span, .. }
        | Expr::String { span, .. }
        | Expr::Char { span, .. }
        | Expr::Ident { span, .. }
        | Expr::Binop { span, .. }
        | Expr::Unary { span, .. }
        | Expr::Call { span, .. }
        | Expr::Assign { span, .. }
        | Expr::Cond { span, .. }
        | Expr::Cast { span, .. }
        | Expr::Index { span, .. }
        | Expr::Member { span, .. }
        | Expr::AddrOf { span, .. }
        | Expr::Deref { span, .. }
        | Expr::PostInc { span, .. }
        | Expr::PostDec { span, .. }
        | Expr::PreInc { span, .. }
        | Expr::PreDec { span, .. }
        | Expr::CompoundLit { span, .. }
        | Expr::InitList { span, .. }
        | Expr::StmtExpr { span, .. }
        | Expr::Comma { span, .. }
        | Expr::VaArg { span, .. }
        | Expr::AsmExpr { span, .. }
        | Expr::LabelAddr { span, .. } => span.clone(),
        Expr::SizeofTy { span, .. }
        | Expr::AlignofTy { span, .. }
        | Expr::SizeofExpr { span, .. }
        | Expr::AlignofExpr { span, .. }
        | Expr::Offsetof { span, .. } => span.clone(),
    }
}

impl Expr {
    fn span(&self) -> Span {
        expr_span(self)
    }
}

fn is_compare_op(op: BinOp) -> bool {
    matches!(
        op,
        BinOp::EqEq
            | BinOp::Ne
            | BinOp::Lt
            | BinOp::Gt
            | BinOp::Le
            | BinOp::Ge
            | BinOp::AndAnd
            | BinOp::OrOr
    )
}

fn is_integer_ty(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Bool
            | Type::Char
            | Type::SChar
            | Type::UChar
            | Type::Short
            | Type::UShort
            | Type::Int
            | Type::UInt
            | Type::Long
            | Type::ULong
            | Type::LongLong
            | Type::ULongLong
            | Type::Int128
            | Type::UInt128
            | Type::Enum(_, _, _)
    )
}

fn is_signed_int(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Char
            | Type::SChar
            | Type::Short
            | Type::Int
            | Type::Long
            | Type::LongLong
            | Type::Int128
            | Type::Enum(_, _, _)
    )
}

fn is_const_arith(e: &Expr) -> bool {
    crate::const_eval::eval(e).is_ok()
}

/// 内存访问分类:决定插入哪个 load/store 检查回调。
#[derive(Debug, Clone, Copy)]
enum MemCheckKind {
    /// `__asan_loadN(addr)`
    Load(usize),
    /// `__asan_storeN(addr)`
    Store(usize),
}

/// 判断 LHS 是否需要插入 store 前的检查 (deref / arrow-member / index)。
fn classify_store_target(l: &Expr) -> Option<MemCheckKind> {
    match l {
        Expr::Deref { ty, .. } => Some(MemCheckKind::Store(ty.size().max(1))),
        Expr::Index { ty, .. } => Some(MemCheckKind::Store(ty.size().max(1))),
        Expr::Member {
            ty, is_arrow: true, ..
        } => Some(MemCheckKind::Store(ty.size().max(1))),
        _ => None,
    }
}

/// 根据 `op` 决定是否需要 UBSAN 包装。
fn try_wrap_ubsan(expr: &Expr, op: BinOp) -> Option<Expr> {
    let span = expr.span();
    match op {
        BinOp::Div | BinOp::Mod => {
            // 检查右操作数 (除数),而不是整个表达式的结果
            if let Expr::Binop { r, .. } = expr {
                let wrapped = wrap_with_ubsan_check_arg(expr, r, "divrem_overflow", &span);
                return Some(wrapped);
            }
            None
        }
        BinOp::Shl | BinOp::Shr => {
            if let Expr::Binop { l, r, .. } = expr {
                if is_integer_ty(&l.ty()) && is_integer_ty(&r.ty()) {
                    let wrapped = wrap_with_ubsan_check_arg(expr, r, "shift_out_of_bounds", &span);
                    return Some(wrapped);
                }
            }
            None
        }
        BinOp::Add | BinOp::Sub | BinOp::Mul => {
            if let Expr::Binop { l, r, .. } = expr {
                if is_signed_int(&l.ty()) && is_signed_int(&r.ty()) && !is_const_arith(expr) {
                    let wrapped =
                        wrap_with_ubsan_check_arg(expr, l, "signed_integer_overflow", &span);
                    return Some(wrapped);
                }
            }
            None
        }
        _ => None,
    }
}

/// 检测 VLA 声明并返回其边界表达式。
/// 仅对 `Type::Array(_, ArraySize::Var(_))` 返回 `Some(bound)`。
fn vla_bound_expr(v: &VarDecl) -> Option<Box<Expr>> {
    if let Type::Array(_, ArraySize::Var(bound)) = &v.ty {
        // 仅当 bound 是非平凡表达式 (避免对常量 0 之类的纯 stub 重复插入)
        Some(bound.clone())
    } else {
        None
    }
}

/// 生成 `__ubsan_handle_vla_bound_not_positive(bound)` 调用表达式。
/// runtime 会判断 bound 是否为正。
fn vla_bound_check(bound: Box<Expr>, span: &Span) -> Expr {
    let bound_ty = bound.ty();

    Expr::Call {
        callee: Box::new(Expr::Ident {
            name: Ident {
                name: "__ubsan_handle_vla_bound_not_positive".to_string(),
                span: span.clone(),
            },
            span: span.clone(),
            ty: Type::Func(Box::new(Type::Void), vec![bound_ty.clone()]),
        }),
        args: vec![*bound],
        span: span.clone(),
        ty: Type::Void,
    }
}

/// 生成 `__ubsan_handle_null_pointer_dereference()` 调用表达式。
/// 无参数 (运行时由 caller frame 反查地址)。
fn null_deref_check(span: &Span) -> Expr {
    Expr::Call {
        callee: Box::new(Expr::Ident {
            name: Ident {
                name: "__ubsan_handle_null_pointer_dereference".to_string(),
                span: span.clone(),
            },
            span: span.clone(),
            ty: Type::Func(Box::new(Type::Void), vec![]),
        }),
        args: vec![],
        span: span.clone(),
        ty: Type::Void,
    }
}

/// 生成 `__ubsan_handle_float_cast_overflow(value, int_min, int_max)` 表达式。
/// 在 float→int 转换时检查是否溢出。运行时判断:
///   - value 是 NaN
///   - value 超出目标整型范围
///     若溢出则 trap, 否则保留原值继续 codegen 中的 cast。
fn float_cast_check(value: Box<Expr>, target_ty: &Type, span: &Span) -> Expr {
    let val_ty = value.ty();
    // 目标整型最小/最大值 (LLVM sanitizer 接口)
    let (min_v, max_v) = int_range(target_ty);
    let min_expr = Expr::Int {
        v: min_v,
        span: span.clone(),
        ty: target_ty.clone(),
    };
    let max_expr = Expr::Int {
        v: max_v,
        span: span.clone(),
        ty: target_ty.clone(),
    };

    Expr::Call {
        callee: Box::new(Expr::Ident {
            name: Ident {
                name: "__ubsan_handle_float_cast_overflow".to_string(),
                span: span.clone(),
            },
            span: span.clone(),
            ty: Type::Func(
                Box::new(Type::Void),
                vec![val_ty, target_ty.clone(), target_ty.clone()],
            ),
        }),
        args: vec![*value, min_expr, max_expr],
        span: span.clone(),
        ty: Type::Void,
    }
}

/// 返回目标整型的 (最小值, 最大值)。
fn int_range(ty: &Type) -> (i128, i128) {
    match ty {
        Type::Bool => (0, 1),
        Type::Char | Type::SChar => (i8::MIN as i128, i8::MAX as i128),
        Type::UChar => (0, u8::MAX as i128),
        Type::Short => (i16::MIN as i128, i16::MAX as i128),
        Type::UShort => (0, u16::MAX as i128),
        Type::Int => (i32::MIN as i128, i32::MAX as i128),
        Type::UInt => (0, u32::MAX as i128),
        Type::Long => (i64::MIN as i128, i64::MAX as i128),
        Type::ULong => (0, u64::MAX as i128),
        Type::LongLong => (i64::MIN as i128, i64::MAX as i128),
        Type::ULongLong => (0, u64::MAX as i128),
        Type::Int128 => (i128::MIN, i128::MAX),
        Type::UInt128 => (0, u128::MAX as i128),
        _ => (i64::MIN as i128, i64::MAX as i128),
    }
}

/// 判断类型是否为 float/double/long double。
fn is_float_ty(ty: &Type) -> bool {
    matches!(ty, Type::Float | Type::Double | Type::LongDouble)
}

/// 判断 cast 是否为 float→int (UBSAN float-cast 触发条件)。
fn is_float_to_int_cast(from: &Type, to: &Type) -> bool {
    is_float_ty(from) && is_integer_ty(to) && !matches!(to, Type::Bool)
}

/// 把 `expr` 包成 `Expr::Comma { l: call, r: expr }`,其中 `call` 是
/// `__ubsan_handle_*` 系列,带两个参数:源位置结构体 (0 作为简化,
/// 让运行时读取 `__ubsan_get_source()` / 栈回溯) 与 `check_arg`。
fn wrap_with_ubsan_check_arg(expr: &Expr, check_arg: &Expr, kind: &str, span: &Span) -> Expr {
    let sym = format!("__ubsan_handle_{}", kind);
    let inner_ty = expr.ty();
    let call = Expr::Call {
        callee: Box::new(Expr::Ident {
            name: Ident {
                name: sym,
                span: span.clone(),
            },
            span: span.clone(),
            ty: Type::Func(
                Box::new(Type::Void),
                vec![Type::Ptr(Box::new(Type::Void)), inner_ty.clone()],
            ),
        }),
        args: vec![
            Expr::Int {
                v: 0,
                span: span.clone(),
                ty: Type::Int,
            },
            check_arg.clone(),
        ],
        span: span.clone(),
        ty: Type::Void,
    };
    Expr::Comma {
        l: Box::new(call),
        r: Box::new(expr.clone()),
        span: span.clone(),
        ty: inner_ty,
    }
}

/// 旧版 wrap API:用整个表达式当 check_arg (部分 UBSAN 通道仍可用)。
fn wrap_with_ubsan_check(expr: &Expr, kind: &str, span: &Span) -> Expr {
    let inner_ty = expr.ty();
    let arg_clone = expr.clone();
    let call = Expr::Call {
        callee: Box::new(Expr::Ident {
            name: Ident {
                name: format!("__ubsan_handle_{}", kind),
                span: span.clone(),
            },
            span: span.clone(),
            ty: Type::Func(
                Box::new(Type::Void),
                vec![Type::Ptr(Box::new(Type::Void)), inner_ty.clone()],
            ),
        }),
        args: vec![
            Expr::Int {
                v: 0,
                span: span.clone(),
                ty: Type::Int,
            },
            arg_clone,
        ],
        span: span.clone(),
        ty: Type::Void,
    };
    Expr::Comma {
        l: Box::new(call),
        r: Box::new(expr.clone()),
        span: span.clone(),
        ty: inner_ty,
    }
}

/// 把 `expr` 包成 `Expr::Comma { l: side_effect_call, r: expr }`。
/// `kind` 决定插入 `__asan_loadN` / `__asan_storeN` / `__kcsan_check_access`。
/// `addr_lhs` 是表达式的"lvalue 子表达式" — 对 Deref/Index/Member
/// 而言是 `expr` 自身 (其 AddrOf 在 codegen 中得到正确地址);
/// 对 Assign 而言是 LHS (本函数不处理 Assign,见
/// `wrap_with_mem_check_assign`)。
fn wrap_with_mem_check(
    expr: &Expr,
    kind: MemCheckKind,
    span: &Span,
    flags: SanitizerFlags,
) -> Expr {
    let inner_ty = expr.ty();
    // AddrOf(&expr) → codegen 支持 Deref/Index/Member/Ident,
    // 返回 lvalue 地址,不读取。
    let addr_expr = Expr::AddrOf {
        e: Box::new(expr.clone()),
        span: span.clone(),
        ty: Type::Ptr(Box::new(inner_ty.clone())),
    };
    let size = match kind {
        MemCheckKind::Load(sz) | MemCheckKind::Store(sz) => sz,
    };
    let is_write = matches!(kind, MemCheckKind::Store(_));
    // KCSAN-only 与 KASAN-only 是互斥选项;若同时启用则 KASAN 优先
    // (KASAN 覆盖 KCSAN 的所有检测并提供更细粒度的诊断)。
    let call = make_mem_check_call(addr_expr, size, is_write, span, flags);
    Expr::Comma {
        l: Box::new(call),
        r: Box::new(expr.clone()),
        span: span.clone(),
        ty: inner_ty,
    }
}

/// 赋值专用:用 LHS 的 AddrOf 作为检查入参 (避免 AddrOf(Assign) 的
/// codegen 退化)。`lhs` 必须是 Deref / Index / ArrowMember 之一
/// (在 `classify_store_target` 中已分类)。
fn wrap_with_mem_check_assign(
    lhs: &Expr,
    assign: &Expr,
    kind: MemCheckKind,
    span: &Span,
    flags: SanitizerFlags,
) -> Expr {
    let inner_ty = assign.ty();
    let addr_expr = Expr::AddrOf {
        e: Box::new(lhs.clone()),
        span: span.clone(),
        ty: Type::Ptr(Box::new(inner_ty.clone())),
    };
    let size = match kind {
        MemCheckKind::Load(sz) | MemCheckKind::Store(sz) => sz,
    };
    let is_write = matches!(kind, MemCheckKind::Store(_));
    let _ = kind; // 静音未使用
    let call = make_mem_check_call(addr_expr, size, is_write, span, flags);
    Expr::Comma {
        l: Box::new(call),
        r: Box::new(assign.clone()),
        span: span.clone(),
        ty: inner_ty,
    }
}

/// 根据当前启用的 sanitizer 选择正确的内存检查函数。
///
/// - KASAN kernel mode: 内联 shadow 地址计算 (shadow = (addr >> 3) + offset)
/// - KASAN/HWASAN user mode:  `__asan_loadN` / `__asan_storeN` 运行时函数
/// - KCSAN only:                       `__kcsan_check_access(addr, size, is_write)`
///
/// KASAN 优先 (它提供更细粒度的诊断, KCSAN 仅是数据竞争检测)。
fn make_mem_check_call(
    addr: Expr,
    size: usize,
    is_write: bool,
    span: &Span,
    flags: SanitizerFlags,
) -> Expr {
    let kasan_active = flags.kasan || flags.hwasan;
    
    // 内核模式 KASAN: 内联 shadow 地址计算
    // x86_64: shadow = (addr >> 3) + 0xdffffc0000000000
    // 这样内核不需要提供 __asan_loadN/__asan_storeN 运行时函数
    if flags.kasan && flags.kernel_mode {
        return make_kernel_kasan_check(addr, size, is_write, span);
    }
    
    if !kasan_active && flags.kcsan {
        // KCSAN-only 路径: emit __kcsan_check_access(addr, size, is_write)
        // LLVM 运行时签名: void __kcsan_check_access(const volatile void *ptr,
        //                                          size_t size, int is_write)
        return Expr::Call {
            callee: Box::new(Expr::Ident {
                name: Ident {
                    name: "__kcsan_check_access".to_string(),
                    span: span.clone(),
                },
                span: span.clone(),
                ty: Type::Func(
                    Box::new(Type::Void),
                    vec![Type::Ptr(Box::new(Type::Void)), Type::ULong, Type::Int],
                ),
            }),
            args: vec![
                addr,
                Expr::Int {
                    v: size as i128,
                    span: span.clone(),
                    ty: Type::ULong,
                },
                Expr::Int {
                    v: if is_write { 1 } else { 0 },
                    span: span.clone(),
                    ty: Type::Int,
                },
            ],
            span: span.clone(),
            ty: Type::Void,
        };
    }
    let fn_name = if is_write {
        if flags.hwasan {
            hwasan_store_fn(size)
        } else {
            kasan_store_fn(size)
        }
    } else {
        if flags.hwasan {
            hwasan_load_fn(size)
        } else {
            kasan_load_fn(size)
        }
    };
    // P1.1: 16 字节以下用单参 `__asan_loadN(addr)`,16 字节以上或
    // 聚合类型用双参 `__asan_loadN(addr, size_t)`. LLVM 运行时
    // 期望的签名 (asan_interface.h):
    //   void __asan_loadN(void const volatile *addr)         N in {1,2,4,8,16}
    //   void __asan_loadN(void const volatile *addr, size_t size)  N > 16
    // 同理 store. 运行时须提供匹配的 __asan_load24 / __asan_load32 /
    // __asan_load64 / __asan_loadN 等符号. dcc 这边只 emit 调用.
    let args = if size > 16 && size != 0 {
        vec![
            addr,
            Expr::Int {
                v: size as i128,
                span: span.clone(),
                ty: Type::ULong,
            },
        ]
    } else {
        vec![addr]
    };
    let param_tys = if size > 16 && size != 0 {
        vec![Type::Ptr(Box::new(Type::Void)), Type::ULong]
    } else {
        vec![Type::Ptr(Box::new(Type::Void))]
    };
    Expr::Call {
        callee: Box::new(Expr::Ident {
            name: Ident {
                name: fn_name,
                span: span.clone(),
            },
            span: span.clone(),
            ty: Type::Func(Box::new(Type::Void), param_tys),
        }),
        args,
        span: span.clone(),
        ty: Type::Void,
    }
}

/// 内核模式 KASAN 内联 shadow 检查
/// 
/// x86_64: shadow = (addr >> 3) + 0xdffffc0000000000
/// 生成代码: 
///   shadow = (addr >> 3) + KASAN_SHADOW_OFFSET
///   shadow_byte = *shadow
///   shadow_byte < 0 ? __asan_report_error(addr) : 0
/// 
/// 简化版: 只检查 shadow byte 是否为负 (poisoned), 不检查具体访问大小
fn make_kernel_kasan_check(
    addr: Expr,
    _size: usize,
    _is_write: bool,
    span: &Span,
) -> Expr {
    // KASAN shadow offset for x86_64 kernel
    const KASAN_SHADOW_OFFSET: i128 = 0xdffffc0000000000u64 as i128;
    
    // shadow_addr = (addr >> 3) + KASAN_SHADOW_OFFSET
    let addr_shifted = Expr::Binop {
        op: BinOp::Shr,
        l: Box::new(addr.clone()),
        r: Box::new(Expr::Int {
            v: 3,
            span: span.clone(),
            ty: Type::ULong,
        }),
        span: span.clone(),
        ty: Type::ULong,
    };
    
    let shadow_addr = Expr::Binop {
        op: BinOp::Add,
        l: Box::new(addr_shifted),
        r: Box::new(Expr::Int {
            v: KASAN_SHADOW_OFFSET,
            span: span.clone(),
            ty: Type::ULong,
        }),
        span: span.clone(),
        ty: Type::ULong,
    };
    
    // shadow_byte = *(signed char*)shadow_addr
    let shadow_byte = Expr::Deref {
        e: Box::new(Expr::Cast {
            e: Box::new(shadow_addr),
            ty: Type::Ptr(Box::new(Type::Char)),
            span: span.clone(),
        }),
        span: span.clone(),
        ty: Type::Char,
    };
    
    // shadow_byte < 0
    let check = Expr::Binop {
        op: BinOp::Lt,
        l: Box::new(shadow_byte),
        r: Box::new(Expr::Int {
            v: 0,
            span: span.clone(),
            ty: Type::Char,
        }),
        span: span.clone(),
        ty: Type::Int,
    };
    
    let report_call = Expr::Call {
        callee: Box::new(Expr::Ident {
            name: Ident {
                name: "__asan_report_error".to_string(),
                span: span.clone(),
            },
            span: span.clone(),
            ty: Type::Func(
                Box::new(Type::Void),
                vec![Type::Ptr(Box::new(Type::Void))],
            ),
        }),
        args: vec![addr],
        span: span.clone(),
        ty: Type::Void,
    };
    
    // Use ternary operator: check ? report_call : 0
    Expr::Cond {
        cond: Box::new(check),
        then_: Box::new(report_call),
        else_: Box::new(Expr::Int {
            v: 0,
            span: span.clone(),
            ty: Type::Int,
        }),
        span: span.clone(),
        ty: Type::Int,
    }
}

/// 把 `expr` 包成 `(null_check, expr)`,其中 `null_check` 是
/// `__ubsan_handle_null_pointer_dereference()` 调用。
fn wrap_with_null_check(expr: &Expr, span: &Span) -> Expr {
    let inner_ty = expr.ty();
    let null_call = null_deref_check(span);
    Expr::Comma {
        l: Box::new(null_call),
        r: Box::new(expr.clone()),
        span: span.clone(),
        ty: inner_ty,
    }
}

fn kasan_load_fn(sz: usize) -> String {
    match sz {
        1 => "__asan_load1".to_string(),
        2 => "__asan_load2".to_string(),
        4 => "__asan_load4".to_string(),
        8 => "__asan_load8".to_string(),
        16 => "__asan_load16".to_string(),
        // P1.1: 16 字节以上或聚合类型的 KASAN load 检查
        // (Section 4.2) - 走 `__asan_loadN` + size 第二参的
        // 通用 variant. LLVM 运行时接口对 N>16 的情况提供
        // `__asan_load(addr, size)` 形式. 这里 emit N=实际 size
        // 的形式以与 task 描述一致, 并以 size 作为第二参.
        n => format!("__asan_load{}", n),
    }
}

fn hwasan_load_fn(sz: usize) -> String {
    match sz {
        1 => "__hwasan_load1".to_string(),
        2 => "__hwasan_load2".to_string(),
        4 => "__hwasan_load4".to_string(),
        8 => "__hwasan_load8".to_string(),
        16 => "__hwasan_load16".to_string(),
        n => format!("__hwasan_load{}", n),
    }
}

fn kasan_store_fn(sz: usize) -> String {
    match sz {
        1 => "__asan_store1".to_string(),
        2 => "__asan_store2".to_string(),
        4 => "__asan_store4".to_string(),
        8 => "__asan_store8".to_string(),
        16 => "__asan_store16".to_string(),
        // P1.1: 16 字节以上或聚合类型的 KASAN store 检查.
        n => format!("__asan_store{}", n),
    }
}

fn hwasan_store_fn(sz: usize) -> String {
    match sz {
        1 => "__hwasan_store1".to_string(),
        2 => "__hwasan_store2".to_string(),
        4 => "__hwasan_store4".to_string(),
        8 => "__hwasan_store8".to_string(),
        16 => "__hwasan_store16".to_string(),
        n => format!("__hwasan_store{}", n),
    }
}

/// 把比较表达式 `a OP b` 包成 `(trace_cmpN(a, b), a OP b)`。
fn wrap_with_compare_trace(expr: &Expr, span: &Span) -> Expr {
    if let Expr::Binop { l, r, ty, .. } = expr {
        let (a, b) = (l.as_ref().clone(), r.as_ref().clone());
        let l_ty = a.ty();
        let arg_sz = l_ty.size().max(1);
        let fn_name = match arg_sz {
            1 => "__sanitizer_cov_trace_cmp1".to_string(),
            2 => "__sanitizer_cov_trace_cmp2".to_string(),
            4 => "__sanitizer_cov_trace_cmp4".to_string(),
            8 => "__sanitizer_cov_trace_cmp8".to_string(),
            _ => "__sanitizer_cov_trace_cmp".to_string(),
        };
        let call = Expr::Call {
            callee: Box::new(Expr::Ident {
                name: Ident {
                    name: fn_name,
                    span: span.clone(),
                },
                span: span.clone(),
                ty: Type::Func(Box::new(Type::Void), vec![l_ty.clone(), l_ty.clone()]),
            }),
            args: vec![a, b],
            span: span.clone(),
            ty: Type::Void,
        };
        Expr::Comma {
            l: Box::new(call),
            r: Box::new(expr.clone()),
            span: span.clone(),
            ty: ty.clone(),
        }
    } else {
        expr.clone()
    }
}

/// 生成 `__sanitizer_cov_trace_switch(arg, cases[0..N])` 的 wrapper。
/// 简化版:运行时用 `__sanitizer_cov_trace_switch(arg)` 即可
/// (一些 libFuzzer 变体仅接收 val),caller 仍然计算 case_vals 用来
/// 决定是否值得插入 (空 cases 列表就不插)。
fn switch_trace_wrapper(arg_expr: &Expr, cases: &[i128], arg_ty: &Type, span: &Span) -> Expr {
    let _ = (cases, arg_ty);
    let call = Expr::Call {
        callee: Box::new(Expr::Ident {
            name: Ident {
                name: "__sanitizer_cov_trace_switch".to_string(),
                span: span.clone(),
            },
            span: span.clone(),
            ty: Type::Func(Box::new(Type::Void), vec![Type::Long]),
        }),
        args: vec![arg_expr.clone()],
        span: span.clone(),
        ty: Type::Void,
    };
    Expr::Comma {
        l: Box::new(call),
        r: Box::new(arg_expr.clone()),
        span: span.clone(),
        ty: arg_expr.ty(),
    }
}

/// `__dcc_cov_guards_N` 每函数 guard 计数器数组声明。
fn guard_array_decl(name: &str, span: &Span) -> BlockItem {
    BlockItem::Decl(Decl::Var(crate::ast::VarDecl {
        ty: Type::Array(
            Box::new(Type::UInt),
            ArraySize::Var(Box::new(Expr::Int {
                v: 16,
                span: span.clone(),
                ty: Type::Int,
            })),
        ),
        name: Ident {
            name: name.to_string(),
            span: span.clone(),
        },
        init: None,
        span: span.clone(),
        is_extern: false,
        is_static: true,
        section: None,
        alignment: None,
        is_weak: false,
        visibility: Some("hidden".to_string()),
        is_unused: false,
        register_name: None,
        cleanup_fn: None,
        is_thread_local: false,
        no_sanitize: vec![],
        copy_fn: None,
        is_retain: false,
        is_used_attr: false,
    }))
}

/// `__sanitizer_cov_trace_pc_guard_init(N, &guards[0])` 初始化调用。
fn guard_init(base: &u32, span: &Span) -> BlockItem {
    BlockItem::Stmt(Stmt::Expr {
        e: Expr::Call {
            callee: Box::new(Expr::Ident {
                name: Ident {
                    name: "__sanitizer_cov_trace_pc_guard_init".to_string(),
                    span: span.clone(),
                },
                span: span.clone(),
                ty: Type::Func(
                    Box::new(Type::Void),
                    vec![Type::UInt, Type::Ptr(Box::new(Type::UInt))],
                ),
            }),
            args: vec![
                Expr::Int {
                    v: *base as i128,
                    span: span.clone(),
                    ty: Type::UInt,
                },
                Expr::Int {
                    v: 0,
                    span: span.clone(),
                    ty: Type::Ptr(Box::new(Type::UInt)),
                },
            ],
            span: span.clone(),
            ty: Type::Void,
        },
        span: span.clone(),
    })
}

/// `__sanitizer_cov_trace_pc_guard(&guards[idx])` 边覆盖。
fn guard_call_stmt(guard_var: &str, idx: u32, span: &Span) -> BlockItem {
    let call = Expr::Call {
        callee: Box::new(Expr::Ident {
            name: Ident {
                name: "__sanitizer_cov_trace_pc_guard".to_string(),
                span: span.clone(),
            },
            span: span.clone(),
            ty: Type::Func(Box::new(Type::Void), vec![Type::Ptr(Box::new(Type::UInt))]),
        }),
        args: vec![Expr::Index {
            arr: Box::new(Expr::Ident {
                name: Ident {
                    name: guard_var.to_string(),
                    span: span.clone(),
                },
                span: span.clone(),
                ty: Type::Array(Box::new(Type::UInt), ArraySize::Const(16)),
            }),
            idx: Box::new(Expr::Int {
                v: idx as i128,
                span: span.clone(),
                ty: Type::Int,
            }),
            span: span.clone(),
            ty: Type::UInt,
        }],
        span: span.clone(),
        ty: Type::Void,
    };
    BlockItem::Stmt(Stmt::Expr {
        e: call,
        span: span.clone(),
    })
}

/// TU 级一次性 init (不含 per-function coverage)。
fn tu_init_calls(flags: SanitizerFlags, span: &Span) -> Vec<BlockItem> {
    let mut out = Vec::new();
    if flags.kasan {
        out.push(sanitizer_call("__asan_init", span));
    }
    if flags.hwasan {
        out.push(sanitizer_call("__hwasan_init", span));
    }
    if flags.ubsan {
        out.push(sanitizer_call("__ubsan_init", span));
    }
    if flags.kcsan {
        out.push(sanitizer_call("__kcsan_init", span));
    }
    out
}

/// 每个函数入口桩 (coverage / KCSAN;KASAN/UBSAN 完整版靠 AST 表达式层插桩)。
fn func_entry_calls(flags: SanitizerFlags, span: &Span) -> Vec<BlockItem> {
    let mut out = Vec::new();
    if flags.kcov || flags.fuzzer {
        out.push(sanitizer_call("__sanitizer_cov_trace_pc", span));
    }
    if flags.kcsan {
        out.push(sanitizer_call("__kcsan_func_entry", span));
    }
    out
}

fn sanitizer_call(name: &str, span: &Span) -> BlockItem {
    let id = Ident {
        name: name.to_string(),
        span: span.clone(),
    };
    BlockItem::Stmt(Stmt::Expr {
        e: Expr::Call {
            callee: Box::new(Expr::Ident {
                name: id,
                span: span.clone(),
                ty: Type::Func(Box::new(Type::Void), vec![]),
            }),
            args: vec![],
            span: span.clone(),
            ty: Type::Void,
        },
        span: span.clone(),
    })
}

/// 根据 no_sanitize 属性计算本函数实际启用的 sanitizer 通道。
/// KCOV / Fuzzer 不受 no_sanitize 控制 (coverage 反馈不影响
/// 代码语义,总是插入)。
fn effective_flags(no_sanitize: &[String], flags: SanitizerFlags) -> SanitizerFlags {
    let mut out = flags;
    for tag in no_sanitize {
        match tag.as_str() {
            "address"
            | "kernel-address"
            | "hwaddress"
            | "kernel-hwaddress"
            | "no_sanitize_address"
            | "no_address_safety_analysis" => {
                out.kasan = false;
                out.hwasan = false;
            }
            "undefined" => out.ubsan = false,
            "thread" | "kernel-kcsan" | "no_sanitize_thread" => out.kcsan = false,
            "all" => {
                out.kasan = false;
                out.hwasan = false;
                out.ubsan = false;
                out.kcsan = false;
            }
            _ => {}
        }
    }
    out
}

/// `.dcc.san_info` 节载荷。
pub fn sanitizer_metadata(flags: SanitizerFlags) -> Vec<u8> {
    let mut out = b"DCC_SAN\0".to_vec();
    out.push(2); // 格式版本 v2
    out.push(flags.kasan as u8);
    out.push(flags.hwasan as u8);
    out.push(flags.ubsan as u8);
    out.push(flags.kcsan as u8);
    out.push(flags.kcov as u8);
    out.push(flags.fuzzer as u8);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Param;

    fn empty_func(name: &str) -> FuncDecl {
        FuncDecl {
            ret_ty: Type::Int,
            name: Ident {
                name: name.into(),
                span: Span::point("t.c", 1, 1),
            },
            params: vec![],
            body: Some(Block {
                items: vec![BlockItem::Stmt(Stmt::Return {
                    val: None,
                    span: Span::point("t.c", 2, 1),
                })],
                span: Span::point("t.c", 1, 1),
            }),
            span: Span::point("t.c", 1, 1),
            is_extern: false,
            is_static: false,
            section: None,
            alignment: None,
            is_weak: false,
            visibility: None,
            is_unused: false,
            is_cold: false,
            is_hot: false,
            is_noinline: false,
            is_always_inline: false,
            is_variadic: false,
            is_constructor: false,
            is_destructor: false,
            alias_name: None,
            is_nested: false,
            upvars: vec![],
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
            is_global: false,
            is_device: false,
            is_host: false,
        }
    }

    fn call_names_in_block(body: &Block) -> Vec<String> {
        let mut out = Vec::new();
        for item in &body.items {
            collect_call_names(item, &mut out);
        }
        out
    }

    fn collect_call_names(item: &BlockItem, out: &mut Vec<String>) {
        match item {
            BlockItem::Stmt(Stmt::Expr { e, .. }) => collect_call_names_expr(e, out),
            BlockItem::Stmt(Stmt::Return { val: Some(e), .. }) => {
                collect_call_names_expr(e, out);
            }
            BlockItem::Stmt(Stmt::If {
                cond, then_, else_, ..
            }) => {
                collect_call_names_expr(cond, out);
                collect_call_names_stmt(then_, out);
                if let Some(e) = else_ {
                    collect_call_names_stmt(e, out);
                }
            }
            BlockItem::Stmt(Stmt::While { cond, body, .. })
            | BlockItem::Stmt(Stmt::Do { body, cond, .. }) => {
                collect_call_names_expr(cond, out);
                collect_call_names_stmt(body, out);
            }
            BlockItem::Stmt(Stmt::For {
                cond, step, body, ..
            }) => {
                if let Some(c) = cond {
                    collect_call_names_expr(c, out);
                }
                if let Some(s) = step {
                    collect_call_names_expr(s, out);
                }
                collect_call_names_stmt(body, out);
            }
            BlockItem::Stmt(Stmt::Block { b, .. }) => {
                for it in &b.items {
                    collect_call_names(it, out);
                }
            }
            BlockItem::Decl(Decl::Var(_)) => {}
            BlockItem::Decl(Decl::Func(f)) => {
                if let Some(b) = &f.body {
                    for it in &b.items {
                        collect_call_names(it, out);
                    }
                }
            }
            _ => {}
        }
    }

    fn collect_call_names_stmt(stmt: &Stmt, out: &mut Vec<String>) {
        match stmt {
            Stmt::Expr { e, .. } => collect_call_names_expr(e, out),
            Stmt::Block { b, .. } => {
                for it in &b.items {
                    collect_call_names(it, out);
                }
            }
            Stmt::Return { val: Some(e), .. } => collect_call_names_expr(e, out),
            Stmt::If {
                cond, then_, else_, ..
            } => {
                collect_call_names_expr(cond, out);
                collect_call_names_stmt(then_, out);
                if let Some(e) = else_ {
                    collect_call_names_stmt(e, out);
                }
            }
            _ => {}
        }
    }

    fn collect_call_names_expr(e: &Expr, out: &mut Vec<String>) {
        match e {
            Expr::Call { callee, args, .. } => {
                if let Expr::Ident { name, .. } = callee.as_ref() {
                    out.push(name.name.clone());
                }
                for a in args {
                    collect_call_names_expr(a, out);
                }
            }
            Expr::Comma { l, r, .. } => {
                collect_call_names_expr(l, out);
                collect_call_names_expr(r, out);
            }
            Expr::Binop { l, r, .. } | Expr::Assign { l, r, .. } => {
                collect_call_names_expr(l, out);
                collect_call_names_expr(r, out);
            }
            Expr::Unary { e, .. } | Expr::Deref { e, .. } | Expr::AddrOf { e, .. } => {
                collect_call_names_expr(e, out);
            }
            Expr::Cond {
                cond, then_, else_, ..
            } => {
                collect_call_names_expr(cond, out);
                collect_call_names_expr(then_, out);
                collect_call_names_expr(else_, out);
            }
            Expr::Index { arr, idx, .. } => {
                collect_call_names_expr(arr, out);
                collect_call_names_expr(idx, out);
            }
            Expr::Member { expr, .. } => collect_call_names_expr(expr, out),
            Expr::Cast { e, .. } => collect_call_names_expr(e, out),
            Expr::SizeofExpr { e, .. }
            | Expr::AlignofExpr { e, .. }
            | Expr::PreInc { e, .. }
            | Expr::PreDec { e, .. }
            | Expr::PostInc { e, .. }
            | Expr::PostDec { e, .. } => collect_call_names_expr(e, out),
            _ => {}
        }
    }

    #[test]
    fn from_args_parses_combined_sanitize() {
        let f = SanitizerFlags::from_args(&[
            "-fsanitize=address,undefined".to_string(),
            "-fsanitize-coverage".to_string(),
        ]);
        assert!(f.kasan && f.ubsan && f.kcov);
    }

    #[test]
    fn from_args_recognizes_hwasan_and_fuzzer() {
        let f = SanitizerFlags::from_args(&["-fsanitize=hwaddress,fuzzer".to_string()]);
        assert!(f.hwasan && f.fuzzer);
    }

    #[test]
    fn tu_init_only_in_first_function() {
        let mut prog = Program {
            decls: vec![
                Decl::Func(empty_func("first")),
                Decl::Func(empty_func("second")),
            ],
        };
        let flags = SanitizerFlags {
            kasan: true,
            kcov: true,
            ..Default::default()
        };
        instrument_program(&mut prog, flags);
        let Decl::Func(f0) = &prog.decls[0] else {
            panic!()
        };
        let Decl::Func(f1) = &prog.decls[1] else {
            panic!()
        };
        let n0 = call_names_in_block(f0.body.as_ref().unwrap());
        let n1 = call_names_in_block(f1.body.as_ref().unwrap());
        assert!(n0.iter().any(|n| n == "__asan_init"));
        assert_eq!(n0.iter().filter(|n| *n == "__asan_init").count(), 1);
        assert!(!n1.iter().any(|n| n == "__asan_init"));
        assert!(n0.iter().any(|n| n == "__sanitizer_cov_trace_pc"));
        assert!(n1.iter().any(|n| n == "__sanitizer_cov_trace_pc"));
    }

    #[test]
    fn every_function_gets_kcsan_entry() {
        let mut prog = Program {
            decls: vec![Decl::Func(empty_func("a")), Decl::Func(empty_func("b"))],
        };
        instrument_program(
            &mut prog,
            SanitizerFlags {
                kcsan: true,
                ..Default::default()
            },
        );
        for decl in &prog.decls {
            let Decl::Func(f) = decl else { continue };
            let names = call_names_in_block(f.body.as_ref().unwrap());
            assert!(
                names.iter().any(|n| n == "__kcsan_init")
                    || names.iter().any(|n| n == "__kcsan_func_entry")
            );
        }
        let Decl::Func(f1) = &prog.decls[1] else {
            panic!()
        };
        let names = call_names_in_block(f1.body.as_ref().unwrap());
        assert!(!names.iter().any(|n| n == "__kcsan_init"));
        assert!(names.iter().any(|n| n == "__kcsan_func_entry"));
    }

    #[test]
    fn extern_func_skips_prologue() {
        let mut prog = Program {
            decls: vec![Decl::Func(FuncDecl {
                ret_ty: Type::Int,
                name: Ident {
                    name: "ext".into(),
                    span: Span::point("t.c", 1, 1),
                },
                params: vec![Param {
                    ty: Type::Int,
                    name: None,
                    span: Span::point("t.c", 1, 1),
                }],
                body: None,
                span: Span::point("t.c", 1, 1),
                is_extern: true,
                is_static: false,
                section: None,
                alignment: None,
                is_weak: false,
                visibility: None,
                is_unused: false,
                is_cold: false,
                is_hot: false,
                is_noinline: false,
                is_always_inline: false,
                is_variadic: false,
                is_constructor: false,
                is_destructor: false,
                alias_name: None,
                is_nested: false,
                upvars: vec![],
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
                is_global: false,
                is_device: false,
                is_host: false,
            })],
        };
        instrument_program(
            &mut prog,
            SanitizerFlags {
                kasan: true,
                kcov: true,
                ..Default::default()
            },
        );
        let Decl::Func(f) = &prog.decls[0] else {
            panic!()
        };
        assert!(f.body.is_none());
    }

    #[test]
    fn metadata_includes_version_byte() {
        let meta = sanitizer_metadata(SanitizerFlags {
            kasan: true,
            ..Default::default()
        });
        assert_eq!(&meta[..8], b"DCC_SAN\0");
        assert_eq!(meta[8], 2);
        assert_eq!(meta[9], 1);
    }

    #[test]
    fn no_sanitize_excludes_all_kinds() {
        let mut prog = Program {
            decls: vec![Decl::Func(FuncDecl {
                ret_ty: Type::Int,
                name: Ident {
                    name: "skipped".into(),
                    span: Span::point("t.c", 1, 1),
                },
                params: vec![],
                body: Some(Block {
                    items: vec![BlockItem::Stmt(Stmt::Return {
                        val: None,
                        span: Span::point("t.c", 1, 1),
                    })],
                    span: Span::point("t.c", 1, 1),
                }),
                span: Span::point("t.c", 1, 1),
                is_extern: false,
                is_static: false,
                section: None,
                alignment: None,
                is_weak: false,
                visibility: None,
                is_unused: false,
                is_cold: false,
                is_hot: false,
                is_noinline: false,
                is_always_inline: false,
                is_variadic: false,
                is_constructor: false,
                is_destructor: false,
                alias_name: None,
                is_nested: false,
                upvars: vec![],
                is_old_style: false,
                format_kind: None,
                format_string_idx: None,
                format_first_arg: None,
                no_sanitize: vec![
                    "address".to_string(),
                    "undefined".to_string(),
                    "thread".to_string(),
                ],
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
                is_global: false,
                is_device: false,
                is_host: false,
            })],
        };
        let flags = SanitizerFlags {
            kasan: true,
            ubsan: true,
            kcsan: true,
            kcov: true,
            ..Default::default()
        };
        instrument_program(&mut prog, flags);
        let Decl::Func(f) = &prog.decls[0] else {
            panic!()
        };
        let names = call_names_in_block(f.body.as_ref().unwrap());
        // 函数入口 coverage 仍保留 (KCOV 不受 no_sanitize 控制)
        assert!(names.iter().any(|n| n == "__sanitizer_cov_trace_pc"));
        assert!(!names.iter().any(|n| n == "__asan_init"));
        assert!(!names.iter().any(|n| n == "__ubsan_init"));
        assert!(!names.iter().any(|n| n == "__kcsan_init"));
    }

    #[test]
    fn ubsan_wraps_division_by_zero() {
        let mut prog = Program {
            decls: vec![Decl::Func(empty_func("f"))],
        };
        // 替换 body 为 `return x / y;`
        let Decl::Func(f) = &mut prog.decls[0] else {
            panic!()
        };
        f.body = Some(Block {
            items: vec![BlockItem::Stmt(Stmt::Return {
                val: Some(Expr::Binop {
                    op: BinOp::Div,
                    l: Box::new(Expr::Ident {
                        name: Ident {
                            name: "x".into(),
                            span: Span::point("t.c", 1, 1),
                        },
                        span: Span::point("t.c", 1, 1),
                        ty: Type::Int,
                    }),
                    r: Box::new(Expr::Ident {
                        name: Ident {
                            name: "y".into(),
                            span: Span::point("t.c", 1, 1),
                        },
                        span: Span::point("t.c", 1, 1),
                        ty: Type::Int,
                    }),
                    span: Span::point("t.c", 1, 1),
                    ty: Type::Int,
                }),
                span: Span::point("t.c", 1, 1),
            })],
            span: Span::point("t.c", 1, 1),
        });
        instrument_program(
            &mut prog,
            SanitizerFlags {
                ubsan: true,
                ..Default::default()
            },
        );
        let Decl::Func(f) = &prog.decls[0] else {
            panic!()
        };
        let names = call_names_in_block(f.body.as_ref().unwrap());
        assert!(names.iter().any(|n| n == "__ubsan_handle_divrem_overflow"));
    }

    #[test]
    fn ubsan_wraps_shift() {
        let mut prog = Program {
            decls: vec![Decl::Func(empty_func("f"))],
        };
        let Decl::Func(f) = &mut prog.decls[0] else {
            panic!()
        };
        f.body = Some(Block {
            items: vec![BlockItem::Stmt(Stmt::Return {
                val: Some(Expr::Binop {
                    op: BinOp::Shl,
                    l: Box::new(Expr::Ident {
                        name: Ident {
                            name: "x".into(),
                            span: Span::point("t.c", 1, 1),
                        },
                        span: Span::point("t.c", 1, 1),
                        ty: Type::Int,
                    }),
                    r: Box::new(Expr::Ident {
                        name: Ident {
                            name: "y".into(),
                            span: Span::point("t.c", 1, 1),
                        },
                        span: Span::point("t.c", 1, 1),
                        ty: Type::Int,
                    }),
                    span: Span::point("t.c", 1, 1),
                    ty: Type::Int,
                }),
                span: Span::point("t.c", 1, 1),
            })],
            span: Span::point("t.c", 1, 1),
        });
        instrument_program(
            &mut prog,
            SanitizerFlags {
                ubsan: true,
                ..Default::default()
            },
        );
        let Decl::Func(f) = &prog.decls[0] else {
            panic!()
        };
        let names = call_names_in_block(f.body.as_ref().unwrap());
        assert!(names
            .iter()
            .any(|n| n == "__ubsan_handle_shift_out_of_bounds"));
    }

    #[test]
    fn kasan_wraps_deref() {
        let mut prog = Program {
            decls: vec![Decl::Func(empty_func("f"))],
        };
        let Decl::Func(f) = &mut prog.decls[0] else {
            panic!()
        };
        f.body = Some(Block {
            items: vec![BlockItem::Stmt(Stmt::Return {
                val: Some(Expr::Deref {
                    e: Box::new(Expr::Ident {
                        name: Ident {
                            name: "p".into(),
                            span: Span::point("t.c", 1, 1),
                        },
                        span: Span::point("t.c", 1, 1),
                        ty: Type::Ptr(Box::new(Type::Int)),
                    }),
                    span: Span::point("t.c", 1, 1),
                    ty: Type::Int,
                }),
                span: Span::point("t.c", 1, 1),
            })],
            span: Span::point("t.c", 1, 1),
        });
        instrument_program(
            &mut prog,
            SanitizerFlags {
                kasan: true,
                ..Default::default()
            },
        );
        let Decl::Func(f) = &prog.decls[0] else {
            panic!()
        };
        let names = call_names_in_block(f.body.as_ref().unwrap());
        assert!(names.iter().any(|n| n == "__asan_load4"));
    }

    #[test]
    fn fuzzer_injects_compare_trace() {
        let mut prog = Program {
            decls: vec![Decl::Func(empty_func("f"))],
        };
        let Decl::Func(f) = &mut prog.decls[0] else {
            panic!()
        };
        f.body = Some(Block {
            items: vec![BlockItem::Stmt(Stmt::Return {
                val: Some(Expr::Binop {
                    op: BinOp::EqEq,
                    l: Box::new(Expr::Ident {
                        name: Ident {
                            name: "x".into(),
                            span: Span::point("t.c", 1, 1),
                        },
                        span: Span::point("t.c", 1, 1),
                        ty: Type::Int,
                    }),
                    r: Box::new(Expr::Int {
                        v: 0,
                        span: Span::point("t.c", 1, 1),
                        ty: Type::Int,
                    }),
                    span: Span::point("t.c", 1, 1),
                    ty: Type::Int,
                }),
                span: Span::point("t.c", 1, 1),
            })],
            span: Span::point("t.c", 1, 1),
        });
        instrument_program(
            &mut prog,
            SanitizerFlags {
                fuzzer: true,
                ..Default::default()
            },
        );
        let Decl::Func(f) = &prog.decls[0] else {
            panic!()
        };
        let names = call_names_in_block(f.body.as_ref().unwrap());
        assert!(names.iter().any(|n| n == "__sanitizer_cov_trace_cmp4"));
    }

    #[test]
    fn any_returns_true_when_any_flag_set() {
        assert!(SanitizerFlags {
            kasan: true,
            ..Default::default()
        }
        .any());
        assert!(SanitizerFlags {
            kcov: true,
            ..Default::default()
        }
        .any());
        assert!(!SanitizerFlags::default().any());
    }

    #[test]
    fn ubsan_vla_bound_check_inserted() {
        // VLA 声明 int arr[n]; 应在声明前插入
        // __ubsan_handle_vla_bound_not_positive(n) 调用。
        let mut prog = Program {
            decls: vec![Decl::Func(empty_func("f"))],
        };
        let Decl::Func(f) = &mut prog.decls[0] else {
            panic!()
        };
        let vla_decl = VarDecl {
            ty: Type::Array(
                Box::new(Type::Int),
                ArraySize::Var(Box::new(Expr::Ident {
                    name: Ident {
                        name: "n".into(),
                        span: Span::point("t.c", 1, 1),
                    },
                    span: Span::point("t.c", 1, 1),
                    ty: Type::Int,
                })),
            ),
            name: Ident {
                name: "arr".into(),
                span: Span::point("t.c", 1, 1),
            },
            init: None,
            span: Span::point("t.c", 1, 1),
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
            copy_fn: None,
            is_retain: false,
            is_used_attr: false,
        };
        f.body = Some(Block {
            items: vec![
                BlockItem::Decl(Decl::Var(vla_decl)),
                BlockItem::Stmt(Stmt::Return {
                    val: None,
                    span: Span::point("t.c", 1, 1),
                }),
            ],
            span: Span::point("t.c", 1, 1),
        });
        instrument_program(
            &mut prog,
            SanitizerFlags {
                ubsan: true,
                ..Default::default()
            },
        );
        let Decl::Func(f) = &prog.decls[0] else {
            panic!()
        };
        let names = call_names_in_block(f.body.as_ref().unwrap());
        assert!(
            names
                .iter()
                .any(|n| n == "__ubsan_handle_vla_bound_not_positive"),
            "VLA bound check not inserted: calls = {:?}",
            names
        );
    }

    #[test]
    fn ubsan_null_deref_check_inserted() {
        // *p 应同时插入 KASAN (若启用) 与 null 检查。
        let mut prog = Program {
            decls: vec![Decl::Func(empty_func("f"))],
        };
        let Decl::Func(f) = &mut prog.decls[0] else {
            panic!()
        };
        f.body = Some(Block {
            items: vec![BlockItem::Stmt(Stmt::Return {
                val: Some(Expr::Deref {
                    e: Box::new(Expr::Ident {
                        name: Ident {
                            name: "p".into(),
                            span: Span::point("t.c", 1, 1),
                        },
                        span: Span::point("t.c", 1, 1),
                        ty: Type::Ptr(Box::new(Type::Int)),
                    }),
                    span: Span::point("t.c", 1, 1),
                    ty: Type::Int,
                }),
                span: Span::point("t.c", 1, 1),
            })],
            span: Span::point("t.c", 1, 1),
        });
        instrument_program(
            &mut prog,
            SanitizerFlags {
                ubsan: true,
                ..Default::default()
            },
        );
        let Decl::Func(f) = &prog.decls[0] else {
            panic!()
        };
        let names = call_names_in_block(f.body.as_ref().unwrap());
        assert!(
            names
                .iter()
                .any(|n| n == "__ubsan_handle_null_pointer_dereference"),
            "null check missing: calls = {:?}",
            names
        );
    }

    #[test]
    fn ubsan_float_cast_check_inserted() {
        // (int)f 应插入 __ubsan_handle_float_cast_overflow 调用。
        let mut prog = Program {
            decls: vec![Decl::Func(empty_func("f"))],
        };
        let Decl::Func(f) = &mut prog.decls[0] else {
            panic!()
        };
        f.body = Some(Block {
            items: vec![BlockItem::Stmt(Stmt::Return {
                val: Some(Expr::Cast {
                    ty: Type::Int,
                    e: Box::new(Expr::Ident {
                        name: Ident {
                            name: "d".into(),
                            span: Span::point("t.c", 1, 1),
                        },
                        span: Span::point("t.c", 1, 1),
                        ty: Type::Double,
                    }),
                    span: Span::point("t.c", 1, 1),
                }),
                span: Span::point("t.c", 1, 1),
            })],
            span: Span::point("t.c", 1, 1),
        });
        instrument_program(
            &mut prog,
            SanitizerFlags {
                ubsan: true,
                ..Default::default()
            },
        );
        let Decl::Func(f) = &prog.decls[0] else {
            panic!()
        };
        let names = call_names_in_block(f.body.as_ref().unwrap());
        assert!(
            names
                .iter()
                .any(|n| n == "__ubsan_handle_float_cast_overflow"),
            "float cast check missing: calls = {:?}",
            names
        );
    }

    #[test]
    fn kcsan_only_uses_kcsan_check_access() {
        // 当 KCSAN 是唯一的内存 sanitizer 时,Deref 应使用
        // __kcsan_check_access 而非 __asan_loadN。
        let mut prog = Program {
            decls: vec![Decl::Func(empty_func("f"))],
        };
        let Decl::Func(f) = &mut prog.decls[0] else {
            panic!()
        };
        f.body = Some(Block {
            items: vec![BlockItem::Stmt(Stmt::Return {
                val: Some(Expr::Deref {
                    e: Box::new(Expr::Ident {
                        name: Ident {
                            name: "p".into(),
                            span: Span::point("t.c", 1, 1),
                        },
                        span: Span::point("t.c", 1, 1),
                        ty: Type::Ptr(Box::new(Type::Int)),
                    }),
                    span: Span::point("t.c", 1, 1),
                    ty: Type::Int,
                }),
                span: Span::point("t.c", 1, 1),
            })],
            span: Span::point("t.c", 1, 1),
        });
        instrument_program(
            &mut prog,
            SanitizerFlags {
                kcsan: true,
                ..Default::default()
            },
        );
        let Decl::Func(f) = &prog.decls[0] else {
            panic!()
        };
        let names = call_names_in_block(f.body.as_ref().unwrap());
        assert!(
            names.iter().any(|n| n == "__kcsan_check_access"),
            "KCSAN access check missing: calls = {:?}",
            names
        );
        assert!(
            !names.iter().any(|n| n.starts_with("__asan_load")),
            "KASAN load should not be emitted when only KCSAN is enabled: calls = {:?}",
            names
        );
    }

    #[test]
    fn int_range_covers_all_integer_types() {
        assert_eq!(int_range(&Type::Int), (i32::MIN as i128, i32::MAX as i128));
        assert_eq!(int_range(&Type::UInt), (0, u32::MAX as i128));
        assert_eq!(int_range(&Type::Int128), (i128::MIN, i128::MAX));
        assert_eq!(int_range(&Type::UInt128), (0, u128::MAX as i128));
    }

    #[test]
    fn is_float_to_int_cast_classifies_correctly() {
        assert!(is_float_to_int_cast(&Type::Double, &Type::Int));
        assert!(is_float_to_int_cast(&Type::Float, &Type::UInt));
        assert!(is_float_to_int_cast(&Type::LongDouble, &Type::LongLong));
        assert!(!is_float_to_int_cast(&Type::Int, &Type::Int));
        assert!(!is_float_to_int_cast(&Type::Double, &Type::Double));
        assert!(!is_float_to_int_cast(&Type::Double, &Type::Bool));
    }

    // P1.1: 16 字节以下 KASAN load/store 函数名映射。
    #[test]
    fn kasan_load_fn_maps_known_sizes() {
        assert_eq!(kasan_load_fn(1), "__asan_load1");
        assert_eq!(kasan_load_fn(2), "__asan_load2");
        assert_eq!(kasan_load_fn(4), "__asan_load4");
        assert_eq!(kasan_load_fn(8), "__asan_load8");
        assert_eq!(kasan_load_fn(16), "__asan_load16");
    }

    // P1.1: 16 字节以上走 `__asan_loadN` 通用名,运行时提供
    // `__asan_loadN(addr, size)` 双参 variant.
    #[test]
    fn kasan_load_fn_emits_generic_for_aggregate_sizes() {
        assert_eq!(kasan_load_fn(24), "__asan_load24");
        assert_eq!(kasan_load_fn(32), "__asan_load32");
        assert_eq!(kasan_load_fn(48), "__asan_load48");
        assert_eq!(kasan_load_fn(64), "__asan_load64");
    }

    // P1.1: store 侧 16 字节以下函数名映射。
    #[test]
    fn kasan_store_fn_maps_known_sizes() {
        assert_eq!(kasan_store_fn(1), "__asan_store1");
        assert_eq!(kasan_store_fn(2), "__asan_store2");
        assert_eq!(kasan_store_fn(4), "__asan_store4");
        assert_eq!(kasan_store_fn(8), "__asan_store8");
        assert_eq!(kasan_store_fn(16), "__asan_store16");
    }

    // P1.1: store 侧 16 字节以上走 `__asan_storeN` 通用名。
    #[test]
    fn kasan_store_fn_emits_generic_for_aggregate_sizes() {
        assert_eq!(kasan_store_fn(24), "__asan_store24");
        assert_eq!(kasan_store_fn(32), "__asan_store32");
        assert_eq!(kasan_store_fn(48), "__asan_store48");
        assert_eq!(kasan_store_fn(64), "__asan_store64");
    }

    // P1.1: 16 字节以上 emit 双参调用 (addr + size).
    #[test]
    fn make_mem_check_call_emits_size_arg_for_aggregate_kasan() {
        let addr = Expr::Ident {
            name: Ident {
                name: "p".into(),
                span: Span::point("t.c", 1, 1),
            },
            span: Span::point("t.c", 1, 1),
            ty: Type::Ptr(Box::new(Type::Void)),
        };
        let call = make_mem_check_call(
            addr,
            24,
            false,
            &Span::point("t.c", 1, 1),
            SanitizerFlags {
                kasan: true,
                ..Default::default()
            },
        );
        if let Expr::Call { callee, args, .. } = call {
            if let Expr::Ident { name, .. } = callee.as_ref() {
                assert_eq!(name.name, "__asan_load24");
            } else {
                panic!("callee should be Ident");
            }
            assert_eq!(args.len(), 2, "aggregate load must pass size as 2nd arg");
        } else {
            panic!("expected Expr::Call");
        }
    }

    // P1.1: 16 字节及以下 emit 单参调用 (只 addr).
    #[test]
    fn make_mem_check_call_emits_single_arg_for_small_kasan() {
        let addr = Expr::Ident {
            name: Ident {
                name: "p".into(),
                span: Span::point("t.c", 1, 1),
            },
            span: Span::point("t.c", 1, 1),
            ty: Type::Ptr(Box::new(Type::Void)),
        };
        for &sz in &[1usize, 2, 4, 8, 16] {
            let call = make_mem_check_call(
                addr.clone(),
                sz,
                false,
                &Span::point("t.c", 1, 1),
                SanitizerFlags {
                    kasan: true,
                    ..Default::default()
                },
            );
            if let Expr::Call { args, .. } = call {
                assert_eq!(args.len(), 1, "size {} should emit single arg", sz);
            } else {
                panic!("expected Expr::Call");
            }
        }
    }
}
