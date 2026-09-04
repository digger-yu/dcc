// src/sanitizer.rs - KASAN/UBSAN/KCSAN/KCOV/Fuzzer/MSAN/DFSan/CFI 完整插桩
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
//     `__ubsan_handle_add/sub/mul/negate_overflow` 之一。
//   * Thread sanitizer: user-mode `-fsanitize=thread` emits libtsan-compatible
//     `__tsan_*` callbacks; `kernel-kcsan` keeps kernel `__kcsan_*` callbacks.
//   * Memory sanitizer (MSAN): `-fsanitize=memory` emits `__msan_check_load_N`
//     / `__msan_check_store_N` on memory accesses to detect use of
//     uninitialized values; `__msan_func_entry` at each function prologue.
//   * DataFlow sanitizer (DFSan): `-fsanitize=dataflow` emits
//     `__dfsan_load_label_N` / `__dfsan_store_label_N` for taint label
//     propagation through memory operations.
//   * Control Flow Integrity (CFI): `-fsanitize=cfi-icall` etc. emit
//     `__cfi_check(callee, type_hash)` before indirect calls to verify
//     target function signature matches expected type.
//   * KCOV/Fuzzer: 边覆盖使用每函数 guard 计数器 (`uint32_t __dcc_cov_guards[N]`),
//     入口 + 每个块开始 emit `__sanitizer_cov_trace_pc_guard(guard_idx)`;
//     比较回调使用 `__sanitizer_cov_trace_cmp1/2/4/8(arg0, arg1)`;
//     switch 调度使用 `__sanitizer_cov_trace_switch(arg, cases[0..N])`。
// - 运行时库: 用户态 address / undefined / thread / memory / dataflow / cfi
//   覆盖由 driver 在最终链接阶段自动追加 sanitizer runtime；`-c` / `-S`
//   仍只留下可重定位 runtime 调用。内核模式 sanitizer 不链接宿主库。
// - 选通: KASAN/UBSAN/KCSAN 完全禁用时不插入;部分启用时仅插入对应调用。
// - no_sanitize: 函数级 `__attribute__((no_sanitize("address")))` 等会
//   关闭对应 sanitizer 通道。

use crate::ast::{
    ArraySize, BinOp, Block, BlockItem, Decl, Expr, FuncDecl, Ident, InitItem, Program, QualType, Stmt, Type,
    UnOp, VarDecl,
};
use crate::cli::{Arch, CliError};
use crate::diagnostics::Span;
use std::collections::{HashMap, HashSet};

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
    /// `-fsanitize-coverage=trace-cmp` compare callbacks.
    pub cov_trace_cmp: bool,
    /// `-fsanitize-coverage=trace-switch` switch callbacks.
    pub cov_trace_switch: bool,
    /// `-fsanitize-coverage=8bit-counters` counter init callbacks.
    pub cov_8bit_counters: bool,
    /// `-fsanitize-coverage=pc-table` PC table init callbacks.
    pub cov_pc_table: bool,
    /// `-fsanitize=fuzzer` (san-coverage + compare + switch)
    pub fuzzer: bool,
    /// `-fsanitize=memory` (MemorySanitizer — uninitialized memory detection)
    pub msan: bool,
    /// `-fsanitize=dataflow` (DataFlowSanitizer — taint / label tracking)
    pub dfsan: bool,
    /// `-fsanitize=cfi-icall` (indirect call type check)
    pub cfi_icall: bool,
    /// `-fsanitize=cfi-vcall` (virtual call type check)
    pub cfi_vcall: bool,
    /// `-fsanitize=cfi-nvcall` (non-virtual call type check)
    pub cfi_nvcall: bool,
    /// `-fsanitize=cfi-derived-cast` (derived cast type check)
    pub cfi_derived_cast: bool,
    /// `-fsanitize=cfi-unrelated-cast` (unrelated cast type check)
    pub cfi_unrelated_cast: bool,
    /// `-fsanitize=cfi-mfcall` (member function call type check)
    pub cfi_mfcall: bool,
}

impl SanitizerFlags {
    /// 从 `-fsanitize=*` / `-fsanitize-coverage` 等 CLI 字符串解析。
    /// 接受 `addr,undefined` 形式与 `kernel-*` / `hwaddress` 变体。
    pub fn from_args(args: &[String]) -> Result<Self, CliError> {
        let mut f = Self::default();
        for a in args {
            Self::apply_flag(a, &mut f)?;
        }
        Ok(f)
    }

    fn apply_flag(arg: &str, f: &mut Self) -> Result<(), CliError> {
        if let Some(rest) = arg.strip_prefix("-fsanitize=") {
            for part in rest.split(',') {
                Self::apply_sanitize_name(part.trim(), f)?;
            }
            return Ok(());
        }
        if let Some(rest) = arg.strip_prefix("-fsanitize-coverage=") {
            for part in rest.split(',') {
                Self::apply_coverage_name(part.trim(), f)?;
            }
            return Ok(());
        }
        if arg == "-fsanitize-coverage" {
            f.kcov = true;
        }
        Ok(())
    }

    fn apply_coverage_name(name: &str, f: &mut Self) -> Result<(), CliError> {
        match name {
            "trace-pc" | "trace-pc-guard" | "coverage" => f.kcov = true,
            "trace-cmp" => {
                f.kcov = true;
                f.cov_trace_cmp = true;
            }
            "trace-switch" => {
                f.kcov = true;
                f.cov_trace_switch = true;
            }
            "8bit-counters" => {
                f.kcov = true;
                f.cov_8bit_counters = true;
            }
            "pc-table" => {
                f.kcov = true;
                f.cov_pc_table = true;
            }
            "" => {}
            _ => {
                return Err(CliError::new(&format!(
                    "unsupported sanitizer coverage: {}",
                    name
                )))
            }
        }
        Ok(())
    }

    fn apply_sanitize_name(name: &str, f: &mut Self) -> Result<(), CliError> {
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
            "memory" => f.msan = true,
            "dataflow" => f.dfsan = true,
            "cfi" => {
                f.cfi_icall = true;
                f.cfi_vcall = true;
                f.cfi_nvcall = true;
                f.cfi_derived_cast = true;
                f.cfi_unrelated_cast = true;
                f.cfi_mfcall = true;
            }
            "cfi-icall" => f.cfi_icall = true,
            "cfi-vcall" => f.cfi_vcall = true,
            "cfi-nvcall" => f.cfi_nvcall = true,
            "cfi-derived-cast" => f.cfi_derived_cast = true,
            "cfi-unrelated-cast" => f.cfi_unrelated_cast = true,
            "cfi-mfcall" => f.cfi_mfcall = true,
            "fuzzer" => {
                f.fuzzer = true;
                f.kcov = true;
                f.cov_trace_cmp = true;
                f.cov_trace_switch = true;
            }
            "coverage" | "trace-pc" | "trace-pc-guard" => f.kcov = true,
            _ => return Err(CliError::new(&format!("unsupported sanitizer: {}", name))),
        }
        Ok(())
    }

    /// 是否启用了任何 sanitizer。
    pub fn any(&self) -> bool {
        self.kasan
            || self.hwasan
            || self.ubsan
            || self.kcsan
            || self.kcov
            || self.cov_trace_cmp
            || self.cov_trace_switch
            || self.cov_8bit_counters
            || self.cov_pc_table
            || self.fuzzer
            || self.msan
            || self.dfsan
            || self.any_cfi()
    }

    /// TU 级 init 是否需要在首个函数前插入。
    pub fn needs_tu_init(&self) -> bool {
        self.kasan
            || self.hwasan
            || self.ubsan
            || self.kcsan
            || self.cov_8bit_counters
            || self.cov_pc_table
            || self.msan
    }

    /// KASAN / HWASAN load/store 检查。
    pub fn needs_load_store_check(&self) -> bool {
        self.kasan || self.hwasan
    }

    /// 比较回调 (trace-cmp / fuzzer)。
    pub fn needs_compare_trace(&self) -> bool {
        self.cov_trace_cmp || self.fuzzer
    }

    /// switch 调度 trace。
    pub fn needs_switch_trace(&self) -> bool {
        self.cov_trace_switch || self.fuzzer
    }

    /// 边覆盖 guard 计数器 (trace-pc-guard)。
    pub fn needs_edge_coverage(&self) -> bool {
        self.kcov || self.cov_8bit_counters || self.cov_pc_table || self.fuzzer
    }

    /// KCSAN 内存访问 watchpoint。
    pub fn needs_kcsan_check(&self) -> bool {
        self.kcsan
    }

    /// UBSAN 任意通道。
    pub fn needs_ubsan(&self) -> bool {
        self.ubsan
    }

    /// MSAN 未初始化内存检查。
    pub fn needs_msan_check(&self) -> bool {
        self.msan
    }

    /// DFSan 标签传播。
    pub fn needs_dfsan_check(&self) -> bool {
        self.dfsan
    }

    /// 任何 CFI 子通道启用。
    pub fn any_cfi(&self) -> bool {
        self.cfi_icall
            || self.cfi_vcall
            || self.cfi_nvcall
            || self.cfi_derived_cast
            || self.cfi_unrelated_cast
            || self.cfi_mfcall
    }

    /// CFI 间接调用检查 (cfi-icall / cfi-mfcall)。
    pub fn needs_cfi_icall(&self) -> bool {
        self.cfi_icall || self.cfi_mfcall
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
    /// 目标架构 (影响 KASAN shadow 偏移)。
    arch: Arch,
    /// KCOV 合并: always_inline 函数名集合。
    inlined_funcs: HashSet<String>,
    /// KCOV 合并: 函数名 -> 调用者函数名。
    inline_callers: HashMap<String, String>,
    /// KCOV 合并: 已分配 guard 的函数 -> (数组名, base_id)。
    func_guard_map: HashMap<String, (String, u32)>,
}

