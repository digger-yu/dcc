// src/codegen/x86_64/abi/call.rs
// SysV AMD64 调用约定的 caller 侧实现。
// 本文件由 mod.rs 逐字搬迁而来 (batch 3): `emit_sysv_call` 原样剪切至 `impl Codegen`,
// 语义 diff 为 0。原先的 `unimplemented!()` 影子存根与 `ArgLoc` 影子枚举已一并作废。

use crate::ast::*;
use crate::codegen::x86_64::{AbiClass, ArgLoc, Codegen};

impl Codegen {
    /// Lower one SysV AMD64 call.  Outgoing stack arguments occupy the lowest
    /// addresses, temporary register staging follows them, and the complete
    /// area is rounded to 16 bytes so `%rsp` is aligned immediately before call.
    pub fn emit_sysv_call(
        &mut self,
        direct: Option<&str>,
        callee: Option<&Expr>,
        args: &[Expr],
        sret_off: Option<i64>,
    ) {
        let gp_regs = [7u8, 6, 2, 1, 8, 9];
        let mut gp = usize::from(sret_off.is_some());
        let mut fp = 0usize;
        let mut stack = 0usize;
        let mut locs: Vec<Vec<ArgLoc>> = Vec::with_capacity(args.len());
        let mut max_force_align = 8usize; // force_mem 区起始对齐 (至少 8)

        for (arg_idx, arg) in args.iter().enumerate() {
            let param_ty = direct
                .and_then(|name| self.func_param_types.get(name))
                .and_then(|ps| ps.get(arg_idx))
                .cloned()
                .or_else(|| {
                    // 间接调用: 从函数指针类型提取参数类型
                    if let Some(callee_expr) = callee {
                        let callee_ty = callee_expr.ty();
                        if let Type::Ptr(ref p) = callee_ty {
                            if let Type::Func(_, ref params) = p.base {
                                return params.get(arg_idx).map(|q| q.base.clone());
                            }
                        }
                    }
                    None
                });
            // C 语义: 数组实参传参时衰减为指向首元素的指针
            // (int a[8]; f(a) 即 f(&a[0])). 若按数组类型分类, 聚合
            // 路径会把整个数组按值拷贝到参数暂存区, 导致
            // printf("%s", buf) 这类调用读到垃圾. 衰减为指针后
            // 走标量路径 gen_expr_decay 传地址.
            let mut effective_ty = match param_ty.unwrap_or_else(|| arg.ty()) {
                Type::Array(elem, _) => Type::Ptr(elem),
                t => t,
            };
            // C 默认参数提升: K&R (无原型) 函数的 float 参数以 double 传递
            if direct
                .map(|n| self.old_style_funcs.contains(n))
                .unwrap_or(false)
                && matches!(effective_ty, Type::Float)
            {
                effective_ty = Type::Double;
            }
            let classes = Self::abi_classes(&effective_ty);
            let need_gp = classes.iter().filter(|c| **c == AbiClass::Integer).count();
            // SseUp 是前一个 Sse 所在 xmm 的高 64 位延续, 不单独占用寄存器。
            let need_fp = classes.iter().filter(|c| **c == AbiClass::Sse).count();
            // Struct/union 类型走 gen_va_arg_struct, 不支持混合 reg_save_area +
            // overflow_arg_area 路径, 所以仍然强制溢出时整类型上栈。
            // 标量类型 (__int128, long long 等) 走 gen_va_arg, 支持三路径,
            // 允许单个 chunk 溢出时落到 overflow_arg_area。
            let is_agg = matches!(effective_ty, Type::Struct(_, _, _, _) | Type::Union(_, _, _, _));
            let force_mem = classes.iter().any(|c| {
                matches!(
                    c,
                    AbiClass::Memory | AbiClass::X87 | AbiClass::X87Up | AbiClass::ComplexX87
                )
            }) || (is_agg && (gp + need_gp > gp_regs.len() || fp + need_fp > 8));
            if force_mem {
                // caller 与 callee 必须用相同的 force_mem 栈参数对齐规则:
                // 命名参数 (gen_params) 用 agg_align = ty.align().max(8),
                // 变参 (gen_va_arg_struct overflow) 也做相同对齐.
                // 这里 caller 继续按 type 声明对齐放置 force_mem 栈参数,
                // 同时确保 force_mem 区起点 (sub 后 rsp) 对齐到最大对齐值.
                let align = effective_ty.align().max(8);
                max_force_align = max_force_align.max(align);
                let aligned = (stack + align - 1) & !(align - 1);
                stack = aligned;
                let size = effective_ty.size().max(8);
                let chunks = size.div_ceil(8);
                locs.push((0..chunks).map(|i| ArgLoc::Stack(stack + i * 8)).collect());
                stack += chunks * 8;
            } else {
                let mut arg_locs = Vec::new();
                // SysV ABI: SseUp 永远是前一个 Sse 所在 xmm 高 64 位的延续,
                // 不单独分配寄存器。记录上一个 Sse chunk 分到的 ArgLoc,
                // SseUp 直接复用之 (寄存器 → 同一 xmm; 栈 → 相邻栈槽)。
                // 适用于一切 16 字节 [Sse, SseUp] 聚合 (含向量字段的结构体、
                // 独立 __m128 向量等); 两个独立 double 字段产出的是
                // [Sse, Sse] (两个不同 xmm), 不会进入此复用路径。
                let mut prev_sse_loc: Option<ArgLoc> = None;
                for class in classes {
                    match class {
                        AbiClass::Integer => {
                            if gp < gp_regs.len() {
                                arg_locs.push(ArgLoc::Gp(gp_regs[gp]));
                                gp += 1;
                            } else {
                                arg_locs.push(ArgLoc::Stack(stack));
                                stack += 8;
                            }
                            prev_sse_loc = None;
                        },
                        AbiClass::Sse => {
                            let loc = if fp < 8 {
                                let l = ArgLoc::Xmm(fp as u8);
                                fp += 1;
                                l
                            } else {
                                let l = ArgLoc::Stack(stack);
                                stack += 8;
                                l
                            };
                            prev_sse_loc = Some(loc);
                            arg_locs.push(loc);
                        },
                        AbiClass::SseUp => {
                            // 复用前一个 Sse 的位置, 不推进 fp/stack。
                            match prev_sse_loc {
                                Some(loc @ ArgLoc::Xmm(_)) => {
                                    arg_locs.push(loc);
                                },
                                Some(ArgLoc::Stack(prev)) => {
                                    // 高 8 字节在相邻栈槽
                                    let hi = ArgLoc::Stack(prev + 8);
                                    stack = prev + 16;
                                    arg_locs.push(hi);
                                },
                                _ => {
                                    // 防御性回退: 不应发生
                                    arg_locs.push(ArgLoc::Stack(stack));
                                    stack += 8;
                                },
                            }
                        },
                        AbiClass::Memory
                        | AbiClass::X87
                        | AbiClass::X87Up
                        | AbiClass::ComplexX87 => {
                            unreachable!()
                        },
                        AbiClass::NoClass => unreachable!(),
                    }
                }
                locs.push(arg_locs);
            }
        }

        let register_pieces = locs
            .iter()
            .flatten()
            .filter(|l| !matches!(l, ArgLoc::Stack(_)))
            .count();
        let hidden_slots = usize::from(sret_off.is_some());
        let callee_scratch_slots = usize::from(direct.is_none());
        let stage_base = (stack + 7) & !7;
        // Indirect calls need one extra scratch slot for the callee.
        // This slot must be included in frame_bytes; otherwise it can
        // alias caller temporaries already pushed above the call frame
        // (e.g. `lhs = (*fp)(...)` overwrites the saved lhs address).
        let stage_slots = register_pieces + hidden_slots + callee_scratch_slots;
        // force_mem 区起点 (sub 后 rsp) 必须对齐到 force_mem 参数的最大对齐值.
        // 否则 align=32 的 agg 会在 16 对齐的地址上错位.
        let frame_align = max_force_align.max(16); // 至少 16 (SysV ABI call 对齐)
        let frame_bytes = (stage_base + stage_slots * 8 + frame_align - 1) & !(frame_align - 1);
        // call 前 rsp 只保证 16 对齐 (SysV ABI). 若 force_mem 区需要 32 对齐,
        // 先 sub 一个 padding 让 call 前 rsp 向上对齐到 frame_align.
        // padding = (call_rsp + frame_align - 1) & ~(frame_align - 1) - call_rsp
        if frame_align > 16 {
            // call 前 rsp 向上对齐到 frame_align.
            // padding = ((rsp + frame_align - 1) & ~(frame_align - 1)) - rsp
            // 原实现先 sub rax, rsp (AT&T: rsp -= rax) 再 neg rax 再 sub,
            // 但中间 sub 错误地破坏了 rsp, 最终 rsp 回到原值, 对齐完全
            // 白做. 结果 32B-aligned 栈参数错位 (pr92904).
            // 修复：保存 rsp_old 到 r10，算 aligned 到 rax,
            // padding = aligned - rsp_old = -((rsp_old - aligned)),
            // 再 sub rsp, padding.
            let _r10: u8 = 10; // caller-saved scratch
                               // mov r10, rsp: REX.W=1 B=1; 0x89 mov r/m, r; modrm 0xE2 (mod=3, reg=4=rsp, r/m=2=r10 low)
            self.emit(&[0x49, 0x89, 0xE2]);
            // lea rax, [rsp + frame_align - 1]; and rax, ~(frame_align - 1)
            self.emit(&[0x48, 0x8D, 0x44, 0x24]);
            self.emit(&[(frame_align - 1) as u8]);
            self.emit(&[0x48, 0x83, 0xE0]);
            self.emit(&[!(frame_align - 1) as u8]);
            // rax = aligned, r10 = rsp_old
            // sub r10, rax (r10 -= rax, 结果为负): 0x29 sub r/m, r; REX.WB; modrm 0xC2 (reg=0=rax, r/m=2=r10 low)
            self.emit(&[0x49, 0x29, 0xC2]);
            // neg r10 (padding = aligned - rsp_old): 0xF7 group 3, /5=NEG; REX.WB; modrm 0xDA (reg=101, r/m=2)
            self.emit(&[0x49, 0xF7, 0xDA]);
            // sub rsp, r10 (应用 padding): 0x29 sub r/m, r; REX.WR; modrm 0xD4 (mod=3, reg=2=r10 low, r/m=4=rsp)
            self.emit(&[0x4C, 0x29, 0xD4]);
        }
        self.emit_sub_rsp(frame_bytes as i64);

        let mut stage = stage_base;
        let sret_slot = if let Some(off) = sret_off {
            self.emit_lea_r13(0, off);
            self.store_rsp_qword(0, stage);
            let slot = stage;
            stage += 8;
            Some(slot)
        } else {
            None
        };
        // For indirect calls, callee is stored in a separate scratch area,
        // not in the parameter staging area (which is used for register-passed args).
        // Callee scratch starts after all parameters.
        let callee_scratch = if direct.is_none() {
            let scratch_start = (stage_base + ((register_pieces + hidden_slots) * 8 + 7)) & !7;
            self.gen_expr(callee.expect("indirect call expression"), 0);
            self.store_rsp_qword(0, scratch_start);
            Some(scratch_start)
        } else {
            None
        };

        let mut staged: Vec<(ArgLoc, usize, bool)> = Vec::new();
        for (arg_idx, (arg, arg_locs)) in args.iter().zip(locs.iter()).enumerate() {
            let param_ty = direct
                .and_then(|name| self.func_param_types.get(name))
                .and_then(|ps| ps.get(arg_idx))
                .cloned()
                .or_else(|| {
                    // 间接调用: 从函数指针类型提取参数类型
                    if let Some(callee_expr) = callee {
                        let callee_ty = callee_expr.ty();
                        if let Type::Ptr(ref p) = callee_ty {
                            if let Type::Func(_, ref params) = p.base {
                                return params.get(arg_idx).map(|q| q.base.clone());
                            }
                        }
                    }
                    None
                });
            // 与上方分类循环一致: 数组实参衰减为指针
            let mut effective_ty = match param_ty.unwrap_or_else(|| arg.ty()) {
                Type::Array(elem, _) => Type::Ptr(elem),
                t => t,
            };
            // K&R 默认参数提升: float → double
            if direct
                .map(|n| self.old_style_funcs.contains(n))
                .unwrap_or(false)
                && matches!(effective_ty, Type::Float)
            {
                effective_ty = Type::Double;
            }
            let aggregate = matches!(effective_ty, Type::Int128 | Type::UInt128 | Type::Complex(_))
                || matches!(
                    effective_ty,
                    Type::Struct(_, _, _, _)
                        | Type::Union(_, _, _, _)
                        | Type::Array(_, ArraySize::Const(_))
                        | Type::Atomic(_)
                );
            if matches!(effective_ty, Type::Vector(_, _)) {
                // vector 实参: 按 GP 8 字节块传递 (dcc 内部标量化约定)
                // 1. 取 vector 数据地址到 rax
                if !self.gen_lvalue_addr(arg, 0) {
                    // rvalue: 先物化到向量临时槽 0 再取地址
                    // 注意: 必须用 gen_vector_load_into_slot (而非 gen_expr),
                    // 后者对 CompoundLit/Cast 等 Vector rvalue 只会留下一个
                    // 临时值 (如 mov rax, 5) 却不存入 vector_temp 槽,
                    // 导致后续从 vector_temp 读时读到栈垃圾 (pr105613).
                    self.gen_vector_load_into_slot(arg, 0);
                    let toff = self.vector_temp_base(0, effective_ty.size());
                    self.emit_lea_r13(0, toff);
                }
                let vec_sz = effective_ty.size() as i64;
                // 2. 逐块装入 rcx 并暂存
                // 关键: vector < 8B 时必须按实际大小读, 不能固定 mov rcx,[rax]
                // (会读到 vector 存储之外的栈垃圾, 如 pr rtl-optimization/16104)
                for (chunk, loc) in arg_locs.iter().enumerate() {
                    let disp = (chunk * 8) as i32;
                    let remaining = vec_sz - (chunk * 8) as i64;
                    let chunk_sz = remaining.min(8);
                    // zero-extend chunk_sz 字节到 rcx (寄存器 1)
                    // xor rcx, rcx; movzx ecx/ecx, [rax+disp]
                    self.emit(&[0x48, 0x31, 0xD1]); // xor rcx, rcx
                    match chunk_sz {
                        1 => {
                            // movzx ecx, byte [rax+disp]
                            if disp < 128 {
                                self.emit(&[0x0F, 0xB6, 0x48, disp as u8]);
                            } else {
                                self.emit(&[0x0F, 0xB6, 0x88]);
                                self.emit(&disp.to_le_bytes());
                            }
                        },
                        2 => {
                            // movzx ecx, word [rax+disp]
                            if disp < 128 {
                                self.emit(&[0x0F, 0xB7, 0x48, disp as u8]);
                            } else {
                                self.emit(&[0x0F, 0xB7, 0x88]);
                                self.emit(&disp.to_le_bytes());
                            }
                        },
                        4 => {
                            // mov ecx, dword [rax+disp] (自动 zero-extend 到 64 位)
                            if disp < 128 {
                                self.emit(&[0x8B, 0x48, disp as u8]);
                            } else {
                                self.emit(&[0x8B, 0x88]);
                                self.emit(&disp.to_le_bytes());
                            }
                        },
                        _ => {
                            // 8字节块: mov rcx, [rax+disp]
                            if disp < 128 {
                                self.emit(&[0x48, 0x8B, 0x48, disp as u8]);
                            } else {
                                self.emit(&[0x48, 0x8B, 0x88]);
                                self.emit(&disp.to_le_bytes());
                            }
                        },
                    }
                    match *loc {
                        ArgLoc::Stack(off) => self.store_rsp_qword(1, off),
                        _ => {
                            self.store_rsp_qword(1, stage);
                            staged.push((*loc, stage, false));
                            stage += 8;
                        },
                    }
                }
            } else if aggregate {
                // _Complex float/double: 两分量均为 SSE 类, 用 XMM 装载/存储
                // 而非整数 GP 中转 (SysV ABI 3.2.3).
                let is_complex_sse = matches!(&effective_ty, Type::Complex(inner)
                    if matches!(&inner.base, Type::Float | Type::Double));
                if is_complex_sse {
                    let inner_sz = match effective_ty {
                        Type::Complex(ref inner) => inner.size() as i64,
                        _ => 8,
                    };
                    // SSE 类复数传参: 每个 eightbyte 均为完整 8 字节。
                    // _Complex float: 单 eightbyte (实部+虚部打包) → movsd 整体装载;
                    // _Complex double: 两个 eightbyte → movsd 各装 8 字节。
                    // 不能用 movss: 只装 4 字节会丢虚部 (xmm 高半为垃圾)。
                    let pfx: u8 = 0xF2;
                    // (Call 实参不再特判: Call 返回已统一物化到 result_off,
                    // 与下方"复杂表达式"路径同构 — gen_expr 后从 result_off
                    // 逐分量加载。旧实现直接从 xmm0/xmm1 取分量, 整数
                    // _Complex 的返回在 rax/rdx, xmm 是垃圾。)
                    for (chunk, loc) in arg_locs.iter().enumerate() {
                        let chunk_off = (chunk as i64) * inner_sz;
                        if let Expr::Ident { name, .. } = arg {
                            if self.is_global_not_local(&name.name) {
                                // global complex: lea rcx, [global+chunk_off]; movsd xmm0, [rcx]
                                // 注意: 必须写入 rcx (寄存器 1), 下方 movsd [rcx] 硬编码 rcx
                                self.emit_global_addr(1, &name.name);
                                if chunk_off != 0 {
                                    self.emit(&[0x48, 0x81, 0xC1]); // add rcx, imm32
                                    self.emit(&(chunk_off as i32).to_le_bytes());
                                }
                                self.emit(&[pfx, 0x0F, 0x10, 0x01]); // movsd/movss xmm0, [rcx]
                            } else {
                                let var_off = self.var_offset(&name.name) + chunk_off;
                                self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x10], var_off);
                            }
                        } else {
                            // 复杂表达式: 先物化到 result_off, 再逐分量加载
                            self.gen_expr(arg, 0);
                            let result_off = -self.frame_size + 8;
                            self.emit_xmm0_r13_disp(
                                &[pfx, 0x41, 0x0F, 0x10],
                                result_off + chunk_off,
                            );
                        }
                        // 存储到 staging 或栈槽
                        match *loc {
                            ArgLoc::Stack(off) => {
                                self.emit_xmm_store_rsp(pfx, 0, off);
                            },
                            _ => {
                                self.emit_xmm_store_rsp(pfx, 0, stage);
                                staged.push((*loc, stage, false));
                                stage += 8;
                            },
                        }
                    }
                } else if let Expr::Ident { name, .. } = arg {
                    // Ident aggregate: load data from variable slots
                    let is_vla_local = !self.is_global_not_local(&name.name)
                        && self.vla_offsets.contains_key(&name.name);
                    if is_vla_local {
                        // VLA 变量: var_offset 指向元数据区, 首槽存基址.
                        // 先加载基址到 rcx, 再从 [rcx + chunk*8] 取数据.
                        let base_off = self.var_offset(&name.name);
                        self.load_stack(1, base_off); // rcx = base addr
                    }
                    for (chunk, loc) in arg_locs.iter().enumerate() {
                        if self.is_global_not_local(&name.name) {
                            self.emit_global_addr(11, &name.name);
                            self.emit(&[0x49, 0x8B, 0x83]);
                            self.emit(&((chunk * 8) as i32).to_le_bytes());
                        } else if is_vla_local {
                            // VLA: mov rax, [rcx + chunk*8], then move to r11
                            let disp = (chunk * 8) as i32;
                            if disp == 0 {
                                self.emit(&[0x48, 0x8B, 0x01]); // mov rax, [rcx]
                            } else if disp < 128 {
                                self.emit(&[0x48, 0x8B, 0x41, disp as u8]); // mov rax, [rcx+disp8]
                            } else {
                                self.emit(&[0x48, 0x8B, 0x81]);
                                self.emit(&disp.to_le_bytes()); // mov rax, [rcx+disp32]
                            }
                            self.mov(11, 0); // r11 = rax (data), for subsequent store
                        } else {
                            // 普通局部标识符：从栈帧加载数据
                            let var_off = self.var_offset(&name.name) + (chunk * 8) as i64;
                            self.load_stack(11, var_off); // r11 = data
                        }
                        match *loc {
                            ArgLoc::Stack(off) => {
                                // 直接存储到调用者栈槽
                                self.store_rsp_qword(11, off);
                            },
                            _ => {
                                // 暂存到 staging area 以便后续传参
                                self.store_rsp_qword(11, stage);
                                staged.push((*loc, stage, false));
                                stage += 8;
                            },
                        }
                    }
                } else if matches!(arg, Expr::CompoundLit { .. }) {
                    // CompoundLit: gen_expr builds data on stack and returns
                    // the ADDRESS in rax.  We must LOAD data from [addr +
                    // chunk*8], not pass the address itself.
                    self.gen_expr(arg, 0); // rax = address
                    self.mov(1, 0); // rcx = address (safe: no calls
                                    // between chunk loads)
                    for (chunk, loc) in arg_locs.iter().enumerate() {
                        // mov rax, [rcx + chunk*8]
                        let disp = (chunk * 8) as i32;
                        if disp == 0 {
                            self.emit(&[0x48, 0x8B, 0x01]); // mov rax, [rcx]
                        } else if disp < 128 {
                            self.emit(&[0x48, 0x8B, 0x41]); // mov rax, [rcx+disp8]
                            self.emit(&[disp as u8]);
                        } else {
                            self.emit(&[0x48, 0x8B, 0x81]); // mov rax, [rcx+disp32]
                            self.emit(&disp.to_le_bytes());
                        }
                        match *loc {
                            ArgLoc::Stack(off) => self.store_rsp_qword(0, off),
                            _ => {
                                self.store_rsp_qword(0, stage);
                                staged.push((*loc, stage, false));
                                stage += 8;
                            },
                        }
                    }
                } else if matches!(
                    arg,
                    Expr::Index { .. } | Expr::Member { .. } | Expr::Deref { .. }
                ) && self.gen_lvalue_addr(arg, 1)
                {
                    // Lvalue 聚合实参 (a[2] / s.f / *p): gen_expr 会加载
                    // 元素值 (pr44575: 全局数组元素被当 4 字节标量读),
                    // 须取 lvalue 地址后按 8 字节块装载 (与 CompoundLit
                    // 一致)。gen_lvalue_addr 失败则落入下方 rvalue 路径。
                    for (chunk, loc) in arg_locs.iter().enumerate() {
                        // mov rax, [rcx + chunk*8]
                        let disp = (chunk * 8) as i32;
                        if disp == 0 {
                            self.emit(&[0x48, 0x8B, 0x01]); // mov rax, [rcx]
                        } else if disp < 128 {
                            self.emit(&[0x48, 0x8B, 0x41]); // mov rax, [rcx+disp8]
                            self.emit(&[disp as u8]);
                        } else {
                            self.emit(&[0x48, 0x8B, 0x81]); // mov rax, [rcx+disp32]
                            self.emit(&disp.to_le_bytes());
                        }
                        match *loc {
                            ArgLoc::Stack(off) => self.store_rsp_qword(0, off),
                            _ => {
                                self.store_rsp_qword(0, stage);
                                staged.push((*loc, stage, false));
                                stage += 8;
                            },
                        }
                    }
                } else {
                    // Other rvalue aggregate (e.g. function returning
                    // struct in rax:rdx): gen_expr leaves value in
                    // rax (and rdx for >8 bytes).
                    self.gen_expr(arg, 0);
                    // __int128 参数: 若 RHS 非 __int128, rdx 未设置, 需扩展
                    if matches!(effective_ty, Type::Int128 | Type::UInt128)
                        && !matches!(arg.ty(), Type::Int128 | Type::UInt128)
                    {
                        if matches!(effective_ty, Type::UInt128)
                            || matches!(
                                arg.ty(),
                                Type::Bool
                                    | Type::UChar
                                    | Type::UShort
                                    | Type::UInt
                                    | Type::ULong
                                    | Type::ULongLong
                            )
                        {
                            self.emit(&[0x48, 0x31, 0xD2]); // xor rdx, rdx
                        } else {
                            self.emit(&[0x48, 0x99]); // cqo
                        }
                    }
                    for (chunk, loc) in arg_locs.iter().enumerate() {
                        let store_reg = if matches!(effective_ty, Type::Int128 | Type::UInt128) {
                            if chunk == 0 {
                                0u8
                            } else {
                                2u8
                            } // rax for low, rdx for high
                        } else {
                            if chunk == 1 {
                                self.mov(0, 2);
                            } // rdx = rax (non-int128 aggregate)
                            0u8 // always store rax
                        };
                        match *loc {
                            ArgLoc::Stack(off) => self.store_rsp_qword(store_reg, off),
                            _ => {
                                self.store_rsp_qword(store_reg, stage);
                                staged.push((*loc, stage, false));
                                stage += 8;
                            },
                        }
                    }
                }
            } else if matches!(effective_ty, Type::LongDouble) {
                // A2: long double — dcc 内部以 64-bit double (xmm0) 表示.
                // 调用方直接写 8 字节 double 到栈上, 被调方用 fldl 读取.
                // 16 字节槽位: 8 字节 double + 8 字节零填充.
                self.gen_expr(arg, 0);
                // 隐式转换到形参 long double (dcc 内部 double 语义):
                // 整数实参 → cvtsi2sd; float 实参 → cvtss2sd。
                let ld_arg_ty = arg.ty();
                if Self::is_integer_type(&ld_arg_ty) {
                    if matches!(
                        ld_arg_ty,
                        Type::Long | Type::ULong | Type::LongLong | Type::ULongLong
                    ) {
                        self.emit(&[0xF2, 0x48, 0x0F, 0x2A, 0xC0]); // cvtsi2sd xmm0, rax
                    } else {
                        self.emit(&[0xF2, 0x0F, 0x2A, 0xC0]); // cvtsi2sd xmm0, eax
                    }
                } else if matches!(ld_arg_ty, Type::Float) {
                    self.emit(&[0xF3, 0x0F, 0x5A, 0xC0]); // cvtss2sd xmm0, xmm0
                }
                // movsd xmm0 → [rsp+first_off]: 写 8 字节 double
                if let Some(ArgLoc::Stack(off)) = arg_locs.first() {
                    let off = *off as i32;
                    if off == 0 {
                        // movsd [rsp], xmm0: F2 0F 11 04 24
                        self.emit(&[0xF2, 0x0F, 0x11, 0x04, 0x24]);
                    } else if off.abs() < 128 {
                        // movsd [rsp+disp8], xmm0: F2 0F 11 44 24 disp8
                        self.emit(&[0xF2, 0x0F, 0x11, 0x44, 0x24, off as u8]);
                    } else {
                        // movsd [rsp+disp32], xmm0: F2 0F 11 84 24 disp32
                        self.emit(&[0xF2, 0x0F, 0x11, 0x84, 0x24]);
                        self.emit(&off.to_le_bytes());
                    }
                }
                // 高 8 字节写零 (long double 占 16 字节, 8 字节数据 + 8 字节填充)
                if arg_locs.len() > 1 {
                    if let Some(ArgLoc::Stack(off)) = arg_locs.get(1) {
                        self.mov_imm(0, 0);
                        self.store_rsp_qword(0, *off);
                    }
                }
            } else if matches!(effective_ty, Type::Float | Type::Double) {
                self.gen_expr(arg, 0);
                // 隐式整数→浮点转换: 实参是整数但形参是浮点类型
                let arg_ty = arg.ty();
                if Self::is_integer_type(&arg_ty) {
                    self.emit_scalar_to_xmm0_if_needed(arg, &effective_ty);
                }
                // float/double 类型不匹配转换:
                // 形参是 float 但实参是 double → cvtsd2ss (double→float)
                // 形参是 double 但实参是 float → cvtss2sd (float→double)
                if matches!(effective_ty, Type::Float) && matches!(arg_ty, Type::Double) {
                    self.emit(&[0xF2, 0x0F, 0x5A, 0xC0]); // cvtsd2ss xmm0, xmm0
                } else if matches!(effective_ty, Type::Double) && matches!(arg_ty, Type::Float) {
                    self.emit(&[0xF3, 0x0F, 0x5A, 0xC0]); // cvtss2sd xmm0, xmm0
                }
                // C 语义: 可变参数函数的 float 参数默认提升为 double
                // printf("%f", 1.0f) 实际传递的是 double 1.0
                // K&R (无原型) 函数同样适用默认参数提升: float → double
                let is_variadic = direct
                    .and_then(|name| self.variadic_funcs.get(name))
                    .copied()
                    .unwrap_or(false);
                let is_old_style = direct
                    .map(|name| self.old_style_funcs.contains(name))
                    .unwrap_or(false);
                let promoted = (is_variadic || is_old_style) && matches!(effective_ty, Type::Float);
                if promoted {
                    // cvtss2sd xmm0, xmm0: float → double
                    self.emit(&[0xF3, 0x0F, 0x5A, 0xC0]);
                }
                // 提升后的 float 按 double 语义 8 字节搬运 (movsd):
                // movss 只写/读低 4 字节, double 高 4 字节成为垃圾,
                // printf 按错误位模式解析 (sse.c: 打印 0.0 而非 1.5)。
                let is_float = matches!(effective_ty, Type::Float) && !promoted;
                let pfx: u8 = if is_float { 0xF3 } else { 0xF2 };
                match arg_locs[0] {
                    ArgLoc::Stack(off) => self.emit_xmm_store_rsp(pfx, 0, off),
                    loc => {
                        self.emit_xmm_store_rsp(pfx, 0, stage);
                        staged.push((loc, stage, is_float));
                        stage += 8;
                    },
                }
            } else if matches!(effective_ty, Type::Int128 | Type::UInt128) {
                // SysV：16 字节整数占两个 GP 槽（低 rax / 高 rdx）
                self.gen_expr(arg, 0);
                if !matches!(arg.ty(), Type::Int128 | Type::UInt128) {
                    if matches!(effective_ty, Type::UInt128)
                        || matches!(
                            arg.ty(),
                            Type::Bool
                                | Type::UChar
                                | Type::UShort
                                | Type::UInt
                                | Type::ULong
                                | Type::ULongLong
                        )
                    {
                        self.emit(&[0x48, 0x31, 0xD2]);
                    } else {
                        self.emit(&[0x48, 0x99]);
                    }
                }
                for (chunk, loc) in arg_locs.iter().enumerate() {
                    let reg = if chunk == 0 { 0u8 } else { 2u8 };
                    match *loc {
                        ArgLoc::Stack(off) => self.store_rsp_qword(reg, off),
                        loc => {
                            self.store_rsp_qword(reg, stage);
                            staged.push((loc, stage, false));
                            stage += 8;
                        },
                    }
                }
            } else {
                self.gen_expr_decay(arg, 0);
                // 隐式浮点→整数转换: 实参是浮点表达式但形参是整数类型
                // (bar(d + 4.0): 求值结果在 xmm0, 分类按形参 Integer 走
                //  GP — 须 cvttsd2si 转回 GP, 否则传入 rax 中的陈旧残留
                //  值, callconv.c bug: bar(d) 得 12 而非 17)
                let sc_arg_ty = arg.ty();
                if matches!(sc_arg_ty, Type::Float | Type::Double | Type::LongDouble)
                    && Self::is_integer_type(&effective_ty)
                {
                    self.emit_implicit_float_to_int(0, &sc_arg_ty, &effective_ty);
                }
                // 零大小类型 (如空结构体) 无 ABI 槽位, 只求值不传参
                if arg_locs.is_empty() {
                    continue;
                }
                match arg_locs[0] {
                    ArgLoc::Stack(off) => self.store_rsp_qword(0, off),
                    loc => {
                        self.store_rsp_qword(0, stage);
                        staged.push((loc, stage, false));
                        stage += 8;
                    },
                }
            }
        }

        if let Some(slot) = sret_slot {
            self.load_rsp_qword(7, slot);
        }
        // 用 while 循环以便在合并时跳过下一个 entry (手动递增 i)
        let mut i = 0;
        while i < staged.len() {
            let (loc, slot, is_float) = staged[i];
            match loc {
                ArgLoc::Gp(reg) => {
                    self.load_rsp_qword(reg, slot);
                    i += 1;
                },
                ArgLoc::Xmm(reg) => {
                    // 检查下一个 staged 是否也是同一个 xmm 寄存器 (SseUp 延续,
                    // 如 16 字节 vector 参数). 如果是, 合并为一条 movups 加载
                    // 避免 movsd 清零高 8 字节导致数据丢失 (pr105613).
                    // 注意: _Complex double 有两个独立 SSE (xmm0, xmm1),
                    // 不会误合并因为寄存器编号不同.
                    if i + 1 < staged.len() {
                        let (next_loc, next_slot, _) = staged[i + 1];
                        match next_loc {
                            ArgLoc::Xmm(nr) if nr == reg && next_slot == slot + 8 => {
                                // 合并: movups xmm_reg, [rsp+slot]
                                self.emit(&[0x0F, 0x10]);
                                if slot < 128 {
                                    self.emit(&[0x44 | ((reg & 7) << 3), 0x24, slot as u8]);
                                } else {
                                    self.emit(&[0x84 | ((reg & 7) << 3), 0x24]);
                                    self.emit(&slot.to_le_bytes());
                                }
                                i += 2; // 跳过下一个 entry
                                continue;
                            },
                            _ => {},
                        }
                    }
                    let pfx: u8 = if is_float { 0xF3 } else { 0xF2 };
                    self.emit_xmm_load_rsp(pfx, reg, slot);
                    i += 1;
                },
                ArgLoc::Stack(_) => unreachable!(),
            }
        }
        // SysV uses AL to communicate the number of vector registers to a
        // variadic callee. Setting it for all calls is benign.
        self.emit(&[0xB0, fp as u8]);
        if let Some(name) = direct {
            // 解析嵌套函数引用 (沿作用域链上溯: 支持从后代嵌套函数
            // 内直接调用兄弟/祖先定义的嵌套函数)
            let nested_ref = self.resolve_nested_func_ref(name);
            let effective_name = match &nested_ref {
                Some((unique, _, _)) => unique.clone(),
                None => self
                    .nested_func_renames
                    .get(&self.current_func_name)
                    .and_then(|m| m.get(name))
                    .cloned()
                    .unwrap_or_else(|| name.to_string()),
            };
            if let Some((_, hops, offs)) = &nested_ref {
                // 静态链 = 定义父帧指针 (SysV ABI: r10, 非参数寄存器)
                self.load_static_chain_for_call(*hops, offs);
            }
            // Resolve GNU asm label (e.g., __asm__("real_name")) for call sites
            let resolved_name = self.resolve_asm_label(&effective_name).to_string();
            self.call_label(&resolved_name);
        } else {
            self.load_rsp_qword(11, callee_scratch.unwrap());
            if self.target.indirect_branch_thunk_extern {
                self.call_label("__x86_indirect_call_thunk_r11");
            } else {
                self.emit(&[0x49, 0xFF, 0xD3]);
            }
        }
        self.emit_add_rsp(frame_bytes as i64);
    }
}
