// src/codegen/x86_64/emit/functions.rs
// Function-level driver: walks the program's function definitions and
// delegates each one to compile_func. Manually migrated from the
// pre-refactor monolithic x86_64.rs (commit 2ff718b) during solution v2
// phase 0.

use crate::ast::*;
use crate::codegen::shared::elf as shared_elf;
use crate::codegen::x86_64::abi::layout::LayoutMap;
use crate::codegen::x86_64::insn::Insn;
use crate::codegen::x86_64::{AbiClass, Codegen};
use std::collections::HashMap;

impl Codegen {
    pub fn emit_functions(&mut self, prog: &Program) {
        // Collect names of __attribute__((copy(fn))) functions: they must
        // not emit code because their namesake symbol is replaced by an
        // alias.
        let copy_fn_names: std::collections::HashSet<String> = prog
            .decls
            .iter()
            .filter_map(|d| {
                if let Decl::Func(f) = d {
                    f.alias_name.as_ref().map(|_| f.name.name.clone())
                } else {
                    None
                }
            })
            .collect();
        let top_funcs: Vec<FuncDecl> = prog
            .decls
            .iter()
            .filter_map(|d| {
                if let Decl::Func(f) = d {
                    // copy(fn) -> alias_name=Some(fn); do not emit a body.
                    if f.alias_name.is_some() {
                        return None;
                    }
                    // Same name as an earlier copy-attribute decl: treat
                    // this decl as an alias and skip its body.
                    if copy_fn_names.contains(&f.name.name) {
                        return None;
                    }
                    // __attribute__((error("..."))): compile-time assertion
                    // only (e.g. kernel __compiletime_assert_*); never emit
                    // a body even if the parser attached one.
                    if f.error_msg.is_some() {
                        return None;
                    }
                    if f.body.is_some() {
                        Some(f.clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect();
        if self.parallel_codegen && self.relocatable && top_funcs.len() >= 2 {
            self.emit_functions_parallel(&top_funcs, prog);
            return;
        }
        for f in &top_funcs {
            self.compile_func(f, prog);
        }
    }
    /// 汇总全程序里「地址被取走」的对象根名，一个 Codegen 实例只算一次。
    ///
    /// 为什么是程序级而不是当前函数级：file-scope 的 `register asm` 变量可以在
    /// **任何**一个函数里被取址，只看本函数体会漏。判据本身复用
    /// `ast_gvn::addr_escaped_names`，后端不再自己爬一遍 AST —— 两套逸出判据里弱的
    /// 那一套会决定产物正确性。集合按名字索引，所以同名变量在别处被取址会一并
    /// 取消绑定（偏保守：只丢绑定，不丢正确性）。
    fn collect_addr_escaped(&mut self, prog: &Program) {
        if self.addr_escaped_done {
            return;
        }
        self.addr_escaped_done = true;
        // 只递一个入口：嵌套函数体由判据本身递归（`ast_gvn::walk_addr_of` 的
        // `Decl::Func` 臂）。后端再手写一份递归就是第二套逸出判据，
        // 而两套里弱的那一套会决定产物（§11.13 的 N8）。
        for d in &prog.decls {
            let crate::ast::Decl::Func(f) = d else {
                continue;
            };
            let Some(b) = &f.body else {
                continue;
            };
            self.addr_escaped
                .extend(crate::ast_gvn::addr_escaped_names(b));
        }
    }

    /// 该名字是否允许绑定到物理寄存器（地址逸出者不允许）。声明处的两条路径共用。
    pub(crate) fn register_binding_allowed(&self, name: &str) -> bool {
        !self.addr_escaped.contains(name)
    }

    pub fn compile_func(&mut self, f: &FuncDecl, prog: &Program) {
        let arg_regs: [u8; 6] = [7, 6, 2, 1, 8, 9];
        self.var_map.clear();
        // 段地址槽登记表是逐函数状态：不清就会把上一个函数的 `t = &gv` 广播到
        // 下一个函数的同名槽位（误发前缀 = 访存到错误的段）。
        self.local_addr_seg.clear();
        self.var_map_total_slots = 0;
        self.vla_offsets.clear();
        // VM 类型尺寸绑定槽是每函数帧内状态 (槽偏移在各函数独立
        // 分配), 跨函数残留会导致 sizeof 读到别的函数帧的槽
        // (20040423-1: 匿名 struct 的空 tag 在 main/sub1 同 key)
        self.vm_struct_size_slots.clear();
        self.homes.clear();
        // file-scope 绑定也要过同一道逸出判据：地址被取走的对象必须留在内存里。
        self.collect_addr_escaped(prog);
        for (n, b) in self.global_homes.clone() {
            if !self.register_binding_allowed(&n) {
                continue;
            }
            self.homes.insert(n, b);
        }
        // 绑定表定稿了，把钉住集合重投影一遍。**为什么现在就要做**：`naked` 分支在
        // 下面的 `reset_reg_tracker()` 之前就 `return`，而旧代码的 `owned_gp` 是当时现查
        // `reg_bindings` 算的；如果 `pinned` 等到 reset 才清，naked 函数里的内联 asm
        // 就会拿上一个函数的绑定当本函数家的名单（多排除会换发号，少排除会盖掉变量）。
        // 只同步名单，不动位图 —— 位图在 naked 路径上旧代码就是保留残值的。
        self.reg_bank
            .resync_pins(self.homes.values().filter_map(|(l, _)| l.reg()).map(|r| r.n));
        self.cleanup_vars.clear();
        self.sret_watermark = 0;
        self.sret_region_base = 0;
        // 帧解算表也是**逐函数**状态：`frame_size` 要等本函数的 `LayoutMap::resolve` 才算出来，
        // 在那之前 `check_frame_budget` 必须处于「尚未知帧底」而不检查的状态（`unresolved()`
        // 的 frame_size = 0，正是那个状态）。不复位就是拿**上一个函数**的帧底来量本函数
        // 规划期的分配：实测 `long_double_abi.cpp` 的 `main`（帧 1000B 左右）在规划期登记
        // `<agg_member_tmp>` 时，被上一个函数的 976B 帧判成 `E1128` 越界 —— 一条假硬错误。
        self.frame_layout = crate::codegen::x86_64::abi::layout::LayoutMap::unresolved();
        // 晚分配账面也是逐函数的（§11.23 步 2/3）。
        self.late_slot_budget = 0;
        self.late_slot_used = 0;
        self.late_var_cursor = 0;
        self.frame_planned = false;
        // 持有记录不得活过一个函数：不准则本函数开头的任何一次 `eval_held()` 会拿到
        // 上一个函数留下的号（那时代码里那个寄存器早已不是那个值）。
        self.eval_holding = None;
        // 前置二/三的两个函数级事实同样不得活过一个函数：覆盖表泄漏会把下一个函数的
        // 变量钉在错误的内存位置上；保存槽表则是本帧坐标，跳帧用它们是直接错值。
        self.home_overrides.clear();
        self.ext_home_saves.clear();
        self.vla_total_size = 0;
        // 静态扫描函数体是否含 VLA 声明 (决定跳转前是否恢复动态栈基)
        self.cur_func_has_vla = f
            .body
            .as_ref()
            .is_some_and(|b| Self::block_contains_vla(&b.items));
        // naked 形态必须在下面那个早返**之前**就定下来：内联 asm 的保存/恢复守护
        // （P12 Phase 5.2a）不得往用户的裸汇编里插 push/pop。
        self.cur_func_is_naked = f.is_naked;
        self.current_sret_offset = None;
        // 记录当前函数名 (用于嵌套函数重命名作用域)
        self.current_func_name = f.name.name.clone();
        // 记录当前函数返回类型 (用于 return 语句截断)
        self.current_func_ret_ty = f.ret_ty.clone();
        // 设置嵌套函数状态
        self.current_func_is_nested = f.is_nested;
        self.current_returns_nonnull = f.returns_nonnull;
        // noreturn: 函数末尾插入 trap 指令
        self.current_is_noreturn = f.is_noreturn;
        // 记录当前函数是否标记 no_instrument_function
        self.current_no_instrument = f.no_instrument;
        self.upvar_offsets.clear();
        // upvar_chain_depth / upvar_intermediate_offsets:
        // 嵌套函数的父级 handler 在调用 compile_func 之前通过
        // save→clear→set 预先填好这两个 map, 子 handler 负责
        // save/restore. compile_func 不再清理它们以免覆盖.
        // 顶层函数首次调用时 map 已为空 (new() 初始化), 后续
        // 顶层调用前由最后一个嵌套 handler 的 restore 清理干净.
        self.trampoline_slots.clear();
        self.trampoline_total_size = 0;
        // 收集当前函数体内定义的标签 (用于嵌套函数的 non-local goto 检测)
        self.local_labels.clear();
        // C++ 零成本异常: 受保护调用点与序言信息都是**每函数一份**; 残留上一条
        // 函数的调用点会让 LSDA 把区间登记到错误的函数 FDE 上。
        self.eh_sites.clear();
        self.cur_prologue_end = 0;
        if let Some(body) = &f.body {
            for item in &body.items {
                if let BlockItem::Stmt(s) = item {
                    Self::collect_local_labels_stmt(s, &mut self.local_labels);
                }
            }
        }
        if f.is_nested {
            for (name, off) in &f.upvars {
                self.upvar_offsets.insert(name.clone(), *off);
            }
        }
        let min_align = self
            .patchable_function_entry
            .map(|(n, _)| n.max(1))
            .unwrap_or(1);
        let func_align = f.alignment.unwrap_or(1).max(min_align).max(1);
        self.text_align = self.text_align.max(func_align as u64);
        while !self.code.len().is_multiple_of(func_align) {
            self.emit_insn(&Insn::nop());
        }
        let func_start = if let Some((n, m)) = self.patchable_function_entry {
            let prefix_len = m.min(n);
            let suffix_len = n.saturating_sub(prefix_len);
            self.text_align = self.text_align.max(n.max(1) as u64);
            for _ in 0..prefix_len {
                self.emit_insn(&Insn::nop());
            }
            let func_start = self.pos();
            // 使用 asm label 覆盖符号名 (如果有)
            let sym_name = self.resolve_asm_label(&f.name.name).to_string();
            self.label(&sym_name);
            for _ in 0..suffix_len {
                self.emit_insn(&Insn::nop());
            }
            func_start
        } else {
            let func_start = self.pos();
            // 使用 asm label 覆盖符号名 (如果有)
            let sym_name = self.resolve_asm_label(&f.name.name).to_string();
            self.label(&sym_name);
            func_start
        };
        // __attribute__((naked)): 不生成 prologue/epilogue，
        // 函数体完全由内联汇编组成，直接发射 asm 指令后返回。
        // 必须在 ftrace/prologue 之前检查，避免插入编译器生成的代码。
        if f.is_naked {
            self.compile_naked_func(f, prog, func_start);
            return;
        }
        // ftrace: -mfentry/-mrecord-mcount insert call __fentry__ before prologue
        if matches!(
            self.target.ftrace_mode,
            crate::cli::FtraceMode::FEntry | crate::cli::FtraceMode::RecordMCount
        ) {
            let call_pos = self.pos();
            self.call_label("__fentry__");
            // -mrecord-mcount: record call site address in __mcount_loc section
            if matches!(self.target.ftrace_mode, crate::cli::FtraceMode::RecordMCount) {
                self.emit_mcount_loc_entry(call_pos, &f.name.name);
            }
        }
        if self.target.frame_pointer {
            self.emit_insn(
                &Insn::stack_op_reg(crate::codegen::x86_64::opcode::StackOp::Push, 5)
                    .with_gas("push %rbp".to_string()),
            );
        }
        // 序言固定序列: push %r13; mov %rsp, %r13. 拆成两条 Insn 以便 -S 渲染各自的 gas 行.
        // 单条 5 字节发射会被 disasm 拆成两行 gas 文本, 渲染器无 gas_text 时只能 fallback.
        self.emit_insn(
            &Insn::stack_op_reg(crate::codegen::x86_64::opcode::StackOp::Push, 13)
                .with_gas("push %r13".to_string()),
        );
        self.emit_insn(
            &Insn::mov_rm_reg(
                crate::codegen::shared::asm_operand::Width::B64,
                13,
                4,
            )
            .with_gas("mov %rsp, %r13".to_string()),
        );
        if self.target.frame_pointer {
            self.emit_insn(
                &Insn::mov_rm_reg(
                    crate::codegen::shared::asm_operand::Width::B64,
                    5,
                    4,
                )
                .with_gas("mov %rsp, %rbp".to_string()),
            );
            // frame-pointer 模式下序言推了**两个**寄存器（rbp 与 r13），而入口时
            // rsp ≡ 8 (mod 16)，两次 8 字节之后仍就是 8 —— 本端的不变式是「体内每个 call
            // 点 rsp ≡ 0 (mod 16)」（`emit/stmt.rs` 里 pr44575 就是它的代价）。实测
            // `-fno-omit-frame-pointer` 下调用 printf("%.1f") 直接 SIGSEGV(139)，
            // 而 gcc 同一源正常。这里多下推 8 字节把它对齐；尾声用 `mov %rsp, %r13`
            // 复位栈指针，所以这 8 字节不需要（也不应该）手写回去。
            // 它必须在 `cur_prologue_end` 之前发出：FDE 的 CFA 规则从那个偏移之后才生效，
            // 落在规则之外的 prologue 差异展开器就看不到了。
            self.emit_insn(
                &Insn::sub_rsp_imm(8).with_gas("sub $8, %rsp".to_string()),
            );
        }
        // C++ 零成本异常: 序言到此结束 —— FDE 的 CFA 规则必须从这一偏移之后
        // 才生效 (之前 rsp 还在变动, CFA 沿用 CIE 的 `rsp + 8`)。
        self.cur_prologue_end = self.pos() - func_start;
        self.expr_depth = 0;
        self.max_expr_depth = 0;
        self.temp_slots = 0;
        self.bf_rmw_depth = 0;
        self.reset_reg_tracker();
        // 逃逸用的 `asm goto` clobber 掉的池号不得再当临时值发出（必须在任何发射**之前**
        // 定完，H1：第一个表达式就可能把值放进那个号）。与分配器读同一个判据。
        self.ban_escaping_asm_regs(f.body.as_ref());
        // 本函数序言/尾声补丁点在 sites 中的起始下标 (与嵌套函数隔离)
        let callee_sites_base = self.callee_save_patch_sites.len();
        if let Some(body) = &f.body {
            self.max_expr_depth = Self::max_expr_depth_stmt(&body.items);
        }
        let temp_slots = (self.max_expr_depth as i32 + 1).max(2);
        self.temp_slots = temp_slots;
        // sret/物化槽最大同时存活字节水位 (与运行时分配器同口径静态扫描)
        let sret_need: i64 = f
            .body
            .as_ref()
            .map_or(0, |b| Self::max_sret_bytes(&b.items));
        // KASAN 红区: 每个局部变量后追加 32 字节 (4 个 8 字节槽位)
        let kasan_rz_slots: i32 = if self.kasan { 4 } else { 0 };
        // Static 局部变量: 已由分析 pass 预注册到 data/bss,
        // 此处仅需计算 real_global_names (排除 static local 的 mangled 名)
        // (mangled 名以 ".Lsl" 开头, 不会与 C 标识符冲突)
        self.real_global_names = self
            .globals_raw
            .keys()
            .filter(|k| !k.starts_with(".Lsl"))
            .cloned()
            .collect();
        // VLA 形参: (名字, 维度表达式, 元素大小)。`char a[2][++N]`
        // 已退化为 Ptr(Array(Char, Var(++N))), 维度表达式保留但需在
        // 序言求值 (C11 6.9.1p10: 每次调用求值, 副作用生效)
        let mut vla_param_dims: Vec<(String, Expr, usize)> = Vec::new();
        // 一维 VLA 形参被丢弃的最外层维度表达式 (Param::vla_dim):
        // 仅需在入口求值副作用, 不影响类型语义 (C11 6.9.1p10)
        let mut param_dim_effects: Vec<Expr> = Vec::new();
        // spill band 的尺寸（P12 Phase 5.1.b）：由 `pre_register_vars` 在同一个逐声明循环里
        // 从**已交付的家**（`VarDecl::home == Spill(slot)`）数出来，而不是另开一份分配或扫描。
        // 今天分配器还不产出 Spill 家 ⇒ 两个分支都是 0 ⇒ 帧与今天逐字节相同（设计 §11.4 的
        // 「band 是解算输入，不是事后发明」在这里兑现）。
        let mut spill_slots: i64 = 0;
        if f.name.name != "main" || self.relocatable {
            // 先预注册函数体中的局部变量 (使参数偏移自然在局部变量下方).
            // 使用 var_map_total_slots 累加器替代 O(n) 遍历.
            if let Some(body) = &f.body {
                Self::pre_register_vars(
                    &mut self.var_map,
                    &body.items,
                    temp_slots,
                    kasan_rz_slots,
                    &self.static_local_map,
                    &f.name.name,
                    &mut self.var_map_total_slots,
                    &mut spill_slots,
                );
            }
            // 预计算已注册局部变量的总大小, 用于参数偏移计算.
            // limits-fndefn.c 有 100,000 个参数, 每次重新遍历 var_map
            // 会导致 O(n²) 超时. 使用累加值避免重复扫描.
            let var_contribution = |ty: &Type| -> i64 {
                match ty {
                    t if Self::type_has_vla(t) => t.size() as i64 + 16,
                    Type::Ptr(q)
                        if matches!(&q.base, Type::Array(_, crate::ast::ArraySize::Var(_))) =>
                    {
                        16
                    },
                    t => (t.size().div_ceil(8) * 8) as i64,
                }
            };
            let mut param_used_total: i64 = self
                .var_map
                .values()
                .filter(|(_, ty)| !matches!(ty, Type::Void))
                .map(|(_, ty)| var_contribution(ty))
                .sum();
            // 注册参数到 var_map (偏移公式自动避开已注册的局部变量)
            // 记录命名参数数量 (va_start 需要)
            self.variadic_named_params =
                f.params.iter().filter(|p| p.name.is_some()).count() as i32;
            self.variadic_named_gp = 0;
            self.variadic_named_fp = 0;
            // A1: 内存返回 (sret) — 隐藏第一参数 rdi 指向调用方缓冲区.
            // 真实参数从 rsi (gp_idx=1) 开始. sret 指针保存在帧底部专用槽
            // (帧建立后计算偏移), 避免与 alloc_sret_slot 的嵌套调用临时缓冲冲突.
            let has_sret = Self::needs_memory_return(&f.ret_ty);
            if has_sret {
                self.variadic_named_gp = 1;
            }
            let mut param_stores: Vec<(u8, i64)> = Vec::new(); // (gp reg, offset)
                                                               // (xmm reg, offset, ty, wide128): wide128=true 表示该 Sse chunk
                                                               // 与紧随的 SseUp 共用同一 xmm, 一次性保存完整 16 字节。
            let mut xmm_param_stores: Vec<(u8, i64, Type, bool)> = Vec::new();
            // (栈上字节偏移, var offset)。SysV 栈参数按类型对齐:
            // long double / __int128 / 16 字节对齐聚合体须落在 16 的
            // 倍数上, 否则 callee 读取位置与 caller 布局错位 (pr44942)。
            let mut stack_param_stores: Vec<(i64, i64)> = Vec::new();
            // D10-2: 栈传的具名 `long double` 形参单独登记：它们不能逐字搬运，
            // 必须在入口处把 SysV 的 80-bit 编码降级成 dcc 内部表示（见消费端）。
            let mut ld_param_stores: Vec<(i64, i64)> = Vec::new();
            // `_Complex long double` 具名形参: (入站 real 偏移, 局部 real 偏移)。
            // imag 紧随各自 real +16。入口处用 fldt/fstpt 逐分量拷贝 (见 Phase 2)。
            let mut cld_param_stores: Vec<(i64, i64)> = Vec::new();
            let mut gp_idx = if has_sret { 1 } else { 0 };
            let mut fp_idx = 0usize;
            let mut stack_arg_off: i64 = 0;
            for p in f.params.iter() {
                let nm_opt = p.name.as_ref();
                // VLA 形参检测: 记录维度表达式, 序言中求值并存 stride 槽
                if let Some(nm) = nm_opt {
                    if let Type::Ptr(q) = &p.ty {
                        if let Type::Array(elem, ArraySize::Var(se)) = &q.base {
                            vla_param_dims.push((
                                nm.name.clone(),
                                (**se).clone(),
                                elem.base.size(),
                            ));
                        }
                    }
                    if let Some(se) = &p.vla_dim {
                        param_dim_effects.push((**se).clone());
                    }
                }
                // __int128 / 聚合体按 8 字节块占用 GP 寄存器或栈槽
                // 所有 struct/union/array 均走聚合路径, 确保 SSE 类
                // 小结构体 (<=8B) 也能正确从 XMM 寄存器接收.
                //
                // 关键: gp_idx / fp_idx / stack_arg_off / param_used_total
                // 必须在有/无名字参数上**一致推进**, 因为 SysV ABI 的
                // 寄存器分配不看参数是否有名字 — 无名字参数 (如原型
                // `f(uint32_t, int32_t *r)`) 也要占 rdi, 后续有名字参数
                // 才会落到正确的寄存器 (rsi). 之前这些推进逻辑全部包
                // 在 `if let Some(nm)` 里, 导致 pr103209 / simple7 中
                // 有名字参数被错误地分配到前一个无名字参数应占据的寄
                // 存器, 栈槽里是垃圾值.
                let is_agg_param = matches!(&p.ty, Type::Int128 | Type::UInt128 | Type::Complex(_))
                    || matches!(
                        &p.ty,
                        Type::Struct(_, _, _, _)
                            | Type::Union(_, _, _, _)
                            | Type::Array(_, ArraySize::Const(_))
                            | Type::Atomic(_)
                            | Type::Vector(_, _)
                    );
                // complex LD 具名形参: 整体走内存, 两个 16 字节栈槽。
                // 与 caller emit_sysv_call 的规划保持一致, 不能进通用聚合
                // 8 字节 chunk 路径 (会破坏 tbyte)。
                let is_cld_param = matches!(&p.ty, Type::Complex(inner)
                    if matches!(&inner.base, Type::LongDouble));
                if is_cld_param {
                    let used_before = param_used_total;
                    param_used_total += 32;
                    let aligned = (stack_arg_off + 15) & !15;
                    stack_arg_off = aligned + 32;
                    if let Some(nm) = nm_opt {
                        let hdr = LayoutMap::hdr_of(temp_slots as i64);
                        // 32 字节 (real@off, imag@off+16) 在帧槽中占 4 个
                        // 8 字节槽。局部槽公式与标量 LD (16B→-(SIZE-8)=-8)
                        // 及通用聚合 base=-(u+hdr)-(chunks-1)*8 一致:
                        // SIZE=32 → -(32-8)=-24。误用 -16 会让 imag 槽顶 2
                        // 字节与相邻 GP 参数槽重叠 (tag qword 覆盖 imag
                        // 指数字节, complex LD 后随的指针/整数参数被踩坏)。
                        let off = -(used_before + hdr) - 24;
                        self.var_map.insert(nm.name.clone(), (off, p.ty.clone()));
                        cld_param_stores.push((aligned, off));
                    }
                } else if is_agg_param {
                    let classes = Self::abi_classes(&p.ty);
                    let chunks = p.ty.size().div_ceil(8);
                    let need_gp = classes.iter().filter(|c| **c == AbiClass::Integer).count();
                    // SseUp 是前一个 Sse 所在 xmm 高 64 位的延续, 不单独占用
                    // 寄存器, 故寄存器计数只数 Sse (SysV ABI 3.2.3)。
                    let need_fp = classes.iter().filter(|c| **c == AbiClass::Sse).count();
                    let agg_align = (p.ty.align() as i64).max(8);
                    let used_before = param_used_total;
                    param_used_total += (p.ty.size().div_ceil(8) * 8) as i64;
                    // SysV ABI: 聚合参数可以部分通过寄存器、部分通过栈传递。
                    // 例如 _Complex double 在只有 1 个 XMM 可用时，实部用寄存器，
                    // 虚部通过栈传递。必须逐个 chunk 处理，不能整体判断。
                    let hdr = LayoutMap::hdr_of(temp_slots as i64);
                    let base_off = -(used_before + hdr) - (chunks as i64 - 1) * 8;
                    if let Some(nm) = nm_opt {
                        self.var_map
                            .insert(nm.name.clone(), (base_off, p.ty.clone()));
                    }
                    let mut local_gp = gp_idx;
                    let mut local_fp = fp_idx;
                    // SysV 3.5.1 all-or-nothing: 任一 eightbyte 放不进剩余寄存器,
                    // 整个对象一次性走 overflow 区。必须与 caller `emit_sysv_call`
                    // 共用同一真源 (aggregate_forces_memory), 否则两侧对同一聚合
                    // 得出不同的拆分方案: caller 整体入栈、callee 却按"低半进
                    // r9 + 高半进栈"取参, 被调方读到的低半是垃圾。
                    let force_mem_agg =
                        Self::aggregate_forces_memory(&classes, 6 - local_gp, 8 - local_fp);
                    // 上一个 Sse chunk 的位置, SseUp 复用之 (同一 xmm 的高半)。
                    // Some((reg, off)): 在寄存器; Some((None, off)): 在栈。
                    let mut prev_sse: Option<(Option<u8>, i64)> = None;
                    for c in 0..chunks {
                        let raw_cls = classes.get(c).copied().unwrap_or(AbiClass::Memory);
                        // 整体走内存时把首 chunk 的分类改写成 Memory, 由下方
                        // Memory 分支一次性排好 chunks 个 overflow 槽并 break。
                        let cls = if force_mem_agg { AbiClass::Memory } else { raw_cls };
                        let chunk_off = base_off + c as i64 * 8;
                        match cls {
                            AbiClass::Integer => {
                                if local_gp < 6 {
                                    // 通过 GP 寄存器传递
                                    if nm_opt.is_some() {
                                        param_stores.push((arg_regs[local_gp], chunk_off));
                                        self.variadic_named_gp += 1;
                                    }
                                    local_gp += 1;
                                } else {
                                    // GP 寄存器不足，通过栈传递
                                    let aligned =
                                        (stack_arg_off + agg_align - 1) & !(agg_align - 1);
                                    stack_arg_off = aligned + 8;
                                    if nm_opt.is_some() {
                                        stack_param_stores.push((aligned, chunk_off));
                                        self.variadic_named_gp += 1;
                                    }
                                }
                            },
                            AbiClass::Sse => {
                                // 下一 chunk 是否为 SseUp: 若是, 本 Sse 与它共用
                                // 同一 xmm, 标记 wide128 一次性保存 16 字节。
                                let next_up = classes.get(c + 1).copied() == Some(AbiClass::SseUp);
                                if local_fp < 8 {
                                    // 通过 XMM 寄存器传递。仅当类型真正占满
                                    // 16 字节 (而非 16B 对齐但只含 8B double
                                    // 的 aligned_d) 时, 才与 SseUp 合并做一次
                                    // 128 位保存, 否则高半是相邻栈槽, 会被覆盖。
                                    let wide128 = next_up && p.ty.size() == 16;
                                    if nm_opt.is_some() {
                                        xmm_param_stores.push((
                                            local_fp as u8,
                                            chunk_off,
                                            p.ty.clone(),
                                            wide128,
                                        ));
                                        self.variadic_named_fp += 1;
                                    }
                                    prev_sse = Some((Some(local_fp as u8), chunk_off));
                                    local_fp += 1;
                                } else {
                                    // XMM 寄存器不足，通过栈传递
                                    let aligned =
                                        (stack_arg_off + agg_align - 1) & !(agg_align - 1);
                                    stack_arg_off = aligned + 8;
                                    if nm_opt.is_some() {
                                        stack_param_stores.push((aligned, chunk_off));
                                        self.variadic_named_fp += 1;
                                    }
                                    prev_sse = Some((None, aligned));
                                }
                            },
                            AbiClass::SseUp => {
                                // SseUp 不占新寄存器: 它复用前一个 Sse 的同一
                                // xmm 的高 64 位。寄存器情形已由 Sse 以 wide128
                                // 一次性 movups 保存, 这里不再推进 fp、不发 store;
                                // 仅当 Sse 落在栈上时, 高 8 字节位于相邻栈槽。
                                match prev_sse {
                                    Some((Some(_), _)) => {
                                        // 寄存器: 无需额外动作 (wide128 已覆盖)
                                    },
                                    Some((None, prev_aligned)) => {
                                        if nm_opt.is_some() {
                                            stack_param_stores.push((prev_aligned + 8, chunk_off));
                                        }
                                        stack_arg_off = prev_aligned + 16;
                                    },
                                    None => {},
                                }
                                // 计数需要与 caller 一致: SseUp 不增加已用 xmm。
                            },
                            AbiClass::Memory
                            | AbiClass::X87
                            | AbiClass::X87Up
                            | AbiClass::ComplexX87 => {
                                // 强制内存类：整个参数通过栈传递
                                let aligned = (stack_arg_off + agg_align - 1) & !(agg_align - 1);
                                stack_arg_off = aligned + chunks as i64 * 8;
                                if nm_opt.is_some() {
                                    for i in 0..chunks {
                                        let off = base_off + i as i64 * 8;
                                        stack_param_stores.push((aligned + i as i64 * 8, off));
                                    }
                                    self.variadic_named_gp += need_gp as i32;
                                    self.variadic_named_fp += need_fp as i32;
                                }
                                break; // 所有 chunks 都已处理
                            },
                            AbiClass::NoClass => unreachable!(),
                        }
                    }
                    // 更新全局索引
                    gp_idx = local_gp;
                    fp_idx = local_fp;
                } else if matches!(&p.ty, Type::LongDouble) && self.target.sse {
                    let used_before = param_used_total;
                    param_used_total += 16;
                    let aligned = (stack_arg_off + 15) & !15;
                    stack_arg_off = aligned + 16;
                    if let Some(nm) = nm_opt {
                        let hdr = LayoutMap::hdr_of(temp_slots as i64);
                        let off = -(used_before + hdr) - 8;
                        self.var_map.insert(nm.name.clone(), (off, p.ty.clone()));
                        // 不走逐字 8+8 搬运：改由 ld_param_stores 在入口降级。
                        ld_param_stores.push((aligned, off));
                    }
                } else if matches!(&p.ty, Type::Float | Type::Double) && self.target.sse {
                    let used_before = param_used_total;
                    param_used_total += 8;
                    if fp_idx < 8 {
                        let saved_fp = fp_idx;
                        fp_idx += 1;
                        if let Some(nm) = nm_opt {
                            let hdr = LayoutMap::hdr_of(temp_slots as i64);
                            let off = -(used_before + hdr);
                            self.var_map.insert(nm.name.clone(), (off, p.ty.clone()));
                            xmm_param_stores.push((saved_fp as u8, off, p.ty.clone(), false));
                            self.variadic_named_fp += 1;
                        }
                    } else {
                        let my_stack_off = stack_arg_off;
                        stack_arg_off += 8;
                        if let Some(nm) = nm_opt {
                            let hdr = LayoutMap::hdr_of(temp_slots as i64);
                            let off = -(used_before + hdr);
                            self.var_map.insert(nm.name.clone(), (off, p.ty.clone()));
                            stack_param_stores.push((my_stack_off, off));
                            self.variadic_named_fp += 1;
                        }
                    }
                } else if gp_idx < 6 {
                    let used_before = param_used_total;
                    let reg = arg_regs[gp_idx];
                    param_used_total += 8;
                    gp_idx += 1;
                    if let Some(nm) = nm_opt {
                        let hdr = LayoutMap::hdr_of(temp_slots as i64);
                        let off = -(used_before + hdr);
                        self.var_map.insert(nm.name.clone(), (off, p.ty.clone()));
                        param_stores.push((reg, off));
                        self.variadic_named_gp += 1;
                    }
                } else {
                    let used_before = param_used_total;
                    param_used_total += 8;
                    let my_stack_off = stack_arg_off;
                    stack_arg_off += 8;
                    if let Some(nm) = nm_opt {
                        let hdr = LayoutMap::hdr_of(temp_slots as i64);
                        let off = -(used_before + hdr);
                        self.var_map.insert(nm.name.clone(), (off, p.ty.clone()));
                        stack_param_stores.push((my_stack_off, off));
                        self.variadic_named_gp += 1;
                    }
                }
            }
            // 记录栈传参数量 (va_start overflow_arg_area 需要):
            // overflow_arg_area 指向最后一个命名栈参数之后,
            // 与 caller 的对齐布局一致
            self.variadic_overflow_offset = 16 + stack_arg_off;
            // 将参数占用的栈空间计入 var_map_total_slots,
            // 使后续 register_var 运行时分配 (如 compound literal 临时变量)
            // 不会与参数栈槽重叠 (根因: compound lit temp 分配到参数 s 的
            // 栈位导致 pr114207.c 等测试 segfault/abort)
            let param_slots = param_used_total / 8;
            self.var_map_total_slots = self.var_map_total_slots.saturating_add(param_slots);
            // 与上面同一根因、同一条轴: 复合字面量 / VM 尺寸元信息等只在代码生成
            // 时才 `register_var` 的对象现在有自己的一条带（§11.23 步 3），它的尺寸就是
            // 本函数在这里定下的 `late_slot_budget`，没有再挤变量区。
            self.plan_late_slot_budget(f.body.as_ref());
            // 帧布局**解算**（P12 Phase 2）：以前这里是一段现场求和，现在只保留
            // 输入；所有带边界由 `abi::layout::LayoutMap::resolve` 一次算完。
            // 求和项与求和顺序与搬迁前字面一致（`var_bytes + hdr + sret_need +
            // sc_slot + cx_band + va_slots + vec_reserve + bf_rmw_reserve +
            // tramp_reserve + stack_reserve`，再 16 对齐）。
            let var_bytes: i64 = self.var_map_total_slots.saturating_mul(8);
            let sc_slot: i64 = if f.is_nested { 8 } else { 0 }; // 嵌套函数需要静态链槽位
            let va_slots: i64 = if f.is_variadic {
                if self.target.sse {
                    48 + 128
                } else {
                    48
                }
            } else {
                0
            };
            // spill band：尺寸来自上面那个逐声明循环（`spill_slots`）。Phase 5.1 今天
            // 分配器不产出 Spill 家 ⇒ 它仍是 0，但band 已经是布局解算的**输入**，所以
            // 打开溢出时不会再长出第七个推算者。
            let layout = crate::codegen::x86_64::abi::layout::LayoutMap::resolve(
                &crate::codegen::x86_64::abi::layout::FramePlan {
                    temp_slots: temp_slots as i64,
                    var_bytes,
                    // 晚分配带（§11.23 步 3）：尺寸就是规划期那份可枚举预算。
                    // 带为 0 的函数（一个发射期对象也没有）仍与今天逐字节相同。
                    late_slots: self.late_slot_budget,
                    sret_need,
                    va_slots,
                    static_chain_slot: sc_slot,
                    bf_rmw_reserve: crate::codegen::x86_64::abi::layout::BF_RMW_RESERVE,
                    vec_reserve: crate::codegen::x86_64::abi::layout::VEC_RESERVE,
                    tramp_reserve: crate::codegen::x86_64::abi::layout::TRAMP_RESERVE,
                    stack_reserve: self.stack_reserve_bytes(),
                    spill_slots,
                },
            );
            self.frame_layout = layout;
            // sret/物化槽区顶界：紧邻变量区底（变量区占 -(hdr+var_bytes)）。
            // 这一条**生成期会变**（`register_var` 会把它跟变量区一起往下压），
            // 所以它留在 Codegen 当游标；而 `frame_size` / `vec_temp_off` 解算后不再变，
            // 镜像字段已删，花费者一律直读 `LayoutMap`（帧的坐标从此只有一份）。
            self.sret_region_base = layout.sret_region_base;
            if self.frame_layout.frame_size > 0 {
                if self.target.stack_clash_protection && self.frame_layout.frame_size > 4096 {
                    self.emit_stack_clash_probes(self.frame_layout.frame_size);
                } else {
                    self.emit_sub_rsp(self.frame_layout.frame_size);
                }
            }
            // -ftrivial-auto-var-init=zero: 零初始化整个栈帧. 对超大帧跳过.
            if self.target.auto_var_init_zero
                && self.frame_layout.frame_size > 0
                && self.frame_layout.frame_size <= 16 * 1024 * 1024
            {
                self.emit_zero_init_frame(self.frame_layout.frame_size);
            }
            // Stack canary: 在帧顶部 (r13-8) 存储金丝雀值
            if self.target.stack_protector && self.frame_layout.canary_fits() {
                // mov rax, seg:[0x28]  → 加载 canary
                // seg = fs (0x64) or gs (0x65) based on guard register
                let seg_prefix = match self.target.stack_protector_guard_reg {
                    crate::cli::StackGuardReg::Gs => 0x65u8,
                    _ => 0x64u8, // Fs is default
                };
                self.emit_insn(&Insn::raw_bytes(&[seg_prefix, 0x48, 0x8b, 0x04, 0x25, 0x28, 0x00, 0x00, 0x00]));
                // mov [r13 - 8], rax  → 存储在帧顶部
                self.emit_insn(&Insn::mov_rm_mem(
                    false,
                    crate::codegen::shared::asm_operand::Width::B64,
                    0,
                    13,
                    -8,
                ));
            }
            // KASAN 栈红区: 毒化整个帧, 然后解除变量区域的毒化
            if self.kasan && self.frame_layout.frame_size > 0 {
                self.emit_kasan_stack_poison_all();
                self.emit_kasan_stack_unpoison_vars();
            }
            // 被调用者保存寄存器: rbx, r12, r14, r15 保存在帧顶部
            // (r13-16 到 r13-40), 用作表达式求值的额外暂存寄存器.
            // 这些寄存器在函数调用后被调用者保证保留, 因此可以跨 call 使用.
            // 生成时先无条件发射 (单遍编译, 此时函数体尚未生成,
            // callee_saved_used 未知), 记录补丁点; 函数体生成完毕后,
            // 未使用的寄存器指令由 patch_callee_saves 改写为 nop.
            // mov [r13 - 16], rbx  (REX.WB: r13 needs REX.B)
            self.callee_save_patch_sites.push((self.pos(), 0x01));
            self.emit_insn(
                &Insn::mov_rm_mem(
                    false,
                    crate::codegen::shared::asm_operand::Width::B64,
                    3,
                    13,
                    -16,
                )
                .with_gas("mov %rbx, -16(%r13)".to_string()),
            );
            // mov [r13 - 24], r12  (REX.WRB: r12 needs REX.R, r13 needs REX.B)
            self.callee_save_patch_sites.push((self.pos(), 0x02));
            self.emit_insn(
                &Insn::mov_rm_mem(
                    false,
                    crate::codegen::shared::asm_operand::Width::B64,
                    12,
                    13,
                    -24,
                )
                .with_gas("mov %r12, -24(%r13)".to_string()),
            );
            // mov [r13 - 32], r14  (REX.WRB: r14 needs REX.R, r13 needs REX.B)
            self.callee_save_patch_sites.push((self.pos(), 0x04));
            self.emit_insn(
                &Insn::mov_rm_mem(
                    false,
                    crate::codegen::shared::asm_operand::Width::B64,
                    14,
                    13,
                    -32,
                )
                .with_gas("mov %r14, -32(%r13)".to_string()),
            );
            // mov [r13 - 40], r15  (REX.WRB: r15 需要 REX.R, r13 需要 REX.B)
            // 这里的 modrm 是 `mod=01 rm=101`（r13+disp8），**没有 SIB**，所以 REX.X
            // 是多余的：旧代码发 0x4F = W|R|X|B，产物里被 objdump 老实标成
            // `rex.WRXB mov %r15,-0x28(%r13)`。X 只在有 SIB 时表达 index 扩展。
            self.callee_save_patch_sites.push((self.pos(), 0x08));
            self.emit_insn(
                &Insn::mov_rm_mem(
                    false,
                    crate::codegen::shared::asm_operand::Width::B64,
                    15,
                    13,
                    -40,
                )
                .with_gas("mov %r15, -40(%r13)".to_string()),
            );
            // 变参 reg save area 放在 frame 底部 (紧贴 rsp 之后).
            // 帧布局 (自下而上):
            //   r13+0 saved r13 / r13+8 ret addr
            //   r13-16..-48 callee-saved 保存区 (rbx/r12/r14/r15)
            //   r13-48..-(48+temp_slots*8) binop temp_slots (槽 d 在 -(48+d*8))
            //   r13-(48+temp_slots*8)..-(48+temp_slots*8+var_bytes) 变量
            //   r13-frame_size..r13-frame_size+cx_band 复数 rvalue 物化带 (见 CX_FRAME_BAND)
            //   r13-frame_size+cx_band..+cx_band+va_slots 变参 reg save area
            // 关键: save area 必须在变量下方, 否则 XMM 槽位会与
            //       变量 (尤其 for-init 的 i) 重叠, 导致变参读取
            //       错误返回循环变量值. 而 save area 下方还要留出复数物化带,
            //       否则复数内存对会写坏 GP 槽 1 (保存的 rsi), 使后续
            //       `va_arg` 从 gp_offset=8 读回复数位模式。
            self.variadic_reg_save_offset = self.frame_layout.variadic_reg_save_off;
            if f.is_variadic {
                for (i, reg) in arg_regs.iter().enumerate() {
                    self.store_stack(*reg, self.variadic_reg_save_offset + i as i64 * 8);
                }
                if self.target.sse {
                    for i in 0..8u8 {
                        self.store_xmm128_stack(
                            i,
                            self.variadic_reg_save_offset + 48 + i as i64 * 16,
                        );
                    }
                }
            }
            // 嵌套函数: 保存静态链. 调用方约定以 r10 传入父帧指针
            // (gen_call: mov r10, r13; trampoline: mov r10, r13), 序言必须
            // 保存同一寄存器. 注意不能用 rdx: rdx 是第 3 个整型参数寄存器,
            // 嵌套函数有 ≥3 个整型参数时静态链会覆盖第 3 个参数.
            if f.is_nested {
                self.static_chain_offset = self.frame_layout.static_chain_off;
                // mov [r13 + static_chain_offset], r10
                self.store_stack(10, self.static_chain_offset);
                // 登记本帧链槽偏移: 后代嵌套函数多层上溯静态链时逐帧使用
                self.nested_chain_offsets
                    .insert(self.current_func_name.clone(), self.static_chain_offset);
            }
            // ftrace: -pg inserts call mcount after prologue
            if matches!(self.target.ftrace_mode, crate::cli::FtraceMode::MCount) {
                self.call_label("mcount");
            }
            // Phase 2: 帧已建立 (r13 已设置), 现在存储参数到栈槽
            for (reg, off) in param_stores {
                self.store_stack(reg, off);
            }
            for (xmm, off, ty, wide128) in xmm_param_stores {
                if matches!(ty, Type::Float) {
                    // K&R / 无原型函数: float 参数经默认提升后以 double 传入.
                    // callee 必须先将 xmm 中的 double 转为 float 再存储.
                    if f.is_old_style {
                        // cvtsd2ss xmmN, xmmN: double → float
                        // 两个 ModRM 字段都是同一个号 ⇒ xmm>=8 必须同时置 REX.R 与 REX.B。
                        // 旧写法先算 `let lo = xmm & 7` 再拿 `lo >= 8` 判扩张，而掩蔽后的
                        // 值永远 <8，于是 xmm 落在 8..15 时两个字段都编成了低 8 号
                        // （写的不是 %xmm8-15而是 %xmm0-7）。
                        self.emit_insn(&Insn::raw_bytes(&[0xF2]));
                        if xmm >= 8 {
                            self.emit_insn(&Insn::raw_bytes(&[0x45])); // REX.R+B，同一个号既在 reg 又在 r/m
                        }
                        self.emit_insn(&Insn::raw_bytes(&[0x0F, 0x5A, 0xC0 | ((xmm & 7) << 3) | (xmm & 7)]));
                    }
                    self.store_xmm32_stack(xmm, off);
                } else if wide128 || (ty.size() == 16 && matches!(ty, Type::Vector(_, _))) {
                    // 128-bit vector 参数 (如 __int128 vector), 或聚合中
                    // [Sse, SseUp] 共用同一 xmm 的 16 字节 (如含向量字段的
                    // 结构体): 保存完整 16 字节。
                    self.store_xmm128_stack(xmm, off);
                } else {
                    self.store_xmm64_stack(xmm, off);
                }
            }
            for (arg_off, off) in ld_param_stores {
                // D10-2（已简化）：SysV AMD64 下 x87 **参数走内存** —— 每个具名
                // `long double` 占一个 16 字节栈槽，内容是调用方 `fstpt` 写出的
                // **真 80-bit 编码**（gcc callee 就是 `fldt 0x10(%rbp)` 读它）。
                //
                // 旧实现在此做 `fldt` → `fstp qword` → `movq $0, off+8` 的“入口
                // 降级一次”，目的是维持“局部槽低 8 字节 = f64 位型、高 8 字节 = 0”
                // 这个旧不变量。现在对象表示与 rvalue 值通道都已是真 80-bit，那次
                // 降级反而**把调用方送来的 64 位尾数舍回 53 位**（并且 `fstp qword`
                // 不写指数字节，配合槽高位清零会把值读成 0）。故改为槽→槽的
                // `fldt`/`fstpt` 直拷，不再有任何精度损失。
                let incoming = 16i64 + arg_off;
                // P11 Bucket AR: typed x87 fldt/fstpt via emit_x87_fld_at/fstp_at
                // (base=r13). Width=10 m80fp maps to (0xDB, /5, /7); encoder
                // derives REX.B=0x41 and mod=10 disp32 automatically.
                self.emit_x87_fld_at(13, incoming as i32, &Type::LongDouble);
                self.emit_x87_fstp_at(13, off as i32, &Type::LongDouble);
            }
            for (arg_off, off) in cld_param_stores {
                // complex LD: 两个 16 字节栈槽, 逐分量 fldt/fstpt 直拷
                // (real@arg_off→off, imag@arg_off+16→off+16), 无精度损失。
                for k in 0..2 {
                    let incoming = 16i64 + arg_off + k * 16;
                    let dst = off + k * 16;
                    self.emit_x87_fld_at(13, incoming as i32, &Type::LongDouble);
                    self.emit_x87_fstp_at(13, dst as i32, &Type::LongDouble);
                }
            }
            for (arg_off, off) in stack_param_stores {
                // 栈传参位于调用方栈顶: 经过 call 压入的返回地址与
                // 被调方 push r13 后, 第一个栈参数位于 [r13 + 16].
                // arg_off 是字节偏移 (含对齐 padding, 与 caller 一致).
                // (变参 overflow_arg_area 同样以 16 为基址 — 见 emit_va_start)
                let incoming = 16i64 + arg_off;
                self.load_stack(0, incoming);
                self.store_stack(0, off);
            }
            // A1: 保存 sret 指针 (rdi) 到帧底部专用槽, 供返回时写回聚合结果
            if has_sret {
                // 位于变参保存区之上、向量临时区之下 (静态链同位置, 二者互斥)
                let sret_off = self.frame_layout.sret_ptr_off;
                self.store_stack(7, sret_off);
                self.current_sret_offset = Some(sret_off);
            }
        } else {
            if let Some(body) = &f.body {
                Self::pre_register_vars(
                    &mut self.var_map,
                    &body.items,
                    temp_slots,
                    kasan_rz_slots,
                    &self.static_local_map,
                    &f.name.name,
                    &mut self.var_map_total_slots,
                    &mut spill_slots,
                );
            }
            // 包含 padding 条目, 每个占 8 字节. 使用累加器替代 O(n) 遍历.
            self.plan_late_slot_budget(f.body.as_ref());
            let var_bytes: i64 = self.var_map_total_slots.saturating_mul(8);
            // 同一份布局解算（无参/`main` 分支）。该分支今天不**参与**变参 save area /
            // 静态链 / 位域 RMW 三个预留（见 `abi/layout.rs` 文件头记的「不对称 1」），
            // 于是这三个带在这里传 0：不对称从「两份公式」变成「可见的输入」，
            // Phase 2 不改它（改了就是改产物）。
            let layout = crate::codegen::x86_64::abi::layout::LayoutMap::resolve(
                &crate::codegen::x86_64::abi::layout::FramePlan {
                    temp_slots: temp_slots as i64,
                    var_bytes,
                    late_slots: self.late_slot_budget,
                    sret_need,
                    va_slots: 0,
                    static_chain_slot: 0,
                    bf_rmw_reserve: 0,
                    vec_reserve: crate::codegen::x86_64::abi::layout::VEC_RESERVE,
                    tramp_reserve: crate::codegen::x86_64::abi::layout::TRAMP_RESERVE,
                    stack_reserve: self.stack_reserve_bytes(),
                    spill_slots,
                },
            );
            self.frame_layout = layout;
            // sret/物化槽区顶界: 紧邻变量区底 (变量区占 -(hdr+var_bytes))。
            self.sret_region_base = layout.sret_region_base;
            if self.frame_layout.frame_size > 0 {
                if self.target.stack_clash_protection && self.frame_layout.frame_size > 4096 {
                    self.emit_stack_clash_probes(self.frame_layout.frame_size);
                } else {
                    self.emit_sub_rsp(self.frame_layout.frame_size);
                }
            }
            // -ftrivial-auto-var-init=zero: 零初始化整个栈帧. 对超大帧跳过.
            if self.target.auto_var_init_zero
                && self.frame_layout.frame_size > 0
                && self.frame_layout.frame_size <= 16 * 1024 * 1024
            {
                self.emit_zero_init_frame(self.frame_layout.frame_size);
            }
            // Stack canary: 在帧顶部 (r13-8) 存储金丝雀值
            if self.target.stack_protector && self.frame_layout.canary_fits() {
                // mov rax, seg:[0x28]  → 加载 canary
                // seg = fs (0x64) or gs (0x65) based on guard register
                let seg_prefix = match self.target.stack_protector_guard_reg {
                    crate::cli::StackGuardReg::Gs => 0x65u8,
                    _ => 0x64u8, // Fs is default
                };
                self.emit_insn(&Insn::raw_bytes(&[seg_prefix, 0x48, 0x8b, 0x04, 0x25, 0x28, 0x00, 0x00, 0x00]));
                // mov [r13 - 8], rax  → 存储在帧顶部
                self.emit_insn(&Insn::raw_bytes(&[0x49, 0x89, 0x85, 0xf8, 0xff, 0xff, 0xff]));
            }
            // KASAN 栈红区: 毒化整个帧, 然后解除变量区域的毒化
            if self.kasan && self.frame_layout.frame_size > 0 {
                self.emit_kasan_stack_poison_all();
                self.emit_kasan_stack_unpoison_vars();
            }
        }
        // VLA 形参维度求值: 序言尾 (非 main 路径两分支汇合点) 执行,
        // 此时帧已建立、参数槽与静态链槽均已写入。副作用 (++N 等) 每次
        // 调用生效 (C11 6.9.1p10); 行大小 = 维度表达式值 * 元素大小,
        // 存入形参指针槽前 off-8 的 stride 槽, 供 sizeof(a[i]) 与索引
        // 步长读取声明时绑定的行大小 (C11 6.2.4p7)。
        // 一维 VLA 形参维度副作用求值 (声明顺序, 先于 stride 求值:
        // 多维参数的内层维度表达式可能依赖外层维度副作用后的值)
        for se in &param_dim_effects {
            self.gen_expr(se, Self::EVAL_BASE);
        }
        for (pname, se, elem_sz) in &vla_param_dims {
            let off = self.var_offset(pname);
            self.gen_expr(se, Self::EVAL_BASE);
            if *elem_sz != 1 {
                self.imul_imm(0, *elem_sz as i128);
            }
            self.emit_store_rax_r13(off - 8);
        }
        // 预收集嵌套函数（编译体之前需要它们的声明列表拿 trampoline 槽）。
        // 唯一符号名已由 `compile_module` 的预扫描一次性定完（见
        // `elf::prescan_nested_names`）：这里**只查不分配**。旧形状在本函数里递增
        // 全局计数并改写三张表，并行下每个 worker 从同一份快照起号、回传又不存
        // 这些表 ⇒ 并行与串行符号名不同且嵌套函数符号整个失踪（N1）。
        let mut nested_decls: Vec<FuncDecl> = Vec::new();
        if let Some(body) = &f.body {
            Self::collect_direct_nested_funcs(&body.items, &mut nested_decls);
        }
        // 嵌套函数 trampoline 预分配 + 运行时写入 (在重命名之后):
        // 为全部直接嵌套函数预分配 trampoline 槽 (不仅是被取地址者):
        // 兄弟/后代嵌套函数的引用经静态链访问定义父帧中的 trampoline,
        // 编译后代时父帧序言已发射完毕, 无法回补槽位, 必须在此统一定位。
        let parent_name = f.name.name.clone();
        for nf_decl in &nested_decls {
            let orig_name = nf_decl.name.name.clone();
            let unique_name = self
                .nested_func_renames
                .get(&parent_name)
                .and_then(|m| m.get(&orig_name))
                .cloned()
                .unwrap_or_else(|| orig_name.clone());
            let tramp_off = self.gen_trampoline(&unique_name);
            self.nested_tramp_offsets.insert(unique_name, tramp_off);
        }
        if !self.trampoline_slots.is_empty() {
            self.emit_mprotect_for_stack();
            self.emit_trampoline_to_stack();
        }
        // -finstrument-functions: 在函数序言后插入 __cyg_profile_func_enter 调用
        if self.target.instrument_functions && !self.current_no_instrument {
            self.emit_instrument_func_enter();
        }
        let ends_ret = if let Some(body) = &f.body {
            for item in &body.items {
                // 顶层声明的行号必须在这里记：本循环把 `BlockItem::Decl(Decl::Var)` 就地
                // 展开（不经过 `gen_block_item`），而 `Stmt` 分支走 `gen_stmt` 自带行号。
                // 缺了这一条，`int t = sink(0);` 这样的声明行在 `.debug_line` 里根本没有
                // entry，gdb 的 `break file.c:那一行` 依旧绑不上。
                if let BlockItem::Decl(Decl::Var(v)) = item {
                    self.note_src_line(v.span.line);
                }
                // Stmt 边界 save/restore 物化槽水位；Decl 变量槽已加入 var_map，全程保留
                // (va_arg 分配的聚合临时槽属于 Stmt，需回收，变量定义槽无需回收)。
                match item {
                    BlockItem::Stmt(s) => {
                        // Stmt 单独处理：从当前水位开始，结束时回收槽位（Decl 变量槽全程保留）
                        let mark = self.save_sret_watermark();
                        self.gen_stmt(s);
                        self.restore_sret_watermark(mark);
                    },
                    BlockItem::Decl(Decl::Var(v)) => {
                        if v.is_static
                            && self
                                .static_local_map
                                .is_static_local(&self.current_func_name, &v.name.name)
                        {
                            if self
                                .static_local_map
                                .needs_runtime_init(&self.current_func_name, &v.name.name)
                            {
                                // static local 需要运行时初始化: 发射初始化代码到全局地址
                                if let Some(init) = &v.init {
                                    self.gen_expr(init, Self::EVAL_BASE);
                                    self.emit_global_store(0, &v.name.name);
                                }
                                continue;
                            }
                            continue; // static 局部变量已在全局数据段分配
                        }
                        // 声明处的绑定走唯一入口 `bind_decl_home`（以前这里是 `compile_func`
                        // 自己抄的一份 `gen_block_items` 逻辑，两处必须同步改）。
                        self.bind_decl_home(v);
                        if let Some(ref fn_name) = v.cleanup_fn {
                            self.cleanup_vars
                                .push((v.name.name.clone(), fn_name.clone()));
                        }
                        // 定义+声明一体的 struct/union 类型绑定 (同 gen_block_items)
                        if matches!(v.ty, Type::Struct(_, _, _, _) | Type::Union(_, _, _, _)) {
                            self.gen_vm_type_bind(&v.ty);
                        }
                        self.register_var(&v.name.name, &v.ty);
                        if Self::type_has_vla(&v.ty) {
                            self.gen_vla_alloc(v);
                        } else if let Some(init) = &v.init {
                            if let Expr::InitList { inits, .. } = init {
                                self.gen_init_list(&v.name.name, &v.ty, inits);
                            } else if let Expr::String { data: str_data, .. } = init {
                                // 字符数组用字符串字面量初始化: char s[N] = "str";
                                // 复制字符串内容到数组，剩余元素零填充
                                // 注意: 指针初始化 (char *p = "str") 不走此路径
                                if matches!(&v.ty, Type::Array(_, _)) {
                                    let arr_size = v.ty.size();
                                    let off = self.var_offset(&v.name.name);
                                    let copy_len = str_data.len().min(arr_size);
                                    // 复制字符串字节到栈上数组
                                    for (i, &byte) in str_data[..copy_len].iter().enumerate() {
                                        self.mov_imm(0, byte as i128);
                                        self.emit_store(0, off + i as i64, 1);
                                    }
                                    // 零填充剩余元素
                                    for i in copy_len..arr_size {
                                        self.mov_imm(0, 0i128);
                                        self.emit_store(0, off + i as i64, 1);
                                    }
                                } else {
                                    // 指针初始化: 生成字符串地址
                                    self.gen_expr(init, Self::EVAL_BASE);
                                    self.mov_to_stack(0, &v.name.name);
                                }
                            } else {
                                let is_vec_init = matches!(&v.ty, Type::Vector(_, _));
                                let is_agg = matches!(
                                    &v.ty,
                                    Type::Struct(_, _, _, _)
                                        | Type::Union(_, _, _, _)
                                        | Type::Array(_, ArraySize::Const(_))
                                        | Type::Atomic(_)
                                ) && v.ty.size() > 8;
                                // CompoundLit returns an address in rax, not a value.
                                // For struct/union types (any size), we must copy the
                                // struct bytes from the compound-literal temp to the
                                // variable, not store the pointer itself.
                                let needs_compound_copy = matches!(init, Expr::CompoundLit { .. })
                                    && matches!(
                                        &v.ty,
                                        Type::Struct(_, _, _, _) | Type::Union(_, _, _, _)
                                    );
                                if is_vec_init || is_agg || needs_compound_copy {
                                    // struct/array/vector 初始化: 整体拷贝
                                    let dst_ty = v.ty.clone();
                                    let dst_name = v.name.name.clone();
                                    self.gen_aggregate_assign(&dst_name, &dst_ty, init);
                                } else if let Type::Complex(inner) = &v.ty {
                                    self.gen_complex_init(&v.name.name, &inner.base, init);
                                } else if let Type::Imaginary(ref inner) = &v.ty {
                                    // _Imaginary 变量初始化: 存储单个浮点值 (虚部)
                                    self.gen_imaginary_init(&v.name.name, &inner.base, init);
                                } else if matches!(
                                    &v.ty,
                                    Type::Float | Type::Double | Type::LongDouble
                                ) {
                                    self.gen_expr(init, Self::EVAL_BASE);
                                    // int 常量初始化浮点变量: 先转换到 xmm0 (double x = 1;)
                                    self.emit_scalar_to_xmm0_if_needed(init, &v.ty);
                                    self.gen_float_store(&v.name.name, &v.ty);
                                } else if matches!(&v.ty, Type::Int128 | Type::UInt128) {
                                    // P1-4 fix: __int128 变量初始化 (rax=low, rdx=high)
                                    self.gen_expr(init, Self::EVAL_BASE);
                                    // 如果初始化器不是 int128 类型，需要符号扩展
                                    if !matches!(init.ty(), Type::Int128 | Type::UInt128) {
                                        if matches!(
                                            init.ty(),
                                            Type::Bool
                                                | Type::UChar
                                                | Type::UShort
                                                | Type::UInt
                                                | Type::ULong
                                                | Type::ULongLong
                                        ) {
                                            // P11: 收敛到 AluRm{Xor, B64, rdx, rdx}.
                                            let insn = Insn::new(
                                                crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                                                    op: crate::codegen::x86_64::opcode::AluOp::Xor,
                                                    width: crate::codegen::shared::asm_operand::Width::B64,
                                                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                                        2,
                                                        crate::codegen::shared::asm_operand::Width::B64,
                                                    ),
                                                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                                                        2,
                                                        crate::codegen::shared::asm_operand::Width::B64,
                                                    ),
                                                },
                                                &[],
                                            );
                                            if !self.emit_insn(&insn) {
                                                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::AluRm { op: crate::codegen::x86_64::opcode::AluOp::Xor, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(2, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Reg(2, crate::codegen::shared::asm_operand::Width::B64)}, &[]));
                                            }
                                        } else {
                                            self.emit_insn(&Insn::sign_extend_rax_to_rdx(crate::codegen::shared::asm_operand::Width::B64)); // cqo
                                        }
                                    }
                                    let off = self.var_offset(&v.name.name);
                                    self.store_stack(0, off); // low → [r13+off]
                                    self.store_stack(2, off + 8); // high → [r13+off+8]
                                } else {
                                    self.gen_expr(init, Self::EVAL_BASE);
                                    // Implicit float→int conversion
                                    let init_ty = init.ty();
                                    if matches!(
                                        &init_ty,
                                        Type::Float | Type::Double | Type::LongDouble
                                    ) && Self::is_integer_type(&v.ty)
                                    {
                                        self.emit_implicit_float_to_int(0, &init_ty, &v.ty);
                                    }
                                    self.mov_to_stack(0, &v.name.name);
                                }
                            }
                        }
                    },
                    // 函数体顶层的局部 struct/union 定义: 含 VLA 字段时
                    // 声明点绑定总大小槽 (与 gen_block_items 同处理)
                    BlockItem::Decl(Decl::StructDef(sd)) => {
                        self.gen_vm_struct_bind(sd, false);
                    },
                    BlockItem::Decl(Decl::UnionDef(sd)) => {
                        self.gen_vm_struct_bind(sd, true);
                    },
                    _ => {},
                }
            }
            Self::ends_with_return(&body.items)
        } else {
            false
        };
        if !ends_ret {
            // C99 5.1.2.2.3: main 无 return 语句时隐式 return 0.
            // 否则 epilogue 直接 ret, eax 为启动代码残留值.
            if f.name.name == "main" && matches!(f.ret_ty, Type::Int) {
                self.emit_insn(&Insn::new(
                    crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                        rip_relative: false,
                        width: crate::codegen::shared::asm_operand::Width::B32,
                        dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B32),
                        src: crate::codegen::x86_64::opcode::Operand::Imm(0),
                    },
                    &[],
                )); // mov eax, 0
            }
            // -finstrument-functions: 自然结尾的函数也需要 exit hook
            // (之前只在 Stmt::Return 路径发射, fallthrough 到函数末尾时遗漏)
            if self.target.instrument_functions && !self.current_no_instrument {
                self.emit_instrument_func_exit();
            }
            self.emit_func_epilogue();
        }
        // 记录函数大小
        let func_end = self.pos();
        self.func_sizes
            .insert(f.name.name.clone(), func_end - func_start);
        // C++ 零成本异常: 归档本函数的受保护调用点与 CFI 事实。必须在嵌套函数
        // 生成之前完成 (嵌套函数会重新 clear `eh_sites`)。
        if !self.eh_sites.is_empty() {
            self.eh_funcs
                .push(crate::codegen::x86_64::eh_tables::EhFuncInfo {
                    name: f.name.name.clone(),
                    text_start: func_start,
                    sites: std::mem::take(&mut self.eh_sites),
                });
        }
        self.func_cfi.insert(
            f.name.name.clone(),
            crate::codegen::x86_64::eh_tables::FuncCfi {
                prologue_end: self.cur_prologue_end,
                callee_saved_used: self.reg_bank.used_mask(),
                frame_pointer: self.target.frame_pointer,
            },
        );
        // 函数体+尾声已生成, callee_saved_used 完整: 回填序言/尾声中
        // 未使用寄存器的保存/恢复指令为 nop. 必须在嵌套函数生成前
        // (嵌套函数会 reset tracker 并追加自己的 sites).
        self.patch_callee_saves(callee_sites_base);
        // Phase 6.2：本帧的位置表在此刻是完整的（`homes` 与 `var_map` 都是最终值），
        // 而下一个函数一开始就把两者复位 —— 所以采集必须卡在这里，嵌套函数之前
        // （见 `collect_debug_var_facts` 的注释）。
        self.collect_debug_var_facts(f);
        // 父函数布局确定后再生成嵌套函数，把捕获变量的真实 r13 偏移传入。
        // 使用预收集的 nested_decls 和预注册的重命名
        if !nested_decls.is_empty() {
            let parent_var_map = self.var_map.clone();
            let parent_name = f.name.name.clone();
            let parent_static_chain_off = self.static_chain_offset;
            // 父级 upvars: name → (origin_offset, chain_depth, intermediate_offsets)
            let parent_upvar_info: HashMap<String, (i64, u32, Vec<i64>)> = f
                .upvars
                .iter()
                .map(|(n, o)| {
                    let off = *o;
                    // 父函数的 upvar 深度 = 当前 upvar_chain_depth.get(n)?
                    // 若无 (一级 upvar), 默认为 1, intermediate 为空
                    let depth = self.upvar_chain_depth.get(n).copied().unwrap_or(1);
                    let intermediates = self
                        .upvar_intermediate_offsets
                        .get(n)
                        .cloned()
                        .unwrap_or_default();
                    (n.clone(), (off, depth, intermediates))
                })
                .collect();
            for mut nf_decl in nested_decls {
                // 计算 upvar 偏移 + 链深度 + 中间偏移
                // intermediates 语义: load_upvar 先做一次
                //   r11 = [child_r13 + child.chain_off] → 父帧
                // 然后对每个 intermediate 做
                //   r11 = [r11 + intermediate] → 再上溯一层
                // 最后访问 [r11 + origin_off]
                // depth=1 (直接父): intermediates = []  (父帧即起源帧)
                // depth=2 (祖父): intermediates = [parent.chain_off]  (再上溯一次到祖父)
                let mut new_upvars: Vec<(String, i64, u32, Vec<i64>)> = Vec::new();
                for (name, off) in &mut nf_decl.upvars {
                    if let Some((parent_off, _)) = parent_var_map.get(name) {
                        // 直接父级 upvar: depth=1, intermediates=[]
                        *off = *parent_off;
                        new_upvars.push((name.clone(), *off, 1, vec![]));
                    } else if let Some((origin_off, parent_depth, parent_intermediates)) =
                        parent_upvar_info.get(name)
                    {
                        // 祖父级 upvar: depth+1, intermediates = [parent.chain_off] + parent_intermediates
                        let mut intermediates = vec![parent_static_chain_off];
                        intermediates.extend(parent_intermediates.iter().copied());
                        *off = *origin_off;
                        new_upvars.push((name.clone(), *off, parent_depth + 1, intermediates));
                    } else {
                        eprintln!(
                            "[WARN] nested upvar {} not found in parent var_map or upvars",
                            name
                        );
                    }
                }
                // 写入新的 upvars + 临时设置 self 的链深度/中间偏移
                nf_decl.upvars = new_upvars
                    .iter()
                    .map(|(n, o, _, _)| (n.clone(), *o))
                    .collect();
                // 保存父级链状态, 设置子级链状态
                let saved_upvar_chain_depth = self.upvar_chain_depth.clone();
                let saved_upvar_intermediate = self.upvar_intermediate_offsets.clone();
                self.upvar_chain_depth.clear();
                self.upvar_intermediate_offsets.clear();
                for (name, _off, depth, intermediates) in &new_upvars {
                    self.upvar_chain_depth.insert(name.clone(), *depth);
                    self.upvar_intermediate_offsets
                        .insert(name.clone(), intermediates.clone());
                }
                // 从预注册的重命名映射中查找唯一名称
                let orig_name = nf_decl.name.name.clone();
                let unique_name = self
                    .nested_func_renames
                    .get(&parent_name)
                    .and_then(|m| m.get(&orig_name))
                    .cloned()
                    .unwrap_or(orig_name.clone());
                // 重命名函数声明并编译
                nf_decl.name.name = unique_name.clone();
                // 将嵌套函数加入 defined_funcs，这样 ELF 发射器知道它的符号
                self.defined_funcs.insert(unique_name.clone());
                self.compile_func(&nf_decl, prog);
                // 恢复父级链状态 (父函数可能还有兄弟嵌套函数)
                self.upvar_chain_depth = saved_upvar_chain_depth;
                self.upvar_intermediate_offsets = saved_upvar_intermediate;
            }
        }
        // 缺陷 B: 本函数 (含上面刚发的嵌套函数) 收尾时, 指向内部 `.L` 标号的
        // 跳转必须已全部可解析。并行 worker 里这一条把「标号在别的 chunk 被
        // 同名覆盖」从静默误编译变成发射期错误 (而不是等到合并/回填之后)。
        self.check_pending_labels_closed(&f.name.name);
    }
}