impl InstrumentCtx {
    fn new(arch: Arch) -> Self {
        Self {
            tu_init_pending: false,
            guard_counter: 0,
            next_guard_id: 0,
            arch,
            inlined_funcs: HashSet::new(),
            inline_callers: HashMap::new(),
            func_guard_map: HashMap::new(),
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
/// `arch` 用于 KASAN-kernel shadow 偏移选择与 HWASAN 内联 tag 检查。
pub fn instrument_program(prog: &mut Program, flags: SanitizerFlags, arch: Arch) {
    if !flags.any() {
        return;
    }
    let mut ctx = InstrumentCtx {
        tu_init_pending: flags.needs_tu_init(),
        ..InstrumentCtx::new(arch)
    };
    // KCOV 跨函数 inline 合并: 预扫描识别 always_inline 函数
    // 及其调用者, 以便在插桩时为内联函数复用调用者的 guard 数组。
    if flags.needs_edge_coverage() {
        build_inline_map(prog, &mut ctx);
    }
    // 两遍处理: 第一遍处理非内联函数, 第二遍处理内联函数。
    // 这样内联函数可以找到调用者已分配的 guard 数组。
    for decl in prog.decls.iter_mut() {
        if let Decl::Func(f) = decl {
            if !ctx.inlined_funcs.contains(f.name.name.as_str()) {
                instrument_func(f, flags, &mut ctx);
            }
        }
    }
    for decl in prog.decls.iter_mut() {
        if let Decl::Func(f) = decl {
            if ctx.inlined_funcs.contains(f.name.name.as_str()) {
                instrument_func(f, flags, &mut ctx);
            }
        }
    }
}

/// 预扫描: 收集 always_inline / flatten 函数名及其调用者映射。
/// 当函数 B 标记为 always_inline 且仅被 A 调用时,
/// B 的 guard 数组可合并到 A 的数组中 (KCOV 跨函数 inline 合并)。
fn build_inline_map(prog: &Program, ctx: &mut InstrumentCtx) {
    let mut inline_set: HashSet<String> = HashSet::new();
    for decl in &prog.decls {
        if let Decl::Func(f) = decl {
            if (f.is_always_inline || f.is_flatten) && f.body.is_some() {
                inline_set.insert(f.name.name.clone());
            }
        }
    }
    if inline_set.is_empty() {
        return;
    }
    let mut caller_count: HashMap<String, Vec<String>> = HashMap::new();
    for decl in &prog.decls {
        if let Decl::Func(f) = decl {
            if f.body.is_none() {
                continue;
            }
            let mut callees: HashSet<String> = HashSet::new();
            collect_func_calls(f, &mut callees);
            for callee in &callees {
                if inline_set.contains(callee) {
                    caller_count
                        .entry(callee.clone())
                        .or_default()
                        .push(f.name.name.clone());
                }
            }
        }
    }
    // 仅当 inline 函数只有一个调用者时才合并
    for (func, callers) in &caller_count {
        if callers.len() == 1 {
            ctx.inlined_funcs.insert(func.clone());
            ctx.inline_callers
                .insert(func.clone(), callers[0].clone());
        }
    }
}

/// 收集函数体中的所有直接调用目标名。
fn collect_func_calls(func: &FuncDecl, callees: &mut HashSet<String>) {
    if let Some(body) = &func.body {
        collect_calls_in_block(body, callees);
    }
}

fn collect_calls_in_block(block: &Block, callees: &mut HashSet<String>) {
    for item in &block.items {
        match item {
            BlockItem::Stmt(s) => collect_calls_in_stmt(s, callees),
            BlockItem::Decl(Decl::Func(f)) => {
                if let Some(body) = &f.body {
                    collect_calls_in_block(body, callees);
                }
            }
            _ => {}
        }
    }
}

fn collect_calls_in_stmt(stmt: &Stmt, callees: &mut HashSet<String>) {
    match stmt {
        Stmt::Expr { e, .. } | Stmt::Return { val: Some(e), .. } => {
            collect_calls_in_expr(e, callees);
        }
        Stmt::If { cond, then_, else_, .. } => {
            collect_calls_in_expr(cond, callees);
            collect_calls_in_stmt(then_, callees);
            if let Some(e) = else_ {
                collect_calls_in_stmt(e, callees);
            }
        }
        Stmt::While { cond, body, .. } | Stmt::Do { body, cond, .. } => {
            collect_calls_in_expr(cond, callees);
            collect_calls_in_stmt(body, callees);
        }
        Stmt::For { init, cond, step, body, .. } => {
            if let Some(i) = init {
                collect_calls_in_stmt(i, callees);
            }
            if let Some(c) = cond {
                collect_calls_in_expr(c, callees);
            }
            if let Some(s) = step {
                collect_calls_in_expr(s, callees);
            }
            collect_calls_in_stmt(body, callees);
        }
        Stmt::Block { b, .. } => collect_calls_in_block(b, callees),
        Stmt::Switch { expr, cases, .. } => {
            collect_calls_in_expr(expr, callees);
            for c in cases {
                collect_calls_in_block(&c.body, callees);
            }
        }
        Stmt::Label { stmt, .. } => collect_calls_in_stmt(stmt, callees),
        _ => {}
    }
}

fn collect_calls_in_expr(expr: &Expr, callees: &mut HashSet<String>) {
    match expr {
        Expr::Call { callee, args, .. } => {
            if let Expr::Ident { name, .. } = callee.as_ref() {
                callees.insert(name.name.clone());
            }
            for a in args {
                collect_calls_in_expr(a, callees);
            }
        }
        Expr::Binop { l, r, .. } => {
            collect_calls_in_expr(l, callees);
            collect_calls_in_expr(r, callees);
        }
        Expr::Unary { e, .. } | Expr::AddrOf { e, .. } | Expr::Deref { e, .. }
        | Expr::Cast { e, .. } | Expr::Real { e, .. } | Expr::Imag { e, .. } => {
            collect_calls_in_expr(e, callees);
        }
        Expr::Cond { cond, then_, else_, .. } => {
            collect_calls_in_expr(cond, callees);
            collect_calls_in_expr(then_, callees);
            collect_calls_in_expr(else_, callees);
        }
        Expr::Comma { l, r, .. } => {
            collect_calls_in_expr(l, callees);
            collect_calls_in_expr(r, callees);
        }
        Expr::Index { arr, idx, .. } => {
            collect_calls_in_expr(arr, callees);
            collect_calls_in_expr(idx, callees);
        }
        Expr::Member { expr, .. } => collect_calls_in_expr(expr, callees),
        Expr::Assign { l, r, .. } => {
            collect_calls_in_expr(l, callees);
            collect_calls_in_expr(r, callees);
        }
        Expr::PreInc { e, .. } | Expr::PreDec { e, .. }
        | Expr::PostInc { e, .. } | Expr::PostDec { e, .. } => {
            collect_calls_in_expr(e, callees);
        }
        Expr::StmtExpr { block, .. } => collect_calls_in_block(block, callees),
        Expr::InitList { inits, .. } | Expr::CompoundLit { inits, .. } => {
            for init in inits {
                match init {
                    InitItem::Expr(e) => collect_calls_in_expr(e, callees),
                    InitItem::Designated { value, .. } => {
                        collect_calls_in_expr(value, callees)
                    }
                }
            }
        }
        _ => {}
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
            cov_trace_cmp: false,
            cov_trace_switch: false,
            cov_8bit_counters: func_flags.cov_8bit_counters,
            cov_pc_table: func_flags.cov_pc_table,
            fuzzer: false,
            msan: func_flags.msan,
            dfsan: false,
            cfi_icall: false,
            cfi_vcall: false,
            cfi_nvcall: false,
            cfi_derived_cast: false,
            cfi_unrelated_cast: false,
            cfi_mfcall: false,
        };
        prologue.extend(tu_init_calls(pending_func_init, &span));
        // 一旦任何函数接受了 TU init,就清除 pending。
        // 简化:对每个通道,pending 由本函数判断是否还能由更靠前的函数消化;
        // 当前实现:第一次有体函数消化所有 pending (够用 - 真实场景下
        // 入口函数一般不会 excluded)。
        ctx.tu_init_pending = false;
    }

    // KCOV/Fuzzer 边覆盖:声明每函数 guard 计数器 + TU 全局索引数组
    // KCOV 合并: 内联函数复用调用者的 guard 数组, 不单独分配。
    let func_name = func.name.name.clone();
    let is_inlined = ctx.inlined_funcs.contains(func_name.as_str());
    let (guard_var, func_guard_base) = if func_flags.needs_edge_coverage() {
        if is_inlined {
            // 内联函数: 复用调用者的 guard 数组
            if let Some(caller) = ctx.inline_callers.get(&func_name).cloned() {
                if let Some((arr, _base)) = ctx.func_guard_map.get(&caller).cloned() {
                    let inline_base = ctx.next_guard();
                    ctx.func_guard_map
                        .insert(func_name, (arr.clone(), inline_base));
                    (Some(arr), inline_base)
                } else {
                    // 调用者未找到 guard, 降级为独立分配
                    let name = ctx.alloc_guard_var();
                    prologue.push(guard_array_decl(&name, &span));
                    let g = ctx.next_guard();
                    prologue.push(guard_init(&g, &span));
                    ctx.func_guard_map.insert(func_name, (name.clone(), g));
                    (Some(name), g)
                }
            } else {
                let name = ctx.alloc_guard_var();
                prologue.push(guard_array_decl(&name, &span));
                let g = ctx.next_guard();
                prologue.push(guard_init(&g, &span));
                ctx.func_guard_map.insert(func_name, (name.clone(), g));
                (Some(name), g)
            }
        } else {
            let name = ctx.alloc_guard_var();
            prologue.push(guard_array_decl(&name, &span));
            let g = ctx.next_guard();
            prologue.push(guard_init(&g, &span));
            ctx.func_guard_map.insert(func_name, (name.clone(), g));
            (Some(name), g)
        }
    } else {
        (None, 0)
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
            ExprKind::Deref(ty.size().max(1), e.as_ref().clone())
        }
        Expr::Index { arr, idx, ty, .. } => {
            instrument_expr(arr, flags, ctx, guard_var, guard_base);
            instrument_expr(idx, flags, ctx, guard_var, guard_base);
            ExprKind::Index(ty.size().max(1), arr.as_ref().clone())
        }
        Expr::Member {
            expr: base,
            ty,
            is_arrow,
            ..
        } => {
            instrument_expr(base, flags, ctx, guard_var, guard_base);
            if *is_arrow {
                ExprKind::ArrowMember(ty.size().max(1), base.as_ref().clone())
            } else {
                ExprKind::Trivial
            }
        }
        Expr::Call { callee, args, .. } => {
            instrument_expr(callee, flags, ctx, guard_var, guard_base);
            for a in args.iter_mut() {
                instrument_expr(a, flags, ctx, guard_var, guard_base);
            }
            // CFI: detect indirect call (callee is not a direct Ident)
            if flags.needs_cfi_icall() && !matches!(callee.as_ref(), Expr::Ident { .. }) {
                ExprKind::IndirectCall(callee.ty())
            } else {
                ExprKind::Trivial
            }
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
            // ++/-- 是读-改-写: 先对操作数走 load 检查 (instrument_expr),
            // 再为内存目标补一个 store 检查 (地址取插桩前的 lvalue)。
            let target_kind = if flags.needs_load_store_check() || flags.needs_kcsan_check() {
                classify_store_target(e)
            } else {
                None
            };
            let lhs = e.as_ref().clone();
            instrument_expr(e, flags, ctx, guard_var, guard_base);
            ExprKind::IncDec(target_kind, Box::new(lhs))
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
        Expr::String { .. }
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
        ExprKind::IndirectCall(ref callee_ty) => {
            // CFI indirect call type check: wrap callee with
            // `__cfi_check(callee, type_hash)` before the call executes.
            if let Expr::Call { callee, .. } = expr {
                let type_hash = cfi_type_hash(callee_ty);
                let wrapped_callee = wrap_with_cfi_check(callee, type_hash, &span);
                *callee = Box::new(wrapped_callee);
            }
        }
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
                let wrapped = wrap_with_ubsan_check(expr, "negate_overflow", &span);
                *expr = wrapped;
            }
        }
        ExprKind::Deref(sz, ptr) => {
            if flags.needs_load_store_check() || flags.needs_kcsan_check() {
                let wrapped = wrap_with_mem_check(expr, MemCheckKind::Load(sz), &span, flags, ctx.arch);
                *expr = wrapped;
            }
            if flags.needs_msan_check() {
                let wrapped = wrap_with_msan_load_check(expr, sz, &span);
                *expr = wrapped;
            }
            if flags.needs_dfsan_check() {
                let wrapped = wrap_with_dfsan_load_label(expr, sz, &span);
                *expr = wrapped;
            }
            if flags.needs_ubsan() {
                let align = expr.ty().align();
                let wrapped = wrap_with_null_check(expr, &ptr, align, &span);
                *expr = wrapped;
            }
        }
        ExprKind::Index(sz, ptr) => {
            if flags.needs_load_store_check() || flags.needs_kcsan_check() {
                let wrapped = wrap_with_mem_check(expr, MemCheckKind::Load(sz), &span, flags, ctx.arch);
                *expr = wrapped;
            }
            if flags.needs_msan_check() {
                let wrapped = wrap_with_msan_load_check(expr, sz, &span);
                *expr = wrapped;
            }
            if flags.needs_dfsan_check() {
                let wrapped = wrap_with_dfsan_load_label(expr, sz, &span);
                *expr = wrapped;
            }
            if flags.needs_ubsan() {
                let align = expr.ty().align();
                let wrapped = wrap_with_null_check(expr, &ptr, align, &span);
                *expr = wrapped;
            }
        }
        ExprKind::ArrowMember(sz, ptr) => {
            if flags.needs_load_store_check() || flags.needs_kcsan_check() {
                let wrapped = wrap_with_mem_check(expr, MemCheckKind::Load(sz), &span, flags, ctx.arch);
                *expr = wrapped;
            }
            if flags.needs_msan_check() {
                let wrapped = wrap_with_msan_load_check(expr, sz, &span);
                *expr = wrapped;
            }
            if flags.needs_dfsan_check() {
                let wrapped = wrap_with_dfsan_load_label(expr, sz, &span);
                *expr = wrapped;
            }
            if flags.needs_ubsan() {
                let align = expr.ty().align();
                let wrapped = wrap_with_null_check(expr, &ptr, align, &span);
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
                let wrapped = wrap_with_mem_check_assign(&l, expr, check_kind, &span, flags, ctx.arch);
                *expr = wrapped;
            }
            // MSAN store check on assign target
            if flags.needs_msan_check() {
                if let Expr::Assign { l, .. } = expr {
                    let store_sz = l.ty().size().max(1);
                    let lhs_for_msan = l.as_ref().clone();
                    let wrapped = wrap_with_msan_store_check(&lhs_for_msan, expr, store_sz, &span);
                    *expr = wrapped;
                }
            }
            // DFSan store label on assign target
            if flags.needs_dfsan_check() {
                if let Expr::Assign { l, .. } = expr {
                    let store_sz = l.ty().size().max(1);
                    let lhs_for_dfsan = l.as_ref().clone();
                    let wrapped = wrap_with_dfsan_store_label(&lhs_for_dfsan, expr, store_sz, &span);
                    *expr = wrapped;
                }
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
        ExprKind::IncDec(Some(check_kind), lhs) => {
            // 对 ++/-- 内存目标 (Deref/Index/ArrowMember) 补 store 检查:
            // 用插桩前 lvalue 的地址作入参, 检查在自增/自减求值前执行。
            let wrapped = wrap_with_mem_check_assign(&lhs, expr, check_kind, &span, flags, ctx.arch);
            *expr = wrapped;
        }
        ExprKind::IncDec(None, _) => {}
    }
}

#[derive(Debug, Clone)]
enum ExprKind {
    Trivial,
    Binop(BinOp),
    Unary(UnOp, Type),
    Deref(usize, Expr),
    Index(usize, Expr),
    ArrowMember(usize, Expr),
    Assign(Option<MemCheckKind>),
    IncDec(Option<MemCheckKind>, Box<Expr>),
    /// Indirect call through function pointer — CFI type check.
    IndirectCall(Type),
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
                    let kind = match op {
                        BinOp::Add => "add_overflow",
                        BinOp::Sub => "sub_overflow",
                        BinOp::Mul => "mul_overflow",
                        _ => unreachable!(),
                    };
                    let wrapped = wrap_with_ubsan_check_arg(expr, l, kind, &span);
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
            ty: Type::Func(Box::new(Type::Void.into()), vec![bound_ty.clone().into()]),
        }),
        args: vec![*bound],
        span: span.clone(),
        ty: Type::Void,
    }
}

