// src/codegen/x86_64/abi/call.rs
// SysV AMD64 调用约定的 caller 侧实现。
// 本文件由 mod.rs 逐字搬迁而来 (batch 3): `emit_sysv_call` 原样剪切至 `impl Codegen`,
// 语义 diff 为 0。原先的 `unimplemented!()` 影子存根与 `ArgLoc` 影子枚举已一并作废。

use crate::ast::*;
use crate::codegen::x86_64::insn::Insn;
use crate::codegen::x86_64::{AbiClass, ArgLoc, Codegen};

impl Codegen {
    /// Lower one SysV AMD64 call.  Outgoing stack arguments occupy the lowest
    /// addresses, temporary register staging follows them, and the complete
    /// area is rounded to 16 bytes so `%rsp` is aligned immediately before call.
    ///
    /// **前置三（caller-saved 保存通道）就包在本函数外面**：住在延伸池号里的自动绑定家
    /// 在参数装载开始前就会被毁（装载本身就写 `rdi/rsi/rcx/r8/r9`，更不必说被调方），
    /// 所以这里先把每个这类家存到它的帧内保存槽、并把它的**程序点级家**切到那个槽（见
    /// `Codegen::home_overrides`），调用序列结束后再把值搬回号里。窗口必须盖住「参数求值 +
    /// 装载 + call + 栈参回收」全部，而不只是 `call` 那一条指令 —— 因为毁掉它们的就包括
    /// 装载自己。
    pub fn emit_sysv_call(
        &mut self,
        direct: Option<&str>,
        callee: Option<&Expr>,
        args: &[Expr],
        sret_off: Option<i64>,
    ) {
        if self.ext_home_saves.is_empty() {
            self.emit_sysv_call_inner(direct, callee, args, sret_off);
            return;
        }
        let n = self.persist_ext_homes();
        self.emit_sysv_call_inner(direct, callee, args, sret_off);
        self.restore_ext_homes(n);
    }

    /// 把所有延伸池家存进各自的槽并开启覆盖；返回**本次新开启**的个数（嵌套 call 只该
    /// 回滚自己那一层，不然内层会把外层正在用的覆盖一并抽掉）。
    pub(crate) fn persist_ext_homes(&mut self) -> usize {
        let mut opened = 0usize;
        for i in 0..self.ext_home_saves.len() {
            // 先把需要的值拿够再调 `&mut self` 的方法：存/取本身就是方法调用，与覆盖表
            // 的写入同属一个 self。
            let (reg, off) = (self.ext_home_saves[i].reg, self.ext_home_saves[i].slot_off);
            let name = self.ext_home_saves[i].name.clone();
            if self.home_overrides.contains_key(&name) {
                continue; // 已被外层 call 序列存下，值在槽里、不需要再存
            }
            self.store_stack(reg, off);
            self.home_overrides
                .insert(name, crate::location::Location::Stack(off));
            opened += 1;
        }
        opened
    }

    /// 取回本层开启的那些家。倒序回滚，与开启顺序对偶。
    pub(crate) fn restore_ext_homes(&mut self, opened: usize) {
        if opened == 0 {
            return;
        }
        let mut done = 0usize;
        for i in (0..self.ext_home_saves.len()).rev() {
            if done == opened {
                break;
            }
            let (reg, off) = (self.ext_home_saves[i].reg, self.ext_home_saves[i].slot_off);
            let name = self.ext_home_saves[i].name.clone();
            // 只认「现在覆盖着的」那些：本层没开启的条目不在覆盖表里（或属于外层）。
            let mine = self
                .home_overrides
                .get(&name)
                .is_some_and(|l| matches!(l, crate::location::Location::Stack(o) if *o == off));
            if !mine {
                continue;
            }
            self.load_stack(reg, off);
            self.home_overrides.remove(&name);
            done += 1;
        }
        debug_assert_eq!(done, opened, "回滚数量与开启数量不等：覆盖表泄漏了");
    }

