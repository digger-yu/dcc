// src/codegen/x86_64/emit/functions.rs
// Function-level driver: walks the program's function definitions and
// delegates each one to compile_func. Manually migrated from the
// pre-refactor monolithic x86_64.rs (commit 2ff718b) during solution v2
// phase 0.

use crate::ast::*;
use crate::codegen::shared::elf as shared_elf;
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
    pub fn compile_func(&mut self, f: &FuncDecl, prog: &Program) {
        let arg_regs: [u8; 6] = [7, 6, 2, 1, 8, 9];
        self.var_map.clear();
        self.var_map_total_slots = 0;
        self.vla_offsets.clear();
        // VM 类型尺寸绑定槽是每函数帧内状态 (槽偏移在各函数独立
        // 分配), 跨函数残留会导致 sizeof 读到别的函数帧的槽
        // (20040423-1: 匿名 struct 的空 tag 在 main/sub1 同 key)
        self.vm_struct_size_slots.clear();
        self.reg_bindings.clear();
        self.reg_bindings.extend(self.global_reg_bindings.clone());
        self.cleanup_vars.clear();
        self.sret_watermark = 0;
        self.sret_region_base = 0;
        self.vla_total_size = 0;
        // 静态扫描函数体是否含 VLA 声明 (决定跳转前是否恢复动态栈基)
        self.cur_func_has_vla = f
            .body
            .as_ref()
            .is_some_and(|b| Self::block_contains_vla(&b.items));
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
            self.code.push(0x90);
        }
        let func_start = if let Some((n, m)) = self.patchable_function_entry {
            let prefix_len = m.min(n);
            let suffix_len = n.saturating_sub(prefix_len);
            self.text_align = self.text_align.max(n.max(1) as u64);
            for _ in 0..prefix_len {
                self.code.push(0x90);
            }
            let func_start = self.pos();
            // 使用 asm label 覆盖符号名 (如果有)
            let sym_name = self.resolve_asm_label(&f.name.name).to_string();
            self.label(&sym_name);
            for _ in 0..suffix_len {
                self.code.push(0x90);
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
            self.emit(&[0x55]); // push rbp
        }
        self.emit(&[0x41, 0x55, 0x49, 0x89, 0xe5]);
        if self.target.frame_pointer {
            self.emit(&[0x48, 0x89, 0xE5]); // mov rbp, rsp
        }
        // C++ 零成本异常: 序言到此结束 —— FDE 的 CFA 规则必须从这一偏移之后
        // 才生效 (之前 rsp 还在变动, CFA 沿用 CIE 的 `rsp + 8`)。
        self.cur_prologue_end = self.pos() - func_start;
        self.expr_depth = 0;
        self.max_expr_depth = 0;
        self.temp_slots = 0;
        self.bf_rmw_depth = 0;
        self.reset_reg_tracker();
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
                        let hdr = 48i64 + temp_slots as i64 * 8;
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
                    let hdr = 48i64 + temp_slots as i64 * 8;
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
                        let hdr = 48i64 + temp_slots as i64 * 8;
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
                            let hdr = 48i64 + temp_slots as i64 * 8;
                            let off = -(used_before + hdr);
                            self.var_map.insert(nm.name.clone(), (off, p.ty.clone()));
                            xmm_param_stores.push((saved_fp as u8, off, p.ty.clone(), false));
                            self.variadic_named_fp += 1;
                        }
                    } else {
                        let my_stack_off = stack_arg_off;
                        stack_arg_off += 8;
                        if let Some(nm) = nm_opt {
                            let hdr = 48i64 + temp_slots as i64 * 8;
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
                        let hdr = 48i64 + temp_slots as i64 * 8;
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
                        let hdr = 48i64 + temp_slots as i64 * 8;
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
            // 时才 `register_var` 的对象也走变量区, 需在定 `sret_region_base` 前预扣。
            self.reserve_late_var_slots(f.body.as_ref());
            // frame_size = 变量实际字节 + 16(保存寄存器) + temp_slots*8
            // 包含 padding 条目, 每个占 8 字节. 使用累加器替代 O(n) 遍历.
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
            // 向量临时槽区: 预留 512 字节 (32 个 16B 槽), 位于 va save/静态链上方,
            // 避免 vector_temp_base 与变量区重叠。嵌套向量 binop 每层
            // 需要更深的操作数槽 (XMM 路径每层 2 槽), 128 字节只够
            // 3 层嵌套, 深链式表达式会溢出到变量区 (pr23135)
            let vec_reserve: i64 = 512;
            // 帧底复数/16B 聚合 rvalue 物化带 (见 `CX_FRAME_BAND`)。本函数其余
            // 帧底偏移一律从带**之上**起算, 使 `-frame_size+8` 的复数内存对既不
            // 落在变参 GP 保存槽里, 也不与向量临时槽 0 重叠。
            let cx_band = Self::CX_FRAME_BAND;
            // 位域 RMW 临时槽: 预留 32 字节 (2 级嵌套, 每级 16 字节)
            let bf_rmw_reserve: i64 = 32;
            // 嵌套函数 trampoline 空间: 预留 256 字节 (最多 8 个 trampoline, 每个 32 字节)
            // 实际 trampoline 代码在代码生成阶段按需写入栈帧
            let tramp_reserve: i64 = 256;
            // 帧头: 16 (saved r13 + ret addr) + 32 (callee-saved) + temp_slots*8
            let hdr: i64 = 48 + temp_slots as i64 * 8;
            let raw: i64 = var_bytes
                .saturating_add(hdr)
                .saturating_add(sret_need)
                .saturating_add(sc_slot)
                .saturating_add(cx_band)
                .saturating_add(va_slots)
                .saturating_add(vec_reserve)
                .saturating_add(bf_rmw_reserve)
                .saturating_add(tramp_reserve)
                .saturating_add(self.stack_reserve_bytes());
            self.frame_size = (raw + 15) & !15; // 16 字节对齐
                                                // sret/物化槽区顶界: 紧邻变量区底 (变量区占 -(hdr+var_bytes))。
            self.sret_region_base = -(hdr + var_bytes);
            // vec 区下移 sret 槽深度, 使槽区夹在变量区与 vec 区之间。
            // 帧底先留出复数物化带 (cx_band), 变参 save area 紧随其后。
            self.vec_temp_off = -self.frame_size + cx_band + va_slots + sc_slot + 8;
            if self.frame_size > 0 {
                if self.target.stack_clash_protection && self.frame_size > 4096 {
                    self.emit_stack_clash_probes(self.frame_size);
                } else {
                    self.emit_sub_rsp(self.frame_size);
                }
            }
            // -ftrivial-auto-var-init=zero: 零初始化整个栈帧. 对超大帧跳过.
            if self.target.auto_var_init_zero
                && self.frame_size > 0
                && self.frame_size <= 16 * 1024 * 1024
            {
                self.emit_zero_init_frame(self.frame_size);
            }
            // Stack canary: 在帧顶部 (r13-8) 存储金丝雀值
            if self.target.stack_protector && self.frame_size >= 8 {
                // mov rax, seg:[0x28]  → 加载 canary
                // seg = fs (0x64) or gs (0x65) based on guard register
                let seg_prefix = match self.target.stack_protector_guard_reg {
                    crate::cli::StackGuardReg::Gs => 0x65u8,
                    _ => 0x64u8, // Fs is default
                };
                self.emit(&[seg_prefix, 0x48, 0x8b, 0x04, 0x25, 0x28, 0x00, 0x00, 0x00]);
                // mov [r13 - 8], rax  → 存储在帧顶部
                self.emit(&[0x49, 0x89, 0x85, 0xf8, 0xff, 0xff, 0xff]);
            }
            // KASAN 栈红区: 毒化整个帧, 然后解除变量区域的毒化
            if self.kasan && self.frame_size > 0 {
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
            self.emit(&[0x49, 0x89, 0x5d, 0xf0]);
            // mov [r13 - 24], r12  (REX.WRB: r12 needs REX.R, r13 needs REX.B)
            self.callee_save_patch_sites.push((self.pos(), 0x02));
            self.emit(&[0x4d, 0x89, 0x65, 0xe8]);
            // mov [r13 - 32], r14  (REX.WRB: r14 needs REX.R, r13 needs REX.B)
            self.callee_save_patch_sites.push((self.pos(), 0x04));
            self.emit(&[0x4d, 0x89, 0x75, 0xe0]);
            // mov [r13 - 40], r15  (REX.WRB: r15 needs REX.R+REX.B, r13 needs REX.B)
            self.callee_save_patch_sites.push((self.pos(), 0x08));
            self.emit(&[0x4f, 0x89, 0x7d, 0xd8]);
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
            self.variadic_reg_save_offset = -self.frame_size + cx_band;
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
                self.static_chain_offset = self.variadic_reg_save_offset + va_slots;
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
                        let lo = xmm & 7;
                        let mut rex = 0u8;
                        if lo >= 8 {
                            rex |= 0x01;
                        } // REX.B
                        if lo >= 8 {
                            rex |= 0x04;
                        } // REX.R
                        if rex != 0 {
                            self.emit(&[
                                0xF2,
                                rex | 0x40,
                                0x0F,
                                0x5A,
                                0xC0 | ((lo & 7) << 3) | (lo & 7),
                            ]);
                        } else {
                            self.emit(&[0xF2, 0x0F, 0x5A, 0xC0 | (lo << 3) | lo]);
                        }
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
                // fld tbyte [r13+incoming]  DB /5: mod=10|reg=5|rm=101 = 0xAD, REX.B=0x41
                // （写成 0x85 会漏掉 reg 字段而变成 `fildl m32int`，静默读 4 字节整数）
                self.emit(&[0x41, 0xDB, 0xAD]);
                self.emit(&(incoming as i32).to_le_bytes());
                // fstp tbyte [r13+off]      DB /7: mod=10 reg=7 rm=101 → 0xBD
                self.emit(&[0x41, 0xDB, 0xBD]);
                self.emit(&(off as i32).to_le_bytes());
            }
            for (arg_off, off) in cld_param_stores {
                // complex LD: 两个 16 字节栈槽, 逐分量 fldt/fstpt 直拷
                // (real@arg_off→off, imag@arg_off+16→off+16), 无精度损失。
                for k in 0..2 {
                    let incoming = 16i64 + arg_off + k * 16;
                    let dst = off + k * 16;
                    self.emit(&[0x41, 0xDB, 0xAD]);
                    self.emit(&(incoming as i32).to_le_bytes());
                    self.emit(&[0x41, 0xDB, 0xBD]);
                    self.emit(&(dst as i32).to_le_bytes());
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
                let sret_off = -self.frame_size + cx_band + va_slots + sc_slot;
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
                );
            }
            // 包含 padding 条目, 每个占 8 字节. 使用累加器替代 O(n) 遍历.
            self.reserve_late_var_slots(f.body.as_ref());
            let var_bytes: i64 = self.var_map_total_slots.saturating_mul(8);
            // 帧头: 16 (saved r13 + ret addr) + 32 (callee-saved) + temp_slots*8
            let hdr: i64 = 48 + temp_slots as i64 * 8;
            let tramp_reserve: i64 = 256; // 嵌套函数 trampoline 空间
            let raw = var_bytes
                .saturating_add(hdr)
                .saturating_add(sret_need)
                .saturating_add(Self::CX_FRAME_BAND) // 帧底复数 rvalue 物化带
                .saturating_add(512) // 向量临时槽区 (嵌套 binop 操作数槽)
                .saturating_add(tramp_reserve)
                .saturating_add(self.stack_reserve_bytes());
            self.frame_size = (raw + 15) & !15;
            // sret/物化槽区顶界: 紧邻变量区底 (变量区占 -(hdr+var_bytes))。
            self.sret_region_base = -(hdr + var_bytes);
            // 向量临时区从复数物化带之上开始 (`-frame_size+8` 属于复数内存对)。
            self.vec_temp_off = -self.frame_size + Self::CX_FRAME_BAND + 8;
            if self.frame_size > 0 {
                if self.target.stack_clash_protection && self.frame_size > 4096 {
                    self.emit_stack_clash_probes(self.frame_size);
                } else {
                    self.emit_sub_rsp(self.frame_size);
                }
            }
            // -ftrivial-auto-var-init=zero: 零初始化整个栈帧. 对超大帧跳过.
            if self.target.auto_var_init_zero
                && self.frame_size > 0
                && self.frame_size <= 16 * 1024 * 1024
            {
                self.emit_zero_init_frame(self.frame_size);
            }
            // Stack canary: 在帧顶部 (r13-8) 存储金丝雀值
            if self.target.stack_protector && self.frame_size >= 8 {
                // mov rax, seg:[0x28]  → 加载 canary
                // seg = fs (0x64) or gs (0x65) based on guard register
                let seg_prefix = match self.target.stack_protector_guard_reg {
                    crate::cli::StackGuardReg::Gs => 0x65u8,
                    _ => 0x64u8, // Fs is default
                };
                self.emit(&[seg_prefix, 0x48, 0x8b, 0x04, 0x25, 0x28, 0x00, 0x00, 0x00]);
                // mov [r13 - 8], rax  → 存储在帧顶部
                self.emit(&[0x49, 0x89, 0x85, 0xf8, 0xff, 0xff, 0xff]);
            }
            // KASAN 栈红区: 毒化整个帧, 然后解除变量区域的毒化
            if self.kasan && self.frame_size > 0 {
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
            self.gen_expr(se, 0);
        }
        for (pname, se, elem_sz) in &vla_param_dims {
            let off = self.var_offset(pname);
            self.gen_expr(se, 0);
            if *elem_sz != 1 {
                self.imul_imm(0, *elem_sz as i128);
            }
            self.emit_store_rax_r13(off - 8);
        }
        // 预收集嵌套函数并预注册重命名 (在编译函数体之前，这样调用点能使用正确的名称)
        let mut nested_decls: Vec<FuncDecl> = Vec::new();
        if let Some(body) = &f.body {
            Self::collect_direct_nested_funcs(&body.items, &mut nested_decls);
            let parent_name = f.name.name.clone();
            for nf_decl in &nested_decls {
                let orig_name = nf_decl.name.name.clone();
                let unique_name =
                    format!("{}_{}.{}", orig_name, parent_name, self.nested_func_rename_counter);
                self.nested_func_rename_counter += 1;
                // 使用作用域化的重命名: (父函数名, 原名) → 唯一名
                self.nested_func_renames
                    .entry(parent_name.clone())
                    .or_default()
                    .insert(orig_name.clone(), unique_name.clone());
                // 记录定义父函数: 后代嵌套函数引用兄弟/祖先嵌套函数时,
                // 沿此映射上溯静态链找到定义父帧
                self.nested_func_parent
                    .insert(unique_name.clone(), parent_name.clone());
                // 更新 nested_funcs 映射
                if let Some(upvars) = self.nested_funcs.remove(&orig_name) {
                    self.nested_funcs.insert(unique_name, upvars);
                }
            }
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
                                    self.gen_expr(init, 0);
                                    self.emit_global_store(0, &v.name.name);
                                }
                                continue;
                            }
                            continue; // static 局部变量已在全局数据段分配
                        }
                        if let Some(ref reg_name) = v.register_name {
                            if let Some(reg_idx) = Self::parse_reg_name(reg_name) {
                                self.reg_bindings.insert(v.name.name.clone(), reg_idx);
                            }
                        }
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
                                    self.gen_expr(init, 0);
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
                                    self.gen_expr(init, 0);
                                    // int 常量初始化浮点变量: 先转换到 xmm0 (double x = 1;)
                                    self.emit_scalar_to_xmm0_if_needed(init, &v.ty);
                                    self.gen_float_store(&v.name.name, &v.ty);
                                } else if matches!(&v.ty, Type::Int128 | Type::UInt128) {
                                    // P1-4 fix: __int128 变量初始化 (rax=low, rdx=high)
                                    self.gen_expr(init, 0);
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
                                            self.emit(&[0x48, 0x31, 0xD2]);
                                        } else {
                                            self.emit(&[0x48, 0x99]); // cqo
                                        }
                                    }
                                    let off = self.var_offset(&v.name.name);
                                    self.store_stack(0, off); // low → [r13+off]
                                    self.store_stack(2, off + 8); // high → [r13+off+8]
                                } else {
                                    self.gen_expr(init, 0);
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
                self.emit(&[0xB8, 0, 0, 0, 0]); // mov eax, 0
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
                callee_saved_used: self.callee_saved_used,
                frame_pointer: self.target.frame_pointer,
            },
        );
        // 函数体+尾声已生成, callee_saved_used 完整: 回填序言/尾声中
        // 未使用寄存器的保存/恢复指令为 nop. 必须在嵌套函数生成前
        // (嵌套函数会 reset tracker 并追加自己的 sites).
        self.patch_callee_saves(callee_sites_base);
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