/// 生成 `__ubsan_handle_type_mismatch_v1(0)` 调用表达式。
/// libubsan 没有独立 null-deref 入口；空指针解引用通过 type_mismatch_v1 report。
fn null_deref_check(span: &Span) -> Expr {
    Expr::Call {
        callee: Box::new(Expr::Ident {
            name: Ident {
                name: "__ubsan_handle_type_mismatch_v1".to_string(),
                span: span.clone(),
            },
            span: span.clone(),
            ty: Type::Func(Box::new(Type::Void.into()), vec![Type::Ptr(Box::new(Type::Void.into())).into()]),
        }),
        args: vec![Expr::Int {
            v: 0,
            span: span.clone(),
            ty: Type::Int,
        }],
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
                Box::new(Type::Void.into()),
                vec![val_ty.into(), target_ty.clone().into(), target_ty.clone().into()],
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
                Box::new(Type::Void.into()),
                vec![Type::Ptr(Box::new(Type::Void.into())).into(), inner_ty.clone().into()],
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
                Box::new(Type::Void.into()),
                vec![Type::Ptr(Box::new(Type::Void.into())).into(), inner_ty.clone().into()],
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
    arch: Arch,
) -> Expr {
    let inner_ty = expr.ty();
    // AddrOf(&expr) → codegen 支持 Deref/Index/Member/Ident,
    // 返回 lvalue 地址,不读取。
    let addr_expr = Expr::AddrOf {
        e: Box::new(expr.clone()),
        span: span.clone(),
        ty: Type::Ptr(Box::new(inner_ty.clone().into())),
    };
    let size = match kind {
        MemCheckKind::Load(sz) | MemCheckKind::Store(sz) => sz,
    };
    let is_write = matches!(kind, MemCheckKind::Store(_));
    let checks = make_mem_check_calls(addr_expr, size, is_write, span, flags, arch);
    chain_mem_checks(checks, expr.clone(), span, inner_ty)
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
    arch: Arch,
) -> Expr {
    let inner_ty = assign.ty();
    let addr_expr = Expr::AddrOf {
        e: Box::new(lhs.clone()),
        span: span.clone(),
        ty: Type::Ptr(Box::new(inner_ty.clone().into())),
    };
    let size = match kind {
        MemCheckKind::Load(sz) | MemCheckKind::Store(sz) => sz,
    };
    let is_write = matches!(kind, MemCheckKind::Store(_));
    let checks = make_mem_check_calls(addr_expr, size, is_write, span, flags, arch);
    chain_mem_checks(checks, assign.clone(), span, inner_ty)
}

/// 把若干顺序执行的检查调用链成 `(c1, (c2, ... (cN, inner)))`。
fn chain_mem_checks(checks: Vec<Expr>, inner: Expr, span: &Span, ty: Type) -> Expr {
    let mut acc = inner;
    for c in checks.into_iter().rev() {
        acc = Expr::Comma {
            l: Box::new(c),
            r: Box::new(acc),
            span: span.clone(),
            ty: ty.clone(),
        };
    }
    acc
}

/// 根据当前启用的 sanitizer 生成内存访问检查调用列表 (按序执行)。
///
/// - ThreadSanitizer user mode: `__tsan_readN/writeN(addr)` or
///   `__tsan_read_range/write_range(addr, size)`.
/// - KCSAN kernel mode: `__kcsan_check_access(addr, size, is_write)`.
/// - KASAN/HWASAN user mode: `__asan_loadN` / `__asan_storeN` 运行时函数。
/// - KASAN kernel mode: 内联 shadow 地址计算 (shadow = (addr >> 3) + offset)。
fn make_mem_check_calls(
    addr: Expr,
    size: usize,
    is_write: bool,
    span: &Span,
    flags: SanitizerFlags,
    arch: Arch,
) -> Vec<Expr> {
    let mut out = Vec::new();
    if flags.kcsan {
        if flags.kernel_mode {
            out.push(make_kcsan_check_call(addr.clone(), size, is_write, span));
        } else {
            out.push(make_tsan_check_call(addr.clone(), size, is_write, span));
        }
    }
    if flags.kasan || flags.hwasan {
        if flags.kasan && flags.kernel_mode {
            // 内核模式 KASAN: 内联 shadow 地址计算 (架构感知)
            out.push(make_kernel_kasan_check(addr, size, is_write, span, arch));
        } else if flags.hwasan && flags.kernel_mode {
            // 内核模式 HWASAN: 内联 tag 检查 (AArch64 MTE 软件 tag)
            out.push(make_kernel_hwasan_check(addr, size, is_write, span));
        } else {
            // 用户态 KASAN/HWASAN: 调运行时函数
            out.push(make_asan_call(addr, size, is_write, span, flags));
        }
    }
    out
}

/// libtsan-compatible memory access callback.
fn make_tsan_check_call(addr: Expr, size: usize, is_write: bool, span: &Span) -> Expr {
    let fn_name = match (is_write, size) {
        (false, 1) => "__tsan_read1",
        (false, 2) => "__tsan_read2",
        (false, 4) => "__tsan_read4",
        (false, 8) => "__tsan_read8",
        (false, 16) => "__tsan_read16",
        (false, _) => "__tsan_read_range",
        (true, 1) => "__tsan_write1",
        (true, 2) => "__tsan_write2",
        (true, 4) => "__tsan_write4",
        (true, 8) => "__tsan_write8",
        (true, 16) => "__tsan_write16",
        (true, _) => "__tsan_write_range",
    };
    let args = if matches!(size, 1 | 2 | 4 | 8 | 16) {
        vec![addr]
    } else {
        vec![
            addr,
            Expr::Int {
                v: size as i128,
                span: span.clone(),
                ty: Type::ULong,
            },
        ]
    };
    let param_tys = if matches!(size, 1 | 2 | 4 | 8 | 16) {
        vec![Type::Ptr(Box::new(Type::Void.into())).into()]
    } else {
        vec![Type::Ptr(Box::new(Type::Void.into())).into(), Type::ULong.into()]
    };
    Expr::Call {
        callee: Box::new(Expr::Ident {
            name: Ident {
                name: fn_name.to_string(),
                span: span.clone(),
            },
            span: span.clone(),
            ty: Type::Func(Box::new(Type::Void.into()), param_tys.into_iter().map(|p: Type| p.into()).collect::<Vec<QualType>>()),
        }),
        args,
        span: span.clone(),
        ty: Type::Void,
    }
}

/// `__kcsan_check_access(addr, size, is_write)` 三参调用
/// (LLVM runtime 签名: const volatile void*, size_t, int)。
fn make_kcsan_check_call(addr: Expr, size: usize, is_write: bool, span: &Span) -> Expr {
    Expr::Call {
        callee: Box::new(Expr::Ident {
            name: Ident {
                name: "__kcsan_check_access".to_string(),
                span: span.clone(),
            },
            span: span.clone(),
            ty: Type::Func(
                Box::new(Type::Void.into()),
                vec![Type::Ptr(Box::new(Type::Void.into())).into(), Type::ULong.into(), Type::Int.into()],
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
    }
}

/// `__asan_loadN/storeN(addr[, size])` 运行时调用。
/// N in {1,2,4,8,16} 使用固定大小接口；其他大小使用
/// `__asan_loadN(addr, size_t)` / `__asan_storeN(addr, size_t)` 通用接口。
///
/// 5.2 注: 16 字节以上聚合类型 (struct/array > 16B) 正确回退到通用
/// `__asan_loadN`/`__asan_storeN` 接口，这是 libasan/compiler-rt 的
/// 标准行为。固定大小接口 {1,2,4,8,16} 由运行时直接提供；其他大小
/// 走通用 N 接口并携带 size_t 第二参。不需要为每个大小生成专用代码。
fn make_asan_call(
    addr: Expr,
    size: usize,
    is_write: bool,
    span: &Span,
    flags: SanitizerFlags,
) -> Expr {
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
    // libasan/hwasan 只提供固定大小 {1,2,4,8,16} 单参接口；
    // 其他大小走通用 N 接口并携带 size_t 第二参。
    let has_fixed_size_runtime = matches!(size, 1 | 2 | 4 | 8 | 16);
    let args = if has_fixed_size_runtime {
        vec![addr]
    } else {
        vec![
            addr,
            Expr::Int {
                v: size as i128,
                span: span.clone(),
                ty: Type::ULong,
            },
        ]
    };
    let param_tys = if has_fixed_size_runtime {
        vec![Type::Ptr(Box::new(Type::Void.into())).into()]
    } else {
        vec![Type::Ptr(Box::new(Type::Void.into())).into(), Type::ULong.into()]
    };
    Expr::Call {
        callee: Box::new(Expr::Ident {
            name: Ident {
                name: fn_name,
                span: span.clone(),
            },
            span: span.clone(),
            ty: Type::Func(Box::new(Type::Void.into()), param_tys.into_iter().map(|p: Type| p.into()).collect::<Vec<QualType>>()),
        }),
        args,
        span: span.clone(),
        ty: Type::Void,
    }
}

/// KASAN-kernel shadow 偏移量按架构选择。
///
/// 每个架构的内核虚拟地址空间布局不同, shadow 基址也不同:
/// - x86_64:     `(addr >> 3) + 0xdffffc0000000000`
/// - AArch64:    `(addr >> 3) + 0xdfff600000000000` (VA_BITS=48)
/// - RISC-V 64:  `(addr >> 3) + 0xdffffe0000000000` (SV39/SV48)
/// - 其他:        回退到 x86_64 偏移
pub fn kasan_shadow_offset_for_arch(arch: Arch) -> i128 {
    match arch {
        Arch::X86_64 => 0xdffffc0000000000u64 as i128,
        Arch::AArch64 => 0xdfff600000000000u64 as i128,
        Arch::RiscV64 => 0xdffffe0000000000u64 as i128,
        _ => 0xdffffc0000000000u64 as i128,
    }
}

/// 内核模式 KASAN 内联 shadow 检查。
///
/// 每个 shadow 字节覆盖 8 字节 granule:
/// - `0`: 整 granule 可访问
/// - `1..7`: 仅前 N 字节可访问
/// - `>= 128` (即负的 signed 值): 已毒化
///
/// 对访问 `[addr, addr+size)` (size 为编译期常量), 记
/// `need = (addr & 7) + size` (以 granule 首字节为原点的末偏移),
/// granule g 的 shadow 值 `v_g = *(u8*)(shadow + (addr >> 3) + g)`:
/// `poisoned_g = (need > 8g) && (v_g >= 128 || (v_g > 0 && need - 8g > v_g))`。
/// (`v_g >= 128` 即负值毒化;`v_g > 0` 且覆盖超过可达前缀即部分毒化越界)
/// 任一 granule 毒化即调 `__asan_report_loadN/storeN(addr)`。
///
/// shadow 偏移按架构选择:
/// - x86_64:     `0xdffffc0000000000`
/// - AArch64:    `0xdfff600000000000` (VA_BITS=48)
/// - RISC-V 64:  `0xdffffe0000000000` (SV39/SV48)
/// - 其他:        回退到 x86_64 偏移
fn make_kernel_kasan_check(addr: Expr, size: usize, is_write: bool, span: &Span, arch: Arch) -> Expr {
    let shadow_offset = kasan_shadow_offset_for_arch(arch);

    let ul = |v: i128| Expr::Int {
        v,
        span: span.clone(),
        ty: Type::ULong,
    };
    let bop = |op: BinOp, l: Expr, r: Expr, ty: Type| Expr::Binop {
        op,
        l: Box::new(l),
        r: Box::new(r),
        span: span.clone(),
        ty,
    };
    let cmp = |op: BinOp, l: Expr, r: Expr| bop(op, l, r, Type::Int);

    // 统一把地址转成无符号整型再参与位运算 (codegen 对同尺寸 cast 是 no-op)
    let addr_u = Expr::Cast {
        e: Box::new(addr.clone()),
        ty: Type::ULong,
        span: span.clone(),
    };
    // shadow_addr = (addr >> 3) + KASAN_SHADOW_OFFSET
    let shadow_idx = bop(BinOp::Shr, addr_u.clone(), ul(3), Type::ULong);
    let shadow_base = bop(BinOp::Add, shadow_idx, ul(shadow_offset), Type::ULong);
    // need = (addr & 7) + size
    let need = bop(
        BinOp::Add,
        bop(BinOp::And, addr_u, ul(7), Type::ULong),
        ul(size as i128),
        Type::ULong,
    );

    // 访问可能触及的 granule 数: need 最大 size + 7
    let granules = (size + 14) / 8;
    let mut poisoned: Option<Expr> = None;
    for g in 0..granules {
        // 读取 granule g 的 shadow 字节 (零扩展, 值域 0..=255)
        let vg = Expr::Deref {
            e: Box::new(Expr::Cast {
                e: Box::new(bop(
                    BinOp::Add,
                    shadow_base.clone(),
                    ul(g as i128),
                    Type::ULong,
                )),
                ty: Type::Ptr(Box::new(Type::UChar.into())),
                span: span.clone(),
            }),
            span: span.clone(),
            ty: Type::UChar,
        };
        // 本 granule 毒化判定:
        //   v >= 128 (负值) 或 (v > 0 && need - 8g > v)
        let pg = cmp(
            BinOp::OrOr,
            cmp(BinOp::Gt, vg.clone(), ul(127)),
            cmp(
                BinOp::AndAnd,
                cmp(BinOp::Gt, vg.clone(), ul(0)),
                cmp(
                    BinOp::Gt,
                    bop(BinOp::Sub, need.clone(), ul((8 * g) as i128), Type::ULong),
                    Expr::Cast {
                        e: Box::new(vg),
                        ty: Type::ULong,
                        span: span.clone(),
                    },
                ),
            ),
        );
        // 仅当 need > 8g 时 granule g 被触及 (g=0 恒触及, 跳过条件)
        let term = if g == 0 {
            pg
        } else {
            cmp(
                BinOp::AndAnd,
                cmp(BinOp::Gt, need.clone(), ul((8 * g) as i128)),
                pg,
            )
        };
        poisoned = Some(match poisoned {
            None => term,
            Some(acc) => cmp(BinOp::OrOr, acc, term),
        });
    }

    let report_name = format!(
        "__asan_report_{}{}",
        if is_write { "store" } else { "load" },
        size
    );
    let report_call = Expr::Call {
        callee: Box::new(Expr::Ident {
            name: Ident {
                name: report_name,
                span: span.clone(),
            },
            span: span.clone(),
            ty: Type::Func(Box::new(Type::Void.into()), vec![Type::Ptr(Box::new(Type::Void.into())).into()]),
        }),
        args: vec![addr],
        span: span.clone(),
        ty: Type::Void,
    };

    // poisoned ? report_call(addr) : 0
    Expr::Cond {
        cond: Box::new(poisoned.unwrap_or_else(|| Expr::Int {
            v: 0,
            span: span.clone(),
            ty: Type::Int,
        })),
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

/// 内核模式 HWASAN 内联 tag 检查 (AArch64 MTE 软件 tag 模式)。
///
/// HWASAN 使用指针顶字节作为 tag (4 bit, 存储在 bits 56..59),
/// 内核模式通过内联代码检查指针 tag 与内存 tag 是否匹配:
/// 1. 提取指针 tag: `(addr >> 56) & 0xF`
/// 2. 读取内存 tag: `*(u8*)((addr & 0x00FFFFFFFFFFFFFF) >> 4)`
///    (内存 tag 存储在每 16 字节 granule 的独立 tag 内存中)
/// 3. 比较: 不匹配则调 `__hwasan_tag_mismatch{size}(addr, size, is_write)`
///
/// 这与 KASAN-kernel 的 shadow 检查不同:
/// - KASAN: 检查 shadow 字节是否毒化 (越界/UAF)
/// - HWASAN: 检查指针 tag 与内存 tag 是否匹配 (类型安全/Use-after-free)
fn make_kernel_hwasan_check(addr: Expr, size: usize, is_write: bool, span: &Span) -> Expr {
    let ul = |v: i128| Expr::Int {
        v,
        span: span.clone(),
        ty: Type::ULong,
    };
    let bop = |op: BinOp, l: Expr, r: Expr, ty: Type| Expr::Binop {
        op,
        l: Box::new(l),
        r: Box::new(r),
        span: span.clone(),
        ty,
    };
    let cmp = |op: BinOp, l: Expr, r: Expr| bop(op, l, r, Type::Int);

    // 统一把地址转成无符号整型
    let addr_u = Expr::Cast {
        e: Box::new(addr.clone()),
        ty: Type::ULong,
        span: span.clone(),
    };

    // 提取指针 tag: (addr >> 56) & 0xF
    let ptr_tag = bop(
        BinOp::And,
        bop(BinOp::Shr, addr_u.clone(), ul(56), Type::ULong),
        ul(0xF),
        Type::ULong,
    );

    // 读取内存 tag: *(u8*)(untagged_addr >> 4)
    // untagged_addr = addr & 0x00FFFFFFFFFFFFFF
    let untagged = bop(
        BinOp::And,
        addr_u.clone(),
        ul(0x00FFFFFFFFFFFFFFu64 as i128),
        Type::ULong,
    );
    let tag_addr = bop(BinOp::Shr, untagged, ul(4), Type::ULong);
    let mem_tag = Expr::Deref {
        e: Box::new(Expr::Cast {
            e: Box::new(tag_addr),
            ty: Type::Ptr(Box::new(Type::UChar.into())),
            span: span.clone(),
        }),
        span: span.clone(),
        ty: Type::UChar,
    };

    // 比较: ptr_tag != (mem_tag & 0xF)
    let mem_tag_u = Expr::Cast {
        e: Box::new(mem_tag),
        ty: Type::ULong,
        span: span.clone(),
    };
    let mem_tag_trimmed = bop(BinOp::And, mem_tag_u, ul(0xF), Type::ULong);
    let tag_mismatch = cmp(BinOp::Ne, ptr_tag, mem_tag_trimmed);

    // 报告函数名: 区分 size
    let report_name = format!("__hwasan_tag_mismatch{}", size);
    let report_call = Expr::Call {
        callee: Box::new(Expr::Ident {
            name: Ident {
                name: report_name,
                span: span.clone(),
            },
            span: span.clone(),
            ty: Type::Func(
                Box::new(Type::Void.into()),
                vec![
                    Type::Ptr(Box::new(Type::Void.into())).into(),
                    Type::ULong.into(),
                    Type::Int.into(),
                ],
            ),
        }),
        args: vec![
            addr,
            ul(size as i128),
            Expr::Int {
                v: if is_write { 1 } else { 0 },
                span: span.clone(),
                ty: Type::Int,
            },
        ],
        span: span.clone(),
        ty: Type::Void,
    };

    // tag_mismatch ? report_call(addr, size, is_write) : 0
    Expr::Cond {
        cond: Box::new(tag_mismatch),
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

/// 生成 `__ubsan_handle_type_mismatch_v1(ptr)` 对齐违规调用表达式。
/// 当指针地址不满足类型对齐要求时由 runtime 报告。
/// 与 null_deref_check 共用同一 handler (GCC/LLVM UBSAN 约定)。
fn alignment_check_handler(ptr: &Expr, span: &Span) -> Expr {
    Expr::Call {
        callee: Box::new(Expr::Ident {
            name: Ident {
                name: "__ubsan_handle_type_mismatch_v1".to_string(),
                span: span.clone(),
            },
            span: span.clone(),
            ty: Type::Func(
                Box::new(Type::Void.into()),
                vec![Type::Ptr(Box::new(Type::Void.into())).into()],
            ),
        }),
        args: vec![ptr.clone()],
        span: span.clone(),
        ty: Type::Void,
    }
}

/// 把 `expr` 包成 `(ptr == 0 ? null_check : 0, expr)`。
/// 当 `align > 1` 时额外插入对齐检查: 若指针地址不满足类型
/// 对齐要求, 调用 `__ubsan_handle_type_mismatch_v1(ptr)`。
/// align > 1 时对齐检查覆盖非空指针的对齐违规; null check
/// 单独覆盖空指针解引用 (与 GCC UBSAN 行为一致)。
fn wrap_with_null_check(expr: &Expr, ptr: &Expr, align: usize, span: &Span) -> Expr {
    let inner_ty = expr.ty();
    // 基础 null check: ptr == 0 ? handler : 0
    let null_cond = Expr::Binop {
        op: BinOp::EqEq,
        l: Box::new(ptr.clone()),
        r: Box::new(Expr::Int {
            v: 0,
            span: span.clone(),
            ty: Type::Int,
        }),
        span: span.clone(),
        ty: Type::Int,
    };
    let null_check = Expr::Cond {
        cond: Box::new(null_cond),
        then_: Box::new(null_deref_check(span)),
        else_: Box::new(Expr::Int {
            v: 0,
            span: span.clone(),
            ty: Type::Int,
        }),
        span: span.clone(),
        ty: Type::Int,
    };
    if align <= 1 {
        // 对齐为 1 (char/byte): 无需对齐检查, 仅 null check
        return Expr::Comma {
            l: Box::new(null_check),
            r: Box::new(expr.clone()),
            span: span.clone(),
            ty: inner_ty,
        };
    }
    // align > 1: 先 null check, 再 alignment check
    // (null_check, (align_check, expr))
    let align_mask = (align - 1) as i128;
    let and_expr = Expr::Binop {
        op: BinOp::And,
        l: Box::new(ptr.clone()),
        r: Box::new(Expr::Int {
            v: align_mask,
            span: span.clone(),
            ty: Type::Int,
        }),
        span: span.clone(),
        ty: Type::Int,
    };
    let align_cond = Expr::Binop {
        op: BinOp::Ne,
        l: Box::new(and_expr),
        r: Box::new(Expr::Int {
            v: 0,
            span: span.clone(),
            ty: Type::Int,
        }),
        span: span.clone(),
        ty: Type::Int,
    };
    let align_check = Expr::Cond {
        cond: Box::new(align_cond),
        then_: Box::new(alignment_check_handler(ptr, span)),
        else_: Box::new(Expr::Int {
            v: 0,
            span: span.clone(),
            ty: Type::Int,
        }),
        span: span.clone(),
        ty: Type::Int,
    };
    let combined = Expr::Comma {
        l: Box::new(align_check),
        r: Box::new(expr.clone()),
        span: span.clone(),
        ty: inner_ty.clone(),
    };
    Expr::Comma {
        l: Box::new(null_check),
        r: Box::new(combined),
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
        // 其他大小使用 libasan 的通用 `__asan_loadN(addr, size)`。
        _ => "__asan_loadN".to_string(),
    }
}

fn hwasan_load_fn(sz: usize) -> String {
    match sz {
        1 => "__hwasan_load1".to_string(),
        2 => "__hwasan_load2".to_string(),
        4 => "__hwasan_load4".to_string(),
        8 => "__hwasan_load8".to_string(),
        16 => "__hwasan_load16".to_string(),
        _ => "__hwasan_loadN".to_string(),
    }
}

fn kasan_store_fn(sz: usize) -> String {
    match sz {
        1 => "__asan_store1".to_string(),
        2 => "__asan_store2".to_string(),
        4 => "__asan_store4".to_string(),
        8 => "__asan_store8".to_string(),
        16 => "__asan_store16".to_string(),
        // 其他大小使用 libasan 的通用 `__asan_storeN(addr, size)`。
        _ => "__asan_storeN".to_string(),
    }
}

fn hwasan_store_fn(sz: usize) -> String {
    match sz {
        1 => "__hwasan_store1".to_string(),
        2 => "__hwasan_store2".to_string(),
        4 => "__hwasan_store4".to_string(),
        8 => "__hwasan_store8".to_string(),
        16 => "__hwasan_store16".to_string(),
        _ => "__hwasan_storeN".to_string(),
    }
}

// ── MSAN (MemorySanitizer) helpers ─────────────────────────────────────

/// MSAN load check: `__msan_check_load_N(addr)`.
/// 在内存加载前调用，检查加载的值是否已初始化。
/// 运行时通过 shadow 内存判断：若对应 shadow 字节标记为未初始化，
/// 则报告 use-of-uninitialized-value 错误。
fn wrap_with_msan_load_check(expr: &Expr, size: usize, span: &Span) -> Expr {
    let inner_ty = expr.ty();
    let addr_expr = Expr::AddrOf {
        e: Box::new(expr.clone()),
        span: span.clone(),
        ty: Type::Ptr(Box::new(inner_ty.clone().into())),
    };
    let fn_name = msan_check_fn_name(size, false);
    let call = Expr::Call {
        callee: Box::new(Expr::Ident {
            name: Ident {
                name: fn_name,
                span: span.clone(),
            },
            span: span.clone(),
            ty: Type::Func(Box::new(Type::Void.into()), vec![Type::Ptr(Box::new(Type::Void.into())).into()]),
        }),
        args: vec![addr_expr],
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

/// MSAN store check: `__msan_check_store_N(addr)`.
/// 在内存存储前调用，验证目标地址区域已被正确初始化。
fn wrap_with_msan_store_check(lhs: &Expr, assign: &Expr, size: usize, span: &Span) -> Expr {
    let inner_ty = assign.ty();
    let addr_expr = Expr::AddrOf {
        e: Box::new(lhs.clone()),
        span: span.clone(),
        ty: Type::Ptr(Box::new(lhs.ty().into())),
    };
    let fn_name = msan_check_fn_name(size, true);
    let call = Expr::Call {
        callee: Box::new(Expr::Ident {
            name: Ident {
                name: fn_name,
                span: span.clone(),
            },
            span: span.clone(),
            ty: Type::Func(Box::new(Type::Void.into()), vec![Type::Ptr(Box::new(Type::Void.into())).into()]),
        }),
        args: vec![addr_expr],
        span: span.clone(),
        ty: Type::Void,
    };
    Expr::Comma {
        l: Box::new(call),
        r: Box::new(assign.clone()),
        span: span.clone(),
        ty: inner_ty,
    }
}

/// MSAN 检查函数名映射。
fn msan_check_fn_name(sz: usize, is_store: bool) -> String {
    let prefix = if is_store {
        "__msan_check_store_"
    } else {
        "__msan_check_load_"
    };
    format!("{}{}", prefix, sz)
}

// ── DFSan (DataFlowSanitizer) helpers ──────────────────────────────────

/// DFSan load label: `__dfsan_load_label_N(addr)` 返回 `dfsan_label` (i16)。
/// 在内存加载前调用，获取该内存位置关联的 taint 标签。
/// 标签随后与表达式结果关联，实现数据流追踪。
fn wrap_with_dfsan_load_label(expr: &Expr, size: usize, span: &Span) -> Expr {
    let inner_ty = expr.ty();
    let addr_expr = Expr::AddrOf {
        e: Box::new(expr.clone()),
        span: span.clone(),
        ty: Type::Ptr(Box::new(inner_ty.clone().into())),
    };
    let fn_name = format!("__dfsan_load_label_{}", size);
    let call = Expr::Call {
        callee: Box::new(Expr::Ident {
            name: Ident {
                name: fn_name,
                span: span.clone(),
            },
            span: span.clone(),
            ty: Type::Func(Box::new(Type::Short.into()), vec![Type::Ptr(Box::new(Type::Void.into())).into()]),
        }),
        args: vec![addr_expr],
        span: span.clone(),
        ty: Type::Short,
    };
    // 将标签获取作为副作用，结果仍为原表达式值
    Expr::Comma {
        l: Box::new(Expr::Comma {
            l: Box::new(call),
            r: Box::new(Expr::Int {
                v: 0,
                span: span.clone(),
                ty: Type::Int,
            }),
            span: span.clone(),
            ty: Type::Int,
        }),
        r: Box::new(expr.clone()),
        span: span.clone(),
        ty: inner_ty,
    }
}

/// DFSan store label: `__dfsan_store_label_N(addr, label)`。
/// 在内存存储后调用，将表达式的 taint 标签关联到目标内存位置。
fn wrap_with_dfsan_store_label(lhs: &Expr, assign: &Expr, size: usize, span: &Span) -> Expr {
    let inner_ty = assign.ty();
    let addr_expr = Expr::AddrOf {
        e: Box::new(lhs.clone()),
        span: span.clone(),
        ty: Type::Ptr(Box::new(lhs.ty().into())),
    };
    let fn_name = format!("__dfsan_store_label_{}", size);
    // label arg: 简化为 0 (clean label)；实际标签由运行时 union 传播
    let call = Expr::Call {
        callee: Box::new(Expr::Ident {
            name: Ident {
                name: fn_name,
                span: span.clone(),
            },
            span: span.clone(),
            ty: Type::Func(
                Box::new(Type::Void.into()),
                vec![Type::Ptr(Box::new(Type::Void.into())).into(), Type::Short.into()],
            ),
        }),
        args: vec![
            addr_expr,
            Expr::Int {
                v: 0,
                span: span.clone(),
                ty: Type::Short,
            },
        ],
        span: span.clone(),
        ty: Type::Void,
    };
    Expr::Comma {
        l: Box::new(call),
        r: Box::new(assign.clone()),
        span: span.clone(),
        ty: inner_ty,
    }
}

// ── CFI (Control Flow Integrity) helpers ───────────────────────────────

/// CFI indirect call check: `__cfi_check(callee_ptr, type_hash)`。
/// 在间接调用前插入，验证目标函数签名是否与预期类型匹配。
/// 若不匹配，运行时触发 CFI violation report。
fn wrap_with_cfi_check(callee: &Expr, type_hash: u64, span: &Span) -> Expr {
    let callee_ty = callee.ty();
    // 取 callee 的地址用于检查
    let callee_ptr = Expr::AddrOf {
        e: Box::new(callee.clone()),
        span: span.clone(),
        ty: Type::Ptr(Box::new(callee_ty.clone().into())),
    };
    let call = Expr::Call {
        callee: Box::new(Expr::Ident {
            name: Ident {
                name: "__cfi_check".to_string(),
                span: span.clone(),
            },
            span: span.clone(),
            ty: Type::Func(
                Box::new(Type::Void.into()),
                vec![Type::Ptr(Box::new(Type::Void.into())).into(), Type::ULong.into()],
            ),
        }),
        args: vec![
            callee_ptr,
            Expr::Int {
                v: type_hash as i128,
                span: span.clone(),
                ty: Type::ULong,
            },
        ],
        span: span.clone(),
        ty: Type::Void,
    };
    // `(cfi_check, callee)` — 检查后返回原 callee 值
    Expr::Comma {
        l: Box::new(call),
        r: Box::new(callee.clone()),
        span: span.clone(),
        ty: callee_ty,
    }
}

/// 根据函数类型计算 CFI type hash (FNV-1a 64-bit)。
/// 用于运行时比较 callee 签名是否匹配。
fn cfi_type_hash(ty: &Type) -> u64 {
    // 简化: 使用类型的字符串表示的 FNV-1a hash
    let s = format!("{:?}", ty);
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
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
                ty: Type::Func(Box::new(Type::Void.into()), vec![l_ty.clone().into(), l_ty.clone().into()]),
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
            ty: Type::Func(Box::new(Type::Void.into()), vec![Type::Long.into()]),
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
            Box::new(Type::UInt.into()),
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
        alias_name: None,
        copy_fn: None,
        is_retain: false,
        is_used_attr: false,
        quals: crate::ast::QualifierFlags::NONE,
        addr_space: crate::ast::AddrSpace::None,
        tls_model: None,
        asm_label: None,
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
                    Box::new(Type::Void.into()),
                    vec![Type::UInt.into(), Type::Ptr(Box::new(Type::UInt.into())).into()],
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
                    ty: Type::Ptr(Box::new(Type::UInt.into())),
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
            ty: Type::Func(Box::new(Type::Void.into()), vec![Type::Ptr(Box::new(Type::UInt.into())).into()]),
        }),
        args: vec![Expr::Index {
            arr: Box::new(Expr::Ident {
                name: Ident {
                    name: guard_var.to_string(),
                    span: span.clone(),
                },
                span: span.clone(),
                ty: Type::Array(Box::new(Type::UInt.into()), ArraySize::Const(16)),
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
/// 内核模式 (-fsanitize=kernel-address / kernel-hwaddress / kernel-kcsan)
/// 不发射用户态 asan/hwasan/tsan init: 内核运行时自初始化,
/// 编译器只负责访存检查 (内联 shadow / kcsan callbacks) 与报告。
fn tu_init_calls(flags: SanitizerFlags, span: &Span) -> Vec<BlockItem> {
    let mut out = Vec::new();
    if flags.kasan && !flags.kernel_mode {
        out.push(sanitizer_call("__asan_init", span));
    }
    if flags.hwasan && !flags.kernel_mode {
        out.push(sanitizer_call("__hwasan_init", span));
    }
    if flags.ubsan {
        // libubsan 没有必需的 TU init 入口；各 handler 懒初始化。
    }
    if flags.kcsan {
        if flags.kernel_mode {
            out.push(sanitizer_call("__kcsan_init", span));
        } else {
            out.push(sanitizer_call("__tsan_init", span));
        }
    }
    if flags.cov_8bit_counters {
        out.push(sanitizer_call("__sanitizer_cov_8bit_counters_init", span));
    }
    if flags.cov_pc_table {
        out.push(sanitizer_call("__sanitizer_cov_pcs_init", span));
    }
    if flags.msan {
        out.push(sanitizer_call("__msan_init", span));
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
        if flags.kernel_mode {
            out.push(sanitizer_call("__kcsan_func_entry", span));
        } else {
            out.push(sanitizer_call("__tsan_func_entry", span));
        }
    }
    if flags.msan {
        // MSAN 每函数入口: 通知运行时当前栈帧范围, 以便
        // 将局部变量 shadow 标记为未初始化。
        out.push(sanitizer_call("__msan_func_entry", span));
    }
    if flags.dfsan {
        // DFSan 每函数入口: 初始化当前帧的标签栈。
        out.push(sanitizer_call("__dfsan_func_entry", span));
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
                ty: Type::Func(Box::new(Type::Void.into()), vec![]),
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
            "memory" | "no_sanitize_memory" => out.msan = false,
            "dataflow" => out.dfsan = false,
            "cfi" | "cfi-icall" => out.cfi_icall = false,
            "cfi-vcall" => out.cfi_vcall = false,
            "cfi-nvcall" => out.cfi_nvcall = false,
            "cfi-derived-cast" => out.cfi_derived_cast = false,
            "cfi-unrelated-cast" => out.cfi_unrelated_cast = false,
            "cfi-mfcall" => out.cfi_mfcall = false,
            "all" => {
                out.kasan = false;
                out.hwasan = false;
                out.ubsan = false;
                out.kcsan = false;
                out.msan = false;
                out.dfsan = false;
                out.cfi_icall = false;
                out.cfi_vcall = false;
                out.cfi_nvcall = false;
                out.cfi_derived_cast = false;
                out.cfi_unrelated_cast = false;
                out.cfi_mfcall = false;
            }
            _ => {}
        }
    }
    out
}

/// `.dcc.san_info` 节载荷。
pub fn sanitizer_metadata(flags: SanitizerFlags) -> Vec<u8> {
    let mut out = b"DCC_SAN\0".to_vec();
    out.push(3); // 格式版本 v3 (新增 MSAN/DFSan/CFI 字段)
    out.push(flags.kasan as u8);
    out.push(flags.hwasan as u8);
    out.push(flags.ubsan as u8);
    out.push(flags.kcsan as u8);
    out.push(flags.kcov as u8);
    out.push(flags.fuzzer as u8);
    out.push(flags.msan as u8);
    out.push(flags.dfsan as u8);
    out.push(flags.cfi_icall as u8);
    out.push(flags.cfi_vcall as u8);
    out.push(flags.cfi_nvcall as u8);
    out.push(flags.cfi_derived_cast as u8);
    out.push(flags.cfi_unrelated_cast as u8);
    out.push(flags.cfi_mfcall as u8);
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
                    is_inline: false,
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
            Stmt::Switch { expr, cases, .. } => {
                collect_call_names_expr(expr, out);
                for case in cases {
                    for it in &case.body.items {
                        collect_call_names(it, out);
                    }
                }
            }
            Stmt::While { cond, body, .. } | Stmt::Do { body, cond, .. } => {
                collect_call_names_expr(cond, out);
                collect_call_names_stmt(body, out);
            }
            Stmt::For {
                cond, step, body, ..
            } => {
                if let Some(c) = cond {
                    collect_call_names_expr(c, out);
                }
                if let Some(s) = step {
                    collect_call_names_expr(s, out);
                }
                collect_call_names_stmt(body, out);
            }
            Stmt::Label { stmt, .. } => collect_call_names_stmt(stmt, out),
            _ => {}
        }
    }

    fn collect_call_names_expr(e: &Expr, out: &mut Vec<String>) {
        match e {
            Expr::Call { callee, args, .. } => {
                if let Expr::Ident { name, .. } = callee.as_ref() {
                    out.push(name.name.clone());
                }
                // Recurse into callee for CFI-wrapped Comma(cfi_check, original)
                collect_call_names_expr(callee, out);
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
    fn from_args_rejects_unknown_sanitizer() {
        let err = SanitizerFlags::from_args(&["-fsanitize=not-a-sanitizer".to_string()])
            .expect_err("unknown sanitizer must be rejected");
        assert!(err.msg.contains("unsupported sanitizer: not-a-sanitizer"));
    }

    #[test]
    fn from_args_parses_combined_sanitize() {
        let f = SanitizerFlags::from_args(&[
            "-fsanitize=address,undefined".to_string(),
            "-fsanitize-coverage".to_string(),
        ])
        .expect("known sanitizers should parse");
        assert!(f.kasan && f.ubsan && f.kcov);
    }

    #[test]
    fn from_args_parses_coverage_suboptions() {
        let f = SanitizerFlags::from_args(&[
            "-fsanitize-coverage=trace-cmp,trace-switch,8bit-counters,pc-table".to_string(),
        ])
        .expect("known coverage sanitizers should parse");
        assert!(f.kcov);
        assert!(f.cov_trace_cmp);
        assert!(f.cov_trace_switch);
        assert!(f.cov_8bit_counters);
        assert!(f.cov_pc_table);
        assert!(f.needs_compare_trace());
        assert!(f.needs_switch_trace());
        assert!(f.needs_edge_coverage());
    }

    #[test]
    fn from_args_recognizes_hwasan_and_fuzzer() {
        let f = SanitizerFlags::from_args(&["-fsanitize=hwaddress,fuzzer".to_string()])
            .expect("known sanitizers should parse");
        assert!(f.hwasan && f.fuzzer);
    }

    #[test]
    fn tu_init_only_in_first_function() {
        let mut prog = Program {
            decls: vec![
                Decl::Func(empty_func("first")),
                Decl::Func(empty_func("second")),
            ],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
        let flags = SanitizerFlags {
            kasan: true,
            kcov: true,
            ..Default::default()
        };
        instrument_program(&mut prog, flags, Arch::X86_64);
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
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
        instrument_program(
            &mut prog,
            SanitizerFlags {
                kcsan: true,
                kernel_mode: true,
                ..Default::default()
            },
            Arch::X86_64,
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
                    quals: crate::ast::QualifierFlags::NONE,
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
                    is_inline: false,
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
        instrument_program(
            &mut prog,
            SanitizerFlags {
                kasan: true,
                kcov: true,
                ..Default::default()
            },
            Arch::X86_64,
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
        assert_eq!(meta[8], 3);
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
                    is_inline: false,
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
        let flags = SanitizerFlags {
            kasan: true,
            ubsan: true,
            kcsan: true,
            kcov: true,
            ..Default::default()
        };
        instrument_program(&mut prog, flags, Arch::X86_64);
        let Decl::Func(f) = &prog.decls[0] else {
            panic!()
        };
        let names = call_names_in_block(f.body.as_ref().unwrap());
        // 函数入口 coverage 仍保留 (KCOV 不受 no_sanitize 控制)
        assert!(names.iter().any(|n| n == "__sanitizer_cov_trace_pc"));
        assert!(!names.iter().any(|n| n == "__asan_init"));
        assert!(!names.iter().any(|n| n == "__ubsan_handle_type_mismatch_v1"));
        assert!(!names.iter().any(|n| n == "__kcsan_init"));
    }

    #[test]
    fn ubsan_wraps_division_by_zero() {
        let mut prog = Program {
            decls: vec![Decl::Func(empty_func("f"))],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
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
            Arch::X86_64,
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
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
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
            Arch::X86_64,
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
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
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
                        ty: Type::Ptr(Box::new(Type::Int.into())),
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
            Arch::X86_64,
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
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
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
            Arch::X86_64,
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
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
        let Decl::Func(f) = &mut prog.decls[0] else {
            panic!()
        };
        let vla_decl = VarDecl {
            ty: Type::Array(
                Box::new(Type::Int.into()),
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
            alias_name: None,
            copy_fn: None,
            is_retain: false,
            is_used_attr: false,
            quals: crate::ast::QualifierFlags::NONE,
            addr_space: crate::ast::AddrSpace::None,
            tls_model: None,
        asm_label: None,
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
            Arch::X86_64,
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
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
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
                        ty: Type::Ptr(Box::new(Type::Int.into())),
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
            Arch::X86_64,
        );
        let Decl::Func(f) = &prog.decls[0] else {
            panic!()
        };
        let names = call_names_in_block(f.body.as_ref().unwrap());
        assert!(
            names.iter().any(|n| n == "__ubsan_handle_type_mismatch_v1"),
            "null check missing: calls = {:?}",
            names
        );
    }

    #[test]
    fn ubsan_alignment_check_inserted_for_int_deref() {
        // *int_ptr 应插入对齐检查 (int align=4 > 1)。
        // 预期生成两次 __ubsan_handle_type_mismatch_v1 调用:
        // 一次用于 null check, 一次用于 alignment check。
        let mut prog = Program {
            decls: vec![Decl::Func(empty_func("f"))],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
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
                        ty: Type::Ptr(Box::new(Type::Int.into())),
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
            Arch::X86_64,
        );
        let Decl::Func(f) = &prog.decls[0] else {
            panic!()
        };
        let names = call_names_in_block(f.body.as_ref().unwrap());
        let mismatch_count = names
            .iter()
            .filter(|n| *n == "__ubsan_handle_type_mismatch_v1")
            .count();
        assert!(
            mismatch_count >= 2,
            "expected null check + alignment check (2 calls to type_mismatch_v1), got {}: {:?}",
            mismatch_count,
            names
        );
    }

    #[test]
    fn ubsan_no_alignment_check_for_char_deref() {
        // *char_ptr 不应插入对齐检查 (char align=1)。
        // 仅应有一次 __ubsan_handle_type_mismatch_v1 调用 (null check)。
        let mut prog = Program {
            decls: vec![Decl::Func(empty_func("f"))],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
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
                        ty: Type::Ptr(Box::new(Type::Char.into())),
                    }),
                    span: Span::point("t.c", 1, 1),
                    ty: Type::Char,
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
            Arch::X86_64,
        );
        let Decl::Func(f) = &prog.decls[0] else {
            panic!()
        };
        let names = call_names_in_block(f.body.as_ref().unwrap());
        let mismatch_count = names
            .iter()
            .filter(|n| *n == "__ubsan_handle_type_mismatch_v1")
            .count();
        assert_eq!(
            mismatch_count, 1,
            "char deref should only have null check (1 call), got {}: {:?}",
            mismatch_count, names
        );
    }

    #[test]
    fn ubsan_float_cast_check_inserted() {
        // (int)f 应插入 __ubsan_handle_float_cast_overflow 调用。
        let mut prog = Program {
            decls: vec![Decl::Func(empty_func("f"))],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
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
            Arch::X86_64,
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
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
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
                        ty: Type::Ptr(Box::new(Type::Int.into())),
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
                kernel_mode: true,
                ..Default::default()
            },
            Arch::X86_64,
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
        assert_eq!(kasan_load_fn(24), "__asan_loadN");
        assert_eq!(kasan_load_fn(32), "__asan_loadN");
        assert_eq!(kasan_load_fn(48), "__asan_loadN");
        assert_eq!(kasan_load_fn(64), "__asan_loadN");
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
        assert_eq!(kasan_store_fn(24), "__asan_storeN");
        assert_eq!(kasan_store_fn(32), "__asan_storeN");
        assert_eq!(kasan_store_fn(48), "__asan_storeN");
        assert_eq!(kasan_store_fn(64), "__asan_storeN");
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
            ty: Type::Ptr(Box::new(Type::Void.into())),
        };
        let call = make_mem_check_calls(
            addr,
            24,
            false,
            &Span::point("t.c", 1, 1),
            SanitizerFlags {
                kasan: true,
                ..Default::default()
            },
            Arch::X86_64,
        )
        .into_iter()
        .next()
        .unwrap();
        if let Expr::Call { callee, args, .. } = call {
            if let Expr::Ident { name, .. } = callee.as_ref() {
                assert_eq!(name.name, "__asan_loadN");
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
            ty: Type::Ptr(Box::new(Type::Void.into())),
        };
        for &sz in &[1usize, 2, 4, 8, 16] {
            let call = make_mem_check_calls(
                addr.clone(),
                sz,
                false,
                &Span::point("t.c", 1, 1),
                SanitizerFlags {
                    kasan: true,
                    ..Default::default()
                },
                Arch::X86_64,
            )
            .into_iter()
            .next()
            .unwrap();
            if let Expr::Call { args, .. } = call {
                assert_eq!(args.len(), 1, "size {} should emit single arg", sz);
            } else {
                panic!("expected Expr::Call");
            }
        }
    }

    // ── Kernel KASAN shadow address computation ──────────────────────

    #[test]
    fn kernel_kasan_uses_inline_shadow_check() {
        // -fsanitize=kernel-address 应生成内联 shadow 地址计算
        // shadow = (addr >> 3) + 0xdffffc0000000000
        // 而非调用 __asan_loadN/__asan_storeN 运行时函数。
        let mut prog = Program {
            decls: vec![Decl::Func(empty_func("f"))],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
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
                        ty: Type::Ptr(Box::new(Type::Int.into())),
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
                kernel_mode: true,
                ..Default::default()
            },
            Arch::X86_64,
        );
        let Decl::Func(f) = &prog.decls[0] else {
            panic!()
        };
        let names = call_names_in_block(f.body.as_ref().unwrap());
        // 内核模式不应 emit __asan_loadN
        assert!(
            !names.iter().any(|n| n.starts_with("__asan_load")),
            "kernel KASAN should not call __asan_loadN, got: {:?}",
            names
        );
        // 4 字节 int 解引用 → __asan_report_load4 (shadow 违规时调用)
        assert!(
            names.iter().any(|n| n == "__asan_report_load4"),
            "kernel KASAN should call __asan_report_load4 on shadow violation, got: {:?}",
            names
        );
    }

    #[test]
    fn kernel_kasan_shadow_check_is_size_aware() {
        // 完整 shadow 检查应感知访问大小: 8 字节 double 解引用报告
        // __asan_report_load8, 且检查表达式按 size 覆盖对应 granule。
        let mut prog = Program {
            decls: vec![Decl::Func(empty_func("f"))],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
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
                        ty: Type::Ptr(Box::new(Type::Double.into())),
                    }),
                    span: Span::point("t.c", 1, 1),
                    ty: Type::Double,
                }),
                span: Span::point("t.c", 1, 1),
            })],
            span: Span::point("t.c", 1, 1),
        });
        instrument_program(
            &mut prog,
            SanitizerFlags {
                kasan: true,
                kernel_mode: true,
                ..Default::default()
            },
            Arch::X86_64,
        );
        let Decl::Func(f) = &prog.decls[0] else {
            panic!()
        };
        let names = call_names_in_block(f.body.as_ref().unwrap());
        assert!(
            names.iter().any(|n| n == "__asan_report_load8"),
            "kernel KASAN should report load8 for a double deref, got: {:?}",
            names
        );
    }

    #[test]
    fn kernel_kasan_store_reports_store_size() {
        // *p = v (kernel mode) 应报告 __asan_report_store4。
        let mut prog = Program {
            decls: vec![Decl::Func(empty_func("f"))],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
        let Decl::Func(f) = &mut prog.decls[0] else {
            panic!()
        };
        f.body = Some(Block {
            items: vec![BlockItem::Stmt(Stmt::Expr {
                e: Expr::Assign {
                    op: BinOp::Add,
                    l: Box::new(Expr::Deref {
                        e: Box::new(Expr::Ident {
                            name: Ident {
                                name: "p".into(),
                                span: Span::point("t.c", 1, 1),
                            },
                            span: Span::point("t.c", 1, 1),
                            ty: Type::Ptr(Box::new(Type::Int.into())),
                        }),
                        span: Span::point("t.c", 1, 1),
                        ty: Type::Int,
                    }),
                    r: Box::new(Expr::Int {
                        v: 42,
                        span: Span::point("t.c", 1, 1),
                        ty: Type::Int,
                    }),
                    span: Span::point("t.c", 1, 1),
                    ty: Type::Int,
                },
                span: Span::point("t.c", 1, 1),
            })],
            span: Span::point("t.c", 1, 1),
        });
        instrument_program(
            &mut prog,
            SanitizerFlags {
                kasan: true,
                kernel_mode: true,
                ..Default::default()
            },
            Arch::X86_64,
        );
        let Decl::Func(f) = &prog.decls[0] else {
            panic!()
        };
        let names = call_names_in_block(f.body.as_ref().unwrap());
        assert!(
            names.iter().any(|n| n == "__asan_report_store4"),
            "kernel KASAN should report store4 for *p = v, got: {:?}",
            names
        );
    }

    #[test]
    fn kernel_kasan_skips_user_mode_tu_init() {
        // 内核模式由内核自初始化 KASAN: 不发射 __asan_init / __hwasan_init。
        let mut prog = Program {
            decls: vec![Decl::Func(empty_func("f"))],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
        instrument_program(
            &mut prog,
            SanitizerFlags {
                kasan: true,
                kernel_mode: true,
                ..Default::default()
            },
            Arch::X86_64,
        );
        let Decl::Func(f) = &prog.decls[0] else {
            panic!()
        };
        let names = call_names_in_block(f.body.as_ref().unwrap());
        assert!(
            !names.iter().any(|n| n == "__asan_init"),
            "kernel KASAN must not emit user-mode __asan_init, got: {:?}",
            names
        );
    }

    #[test]
    fn kernel_kasan_shadow_offset_is_correct() {
        // 验证 x86_64 内核 shadow 偏移常量
        let offset: i128 = 0xdffffc0000000000u64 as i128;
        assert_eq!(offset, 0xdffffc0000000000u64 as i128);
        // shadow = (addr >> 3) + offset
        // 对典型内核地址 0xffff888000000000:
        let kernel_addr: i128 = 0xffff888000000000u64 as i128;
        let shadow = (kernel_addr >> 3) + offset;
        // shadow 应指向有效 shadow 内存区域
        assert!(shadow > 0, "shadow address must be positive");
    }

    // ── HWASAN ────────────────────────────────────────────────────────

    #[test]
    fn hwasan_uses_hwasan_load_store_names() {
        // -fsanitize=hwaddress 应使用 __hwasan_loadN / __hwasan_storeN
        // 而非 __asan_loadN / __asan_storeN。
        let mut prog = Program {
            decls: vec![Decl::Func(empty_func("f"))],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
        let Decl::Func(f) = &mut prog.decls[0] else {
            panic!()
        };
        f.body = Some(Block {
            items: vec![
                BlockItem::Stmt(Stmt::Expr {
                    e: Expr::Assign {
                        op: BinOp::Add,
                        l: Box::new(Expr::Deref {
                            e: Box::new(Expr::Ident {
                                name: Ident {
                                    name: "p".into(),
                                    span: Span::point("t.c", 1, 1),
                                },
                                span: Span::point("t.c", 1, 1),
                                ty: Type::Ptr(Box::new(Type::Int.into())),
                            }),
                            span: Span::point("t.c", 1, 1),
                            ty: Type::Int,
                        }),
                        r: Box::new(Expr::Int {
                            v: 7,
                            span: Span::point("t.c", 1, 1),
                            ty: Type::Int,
                        }),
                        span: Span::point("t.c", 1, 1),
                        ty: Type::Int,
                    },
                    span: Span::point("t.c", 1, 1),
                }),
                BlockItem::Stmt(Stmt::Return {
                    val: Some(Expr::Deref {
                        e: Box::new(Expr::Ident {
                            name: Ident {
                                name: "p".into(),
                                span: Span::point("t.c", 1, 1),
                            },
                            span: Span::point("t.c", 1, 1),
                            ty: Type::Ptr(Box::new(Type::Int.into())),
                        }),
                        span: Span::point("t.c", 1, 1),
                        ty: Type::Int,
                    }),
                    span: Span::point("t.c", 1, 1),
                }),
            ],
            span: Span::point("t.c", 1, 1),
        });
        instrument_program(
            &mut prog,
            SanitizerFlags {
                hwasan: true,
                ..Default::default()
            },
            Arch::X86_64,
        );
        let Decl::Func(f) = &prog.decls[0] else {
            panic!()
        };
        let names = call_names_in_block(f.body.as_ref().unwrap());
        assert!(
            names.iter().any(|n| n == "__hwasan_load4"),
            "HWASAN should emit __hwasan_load4, got: {:?}",
            names
        );
        assert!(
            names.iter().any(|n| n == "__hwasan_store4"),
            "HWASAN should emit __hwasan_store4, got: {:?}",
            names
        );
        assert!(
            !names.iter().any(|n| n.starts_with("__asan_load")),
            "HWASAN should not emit __asan_loadN, got: {:?}",
            names
        );
        assert!(
            !names.iter().any(|n| n.starts_with("__asan_store")),
            "HWASAN should not emit __asan_storeN, got: {:?}",
            names
        );
    }

    #[test]
    fn hwasan_store_fn_maps_known_sizes() {
        assert_eq!(hwasan_store_fn(1), "__hwasan_store1");
        assert_eq!(hwasan_store_fn(2), "__hwasan_store2");
        assert_eq!(hwasan_store_fn(4), "__hwasan_store4");
        assert_eq!(hwasan_store_fn(8), "__hwasan_store8");
        assert_eq!(hwasan_store_fn(16), "__hwasan_store16");
        assert_eq!(hwasan_store_fn(32), "__hwasan_storeN");
    }

    #[test]
    fn hwasan_load_fn_maps_known_sizes() {
        assert_eq!(hwasan_load_fn(1), "__hwasan_load1");
        assert_eq!(hwasan_load_fn(2), "__hwasan_load2");
        assert_eq!(hwasan_load_fn(4), "__hwasan_load4");
        assert_eq!(hwasan_load_fn(8), "__hwasan_load8");
        assert_eq!(hwasan_load_fn(16), "__hwasan_load16");
        assert_eq!(hwasan_load_fn(32), "__hwasan_loadN");
    }

    // ── KCSAN ─────────────────────────────────────────────────────────

    #[test]
    fn kcsan_store_check_passes_is_write_one() {
        // KCSAN-only 模式下，赋值 LHS 的 store 检查应调用
        // __kcsan_check_access(addr, size, 1) — is_write=1。
        let addr = Expr::Ident {
            name: Ident {
                name: "p".into(),
                span: Span::point("t.c", 1, 1),
            },
            span: Span::point("t.c", 1, 1),
            ty: Type::Ptr(Box::new(Type::Void.into())),
        };
        let call = make_mem_check_calls(
            addr,
            4,
            true, // is_write
            &Span::point("t.c", 1, 1),
            SanitizerFlags {
                kcsan: true,
                kernel_mode: true,
                ..Default::default()
            },
            Arch::X86_64,
        )
        .into_iter()
        .next()
        .unwrap();
        if let Expr::Call { callee, args, .. } = call {
            if let Expr::Ident { name, .. } = callee.as_ref() {
                assert_eq!(name.name, "__kcsan_check_access");
            } else {
                panic!("callee should be Ident");
            }
            assert_eq!(
                args.len(),
                3,
                "KCSAN check must have 3 args (addr, size, is_write)"
            );
            // is_write arg should be 1 for store
            if let Expr::Int { v, .. } = &args[2] {
                assert_eq!(*v, 1, "is_write must be 1 for store");
            } else {
                panic!("3rd arg should be Int");
            }
        } else {
            panic!("expected Expr::Call");
        }
    }

    #[test]
    fn kcsan_load_check_passes_is_write_zero() {
        let addr = Expr::Ident {
            name: Ident {
                name: "p".into(),
                span: Span::point("t.c", 1, 1),
            },
            span: Span::point("t.c", 1, 1),
            ty: Type::Ptr(Box::new(Type::Void.into())),
        };
        let call = make_mem_check_calls(
            addr,
            8,
            false, // is_write = false for load
            &Span::point("t.c", 1, 1),
            SanitizerFlags {
                kcsan: true,
                kernel_mode: true,
                ..Default::default()
            },
            Arch::X86_64,
        )
        .into_iter()
        .next()
        .unwrap();
        if let Expr::Call { args, .. } = call {
            if let Expr::Int { v, .. } = &args[2] {
                assert_eq!(*v, 0, "is_write must be 0 for load");
            } else {
                panic!("3rd arg should be Int");
            }
        } else {
            panic!("expected Expr::Call");
        }
    }

    #[test]
    fn kcsan_and_kasan_coexist_emit_both_checks() {
        // KCSAN 与 KASAN 同时启用时, 同一访存应同时发射
        // __kcsan_check_access 与 __asan_loadN (竞争检测 + 越界检测互补)。
        let mut prog = Program {
            decls: vec![Decl::Func(empty_func("f"))],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
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
                        ty: Type::Ptr(Box::new(Type::Int.into())),
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
                kcsan: true,
                kernel_mode: true,
                ..Default::default()
            },
            Arch::X86_64,
        );
        let Decl::Func(f) = &prog.decls[0] else {
            panic!()
        };
        let names = call_names_in_block(f.body.as_ref().unwrap());
        assert!(
            names.iter().any(|n| n == "__asan_report_load4"),
            "KASAN report check missing when KCSAN coexists: {:?}",
            names
        );
        assert!(
            names.iter().any(|n| n == "__kcsan_check_access"),
            "KCSAN check missing when KASAN coexists: {:?}",
            names
        );
    }

    #[test]
    fn kasan_store_check_on_incdec_deref() {
        // (*p)++ 是读-改-写: KASAN 下应在自增前发射 __asan_store4。
        let mut prog = Program {
            decls: vec![Decl::Func(empty_func("f"))],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
        let Decl::Func(f) = &mut prog.decls[0] else {
            panic!()
        };
        f.body = Some(Block {
            items: vec![BlockItem::Stmt(Stmt::Expr {
                e: Expr::PostInc {
                    e: Box::new(Expr::Deref {
                        e: Box::new(Expr::Ident {
                            name: Ident {
                                name: "p".into(),
                                span: Span::point("t.c", 1, 1),
                            },
                            span: Span::point("t.c", 1, 1),
                            ty: Type::Ptr(Box::new(Type::Int.into())),
                        }),
                        span: Span::point("t.c", 1, 1),
                        ty: Type::Int,
                    }),
                    span: Span::point("t.c", 1, 1),
                    ty: Type::Int,
                },
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
            Arch::X86_64,
        );
        let Decl::Func(f) = &prog.decls[0] else {
            panic!()
        };
        let names = call_names_in_block(f.body.as_ref().unwrap());
        assert!(
            names.iter().any(|n| n.starts_with("__asan_store")),
            "KASAN should inject __asan_storeN before (*p)++, got: {:?}",
            names
        );
        assert!(
            names.iter().any(|n| n.starts_with("__asan_load")),
            "KASAN should also inject __asan_loadN for the read part of (*p)++, got: {:?}",
            names
        );
    }

    #[test]
    fn kcsan_store_check_on_incdec_index() {
        // a[i]-- (KCSAN-only) 应在自减前发射 __kcsan_check_access(addr,4,1)。
        let mut prog = Program {
            decls: vec![Decl::Func(empty_func("f"))],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
        let Decl::Func(f) = &mut prog.decls[0] else {
            panic!()
        };
        f.body = Some(Block {
            items: vec![BlockItem::Stmt(Stmt::Expr {
                e: Expr::PreDec {
                    e: Box::new(Expr::Index {
                        arr: Box::new(Expr::Ident {
                            name: Ident {
                                name: "arr".into(),
                                span: Span::point("t.c", 1, 1),
                            },
                            span: Span::point("t.c", 1, 1),
                            ty: Type::Ptr(Box::new(Type::Int.into())),
                        }),
                        idx: Box::new(Expr::Int {
                            v: 0,
                            span: Span::point("t.c", 1, 1),
                            ty: Type::Int,
                        }),
                        span: Span::point("t.c", 1, 1),
                        ty: Type::Int,
                    }),
                    span: Span::point("t.c", 1, 1),
                    ty: Type::Int,
                },
                span: Span::point("t.c", 1, 1),
            })],
            span: Span::point("t.c", 1, 1),
        });
        instrument_program(
            &mut prog,
            SanitizerFlags {
                kcsan: true,
                kernel_mode: true,
                ..Default::default()
            },
            Arch::X86_64,
        );
        let Decl::Func(f) = &prog.decls[0] else {
            panic!()
        };
        let names = call_names_in_block(f.body.as_ref().unwrap());
        assert!(
            names.iter().any(|n| n == "__kcsan_check_access"),
            "KCSAN should inject __kcsan_check_access before a[i]--, got: {:?}",
            names
        );
    }

    // ── Fuzzer ────────────────────────────────────────────────────────

    #[test]
    fn fuzzer_injects_switch_trace() {
        // fuzzer 模式下，switch 语句的 expr 应被包成
        // `Comma { l: Call(__sanitizer_cov_trace_switch, x), r: x }`。
        let mut prog = Program {
            decls: vec![Decl::Func(empty_func("f"))],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
        let Decl::Func(f) = &mut prog.decls[0] else {
            panic!()
        };
        f.body = Some(Block {
            items: vec![BlockItem::Stmt(Stmt::Switch {
                expr: Expr::Ident {
                    name: Ident {
                        name: "x".into(),
                        span: Span::point("t.c", 1, 1),
                    },
                    span: Span::point("t.c", 1, 1),
                    ty: Type::Int,
                },
                cases: vec![crate::ast::Case {
                    val: Some(Expr::Int {
                        v: 1,
                        span: Span::point("t.c", 2, 1),
                        ty: Type::Int,
                    }),
                    range_end: None,
                    body: Block {
                        items: vec![BlockItem::Stmt(Stmt::Break {
                            span: Span::point("t.c", 3, 1),
                        })],
                        span: Span::point("t.c", 2, 1),
                    },
                    span: Span::point("t.c", 2, 1),
                }],
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
            Arch::X86_64,
        );
        let Decl::Func(f) = &prog.decls[0] else {
            panic!()
        };
        // 直接检查 Switch 的 expr 是否被包成 Comma(Call, _)
        let body = f.body.as_ref().unwrap();
        let mut found_switch_trace = false;
        for item in &body.items {
            if let BlockItem::Stmt(Stmt::Switch {
                expr: Expr::Comma { l, .. },
                ..
            }) = item
            {
                let is_trace_switch = matches!(
                    l.as_ref(),
                    Expr::Call { callee, .. }
                        if matches!(
                            callee.as_ref(),
                            Expr::Ident { name, .. }
                                if name.name == "__sanitizer_cov_trace_switch"
                        )
                );
                if is_trace_switch {
                    found_switch_trace = true;
                }
            }
        }
        assert!(
            found_switch_trace,
            "fuzzer should inject __sanitizer_cov_trace_switch as switch expr wrapper"
        );
    }

    // ── KCOV edge coverage ────────────────────────────────────────────

    #[test]
    fn kcov_injects_guard_at_if_branches() {
        // KCOV 模式应在 if/else 分支前注入 guard 调用。
        let mut prog = Program {
            decls: vec![Decl::Func(empty_func("f"))],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
        let Decl::Func(f) = &mut prog.decls[0] else {
            panic!()
        };
        f.body = Some(Block {
            items: vec![BlockItem::Stmt(Stmt::If {
                cond: Expr::Int {
                    v: 1,
                    span: Span::point("t.c", 1, 1),
                    ty: Type::Int,
                },
                then_: Box::new(Stmt::Return {
                    val: None,
                    span: Span::point("t.c", 2, 1),
                }),
                else_: Some(Box::new(Stmt::Return {
                    val: None,
                    span: Span::point("t.c", 3, 1),
                })),
                span: Span::point("t.c", 1, 1),
            })],
            span: Span::point("t.c", 1, 1),
        });
        instrument_program(
            &mut prog,
            SanitizerFlags {
                kcov: true,
                ..Default::default()
            },
            Arch::X86_64,
        );
        let Decl::Func(f) = &prog.decls[0] else {
            panic!()
        };
        let names = call_names_in_block(f.body.as_ref().unwrap());
        assert!(
            names.iter().any(|n| n == "__sanitizer_cov_trace_pc_guard"),
            "KCOV should inject trace_pc_guard at branches, got: {:?}",
            names
        );
        assert!(
            names.iter().any(|n| n == "__sanitizer_cov_trace_pc"),
            "KCOV should inject trace_pc at function entry, got: {:?}",
            names
        );
    }

    // ── Effective flags / no_sanitize ─────────────────────────────────

    #[test]
    fn effective_flags_kernel_kcsan_disables_kcsan() {
        let flags = SanitizerFlags {
            kcsan: true,
            kernel_mode: true,
            ..Default::default()
        };
        let eff = effective_flags(&["kernel-kcsan".to_string()], flags);
        assert!(!eff.kcsan);
    }

    #[test]
    fn effective_flags_hwaddress_disables_hwasan() {
        let flags = SanitizerFlags {
            hwasan: true,
            ..Default::default()
        };
        let eff = effective_flags(&["hwaddress".to_string()], flags);
        assert!(!eff.hwasan);
    }

    // ── Assign with Deref LHS (KASAN store path) ──────────────────────

    #[test]
    fn kasan_store_check_on_deref_assign() {
        // *p = v 应在赋值前插入 __asan_storeN 检查。
        let mut prog = Program {
            decls: vec![Decl::Func(empty_func("f"))],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
        let Decl::Func(f) = &mut prog.decls[0] else {
            panic!()
        };
        f.body = Some(Block {
            items: vec![BlockItem::Stmt(Stmt::Expr {
                e: Expr::Assign {
                    op: BinOp::Add,
                    l: Box::new(Expr::Deref {
                        e: Box::new(Expr::Ident {
                            name: Ident {
                                name: "p".into(),
                                span: Span::point("t.c", 1, 1),
                            },
                            span: Span::point("t.c", 1, 1),
                            ty: Type::Ptr(Box::new(Type::Int.into())),
                        }),
                        span: Span::point("t.c", 1, 1),
                        ty: Type::Int,
                    }),
                    r: Box::new(Expr::Ident {
                        name: Ident {
                            name: "v".into(),
                            span: Span::point("t.c", 1, 1),
                        },
                        span: Span::point("t.c", 1, 1),
                        ty: Type::Int,
                    }),
                    span: Span::point("t.c", 1, 1),
                    ty: Type::Int,
                },
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
            Arch::X86_64,
        );
        let Decl::Func(f) = &prog.decls[0] else {
            panic!()
        };
        let names = call_names_in_block(f.body.as_ref().unwrap());
        assert!(
            names.iter().any(|n| n.starts_with("__asan_store")),
            "KASAN should inject __asan_storeN on *p = v, got: {:?}",
            names
        );
    }

    #[test]
    fn kasan_store_check_on_index_assign() {
        // arr[i] = v 应在赋值前插入 __asan_storeN 检查。
        let mut prog = Program {
            decls: vec![Decl::Func(empty_func("f"))],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
        let Decl::Func(f) = &mut prog.decls[0] else {
            panic!()
        };
        f.body = Some(Block {
            items: vec![BlockItem::Stmt(Stmt::Expr {
                e: Expr::Assign {
                    op: BinOp::Add,
                    l: Box::new(Expr::Index {
                        arr: Box::new(Expr::Ident {
                            name: Ident {
                                name: "arr".into(),
                                span: Span::point("t.c", 1, 1),
                            },
                            span: Span::point("t.c", 1, 1),
                            ty: Type::Ptr(Box::new(Type::Int.into())),
                        }),
                        idx: Box::new(Expr::Int {
                            v: 0,
                            span: Span::point("t.c", 1, 1),
                            ty: Type::Int,
                        }),
                        span: Span::point("t.c", 1, 1),
                        ty: Type::Int,
                    }),
                    r: Box::new(Expr::Int {
                        v: 42,
                        span: Span::point("t.c", 1, 1),
                        ty: Type::Int,
                    }),
                    span: Span::point("t.c", 1, 1),
                    ty: Type::Int,
                },
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
            Arch::X86_64,
        );
        let Decl::Func(f) = &prog.decls[0] else {
            panic!()
        };
        let names = call_names_in_block(f.body.as_ref().unwrap());
        assert!(
            names.iter().any(|n| n.starts_with("__asan_store")),
            "KASAN should inject __asan_storeN on arr[i] = v, got: {:?}",
            names
        );
    }

    // ── SanitizerFlags parsing ────────────────────────────────────────

    #[test]
    fn from_args_kernel_hwaddress() {
        let f = SanitizerFlags::from_args(&["-fsanitize=kernel-hwaddress".to_string()])
            .expect("known sanitizer should parse");
        assert!(f.hwasan && f.kernel_mode);
    }

    #[test]
    fn from_args_kernel_kcsan() {
        let f = SanitizerFlags::from_args(&["-fsanitize=kernel-kcsan".to_string()])
            .expect("known sanitizer should parse");
        assert!(f.kcsan && f.kernel_mode);
    }

    #[test]
    fn from_args_trace_pc_guard() {
        let f = SanitizerFlags::from_args(&["-fsanitize=trace-pc-guard".to_string()])
            .expect("known sanitizer should parse");
        assert!(f.kcov);
    }

    // ── MSAN (MemorySanitizer) ───────────────────────────────────────

    #[test]
    fn from_args_parses_msan() {
        let f = SanitizerFlags::from_args(&["-fsanitize=memory".to_string()])
            .expect("known sanitizer should parse");
        assert!(f.msan);
        assert!(f.needs_msan_check());
        assert!(f.needs_tu_init());
    }

    #[test]
    fn msan_wraps_deref_with_load_check() {
        // *p 应插入 __msan_check_load_4 调用。
        let mut prog = Program {
            decls: vec![Decl::Func(empty_func("f"))],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
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
                        ty: Type::Ptr(Box::new(Type::Int.into())),
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
                msan: true,
                ..Default::default()
            },
            Arch::X86_64,
        );
        let Decl::Func(f) = &prog.decls[0] else {
            panic!()
        };
        let names = call_names_in_block(f.body.as_ref().unwrap());
        assert!(
            names.iter().any(|n| n == "__msan_check_load_4"),
            "MSAN should emit __msan_check_load_4 for int deref, got: {:?}",
            names
        );
    }

    #[test]
    fn msan_inserts_func_entry_at_every_function() {
        let mut prog = Program {
            decls: vec![Decl::Func(empty_func("a")), Decl::Func(empty_func("b"))],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
        instrument_program(
            &mut prog,
            SanitizerFlags {
                msan: true,
                ..Default::default()
            },
            Arch::X86_64,
        );
        for decl in &prog.decls {
            let Decl::Func(f) = decl else { continue };
            let names = call_names_in_block(f.body.as_ref().unwrap());
            assert!(
                names.iter().any(|n| n == "__msan_func_entry"),
                "MSAN should emit __msan_func_entry for every function, got: {:?}",
                names
            );
        }
    }

    #[test]
    fn msan_inserts_tu_init() {
        // MSAN 应在首个函数前插入 __msan_init。
        let mut prog = Program {
            decls: vec![Decl::Func(empty_func("first"))],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
        instrument_program(
            &mut prog,
            SanitizerFlags {
                msan: true,
                ..Default::default()
            },
            Arch::X86_64,
        );
        let Decl::Func(f) = &prog.decls[0] else {
            panic!()
        };
        let names = call_names_in_block(f.body.as_ref().unwrap());
        assert!(
            names.iter().any(|n| n == "__msan_init"),
            "MSAN should emit __msan_init in first function, got: {:?}",
            names
        );
    }

    #[test]
    fn msan_check_fn_name_maps_sizes() {
        assert_eq!(msan_check_fn_name(1, false), "__msan_check_load_1");
        assert_eq!(msan_check_fn_name(4, false), "__msan_check_load_4");
        assert_eq!(msan_check_fn_name(8, true), "__msan_check_store_8");
        assert_eq!(msan_check_fn_name(16, true), "__msan_check_store_16");
    }

    // ── DFSan (DataFlowSanitizer) ────────────────────────────────────

    #[test]
    fn from_args_parses_dfsan() {
        let f = SanitizerFlags::from_args(&["-fsanitize=dataflow".to_string()])
            .expect("known sanitizer should parse");
        assert!(f.dfsan);
        assert!(f.needs_dfsan_check());
    }

    #[test]
    fn dfsan_wraps_deref_with_load_label() {
        // *p 应插入 __dfsan_load_label_4 调用。
        let mut prog = Program {
            decls: vec![Decl::Func(empty_func("f"))],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
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
                        ty: Type::Ptr(Box::new(Type::Int.into())),
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
                dfsan: true,
                ..Default::default()
            },
            Arch::X86_64,
        );
        let Decl::Func(f) = &prog.decls[0] else {
            panic!()
        };
        let names = call_names_in_block(f.body.as_ref().unwrap());
        assert!(
            names.iter().any(|n| n == "__dfsan_load_label_4"),
            "DFSan should emit __dfsan_load_label_4 for int deref, got: {:?}",
            names
        );
    }

    #[test]
    fn dfsan_inserts_func_entry() {
        let mut prog = Program {
            decls: vec![Decl::Func(empty_func("f"))],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
        instrument_program(
            &mut prog,
            SanitizerFlags {
                dfsan: true,
                ..Default::default()
            },
            Arch::X86_64,
        );
        let Decl::Func(f) = &prog.decls[0] else {
            panic!()
        };
        let names = call_names_in_block(f.body.as_ref().unwrap());
        assert!(
            names.iter().any(|n| n == "__dfsan_func_entry"),
            "DFSan should emit __dfsan_func_entry, got: {:?}",
            names
        );
    }

    // ── CFI (Control Flow Integrity) ─────────────────────────────────

    #[test]
    fn from_args_parses_cfi_variants() {
        let f = SanitizerFlags::from_args(&["-fsanitize=cfi".to_string()])
            .expect("known sanitizer should parse");
        assert!(f.cfi_icall);
        assert!(f.cfi_vcall);
        assert!(f.cfi_nvcall);
        assert!(f.cfi_derived_cast);
        assert!(f.cfi_unrelated_cast);
        assert!(f.cfi_mfcall);
        assert!(f.any_cfi());
        assert!(f.needs_cfi_icall());
    }

    #[test]
    fn from_args_parses_individual_cfi() {
        let f = SanitizerFlags::from_args(&["-fsanitize=cfi-icall".to_string()])
            .expect("known sanitizer should parse");
        assert!(f.cfi_icall);
        assert!(!f.cfi_vcall);
        assert!(f.needs_cfi_icall());
    }

    #[test]
    fn cfi_wraps_indirect_call() {
        // fn_ptr(args) 应插入 __cfi_check 调用。
        let mut prog = Program {
            decls: vec![Decl::Func(empty_func("f"))],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
        let Decl::Func(f) = &mut prog.decls[0] else {
            panic!()
        };
        // Build: return fn_ptr(42) — indirect call through function pointer
        f.body = Some(Block {
            items: vec![BlockItem::Stmt(Stmt::Return {
                val: Some(Expr::Call {
                    callee: Box::new(Expr::Ident {
                        name: Ident {
                            name: "fn_ptr".into(),
                            span: Span::point("t.c", 1, 1),
                        },
                        span: Span::point("t.c", 1, 1),
                        ty: Type::Func(
                            Box::new(Type::Int.into()),
                            vec![Type::Int.into()],
                        ),
                    }),
                    args: vec![Expr::Int {
                        v: 42,
                        span: Span::point("t.c", 1, 1),
                        ty: Type::Int,
                    }],
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
                cfi_icall: true,
                ..Default::default()
            },
            Arch::X86_64,
        );
        let Decl::Func(f) = &prog.decls[0] else {
            panic!()
        };
        let names = call_names_in_block(f.body.as_ref().unwrap());
        // fn_ptr is an Ident so it's a direct call — no CFI check
        assert!(
            !names.iter().any(|n| n == "__cfi_check"),
            "direct call through Ident should not trigger CFI check, got: {:?}",
            names
        );
    }

    #[test]
    fn cfi_wraps_indirect_call_through_deref() {
        // (*fn_ptr)(args) 应插入 __cfi_check 调用。
        let mut prog = Program {
            decls: vec![Decl::Func(empty_func("f"))],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
        let Decl::Func(f) = &mut prog.decls[0] else {
            panic!()
        };
        // Build: return (*fn_ptr)(42) — indirect call through deref
        f.body = Some(Block {
            items: vec![BlockItem::Stmt(Stmt::Return {
                val: Some(Expr::Call {
                    callee: Box::new(Expr::Deref {
                        e: Box::new(Expr::Ident {
                            name: Ident {
                                name: "fn_ptr".into(),
                                span: Span::point("t.c", 1, 1),
                            },
                            span: Span::point("t.c", 1, 1),
                            ty: Type::Ptr(Box::new(Type::Func(
                                Box::new(Type::Int.into()),
                                vec![Type::Int.into()],
                            ).into())),
                        }),
                        span: Span::point("t.c", 1, 1),
                        ty: Type::Func(
                            Box::new(Type::Int.into()),
                            vec![Type::Int.into()],
                        ),
                    }),
                    args: vec![Expr::Int {
                        v: 42,
                        span: Span::point("t.c", 1, 1),
                        ty: Type::Int,
                    }],
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
                cfi_icall: true,
                ..Default::default()
            },
            Arch::X86_64,
        );
        let Decl::Func(f) = &prog.decls[0] else {
            panic!()
        };
        let names = call_names_in_block(f.body.as_ref().unwrap());
        assert!(
            names.iter().any(|n| n == "__cfi_check"),
            "indirect call through Deref should trigger __cfi_check, got: {:?}",
            names
        );
    }

    #[test]
    fn cfi_type_hash_is_deterministic() {
        let ty = Type::Func(Box::new(Type::Int.into()), vec![Type::Int.into()]);
        let h1 = cfi_type_hash(&ty);
        let h2 = cfi_type_hash(&ty);
        assert_eq!(h1, h2, "same type must produce same hash");
        // Different type should produce different hash (with high probability)
        let ty2 = Type::Func(Box::new(Type::Void.into()), vec![Type::Long.into()]);
        assert_ne!(cfi_type_hash(&ty), cfi_type_hash(&ty2));
    }

    // ── no_sanitize for new sanitizers ───────────────────────────────

    #[test]
    fn effective_flags_memory_disables_msan() {
        let flags = SanitizerFlags {
            msan: true,
            ..Default::default()
        };
        let eff = effective_flags(&["memory".to_string()], flags);
        assert!(!eff.msan);
    }

    #[test]
    fn effective_flags_dataflow_disables_dfsan() {
        let flags = SanitizerFlags {
            dfsan: true,
            ..Default::default()
        };
        let eff = effective_flags(&["dataflow".to_string()], flags);
        assert!(!eff.dfsan);
    }

    #[test]
    fn effective_flags_cfi_icall_disables_only_icall() {
        let flags = SanitizerFlags {
            cfi_icall: true,
            cfi_vcall: true,
            ..Default::default()
        };
        let eff = effective_flags(&["cfi-icall".to_string()], flags);
        assert!(!eff.cfi_icall);
        assert!(eff.cfi_vcall);
    }

    #[test]
    fn effective_flags_all_disables_everything() {
        let flags = SanitizerFlags {
            msan: true,
            dfsan: true,
            cfi_icall: true,
            cfi_vcall: true,
            ..Default::default()
        };
        let eff = effective_flags(&["all".to_string()], flags);
        assert!(!eff.msan);
        assert!(!eff.dfsan);
        assert!(!eff.cfi_icall);
        assert!(!eff.cfi_vcall);
    }

    // ── Metadata v3 ───────────────────────────────────────────────────

    #[test]
    fn metadata_v3_includes_msan_dfsan_cfi() {
        let meta = sanitizer_metadata(SanitizerFlags {
            msan: true,
            dfsan: true,
            cfi_icall: true,
            ..Default::default()
        });
        assert_eq!(&meta[..8], b"DCC_SAN\0");
        assert_eq!(meta[8], 3); // version v3
        // msan at offset 15
        assert_eq!(meta[15], 1);
        // dfsan at offset 16
        assert_eq!(meta[16], 1);
        // cfi_icall at offset 17
        assert_eq!(meta[17], 1);
        // cfi_vcall at offset 18 — not set
        assert_eq!(meta[18], 0);
    }

    #[test]
    fn any_returns_true_for_new_sanitizers() {
        assert!(SanitizerFlags {
            msan: true,
            ..Default::default()
        }
        .any());
        assert!(SanitizerFlags {
            dfsan: true,
            ..Default::default()
        }
        .any());
        assert!(SanitizerFlags {
            cfi_icall: true,
            ..Default::default()
        }
        .any());
    }

    // ── KASAN shadow offset per architecture ────────────────────────────

    #[test]
    fn kasan_shadow_offset_x86_64() {
        assert_eq!(
            kasan_shadow_offset_for_arch(Arch::X86_64),
            0xdffffc0000000000u64 as i128
        );
    }

    #[test]
    fn kasan_shadow_offset_aarch64() {
        assert_eq!(
            kasan_shadow_offset_for_arch(Arch::AArch64),
            0xdfff600000000000u64 as i128
        );
    }

    #[test]
    fn kasan_shadow_offset_riscv64() {
        assert_eq!(
            kasan_shadow_offset_for_arch(Arch::RiscV64),
            0xdffffe0000000000u64 as i128
        );
    }

    #[test]
    fn kasan_shadow_offset_fallback() {
        assert_eq!(
            kasan_shadow_offset_for_arch(Arch::Mips64),
            0xdffffc0000000000u64 as i128
        );
    }

    // ── HWASAN kernel inline tag check ────────────────────────────────

    #[test]
    fn kernel_hwasan_generates_inline_tag_check() {
        // kernel-hwaddress load should NOT call __hwasan_load4 (user-mode)
        // but should call __hwasan_tag_mismatch4 (inline tag check report)
        let mut prog = Program {
            decls: vec![Decl::Func(empty_func("f"))],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
        let Decl::Func(f) = &mut prog.decls[0] else {
            panic!()
        };
        f.body = Some(Block {
            items: vec![BlockItem::Stmt(Stmt::Expr {
                e: Expr::Deref {
                    e: Box::new(Expr::Ident {
                        name: Ident {
                            name: "p".into(),
                            span: Span::point("t.c", 1, 1),
                        },
                        span: Span::point("t.c", 1, 1),
                        ty: Type::Ptr(Box::new(Type::Int.into())),
                    }),
                    span: Span::point("t.c", 1, 1),
                    ty: Type::Int,
                },
                span: Span::point("t.c", 1, 1),
            })],
            span: Span::point("t.c", 1, 1),
        });
        instrument_program(
            &mut prog,
            SanitizerFlags {
                hwasan: true,
                kernel_mode: true,
                ..Default::default()
            },
            Arch::AArch64,
        );
        let Decl::Func(f) = &prog.decls[0] else {
            panic!()
        };
        let names = call_names_in_block(f.body.as_ref().unwrap());
        assert!(
            !names.iter().any(|n| n == "__hwasan_load4"),
            "kernel-hwaddress must not call user-mode __hwasan_load4"
        );
        assert!(
            names.iter().any(|n| n == "__hwasan_tag_mismatch4"),
            "kernel-hwaddress should call __hwasan_tag_mismatch4, got: {:?}",
            names
        );
    }

    #[test]
    fn kernel_hwasan_store_uses_tag_mismatch() {
        let mut prog = Program {
            decls: vec![Decl::Func(empty_func("f"))],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
        let Decl::Func(f) = &mut prog.decls[0] else {
            panic!()
        };
        f.body = Some(Block {
            items: vec![BlockItem::Stmt(Stmt::Expr {
                e: Expr::Assign {
                    op: BinOp::Add,
                    l: Box::new(Expr::Deref {
                        e: Box::new(Expr::Ident {
                            name: Ident {
                                name: "p".into(),
                                span: Span::point("t.c", 1, 1),
                            },
                            span: Span::point("t.c", 1, 1),
                            ty: Type::Ptr(Box::new(Type::Int.into())),
                        }),
                        span: Span::point("t.c", 1, 1),
                        ty: Type::Int,
                    }),
                    r: Box::new(Expr::Int {
                        v: 42,
                        span: Span::point("t.c", 1, 1),
                        ty: Type::Int,
                    }),
                    span: Span::point("t.c", 1, 1),
                    ty: Type::Int,
                },
                span: Span::point("t.c", 1, 1),
            })],
            span: Span::point("t.c", 1, 1),
        });
        instrument_program(
            &mut prog,
            SanitizerFlags {
                hwasan: true,
                kernel_mode: true,
                ..Default::default()
            },
            Arch::AArch64,
        );
        let Decl::Func(f) = &prog.decls[0] else {
            panic!()
        };
        let names = call_names_in_block(f.body.as_ref().unwrap());
        assert!(
            names.iter().any(|n| n.starts_with("__hwasan_tag_mismatch")),
            "kernel-hwaddress store should call __hwasan_tag_mismatch*, got: {:?}",
            names
        );
    }

    // ── KCOV cross-function inline merging ─────────────────────────────

    #[test]
    fn kcov_inline_merge_reuses_caller_guard_array() {
        // When an always_inline function is called by exactly one caller,
        // it should reuse the caller's guard array name.
        let mut helper = empty_func("helper");
        helper.is_always_inline = true;
        let mut main_fn = empty_func("main");
        // Replace main's body with a call to helper + return
        main_fn.body = Some(Block {
            items: vec![
                BlockItem::Stmt(Stmt::Expr {
                    e: Expr::Call {
                        callee: Box::new(Expr::Ident {
                            name: Ident {
                                name: "helper".into(),
                                span: Span::point("t.c", 6, 1),
                            },
                            span: Span::point("t.c", 6, 1),
                            ty: Type::Func(Box::new(Type::Int.into()), vec![]),
                        }),
                        args: vec![],
                        span: Span::point("t.c", 6, 1),
                        ty: Type::Int,
                    },
                    span: Span::point("t.c", 6, 1),
                }),
                BlockItem::Stmt(Stmt::Return {
                    val: None,
                    span: Span::point("t.c", 7, 1),
                }),
            ],
            span: Span::point("t.c", 5, 1),
        });
        let mut prog = Program {
            decls: vec![Decl::Func(helper), Decl::Func(main_fn)],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
        instrument_program(
            &mut prog,
            SanitizerFlags {
                kcov: true,
                ..Default::default()
            },
            Arch::X86_64,
        );
        // main should have its own guard array declaration
        let mut main_guard_var: Option<String> = None;
        let mut helper_has_guard_decl = false;
        for decl in &prog.decls {
            let Decl::Func(f) = decl else { continue };
            let body = f.body.as_ref().unwrap();
            for item in &body.items {
                if let BlockItem::Decl(Decl::Var(v)) = item {
                    if v.name.name.starts_with("__dcc_cov_guards_") {
                        if f.name.name == "main" {
                            main_guard_var = Some(v.name.name.clone());
                        } else if f.name.name == "helper" {
                            helper_has_guard_decl = true;
                        }
                    }
                }
            }
        }
        assert!(main_guard_var.is_some(), "main should have guard array");
        // helper (always_inline, single caller) should NOT declare its own
        // guard array; it reuses main's array
        assert!(
            !helper_has_guard_decl,
            "inlined helper should not declare its own guard array"
        );
    }
}