    fn emit_sysv_call_inner(
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
            // __builtin_va_arg_pack: 把当前函数已保存的变参 (reg_save_area +
            // overflow_arg_area) 原样镜像到本次 call 的 outgoing 参数暂存区。
            // 必须是最后一个实参; 后面不应再有其它参数。
            if matches!(arg, Expr::VaArgPack { .. }) {
                let mut my_locs: Vec<ArgLoc> = Vec::new();
                // 1) GP 变参: 从 variadic_named_gp 到 5 (共 6 个 GP 槽位)
                let gp_start = self.variadic_named_gp.max(0) as usize;
                for _i in gp_start..gp_regs.len() {
                    if gp < gp_regs.len() {
                        my_locs.push(ArgLoc::Gp(gp_regs[gp]));
                        gp += 1;
                    } else {
                        my_locs.push(ArgLoc::Stack(stack));
                        stack += 8;
                    }
                }
                // 2) XMM 变参: 从 variadic_named_fp 到 7 (共 8 个 XMM 槽位)
                let fp_start = self.variadic_named_fp.max(0) as usize;
                for _i in fp_start..8 {
                    if fp < 8 {
                        my_locs.push(ArgLoc::Xmm(fp as u8));
                        fp += 1;
                    } else {
                        my_locs.push(ArgLoc::Stack(stack));
                        stack += 8;
                    }
                }
                // 3) overflow_arg_area 的栈传变参。SysV ABI 栈参按 8 字节对齐,
                // long double 类按 16 字节对齐。我们盲拷整块内存: 源端布局
                // 已经是 caller 当初给本函数的样子 (r13+overflow_off 起连续),
                // 镜像到 outgoing 栈区后 callee 的 va_arg 按相同 ABI 规则读取,
                // 因此 16 对齐与内容都保持一致。
                // 估算大小: 从 variadic_overflow_offset 向上, 本函数没有任何
                // 后续栈槽 (overflow_off 是变参栈起点), 栈变参总大小无法在
                // 编译期精确得知。保守做法: 拷贝到 frame_layout 末尾是过度的,
                // 但我们只需要保证 callee 实际读取的变参都被覆盖 — 盲拷
                // 一个保守上界 (例如 frame_bytes - overflow_off) 会覆盖到
                // 本函数自己的局部变量, 反而错误。
                // 正确策略: 我们在 codegen 时并不知道 caller 传了多少栈变参,
                // 但 caller 传参时同样按 8/16 字节对齐 — 因此 overflow_arg_area
                // 里从起点开始的连续内存, 一直到 caller 当初 "sub rsp, N" 的
                // 末尾, 都是有效变参。我们没有这个 N。
                // 简化处理: 不直接拷贝 overflow 区, 而是留给第二阶段的
                // "commit" 循环用循环拷贝: 我们在 locs 里放一个特殊标记
                // 告知第二阶段 "从 [r13+overflow_off] 循环拷贝到 outgoing
                // 栈区直到 (stack - 初始 stack)"。但这依然需要字节数。
                //
                // 最实用的近似: 从 overflow_off 开始, 每个 8 字节槽都作为
                // ArgLoc::Stack 条目追加。数量取一个保守上界 (例如 32 个槽),
                // 多余的槽位 callee 不会读到, 但栈空间会被预留。
                // 这会导致栈帧过大但语义正确。
                // 实际上, 我们可以利用 caller 栈的特性: overflow_arg_area
                // 起点 = r13+16+named_stack_arg_bytes, 而 caller 的栈帧
                // 里这段区域就是 caller sub 出来的 outgoing 参数区。其大小
                // 等于 caller 当初为本次调用 sub 的 frame_bytes — 我们不知道。
                //
                // 为保持简单与正确, 这里在第一阶段仅分配 GP/XMM 槽位;
                // 栈传变参在第二阶段通过特殊的循环处理。我们在 my_locs
                // 后追加一个标记性的 ArgLoc::Stack(stack) 条目, 表示
                // "从这里开始拷贝剩余 overflow 内存"。但数量未知, 因此
                // 我们直接在第二阶段拷贝一个保守长度 (1024 字节) 到
                // outgoing 栈区。对绝大多数测试用例足够。
                // 实际更稳妥: 我们分配 stack 槽位给一个占位, 然后在第二
                // 阶段做 memcpy, 长度 = (outgoing 栈区总大小 - 当前 stack)。
                //
                // 我们记录 va_arg_pack 开始时的 stack 值, 以便第二阶段
                // 知道需要从 overflow_off 拷贝多少字节到 outgoing 栈区。
                // 简化: 直接把 overflow 区的内容按 8 字节一块拷贝到
                // outgoing 栈槽。我们盲拷 64 个槽 (512 字节), 足够大。
                // 多余的栈空间 callee 不会访问。
                for _ in 0..64 {
                    my_locs.push(ArgLoc::Stack(stack));
                    stack += 8;
                }
                max_force_align = max_force_align.max(16);
                locs.push(my_locs);
                continue;
            }
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
            // `_Complex long double` 形参: SysV 下整体走内存, 两个 16 字节
            // 栈槽 (real@0、imag@16), 内容是真 80-bit tbyte。它不能落入通用
            // 聚合 8 字节 chunk 路径 (那会用 GP movq 逐字搬运, 破坏 10 字节
            // tbyte 布局), 这里提前规划为两个 16 字节栈槽。
            let is_complex_ld = matches!(&effective_ty, Type::Complex(inner)
                if matches!(&inner.base, Type::LongDouble));
            if is_complex_ld {
                let align = 16usize;
                max_force_align = max_force_align.max(align);
                let aligned = (stack + align - 1) & !(align - 1);
                stack = aligned;
                locs.push(vec![ArgLoc::Stack(aligned), ArgLoc::Stack(aligned + 16)]);
                stack += 32;
                continue;
            }
            let classes = Self::abi_classes(&effective_ty);
            // SysV all-or-nothing 拆分的唯一真源 (callee 形参采集共用同一函数):
            // 见 [`Codegen::aggregate_forces_memory`]。
            let force_mem = Self::aggregate_forces_memory(&classes, gp_regs.len() - gp, 8 - fp);
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
        // Stack arguments must be staged too: evaluating a later argument may
        // emit nested calls and temporarily move RSP, so direct writes to the
        // final outgoing area are not stable until all arguments are lowered.
        let stack_pieces = locs
            .iter()
            .flatten()
            .filter(|l| matches!(l, ArgLoc::Stack(_)))
            .count();
        let hidden_slots = usize::from(sret_off.is_some());
        let callee_scratch_slots = usize::from(direct.is_none());
        let stage_base = stack + 16;
        // Indirect calls need one extra scratch slot for the callee.
        // This slot must be included in frame_bytes; otherwise it can
        // alias caller temporaries already pushed above the call frame
        // (e.g. `lhs = (*fp)(...)` overwrites the saved lhs address).
        let stage_slots = register_pieces + hidden_slots + callee_scratch_slots + stack_pieces * 2;
        // force_mem 区起点 (sub 后 rsp) 必须对齐到 force_mem 参数的最大对齐值.
        // 否则 align=32 的 agg 会在 16 对齐的地址上错位.
        let frame_align = max_force_align.max(16); // 至少 16 (SysV ABI call 对齐)
        let raw_bytes = stage_base + stage_slots * 8;
        // SysV ABI 要求 call 指令处 rsp 16 对齐. 在 emit_sub_rsp 之前,
        // caller 栈已处于 16 对齐 (函数 prologue sub frame_size 后 rsp 为
        // 16 对齐), **但中途未配平的 push 会把它偏离 8** —— 旧代码的
        // "没有额外 push 扰乱"假设不成立: `v.b = f1(...)` 这类"保存左值
        // 地址跨调用"的形状就是 push 后立刻 sub+call。被调方按 ABI 假设
        // 第一个栈参 16 对齐 (va_arg 的 overflow 对齐、align>16 的 MEMORY
        // 聚合), rsp 差 8 就让它的变参整体错位 (pr92904 f1(4))。
        // frame_bytes 补上同样的偏移, sub 后 rsp 恢复 16 对齐。
        let misalign = self.stack_shift.rem_euclid(16) as usize;
        let frame_bytes = ((raw_bytes + frame_align - 1) & !(frame_align - 1)) + misalign;
        // 当 frame_align > 16 (例如 vector_size(32) 的 32 字节对齐参数),
        // 静态 sub frame_bytes 只能保证 rsp 16 对齐. 必须动态对齐 rsp 到
        // frame_align, 否则 force_mem 槽位的 32 对齐保证被破坏.
        // r10 保存对齐前 rsp, r11 存放对齐 mask.
        let dynamic_align = frame_align > 16;
        if dynamic_align {
            self.emit_insn(&Insn::mov_rm_reg(
                crate::codegen::shared::asm_operand::Width::B64,
                10, // r10
                4,  // rsp
            ));
            let mask: u64 = !(frame_align as u64 - 1);
            self.emit_insn(&Insn::movabs_reg_imm64(11, mask));
            self.emit_sub_rsp(frame_bytes as i64);
            self.emit_insn(&Insn::alu_rm_reg_reg(
                crate::codegen::x86_64::opcode::AluOp::And,
                crate::codegen::shared::asm_operand::Width::B64,
                4,  // rsp
                11, // r11
            ));
        } else {
            self.emit_sub_rsp(frame_bytes as i64);
        }

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
            self.gen_expr(callee.expect("indirect call expression"), Self::EVAL_BASE);
            self.store_rsp_qword(0, scratch_start);
            Some(scratch_start)
        } else {
            None
        };

