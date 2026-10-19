// src/codegen/x86_64/emit/expr/cast.rs
// Expr::Cast arm 的 codegen (gen_expr 中的显式类型转换分支, 含 vector
// 复合字面量、同宽 bitcast、指针/数组 decay、整数/浮点窄扩展等)。
// 本文件由 mod.rs 逐字搬迁而来 (batch 4f): arm body 原样剪切进 `impl Codegen`
// 新方法 `gen_expr_cast`, 语义 diff 为 0。旧影子 `gen_cast_expr` 自由函数一并作废。

use crate::ast::{Expr, Type};
use crate::codegen::x86_64::insn::Insn;
use crate::codegen::x86_64::{AggDst, Codegen};

impl Codegen {
    /// gen_expr 中 Expr::Cast arm 的搬迁体, 逐字复制, 未改逻辑。
    pub fn gen_expr_cast(&mut self, ty: &Type, e: &Expr, dst: u8) {
        // (v4si){...} 复合字面量：parser 降级为 Cast + InitList
        if let (Type::Vector(elem_ty, n), Expr::InitList { inits, .. }) = (ty, e) {
            self.gen_vector_init_rvalue(inits, &elem_ty.base, *n, 0);
            // rax 指向临时槽, 供后续 aggregate_assign 或向量操作使用
            let vec_size = elem_ty.base.size() * n;
            let temp_off = self.vector_temp_base(0, vec_size);
            self.emit_lea_r13(0, temp_off);
            return;
        }
        let src_ty = e.ty();
        // 标量整数→向量 (同尺寸): 直接用 movq 将 GP 寄存器值移入 XMM
        // 然后存到临时槽, 供后续 aggregate assign 使用
        if let Type::Vector(_, _) = ty {
            if src_ty.size() == ty.size() && src_ty.size() <= 8 && src_ty.is_int() {
                self.gen_expr(e, dst);
                if dst != 0 {
                    self.mov(0, dst);
                }
                // movq xmm0, rax: 66 REX.W 0F 6E C0
                self.emit_movq_gp_to_xmm(0, 0);
                // 存 xmm0 到临时槽, 供后续 aggregate assign 从内存读
                let vec_size = ty.size();
                let temp_off = self.vector_temp_base(0, vec_size);
                // movq [r13+temp_off], xmm0: 66 REX.B 0F D6 /r (store)
                // 位移可能超出 8 位, 用 mod=10 (32-bit disp)
                // ModRM: mod=10, reg=xmm0(000), r/m=100(SIB)
                // SIB: scale=00, index=100(none), base=101(r13) + REX.B
                let rex = 0x41; // REX.B for r13
                self.emit_insn(&Insn::raw_bytes(&[0x66, rex, 0x0F, 0xD6, 0x84, 0x25]));
                self.emit_insn(&Insn::raw_bytes(&(temp_off as i32).to_le_bytes()));
                // rax = 临时槽地址 (供后续使用)
                self.emit_lea_r13(0, temp_off);
                return;
            }
        }
        // 向量→标量整数 (同尺寸): 从向量内存加载到 GP 寄存器
        // 关键: vector 可能 < 8B, 不能固定 movq 读 8 字节 (读到垃圾).
        // 改为按 vector 实际 size 直接 load 到 GP, 再 zero-extend 到 64 位.
        if let Type::Vector(_, _) = src_ty {
            if src_ty.size() == ty.size() && ty.size() <= 8 && ty.is_int() {
                // 取 vector 地址到 rax (rvalue 先物化到临时槽)
                if !self.gen_lvalue_addr(e, Self::EVAL_BASE) {
                    self.gen_expr(e, Self::EVAL_BASE);
                    let toff = self.vector_temp_base(0, src_ty.size());
                    self.emit_lea_r13(0, toff);
                }
                // rax = vector 内存地址, 按实际 size 加载到 rax
                let sz = src_ty.size();
                match sz {
                    1 => {
                        // P11: movzx eax, byte [rax] via MovExt mem typed.
                        self.emit_insn(&Insn::mov_ext_mem(
                            false,
                            crate::codegen::shared::asm_operand::Width::B8,
                            crate::codegen::shared::asm_operand::Width::B32,
                            0,
                            0,
                            0,
                        ));
                    },
                    2 => {
                        // P11: movzx eax, word [rax] via MovExt mem typed.
                        self.emit_insn(&Insn::mov_ext_mem(
                            false,
                            crate::codegen::shared::asm_operand::Width::B16,
                            crate::codegen::shared::asm_operand::Width::B32,
                            0,
                            0,
                            0,
                        ));
                    },
                    4 => {
                        // mov eax, dword [rax] (自动 zero-extend 到 64 位)
                        self.emit_insn(&Insn::new(
                            crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                                rip_relative: false,
                                width: crate::codegen::shared::asm_operand::Width::B32,
                                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                    0,
                                    crate::codegen::shared::asm_operand::Width::B32,
                                ),
                                src: crate::codegen::x86_64::opcode::Operand::Mem(
                                    crate::codegen::x86_64::opcode::MemOperand {
                                        disp: 0,
                                        base: Some(0),
                                        index: None,
                                        seg: None,
                                    },
                                ),
                            },
                            &[],
                        ));
                    },
                    8 => {
                        // mov rax, qword [rax] (64 位无 truncation)
                        self.emit_insn(&Insn::new(
                            crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                                rip_relative: false,
                                width: crate::codegen::shared::asm_operand::Width::B64,
                                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                    0,
                                    crate::codegen::shared::asm_operand::Width::B64,
                                ),
                                src: crate::codegen::x86_64::opcode::Operand::Mem(
                                    crate::codegen::x86_64::opcode::MemOperand {
                                        disp: 0,
                                        base: Some(0),
                                        index: None,
                                        seg: None,
                                    },
                                ),
                            },
                            &[],
                        ));
                    },
                    _ => {
                        // 更大? 不可能, 条件已限定 <= 8
                        self.emit_insn(&Insn::new(
                            crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                                rip_relative: false,
                                width: crate::codegen::shared::asm_operand::Width::B64,
                                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                    0,
                                    crate::codegen::shared::asm_operand::Width::B64,
                                ),
                                src: crate::codegen::x86_64::opcode::Operand::Mem(
                                    crate::codegen::x86_64::opcode::MemOperand {
                                        disp: 0,
                                        base: Some(0),
                                        index: None,
                                        seg: None,
                                    },
                                ),
                            },
                            &[],
                        ));
                    },
                }
                if dst != 0 {
                    self.mov(dst, 0);
                }
                return;
            }
        }
        // ── _Complex → _Complex: 逐分量算术转换 (C6.3.1.8p4) ────────────────
        // 复数的**元素类型参与值表示**, 不像对象表示那样可整体位拷贝/位重铸:
        // `_Complex float` 拓宽到 `_Complex double` 必须对实部、虚部各做一次
        // cvtss2sd。旧实现在这条轴上没有任何臂: 同宽 (如 `_Complex int` →
        // `_Complex float`) 落 `gen_same_size_bitcast` 做位重铸, 不同宽落末尾
        // `_ => {}` 空操作 —— 两者都是「把源分量的位模式当成目标分量的位模
        // 式」, 于是 `cimag(zf)` (libm 形参是 `_Complex double`) 把 1.25f 与
        // 2.5f 的位模式读成一个 double (5.39e-315)。
        // 动作只有一个: 按目标复数类型**赋值**一次。分量宽度与精度转换全部
        // 委托 `gen_aggregate_assign_to` (复数分量转换的单一真源, 与 `=` 运算
        // 共用), 本站点不重复实现任何宽度/精度判据; 交付沿用 complex rvalue
        // 统一约定 (`finish_complex_assign_value`: 实部 @ `complex_result_off`、
        // 虚部 @ +inner、rax = 该带地址、f/d 再按目标内型重载实部到 xmm0)。
        if let Type::Complex(ref dst_c) = ty {
            let src_inner = match src_ty {
                Type::Complex(ref c) | Type::Imaginary(ref c) => Some(c.base.clone()),
                _ => None,
            };
            if let Some(si) = src_inner {
                if si != dst_c.base {
                    let slot = self.alloc_sret_slot(ty.size().max(16));
                    let cdst = AggDst::Local {
                        off: slot,
                        vla: None,
                    };
                    self.gen_aggregate_assign_to(&cdst, ty, e);
                    self.finish_complex_assign_value(&cdst, ty);
                    if dst != 0 {
                        self.mov(dst, 0);
                    }
                    return;
                }
            }
        }
        if self.gen_same_size_bitcast(e, ty, dst) {
            return;
        }
        // 数组在 cast 中 decay 为首元素地址, 如 (const __m128i *) buf.
        // 直接 gen_expr 会误读数组内容 (mov 而非 lea).
        if matches!(src_ty, Type::Array(_, _)) && self.gen_lvalue_addr(e, dst) {
            return;
        }
        // ── a6: `long double` 交界转换集中分流（单一真源）─────────────────────
        // 值通道判据只有 `float_in_st0` 一个；st(0) ↔ xmm0 / GP 寄存器的搬运
        // 全部经 special/x87_floating_point.rs 的 Layer0 原语。下面既有的
        // `(LongDouble, 整型)` 分支按 xmm0 取值，所以那一类先收窄再落回原分支。
        let src_is_ld = matches!(src_ty, Type::LongDouble);
        let dst_is_ld = matches!(ty, Type::LongDouble);
        let src_is_num = (Self::is_integer_type(&src_ty)
            && !matches!(src_ty, Type::Int128 | Type::UInt128))
            || matches!(src_ty, Type::Float | Type::Double);
        let dst_is_num = (Self::is_integer_type(ty) && !matches!(ty, Type::Int128 | Type::UInt128))
            || matches!(ty, Type::Float | Type::Double);
        let mut src_materialized = false;
        if dst_is_ld && !src_is_ld && src_is_num {
            // → long double：整数 / float / double 精确拓宽到 st(0)（80-bit 是
            // 它们的真超集，不存在舍入）。结果交付在 st(0)，`dst` 寄存器不承载。
            self.gen_expr(e, dst);
            self.emit_operand_to_st0(&src_ty, dst);
            return;
        }
        if src_is_ld && !dst_is_ld && dst_is_num {
            self.gen_expr(e, dst); // st(0) = 80-bit 值
            src_materialized = true;
            match ty {
                // 80→64 / 80→32 都是单步舍入，不经中间格式
                Type::Double => {
                    self.emit_x87_to_xmm0();
                    return;
                },
                Type::Float => {
                    self.emit_x87_to_xmm0_f32();
                    return;
                },
                // ld → 整型：先落 f64 到 xmm0，再走下面按 xmm0 取值的既有分支。
                // （80→64→int 存在罕见双舍入边界差，见迁移文档“仍未解决”清单）
                // 例外：64 位 (有/无符号) 直接走 x87 精确整数转换，必须保持
                // st(0) 不动——经 f64 会丢掉 >= 2^52 的精度。
                _ => {
                    if !matches!(
                        ty,
                        Type::ULong | Type::ULongLong | Type::Long | Type::LongLong
                    ) {
                        self.emit_x87_to_xmm0();
                    }
                },
            }
        }
        if !src_materialized {
            self.gen_expr(e, dst);
        }
        // 类型转换
        match (&src_ty, ty) {
            (src, Type::Int128 | Type::UInt128) if !matches!(src, Type::Int128 | Type::UInt128) => {
                if dst != 0 {
                    self.mov(0, dst);
                }
                if matches!(
                    src,
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
                if dst != 0 {
                    self.mov(dst, 0);
                }
            },
            // unsigned → signed same-size integer cast:
            // The value in dst is zero-extended (from loading
            // unsigned), but the signed consumer (e.g. idiv)
            // needs sign-extension.  Emit movsxd / movswx /
            // movsbw to re-interpret.
            (Type::UInt, Type::Int) => {
                // P11: movsxd dst, dst (sign-extend 32→64) via MovExt typed.
                use crate::codegen::x86_64::emit::gas::REG32 as GAS_REG32;
                use crate::codegen::x86_64::emit::gas::REG64 as GAS_REG64;
                let gas = format!(
                    "movsxd {}, {}",
                    GAS_REG32[(dst & 15) as usize],
                    GAS_REG64[(dst & 15) as usize]
                );
                let insn = Insn::mov_ext(
                    true,
                    crate::codegen::shared::asm_operand::Width::B32,
                    crate::codegen::shared::asm_operand::Width::B64,
                    dst,
                    dst,
                )
                .with_gas(gas);
                self.emit_insn(&insn);
            },
            // signed → unsigned same-size: 截断到类型宽度并零扩展
            // 如 (unsigned int)(signed_expr) 需要确保高位为 0
            (Type::Int, Type::UInt)
            | (Type::Long, Type::ULong)
            | (Type::LongLong, Type::ULongLong) => {
                // mov dst32, dst32 (零扩展 32→64) 或 mov dst64, dst64
                let sz = ty.size();
                if sz <= 4 {
                    // P11: mov dst32, dst32 via MovRm B32 typed (self-MODRM, REX.R+B=0x05 when dst>=8).
                    self.emit_insn(&Insn::mov_rm_reg(
                        crate::codegen::shared::asm_operand::Width::B32,
                        dst,
                        dst,
                    ));
                }
                // 64-bit: 无需处理 (已经是 64 位)
            },
            (Type::UShort, Type::Short) => {
                // movswl dst, dst (sign-extend 16→32, zero-extend to 64)
                self.emit_insn(&Insn::mov_ext(
                    true,
                    crate::codegen::shared::asm_operand::Width::B16,
                    crate::codegen::shared::asm_operand::Width::B64,
                    dst,
                    dst,
                ));
            },
            (Type::UChar, Type::SChar | Type::Char) => {
                // movsbl dst, dst (sign-extend 8→32, zero to 64)
                self.emit_insn(&Insn::mov_ext(
                    true,
                    crate::codegen::shared::asm_operand::Width::B8,
                    crate::codegen::shared::asm_operand::Width::B64,
                    dst,
                    dst,
                ));
            },
            // int → float/double
            // 源整数值由 gen_expr(e, dst) 求值到 dst 寄存器,
            // 转换指令必须读取 dst (而非固定 rax), 否则嵌套在
            // Binop 右操作数 (dst=rcx 等) 时会读到错误寄存器.
            (
                src @ (Type::Int | Type::Long | Type::LongLong | Type::Char | Type::Short),
                Type::Float,
            ) => {
                // P11: cvtsi2ss xmm0, dst via typed Cvt2Sd. 字节与 disasm 输出均与
                // 手写 raw_with_gas 路径逐条等价 (encoder 只在 wide||dst>=8 时发 REX).
                let wide = matches!(src, Type::Long | Type::LongLong);
                if wide {
                    self.emit_insn(&Insn::cvt2sd_reg(
                        true,
                        crate::codegen::x86_64::opcode::SimdWidth::SS,
                        0,
                        dst,
                    ));
                } else {
                    self.emit_insn(&Insn::cvt2sd_reg_b32(
                        true,
                        crate::codegen::x86_64::opcode::SimdWidth::SS,
                        0,
                        dst,
                    ));
                }
            },
            (
                src @ (Type::Int | Type::Long | Type::LongLong | Type::Char | Type::Short),
                Type::Double | Type::LongDouble,
            ) => {
                // P11: cvtsi2sd xmm0, dst via typed Cvt2Sd. 同上字节等价.
                let wide = matches!(src, Type::Long | Type::LongLong);
                if wide {
                    self.emit_insn(&Insn::cvt2sd_reg(
                        true,
                        crate::codegen::x86_64::opcode::SimdWidth::SD,
                        0,
                        dst,
                    ));
                } else {
                    self.emit_insn(&Insn::cvt2sd_reg_b32(
                        true,
                        crate::codegen::x86_64::opcode::SimdWidth::SD,
                        0,
                        dst,
                    ));
                }
                // long double 的值通道是 st(0), 不是 xmm0。cvtsi2sd 只把 (double)int
                // 放进 xmm0; 若不搬到 st(0), 消费端 emit_store_scalar_float_at 见
                // src_ty==LongDouble 会以为值已在 st(0) 而跳过搬运, 随后 fstp tbyte
                // 从**空** x87 栈弹寄存器 → indefinite QNaN (局部 `ld = (ld)int`
                // 静默变 NaN, 实测 fp-int-convert 的 (signed char)0→_Float64x=-nan)。
                // 补一步 xmm0→st(0) (double 精度, 与 dcc 的 long double 运算精度一致)。
                if matches!(ty, Type::LongDouble) {
                    self.emit_xmm0_to_st0(&Type::Double);
                }
            },
            // float/double → int
            // 浮点值按约定在 xmm0, 转换结果必须写入 dst 寄存器,
            // 否则嵌套在 Binop 右操作数 (dst=rcx) 时结果被覆盖.
            (Type::Float, Type::Int | Type::Long | Type::LongLong | Type::Char | Type::Short) => {
                // cvttss2si dst, xmm0 (F3 0F 2C /r)
                let wide = matches!(ty, Type::Long | Type::LongLong);
                // cvt 的 REX 只在真的扩了字段时才发：`0x40 | 0 | 0` 是什么也不扩的裸前缀，
                // 多占一字节并被 objdump 标成 `rex cvttsd2si`（多余位的唯一读数）。
                let rex = (if wide { 0x08 } else { 0 }) | (if dst >= 8 { 0x04 } else { 0 });
                let dst_name = crate::codegen::x86_64::abi::regnames::att_name_of_num(
                    dst,
                    if wide { 64 } else { 32 },
                )
                .unwrap_or_else(|| "%eax".to_string());
                let gas = format!("cvttss2si %xmm0, {}", dst_name);
                if rex != 0 {
                    self.emit_insn(&Insn::raw_with_gas(
                        &[0xF3, 0x40 | rex, 0x0F, 0x2C, 0xC0 | ((dst & 7) << 3)],
                        &gas,
                    ));
                } else {
                    self.emit_insn(&Insn::raw_with_gas(
                        &[0xF3, 0x0F, 0x2C, 0xC0 | ((dst & 7) << 3)],
                        &gas,
                    ));
                }
                self.emit_float_to_int_saturate(dst, &Type::Float, wide);
            },
            (Type::Double, Type::Int | Type::Long | Type::LongLong | Type::Char | Type::Short) => {
                // cvttsd2si dst, xmm0 (F2 0F 2C /r)
                let wide = matches!(ty, Type::Long | Type::LongLong);
                // cvt 的 REX 只在真的扩了字段时才发：`0x40 | 0 | 0` 是什么也不扩的裸前缀，
                // 多占一字节并被 objdump 标成 `rex cvttsd2si`（多余位的唯一读数）。
                let rex = (if wide { 0x08 } else { 0 }) | (if dst >= 8 { 0x04 } else { 0 });
                let dst_name = crate::codegen::x86_64::abi::regnames::att_name_of_num(
                    dst,
                    if wide { 64 } else { 32 },
                )
                .unwrap_or_else(|| "%eax".to_string());
                let gas = format!("cvttsd2si %xmm0, {}", dst_name);
                if rex != 0 {
                    self.emit_insn(&Insn::raw_with_gas(
                        &[0xF2, 0x40 | rex, 0x0F, 0x2C, 0xC0 | ((dst & 7) << 3)],
                        &gas,
                    ));
                } else {
                    self.emit_insn(&Insn::raw_with_gas(
                        &[0xF2, 0x0F, 0x2C, 0xC0 | ((dst & 7) << 3)],
                        &gas,
                    ));
                }
                self.emit_float_to_int_saturate(dst, &Type::Double, wide);
            },
            (
                Type::LongDouble,
                Type::Int | Type::Long | Type::LongLong | Type::Char | Type::Short,
            ) => {
                let wide = matches!(ty, Type::Long | Type::LongLong);
                // 64 位有符号走精确 x87 整数转换：值仍在 st(0)（上面已跳过
                // emit_x87_to_xmm0），经 f64 会丢掉 >= 2^52 的精度。
                if wide {
                    self.emit_x87_long_double_to_int64(dst);
                    return;
                }
                // long double → 窄整型：值已由上面落成 f64 到 xmm0
                // cvttsd2si dst, xmm0
                // cvt 的 REX 只在真的扩了字段时才发：`0x40 | 0 | 0` 是什么也不扩的裸前缀，
                // 多占一字节并被 objdump 标成 `rex cvttsd2si`（多余位的唯一读数）。
                let rex = (if wide { 0x08 } else { 0 }) | (if dst >= 8 { 0x04 } else { 0 });
                let dst_name = crate::codegen::x86_64::abi::regnames::att_name_of_num(
                    dst,
                    if wide { 64 } else { 32 },
                )
                .unwrap_or_else(|| "%eax".to_string());
                let gas = format!("cvttsd2si %xmm0, {}", dst_name);
                if rex != 0 {
                    self.emit_insn(&Insn::raw_with_gas(
                        &[0xF2, 0x40 | rex, 0x0F, 0x2C, 0xC0 | ((dst & 7) << 3)],
                        &gas,
                    ));
                } else {
                    self.emit_insn(&Insn::raw_with_gas(
                        &[0xF2, 0x0F, 0x2C, 0xC0 | ((dst & 7) << 3)],
                        &gas,
                    ));
                }
                self.emit_float_to_int_saturate(dst, &Type::LongDouble, wide);
            },
            (Type::Complex(inner), Type::Int | Type::Long | Type::Char | Type::Short) => {
                let is_f32 = matches!(&inner.base, Type::Float);
                match e {
                    Expr::Binop { .. } => {
                        // gen_complex_binop 已将实部留在 xmm0
                    },
                    Expr::Ident { name, .. } => {
                        let off = self.var_offset(&name.name);
                        let pfx = if is_f32 { 0xF3u8 } else { 0xF2u8 };
                        self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x10], off);
                    },
                    _ => {
                        let result_off = self.complex_result_off();
                        let pfx = if is_f32 { 0xF3u8 } else { 0xF2u8 };
                        self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x10], result_off);
                    },
                }
                if is_f32 {
                    // P11: cvttss2si eax, xmm0 — dst/gpr 固定 0, encoder 字节与手写等价.
                    self.emit_insn(&Insn::cvt2sd_reg_b32(
                        false,
                        crate::codegen::x86_64::opcode::SimdWidth::SS,
                        0,
                        0,
                    ));
                } else {
                    self.emit_insn(&Insn::cvt2sd_reg_b32(
                        false,
                        crate::codegen::x86_64::opcode::SimdWidth::SD,
                        0,
                        0,
                    ));
                }
                if dst != 0 {
                    self.mov(dst, 0);
                }
            },
            // float ↔ double
            (Type::Float, Type::Double | Type::LongDouble) => {
                // cvtss2sd xmm0, xmm0 (F3 0F 5A C0)
                self.emit_insn(&Insn::simd_arith_reg(
                    crate::codegen::x86_64::opcode::SimdArithOp::CvtSdSs,
                    crate::codegen::x86_64::opcode::SimdWidth::SS,
                    0,
                    0,
                ));
            },
            (Type::Double | Type::LongDouble, Type::Float) => {
                // cvtsd2ss xmm0, xmm0 (F2 0F 5A C0)
                self.emit_insn(&Insn::simd_arith_reg(
                    crate::codegen::x86_64::opcode::SimdArithOp::CvtSdSs,
                    crate::codegen::x86_64::opcode::SimdWidth::SD,
                    0,
                    0,
                ));
            },
            // unsigned int (<=32bit) → float/double:
            // 零扩展到 64 位后值一定落在 signed int64 正半轴,
            // 可直接用 cvtsi2ss/cvtsi2sd (与 signed 路径相同指令).
            (src @ (Type::Bool | Type::UChar | Type::UShort | Type::UInt), Type::Float) => {
                // 零扩展: 截断到源宽度, 高 32 位自动清零
                self.emit_zero_extend(dst, src);
                // P11: cvtsi2ss xmm0, dst via Cvt2Sd typed (REX.W=0x48|B when dst>=8).
                let dst_name = crate::codegen::x86_64::abi::regnames::att_name_of_num(dst, 64)
                    .unwrap_or_else(|| "%rax".to_string());
                let gas = format!("cvtsi2ss {}, %xmm0", dst_name);
                let insn =
                    Insn::cvt2sd_reg(true, crate::codegen::x86_64::opcode::SimdWidth::SS, 0, dst)
                        .with_gas(gas);
                self.emit_insn(&insn);
            },
            (
                src @ (Type::Bool | Type::UChar | Type::UShort | Type::UInt),
                Type::Double | Type::LongDouble,
            ) => {
                self.emit_zero_extend(dst, src);
                // P11: cvtsi2sd xmm0, dst via Cvt2Sd typed.
                let dst_name = crate::codegen::x86_64::abi::regnames::att_name_of_num(dst, 64)
                    .unwrap_or_else(|| "%rax".to_string());
                let gas = format!("cvtsi2sd {}, %xmm0", dst_name);
                let insn =
                    Insn::cvt2sd_reg(true, crate::codegen::x86_64::opcode::SimdWidth::SD, 0, dst)
                        .with_gas(gas);
                self.emit_insn(&insn);
            },
            // unsigned long/longlong → float/double:
            // 64 位无符号值可能超过 signed int64 范围.
            // 策略: 检查符号位, 正数直接 cvtsi2ss/sd;
            // 负数 (bit63=1) 先 shr 1, 转换后乘 2.0.
            (Type::ULong | Type::ULongLong, Type::Float) => {
                self.emit_ulong_to_float(dst, false);
            },
            (Type::ULong | Type::ULongLong, Type::Double) => {
                self.emit_ulong_to_float(dst, true);
            },
            // u64 → long double: 直接走 emit_int_to_st0 的 u64 拆半路径 (x87 fild 64位)
            // —— 这是唯一能精确表达整个 u64 范围的通道 (xmm0-f64 只有 53 位尾数).
            (Type::ULong | Type::ULongLong, Type::LongDouble) => {
                self.emit_int_to_st0(ty, dst);
            },
            // float/double → unsigned int (<=32bit):
            // 单一真源 `emit_float_to_unsigned` —— 用 64 位有符号截断转换再按
            // 目标宽度截断 (旧实现在此手写 32 位 cvtt + 饱和, 会把 [2^31,2^32)
            // 夹成 INT_MAX), 负源值钳 0。
            (
                src @ (Type::Float | Type::Double | Type::LongDouble),
                Type::Bool | Type::UChar | Type::UShort | Type::UInt,
            ) => {
                // long double 内部为 double, 值已在 xmm0
                self.emit_float_to_unsigned(dst, src, ty);
            },
            // float/double → ulong/ulonglong:
            // cvttss2si/cvttsd2si 结果若为非负即正确;
            // 若为负 (说明原值 > INT64_MAX), 加 2^64 校正.
            (
                src @ (Type::Float | Type::Double | Type::LongDouble),
                Type::ULong | Type::ULongLong,
            ) => {
                // long double 内部为 double, 值已在 xmm0
                self.emit_float_to_unsigned(dst, src, ty);
            },
            // 整数窄化转换: 源类型宽度 > 目标类型宽度时需要截断
            (src, dst_ty) if src.size() > dst_ty.size() => {
                // 目标为无符号小类型: 截断到目标宽度 (零扩展语义)
                // 目标为有符号小类型: 截断后符号扩展
                match dst_ty {
                    Type::Bool | Type::UChar => {
                        // P11: and dst64, 0xFF via AluRm B64 imm32 typed (REX.W=0x48|B).
                        self.emit_insn(&Insn::alu_rm_reg_imm32_b64(
                            0x4, /* /4=and */
                            dst, 0xFF,
                        ));
                    },
                    Type::Char | Type::SChar | Type::Short => {
                        // 截断到 8/16 位后符号扩展
                        let bits = dst_ty.size() * 8;
                        if bits == 8 {
                            // movsbq dst, dst (sign-extend 8→64)
                            self.emit_insn(&Insn::mov_ext(
                                true,
                                crate::codegen::shared::asm_operand::Width::B8,
                                crate::codegen::shared::asm_operand::Width::B64,
                                dst,
                                dst,
                            ));
                        } else {
                            // movswq dst, dst (sign-extend 16→64)
                            self.emit_insn(&Insn::mov_ext(
                                true,
                                crate::codegen::shared::asm_operand::Width::B16,
                                crate::codegen::shared::asm_operand::Width::B64,
                                dst,
                                dst,
                            ));
                        }
                    },
                    Type::UShort => {
                        // P11: and dst64, 0xFFFF via AluRm B64 imm32 typed.
                        self.emit_insn(&Insn::alu_rm_reg_imm32_b64(
                            0x4, /* /4=and */
                            dst, 0xFFFF,
                        ));
                    },
                    Type::UInt => {
                        // mov eax, eax (32位零扩展到64位)
                        // P11: via MovRm B32 typed (self-MODRM, REX.R+B=0x05 when dst>=8).
                        use crate::codegen::x86_64::emit::gas::REG32 as GAS_REG32;
                        let gas = format!(
                            "mov {}, {}",
                            GAS_REG32[(dst & 15) as usize],
                            GAS_REG32[(dst & 15) as usize]
                        );
                        let insn = Insn::mov_rm_reg(
                            crate::codegen::shared::asm_operand::Width::B32,
                            dst,
                            dst,
                        )
                        .with_gas(gas);
                        self.emit_insn(&insn);
                    },
                    Type::Int => {
                        // movsxd dst, dst (sign-extend 32→64)
                        // 截断到 32 位后符号扩展, 供后续有符号运算
                        // (如 idiv, cmp) 使用正确的 64 位值.
                        // P11: via MovExt typed (REX.W=1, REX.R+B self-MODRM).
                        use crate::codegen::x86_64::emit::gas::REG32 as GAS_REG32;
                        use crate::codegen::x86_64::emit::gas::REG64 as GAS_REG64;
                        let gas = format!(
                            "movsxd {}, {}",
                            GAS_REG32[(dst & 15) as usize],
                            GAS_REG64[(dst & 15) as usize]
                        );
                        let insn = Insn::mov_ext(
                            true,
                            crate::codegen::shared::asm_operand::Width::B32,
                            crate::codegen::shared::asm_operand::Width::B64,
                            dst,
                            dst,
                        )
                        .with_gas(gas);
                        self.emit_insn(&insn);
                    },
                    _ => {},
                }
            },
            // 整数缩小转换: 截断到目标类型宽度并符号/零扩展.
            // 例如 (char)0xff: Int 值 255 需截断为 8 位再符号扩展为 -1.
            // 前面已处理的特定转换 (Int128/浮点/复数等) 不会到达此处.
            (src, dst_ty) if Self::is_int_narrow(src, dst_ty) => {
                self.gen_int_narrow_cast(dst, dst_ty);
            },
            // 整数宽度提升但符号性改变 (sign-change widening).
            // 小类型 (< int) 在寄存器中以符号/零扩展形式存在于 64 位中.
            // 当 Cast 到不同符号性的类型时, 需重新做扩展.
            (src, dst_ty)
                if Self::is_integer_type(src)
                    && Self::is_integer_type(dst_ty)
                    && src.size() < 4 =>
            {
                let src_signed = Self::is_signed_int(src) || matches!(src, Type::Char);
                let dst_signed = Self::is_signed_int(dst_ty) || matches!(dst_ty, Type::Char);
                if src_signed && !dst_signed {
                    // 有符号源 → 无符号目标: 寄存器中是符号扩展值,
                    // 需截断到目标宽度 (零扩展语义).
                    let dst_bits = dst_ty.size() * 8;
                    match dst_bits {
                        8 => {
                            // movzbl %al, %eax (截断到 8 位, 零扩展到 32 位)
                            // P11: MovExt encode 自动处理 REX (dst>=8 → REX.R+B=0x04+0x01=0x05).
                            let insn = Insn::new(
                                crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                                    rip_relative: false,
                                    sign: false,
                                    from: crate::codegen::shared::asm_operand::Width::B8,
                                    to: crate::codegen::shared::asm_operand::Width::B32,
                                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                        dst,
                                        crate::codegen::shared::asm_operand::Width::B32,
                                    ),
                                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                                        dst,
                                        crate::codegen::shared::asm_operand::Width::B8,
                                    ),
                                },
                                &[],
                            );
                            self.emit_insn(&insn);
                        },
                        16 => {
                            // movzwl %ax, %eax (截断到 16 位, 零扩展到 32 位)
                            let insn = Insn::new(
                                crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                                    rip_relative: false,
                                    sign: false,
                                    from: crate::codegen::shared::asm_operand::Width::B16,
                                    to: crate::codegen::shared::asm_operand::Width::B32,
                                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                        dst,
                                        crate::codegen::shared::asm_operand::Width::B32,
                                    ),
                                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                                        dst,
                                        crate::codegen::shared::asm_operand::Width::B16,
                                    ),
                                },
                                &[],
                            );
                            self.emit_insn(&insn);
                        },
                        32 => {
                            // mov dst32, dst32 (截断到 32 位, 零扩展到 64 位)
                            // P11: via MovRm B32 typed (self-MODRM, REX.R+B=0x05 when dst>=8).
                            self.emit_insn(&Insn::mov_rm_reg(
                                crate::codegen::shared::asm_operand::Width::B32,
                                dst,
                                dst,
                            ));
                        },
                        _ => {},
                    }
                } else if !src_signed && dst_signed && dst_ty.size() <= src.size() {
                    // 无符号源 → 有符号目标, 目标不比源宽:
                    // 寄存器中是零扩展值, 需截断到目标宽度并符号扩展.
                    let dst_bits = dst_ty.size() * 8;
                    match dst_bits {
                        8 => {
                            // movsbq %al, %rax (截断到 8 位, 符号扩展)
                            self.emit_insn(&Insn::mov_ext(
                                true,
                                crate::codegen::shared::asm_operand::Width::B8,
                                crate::codegen::shared::asm_operand::Width::B64,
                                dst,
                                dst,
                            ));
                        },
                        16 => {
                            // movswq %ax, %rax (截断到 16 位, 符号扩展)
                            self.emit_insn(&Insn::mov_ext(
                                true,
                                crate::codegen::shared::asm_operand::Width::B16,
                                crate::codegen::shared::asm_operand::Width::B64,
                                dst,
                                dst,
                            ));
                        },
                        _ => {},
                    }
                }
                // 注: 无符号源 → 有符号目标, 目标更宽时, 零扩展值
                // 已正确表示非负有符号值, 无需转换.
            },
            // 其他情况: 不转换 (同类型/指针/聚合等无需转换)
            _ => {},
        }
    }
}