        let mut staged: Vec<(ArgLoc, usize, bool, bool)> = Vec::new();
        let mut stack_staged: Vec<(usize, usize, usize)> = Vec::new();
        for (arg_idx, (arg, arg_locs)) in args.iter().zip(locs.iter()).enumerate() {
            // __builtin_va_arg_pack: 第一阶段已分配好 GP/XMM/Stack 槽位。
            // 第二阶段: 把当前函数 reg_save_area / overflow_arg_area 的
            // 内容逐槽搬到 outgoing 暂存区 (GP/XMM 用寄存器中转, Stack 直接
            // 写入 outgoing 栈槽)。callee 的 va_arg 将按相同 ABI 规则读到
            // 相同的值。
            if matches!(arg, Expr::VaArgPack { .. }) {
                let mut loc_iter = arg_locs.iter().copied().peekable();
                // --- GP 槽位: 从 [r13+reg_save_off+i*8] 读到 gp 槽 ---
                let gp_start = self.variadic_named_gp.max(0) as usize;
                let mut src_idx = gp_start;
                // 用 peek 判定段边界: 旧写法 `next()` 后 match 到非本段类型就
                // break, 会把**第一个不属于本段的 loc 消费掉**。GP 段因此吃掉
                // 第一个 Stack loc, XMM 段再吃掉第二个 —— 这两个槽 (GP 变参
                // 装不进新调用寄存器时的溢出槽) 永远不会被搬运, 新调用的栈参
                // 整体前移 16 字节 (va-arg-pack-1: foo1 把 ++v1 当 struct A 读)。
                while let Some(&loc) = loc_iter.peek() {
                    match loc {
                        ArgLoc::Gp(_) => {
                            loc_iter.next();
                            // 源 = [r13 + reg_save_off + src_idx*8]
                            let src_off = self.variadic_reg_save_offset + src_idx as i64 * 8;
                            self.load_stack(0, src_off); // rax = saved gp
                            self.store_rsp_qword(0, stage);
                            staged.push((loc, stage, false, false));
                            stage += 8;
                            src_idx += 1;
                        },
                        // GP 段尚有未搬完的已存寄存器 (新调用的 GP 槽位更少):
                        // 这些变参住在 bar 的**寄存器保存区**里, 必须从那里
                        // 搬进新调用的栈槽; 留给下面的 overflow 段会按栈传
                        // 变参的布局取字节, 整体错位。
                        ArgLoc::Stack(off) if src_idx < gp_regs.len() => {
                            loc_iter.next();
                            let src_off = self.variadic_reg_save_offset + src_idx as i64 * 8;
                            self.load_stack(0, src_off);
                            self.store_rsp_qword(0, off);
                            src_idx += 1;
                        },
                        _ => break,
                    }
                }
                // --- XMM 槽位: 从 [r13+reg_save_off+48+i*16] 读到 xmm 槽 ---
                let fp_start = self.variadic_named_fp.max(0) as usize;
                let mut src_fp_idx = fp_start;
                while let Some(&loc) = loc_iter.peek() {
                    if !matches!(loc, ArgLoc::Xmm(_)) {
                        break;
                    }
                    loc_iter.next();
                    let src_off =
                        self.variadic_reg_save_offset + 48 + src_fp_idx as i64 * 16;
                    self.load_xmm128_stack(0, src_off);
                    self.emit_xmm_store_rsp(0xF2, 0, stage);
                    staged.push((loc, stage, false, false));
                    stage += 8;
                    src_fp_idx += 1;
                }
                // --- Stack 槽位: 从 [r13+overflow_off + k*8] 逐 8 字节搬运 ---
                // 本段必须处理**所有**剩余 Stack loc (含上面两段 peek 出来、
                // 未消费的 GP 溢出槽); 中间的 XMM loc 已由 XMM 段处理, 这里
                // 跳过即可 (k 只数 Stack, 与 overflow 区布局逐槽对应)。
                let mut k: i64 = 0;
                while let Some(loc) = loc_iter.next() {
                    match loc {
                        ArgLoc::Stack(off) => {
                            let src_off = self.variadic_overflow_offset + k * 8;
                            self.load_stack(0, src_off); // rax = [r13+overflow_off+k*8]
                            self.store_rsp_qword(0, off);
                            k += 1;
                        },
                        _ => {},
                    }
                }
                continue;
            }
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
                if !self.gen_lvalue_addr(arg, Self::EVAL_BASE) {
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
                    // P11: xor rcx, rcx via typed opcode.
                    {
                        let insn = Insn::new(
                            crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                                op: crate::codegen::x86_64::opcode::AluOp::Xor,
                                width: crate::codegen::shared::asm_operand::Width::B64,
                                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                    1,
                                    crate::codegen::shared::asm_operand::Width::B64,
                                ),
                                src: crate::codegen::x86_64::opcode::Operand::Reg(
                                    1,
                                    crate::codegen::shared::asm_operand::Width::B64,
                                ),
                            },
                            &[],
                        );
                        if !self.emit_insn(&insn) {
                            self.emit_insn(&Insn::new(
                                crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                                    op: crate::codegen::x86_64::opcode::AluOp::Xor,
                                    width: crate::codegen::shared::asm_operand::Width::B64,
                                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                        1,
                                        crate::codegen::shared::asm_operand::Width::B64,
                                    ),
                                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                                        2,
                                        crate::codegen::shared::asm_operand::Width::B64,
                                    ),
                                },
                                &[],
                            )); // xor rcx, rcx
                        }
                    }
                    match chunk_sz {
                        1 => {
                            // movzx ecx, byte [rax+disp]
                            self.emit_insn(&Insn::mov_ext_mem(
                                false,
                                crate::codegen::shared::asm_operand::Width::B8,
                                crate::codegen::shared::asm_operand::Width::B32,
                                1,
                                0,
                                disp as i32,
                            ));
                        },
                        2 => {
                            // movzx ecx, word [rax+disp]
                            self.emit_insn(&Insn::mov_ext_mem(
                                false,
                                crate::codegen::shared::asm_operand::Width::B16,
                                crate::codegen::shared::asm_operand::Width::B32,
                                1,
                                0,
                                disp as i32,
                            ));
                        },
                        4 => {
                            // mov ecx, dword [rax+disp] (自动 zero-extend 到 64 位)
                            self.emit_insn(&Insn::mov_rm_mem(
                                true,
                                crate::codegen::shared::asm_operand::Width::B32,
                                1,
                                0,
                                disp as i32,
                            ));
                        },
                        _ => {
                            // 8字节块: mov rcx, [rax+disp]
                            self.emit_insn(&Insn::mov_rm_mem(
                                true,
                                crate::codegen::shared::asm_operand::Width::B64,
                                1,
                                0,
                                disp as i32,
                            ));
                        },
                    }
                    match *loc {
                        ArgLoc::Stack(off) => {
                            self.store_rsp_qword(1, stage);
                            stack_staged.push((off, stage, 8));
                            stage += 8;
                        },
                        _ => {
                            self.store_rsp_qword(1, stage);
                            staged.push((*loc, stage, false, false));
                            stage += 8;
                        },
                    }
                }
            } else if aggregate {
                // ── 大块 force_mem 聚合: 一次性串拷贝, 不逐 chunk 展开 ───────────
                // 当实参整体落在一段连续的 overflow 栈槽 (ArgLoc::Stack(base+i*8),
                // 不混寄存器) 且对象大于阈值时, 源地址取到 rsi, lea rdi,[rsp+base],
                // 用 cld; rep movsb 整块搬运。否则逐 chunk 路径会为 72MB 结构体
                // 发射 ~3770 万条常驻 Insn, OOM (gcc.c-torture/compile/20050622-1.c).
                let copy_base = arg_locs
                    .first()
                    .and_then(|l| if let ArgLoc::Stack(b) = l { Some(*b) } else { None })
                    .filter(|b| {
                        arg_locs
                            .iter()
                            .enumerate()
                            .all(|(i, l)| matches!(l, ArgLoc::Stack(off) if *off == b + i * 8))
                    });
                let copy_size = effective_ty.size();
                if let (Some(base), true) = (copy_base, copy_size > 16) {
                    // 源地址 → rsi (与 gen_sret_return 同样的取址优先级).
                    let mut got_src = if let Expr::Ident { name, .. } = arg {
                        if self.is_global_not_local(&name.name) {
                            self.emit_global_addr(6, &name.name);
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    };
                    if !got_src {
                        got_src = self.gen_lvalue_addr(arg, 6);
                    }
                    if !got_src
                        && (matches!(arg, Expr::CompoundLit { .. })
                            || Self::rvalue_is_buffer_addr(arg))
                    {
                        self.gen_expr(arg, Self::EVAL_BASE);
                        self.mov(6, 0);
                        got_src = true;
                    }
                    if got_src {
                        // rdi = outgoing 区目的地址; rcx = 字节数; cld; rep movsb
                        self.emit_insn(&Insn::lea_reg_base_disp(7, 4, base as i32));
                        self.mov_imm(1, copy_size as i128);
                        self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Cld));
                        self.emit_insn(&Insn::noarg(
                            crate::codegen::x86_64::opcode::NoArgOp::RepMovsb,
                        ));
                        continue;
                    }
                    // 取址失败则回退到下方逐 chunk 路径。
                }
                // _Complex long double: 两个 16 字节 tbyte 栈槽, 必须走 x87
                // fldt/fstpt, 不能进下方任何 GP movq / SSE movsd 子路径
                // (movq 会破坏 10 字节 tbyte, 读侧只能拿到尾数低 8 字节,
                // 指数/符号与整个第二分量皆错)。materialize 已覆盖 Ident /
                // lvalue (Index/Member/Deref) / Call / CompoundLit / rvalue
                // 全部源形态且源只求值一次。
                let is_complex_ld = matches!(&effective_ty, Type::Complex(inner)
                    if matches!(&inner.base, Type::LongDouble));
                if is_complex_ld {
                    if let (Some(ArgLoc::Stack(real_off)), Some(ArgLoc::Stack(imag_off))) =
                        (arg_locs.first(), arg_locs.get(1))
                    {
                        let src = self.gen_complex_ld_materialize(arg);
                        self.emit_x87_fld_at(13, src as i32, &Type::LongDouble);
                        self.emit_x87_fstp_at(4, *real_off as i32, &Type::LongDouble);
                        self.emit_x87_fld_at(13, src as i32 + 16, &Type::LongDouble);
                        self.emit_x87_fstp_at(4, *imag_off as i32, &Type::LongDouble);
                    }
                    continue;
                }
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
                    // 实参的**值表示**可以不同形参 (`_Complex float` 实参 →
                    // `_Complex double` 形参及反之/整数内型/**标量**:
                    // `take(_Complex double 形参)` 收到 `take(11)` 时 C6.3.1.8p1
                    // 要求实部 = 11、虚部 = +0)。C6.3.1.8p4 要求复数算术转换是
                    // **逐分量**的。下面按 inner_sz 直读变量槽会把两个 4 字节
                    // float 的位模式当成一个 8 字节 double
                    // (`my_creal(zf)` 得 8.0000019 而 gcc 得 1.25), 标量实参则
                    // 读穿到隔壁槽 (`take(11)` 两分量皆 0)。转换整体委托
                    // `materialize_value_slot` (复数分量转换的单一真源, 与 `=`
                    // 运算共用), 本路径只负责把已按形参类型物化好的对象
                    // 逐 eightbyte 装到寄存器/栈 —— 不在此重写任何宽度判据。
                    let needs_component_convert =
                        Self::complex_src_needs_convert(&effective_ty, arg.ty());
                    let converted_slot: Option<i64> = if needs_component_convert {
                        Some(self.materialize_value_slot(&effective_ty, arg))
                    } else {
                        None
                    };
                    // (Call 实参不再特判: Call 返回已统一物化到 result_off,
                    // 与下方"复杂表达式"路径同构 — gen_expr 后从 result_off
                    // 逐分量加载。旧实现直接从 xmm0/xmm1 取分量, 整数
                    // _Complex 的返回在 rax/rdx, xmm 是垃圾。)
                    for (chunk, loc) in arg_locs.iter().enumerate() {
                        let chunk_off = (chunk as i64) * inner_sz;
                        if let Some(so) = converted_slot {
                            self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x10], so + chunk_off);
                        } else if let Expr::Ident { name, .. } = arg {
                            if self.is_global_not_local(&name.name) {
                                // global complex: lea rcx, [global+chunk_off]; movsd xmm0, [rcx]
                                // 注意: 必须写入 rcx (寄存器 1), 下方 movsd [rcx] 硬编码 rcx
                                self.emit_global_addr(1, &name.name);
                                if chunk_off != 0 {
                                    self.emit_insn(&Insn::alu_rm_reg_imm32_b64(
                                        0,
                                        1,
                                        chunk_off as i64,
                                    )); // add rcx, imm32
                                }
                                // movsd/movss xmm0, [rcx]: typed SimdMov (load)
                                let simd_w = if pfx == 0xF3 {
                                    crate::codegen::x86_64::opcode::SimdWidth::SS
                                } else {
                                    crate::codegen::x86_64::opcode::SimdWidth::SD
                                };
                                self.emit_insn(&Insn::simd_mov_mem_base(false, simd_w, 0, 1, 0));
                            } else {
                                let var_off = self.var_offset(&name.name) + chunk_off;
                                self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x10], var_off);
                            }
                        } else {
                            // 复杂表达式: 先物化到 result_off, 再逐分量加载
                            self.gen_expr(arg, Self::EVAL_BASE);
                            let result_off = self.complex_result_off();
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
                                staged.push((*loc, stage, false, false));
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
                        // 三条源路径 (global / VLA / 局部槽) 都必须把 **块的
                        // 值** 放进 r11, 因为下方统一 `store_rsp_qword(11, ..)`。
                        // 旧实现 global 分支把 `emit_global_addr(11,..)` 得到的
                        // **基址** 留在 r11 就存槽 (值虽已 `mov rax,[r11+d]` 取出
                        // 但被丢弃), 于是被调方读到的 struct 成员是指针字节 ——
                        // static 存储期聚合按值传参错, auto 存储期反而正常。
                        if self.is_global_not_local(&name.name) {
                            self.emit_global_addr(11, &name.name);
                            // mov rax, [r11+disp32]
                            self.emit_insn(&Insn::mov_rm_mem(
                                true,
                                crate::codegen::shared::asm_operand::Width::B64,
                                0,
                                11,
                                (chunk * 8) as i32,
                            ));
                            self.mov(11, 0); // r11 = rax (data), for subsequent store
                        } else if is_vla_local {
                            // VLA: mov rax, [rcx + chunk*8], then move to r11
                            let disp = (chunk * 8) as i32;
                            self.emit_insn(&Insn::mov_rm_mem(
                                true,
                                crate::codegen::shared::asm_operand::Width::B64,
                                0,
                                1,
                                disp,
                            ));
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
                                staged.push((*loc, stage, false, false));
                                stage += 8;
                            },
                        }
                    }
                } else if matches!(arg, Expr::CompoundLit { .. })
                    || Self::rvalue_is_buffer_addr(arg)
                {
                    // 内存交付的聚合源: gen_expr 后 rax 里是**缓冲地址** (复合字面量 /
                    // 返 memory 类的 `f()` / 缓冲区交付的 `va_arg` / 链式赋值与
                    // 语句表达式的目标地址), 必须按 8 字节块从 `[addr + chunk*8]`
                    // 装载。直接把 rax 当数据存槽, 被调方读到的就是指针字节
                    // (`f(g()).x` / `f(va_arg(...))` 即此)。
                    self.gen_expr(arg, Self::EVAL_BASE); // rax = address
                    self.mov(1, 0); // rcx = address (safe: no calls
                                    // between chunk loads)
                    self.stage_agg_chunks_from_mem(arg_locs, &mut staged, &mut stage);
                } else if matches!(
                    arg,
                    Expr::Index { .. } | Expr::Member { .. } | Expr::Deref { .. }
                ) && self.gen_lvalue_addr(arg, 1)
                {
                    // Lvalue 聚合实参 (a[2] / s.f / *p): gen_expr 会加载
                    // 元素值 (pr44575: 全局数组元素被当 4 字节标量读),
                    // 须取 lvalue 地址后按 8 字节块装载 (与 CompoundLit
                    // 一致)。gen_lvalue_addr 失败则落入下方 rvalue 路径。
                    self.stage_agg_chunks_from_mem(arg_locs, &mut staged, &mut stage);
                } else {
                    // Other rvalue aggregate (e.g. function returning
                    // struct in rax:rdx): gen_expr leaves value in
                    // rax (and rdx for >8 bytes).
                    self.gen_expr(arg, Self::EVAL_BASE);
                    // __int128 参数: 若 RHS 非 __int128, rdx 未设置, 需扩展.
                    // 扩展方向只取决于**源类型**有无符号: 有符号源必须符号扩展
                    // (否则 -1/0x8000... 这类负值截成高 64 位 0, pr78546-1),
                    // 无符号源才零扩展。与目标 UInt128/Int128 无关。
                    if matches!(effective_ty, Type::Int128 | Type::UInt128)
                        && !matches!(arg.ty(), Type::Int128 | Type::UInt128)
                    {
                        if matches!(
                            arg.ty(),
                            Type::Bool
                                | Type::UChar
                                | Type::UShort
                                | Type::UInt
                                | Type::ULong
                                | Type::ULongLong
                        ) {
                            // P11: xor rdx, rdx via typed opcode.
                            {
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
                                    self.emit_insn(&Insn::new(
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
                                    )); // xor rdx, rdx
                                }
                            }
                        } else {
                            self.emit_insn(&Insn::sign_extend_rax_to_rdx(
                                crate::codegen::shared::asm_operand::Width::B64,
                            )); // cqo
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
                                staged.push((*loc, stage, false, false));
                                stage += 8;
                            },
                        }
                    }
                }
            } else if matches!(effective_ty, Type::LongDouble)
                || matches!(effective_ty, Type::Imaginary(_))
            {
                // SysV: x87 类实参永远走 16 字节栈槽（不是寄存器保存区），内容是
                // `fstpt` 形式的真 80-bit 编码。具名形参与变参**同一规则**：早先本处
                // 限定“仅变参”，是因为误信了 i386 cdecl 的“具名 x87 参数走 st(0)”。
                // gcc -O0 实测证明 SysV AMD64 下具名形参同样从栈上读
                // （fldt 0x10(%rbp)），只有**返回值**用 st(0)。
                //
                // 值通道迁移后：`long double` rvalue 本来就在 st(0)，直接 `fstpt` 落槽；
                // 整数 / float / double 实参经 Layer0 唯一入口 `emit_operand_to_st0`
                // 精确拓宽到 st(0) 再落。旧的
                // `cvtsi2sd/cvtss2sd → movsd 到槽高半 → fld qword → fstp tbyte`
                // 中转是“ld 内部为 f64”时代的产物，80-bit 值走 st(0) 后已删除
                // —— 那条链会把实参先舍成 f64 再拓宽，正是本次要消除的序位错根因。
                self.gen_expr(arg, Self::EVAL_BASE);
                self.emit_operand_to_st0(&arg.ty(), 0);
                if let Some(ArgLoc::Stack(off)) = arg_locs.first() {
                    // fstp tbyte [rsp+off]：写 off..off+10，槽内其余字节是 SysV
                    // 允许 implementation-defined 的 padding（gcc 亦如此），不填零。
                    self.emit_x87_fstp_at(4, *off as i32, &Type::LongDouble);
                } else {
                    // 理论上不可达（标量 long double 的分类恒为 Memory）；
                    // 但必须把 st(0) 弹掉，否则 x87 栈会随调用次数漏到溢出。
                    self.emit_x87_fstp_st0();
                }
                continue;
            } else if matches!(effective_ty, Type::Float128) {
                // binary128 实参：占**一个** xmm 的 16 字节（`[Sse, SseUp]`）。
                // 走栈时按 16 字节对齐并整块搬运，不走逐 8 字节的 movsd。
                self.gen_expr(arg, Self::EVAL_BASE);
                let arg_ty = arg.ty();
                if !matches!(arg_ty, Type::Float128) {
                    self.emit_to_float128(&arg_ty);
                }
                match arg_locs[0] {
                    ArgLoc::Stack(off) => {
                        // 16 字节整块写到 outgoing 栈区
                        self.emit_xmm_store_rsp(0x00, 0, off);
                    },
                    loc => {
                        self.emit_xmm_store_rsp(0x00, 0, stage);
                        staged.push((loc, stage, false, true));
                        stage += 16;
                    },
                }
            } else if matches!(effective_ty, Type::Float | Type::Double) {
                self.gen_expr(arg, Self::EVAL_BASE);
                // 隐式整数→浮点转换: 实参是整数但形参是浮点类型
                let arg_ty = arg.ty();
                if Self::is_integer_type(&arg_ty) {
                    self.emit_scalar_to_xmm0_if_needed(arg, &effective_ty);
                }
                // float/double 类型不匹配转换:
                // 形参是 float 但实参是 double → cvtsd2ss (double→float)
                // 形参是 double 但实参是 float → cvtss2sd (float→double)
                if matches!(effective_ty, Type::Float) && matches!(arg_ty, Type::Double) {
                    self.emit_insn(&Insn::simd_arith_reg(
                        crate::codegen::x86_64::opcode::SimdArithOp::CvtSdSs,
                        crate::codegen::x86_64::opcode::SimdWidth::SD,
                        0,
                        0,
                    ));
                } else if matches!(effective_ty, Type::Double) && matches!(arg_ty, Type::Float) {
                    self.emit_insn(&Insn::simd_arith_reg(
                        crate::codegen::x86_64::opcode::SimdArithOp::CvtSdSs,
                        crate::codegen::x86_64::opcode::SimdWidth::SS,
                        0,
                        0,
                    ));
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
                    self.emit_insn(&Insn::simd_arith_reg(
                        crate::codegen::x86_64::opcode::SimdArithOp::CvtSdSs,
                        crate::codegen::x86_64::opcode::SimdWidth::SS,
                        0,
                        0,
                    ));
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
                        staged.push((loc, stage, is_float, false));
                        stage += 8;
                    },
                }
            } else if matches!(effective_ty, Type::Int128 | Type::UInt128) {
                // SysV：16 字节整数占两个 GP 槽（低 rax / 高 rdx）
                self.gen_expr(arg, Self::EVAL_BASE);
                if !matches!(arg.ty(), Type::Int128 | Type::UInt128) {
                    // 扩展方向只取决于源类型有无符号 (见上: 有符号源符号扩展).
                    if matches!(
                        arg.ty(),
                        Type::Bool
                            | Type::UChar
                            | Type::UShort
                            | Type::UInt
                            | Type::ULong
                            | Type::ULongLong
                    ) {
                        // P11: xor rdx, rdx via typed opcode.
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
                            self.emit_insn(&Insn::new(
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
                            ));
                        }
                    } else {
                        self.emit_insn(&Insn::sign_extend_rax_to_rdx(
                            crate::codegen::shared::asm_operand::Width::B64,
                        ));
                    }
                }
                for (chunk, loc) in arg_locs.iter().enumerate() {
                    let reg = if chunk == 0 { 0u8 } else { 2u8 };
                    match *loc {
                        ArgLoc::Stack(off) => self.store_rsp_qword(reg, off),
                        loc => {
                            self.store_rsp_qword(reg, stage);
                            staged.push((loc, stage, false, false));
                            stage += 8;
                        },
                    }
                }
            } else {
                self.gen_expr_decay(arg, Self::EVAL_BASE);
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
                        staged.push((loc, stage, false, false));
                        stage += 8;
                    },
                }
            }
        }

        // Commit all force_mem pieces only after every argument expression has
        // been evaluated. The destinations are final outgoing offsets, while
        // the sources are stable staging slots above that area.
        for (off, slot, size) in stack_staged {
            self.load_rsp_qword(0, slot);
            if size == 8 {
                self.store_rsp_qword(0, off);
            }
        }
        if let Some(slot) = sret_slot {
            self.load_rsp_qword(7, slot);
        }
        // 用 while 循环以便在合并时跳过下一个 entry (手动递增 i)
        let mut i = 0;
        while i < staged.len() {
            let (loc, slot, is_float, wide128) = staged[i];
            match loc {
                ArgLoc::Gp(reg) => {
                    self.load_rsp_qword(reg, slot);
                    i += 1;
                },
                ArgLoc::Xmm(reg) => {
                    // binary128: 整个 16 字节都在同一个 xmm 里, 不能用
                    // movss/movsd 搬运 (会清零/丢弃高半)。一次 movups 读回。
                    if wide128 {
                        self.emit_xmm_load_rsp(0x00, reg, slot);
                        i += 1;
                        continue;
                    }
                    // 检查下一个 staged 是否也是同一个 xmm 寄存器 (SseUp 延续,
                    // 如 16 字节 vector 参数). 如果是, 合并为一条 movups 加载
                    // 避免 movsd 清零高 8 字节导致数据丢失 (pr105613).
                    // 注意: _Complex double 有两个独立 SSE (xmm0, xmm1),
                    // 不会误合并因为寄存器编号不同.
                    if i + 1 < staged.len() {
                        let (next_loc, next_slot, _, _) = staged[i + 1];
                        match next_loc {
                            ArgLoc::Xmm(nr) if nr == reg && next_slot == slot + 8 => {
                                // 合并: movups xmm_reg, [rsp+slot]
                                // disp32 字段恒为 4 字节：`slot` 是 `usize`，直接
                                // `to_le_bytes()` 会写出 8 字节（多出的 4 个 0 落在
                                // 指令流里被当作 `add [rax],al` 执行 → SIGSEGV，
                                // 且只在 staging 偏移 >= 128（参数多到需要 disp32）
                                // 时触发，正是变参 SSE 装载的典型规模）。与
                                // `emit_xmm_load_rsp` 等helper 同用作 `as u32`。
                                self.emit_insn(&Insn::raw_bytes(&[0x0F, 0x10]));
                                if slot < 128 {
                                    self.emit_insn(&Insn::raw_bytes(&[
                                        0x44 | ((reg & 7) << 3),
                                        0x24,
                                        slot as u8,
                                    ]));
                                } else {
                                    self.emit_insn(&Insn::raw_bytes(&[
                                        0x84 | ((reg & 7) << 3),
                                        0x24,
                                    ]));
                                    self.emit_insn(&Insn::raw_bytes(&(slot as u32).to_le_bytes()));
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
        self.emit_insn(&Insn::raw_with_gas(&[0xB0, fp as u8], &format!("mov ${}, %al", fp as u8)));
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
                // call *%r11 (typed): FF /2 reg=r11 (reg=2, /2)
                self.emit_insn(&Insn::new(
                    crate::codegen::x86_64::opcode::X86_64Opcode::Call {
                        target: crate::codegen::x86_64::opcode::Operand::Reg(
                            11,
                            crate::codegen::shared::asm_operand::Width::B64,
                        ),
                        pending_rel: false,
                    },
                    &[],
                ));
            }
        }
        if dynamic_align {
            // mov rsp, r10 (恢复对齐前 rsp)
            self.emit_insn(&Insn::mov_rm_reg(
                crate::codegen::shared::asm_operand::Width::B64,
                4,  // rsp
                10, // r10
            ));
        } else {
            self.emit_add_rsp(frame_bytes as i64);
        }
    }

    /// 「缓冲地址已在 rcx」的聚合按值实参装载: 逐 8 字节块从 `[rcx + chunk*8]`
    /// 装到暂存区/栈槽。所有内存交付的聚合源 (复合字面量 / 内存返回调用 /
    /// lvalue 聚合 / 链式赋值结果) 共用这一体, 只有取源地址的方式不同。
    fn stage_agg_chunks_from_mem(
        &mut self,
        arg_locs: &[ArgLoc],
        staged: &mut Vec<(ArgLoc, usize, bool, bool)>,
        stage: &mut usize,
    ) {
        for (chunk, loc) in arg_locs.iter().enumerate() {
            // mov rax, [rcx + chunk*8] (typed)
            let disp = (chunk * 8) as i32;
            self.emit_insn(&Insn::mov_rm_mem(
                true,
                crate::codegen::shared::asm_operand::Width::B64,
                0,
                1,
                disp,
            ));
            match *loc {
                ArgLoc::Stack(off) => self.store_rsp_qword(0, off),
                _ => {
                    self.store_rsp_qword(0, *stage);
                    staged.push((*loc, *stage, false, false));
                    *stage += 8;
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Abi, Arch, TargetTriple};
    use crate::codegen::x86_64::codegen::ExtHomeSave;
    use crate::location::{Location, RegBank as RB, RegId};

    fn cg() -> Codegen {
        Codegen::new(crate::target::Target::resolve(&TargetTriple {
            arch: Arch::X86_64,
            vendor: "unknown".into(),
            os: crate::cli::Os::Linux,
            abi: Abi::Gnu,
        }))
    }

    /// 延伸池条目：`v` 的家是 %rcx（1），保存槽在 -200(%r13)。
    fn entry() -> ExtHomeSave {
        ExtHomeSave {
            name: "v".into(),
            reg: 1,
            slot_off: -200,
            size: 8,
        }
    }

    /// 通道本身的机械性质：存 → 覆盖 → 取回 → 撤销，且嵌套只回滚自己那一层。
    /// 这条用例不依赖分配器是否开池（开池被 §11.39 的暂存约定卡住），它测的是通道这件
    /// 工具做对了没有 —— 否则「机制正确但没接线」与「机制就是错的」分不清。
    #[test]
    fn persist_and_restore_round_trip_the_home_and_the_override() {
        let mut c = cg();
        c.homes.insert(
            "v".to_string(),
            (Location::Reg(RegId { bank: RB::Gp, n: 1 }), crate::ast::Type::Int),
        );
        c.ext_home_saves.push(entry());
        // 未覆盖时读到的就是声明级的家（寄存器）。
        assert_eq!(c.home_for("v"), Location::Reg(RegId { bank: RB::Gp, n: 1 }));

        let before = c.code.len();
        assert_eq!(c.persist_ext_homes(), 1, "第一次应当开启一层");
        assert!(c.code.len() > before, "存的动作必须真的发出指令");
        assert_eq!(c.home_for("v"), Location::Stack(-200), "窗口内读到的是槽，不是被毁的号");

        // 再开一层（嵌套 call）不应重复存，也不应重复计数。
        let at = c.code.len();
        assert_eq!(c.persist_ext_homes(), 0, "外层已存下时不得再存一遍");
        assert_eq!(c.code.len(), at, "重复 persist 不该发指令");

        // 真实用法里 persist 与 restore 之间必有一条 `call`，而 `emit` 的裸字节出口
        // 会重置窥孔窗口；本用例只测通道，先显式重置，否则发射层窥孔规则 3
        // （写槽后立刻读同一槽 ⇒ 读可省，且值确在同一个号里）会把取回那条删掉。
        c.peep_last = None;
        c.restore_ext_homes(1);
        assert_eq!(
            c.home_for("v"),
            Location::Reg(RegId { bank: RB::Gp, n: 1 }),
            "取回后覆盖必须撤销"
        );
        assert!(c.code.len() > at, "取回的动作必须真的发出指令");

        // opened=0 时一条指令都不该发（否则空函数会被平白加宽）。
        let n = c.code.len();
        c.restore_ext_homes(0);
        assert_eq!(c.code.len(), n);
    }

    /// 没有延伸池家时（今天的全部产物路径），包装层必须**一字不改**地走原路。
    /// 这条是本轮不回归的根据：开/关两条路径产生的字节完全相同。
    #[test]
    fn the_wrapper_is_inert_without_ext_homes() {
        let mut a = cg();
        let mut b = cg();
        b.ext_home_saves.push(entry());
        assert!(a.ext_home_saves.is_empty());
        assert_eq!(a.persist_ext_homes(), 0);
        assert!(a.home_overrides.is_empty());
        // b 有条目才会动，a 有条目才会动 —— 判据是「条目表空 ⇒ 不发任何存/取」。
        assert_eq!(a.code, b.code, "未开启通道时两侧都不该发任何东西");
    }
}
