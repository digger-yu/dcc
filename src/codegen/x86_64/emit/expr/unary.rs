// src/codegen/x86_64/emit/expr/unary.rs
// Expr::Unary arm 的 codegen (gen_expr 中的一元运算分支)。
// 本文件由 mod.rs 逐字搬迁而来 (batch 4b): arm body 原样剪切进 `impl Codegen`
// 新方法 `gen_expr_unary_ops`, 语义 diff 为 0。旧影子 `gen_unary_expr` 自由函数
// 与 `emit_negate_instruction` / `emit_not_instruction` / `gen_deref_operand` /
// `make_nested_label` 影子实现一并作废。

use crate::ast::{Expr, Type, UnOp};
use crate::codegen::x86_64::insn::Insn;
use crate::codegen::x86_64::opcode::{SimdArithOp, SimdWidth};
use crate::codegen::x86_64::{Codegen, JumpKind};

impl Codegen {
    /// gen_expr 中 Expr::Unary arm 的搬迁体, 逐字复制, 未改逻辑。
    pub fn gen_expr_unary_ops(&mut self, op: &UnOp, e: &Expr, dst: u8) {
        // 浮点字面量取负: 编译期折叠, 直接生成相反值的 Float 表达式
        if matches!(op, UnOp::Neg) {
            if let Expr::Float { c, ty, span, .. } = e {
                let negated = Expr::Float {
                    c: c.negate(),
                    ty: ty.clone(),
                    span: span.clone(),
                };
                self.gen_expr(&negated, dst);
                return;
            }
        }
        if matches!(e.ty(), Type::Int128 | Type::UInt128) {
            self.gen_expr(e, Self::EVAL_BASE);
            match op {
                UnOp::Neg => {
                    self.emit_insn(&Insn::unary_rm_reg(
                        crate::codegen::x86_64::opcode::UnaryOp::Neg,
                        crate::codegen::shared::asm_operand::Width::B64,
                        0,
                    )); // neg rax
                        // P11: adc rdx, 0 via AluRm typed (imm=0 fits i8).
                        self.emit_insn(&Insn::new(
                            crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                                op: crate::codegen::x86_64::opcode::AluOp::Adc,
                                width: crate::codegen::shared::asm_operand::Width::B64,
                                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                    2,
                                    crate::codegen::shared::asm_operand::Width::B64,
                                ),
                                src: crate::codegen::x86_64::opcode::Operand::Imm(0),
                            },
                            &[],
                        ));
                    self.emit_insn(&Insn::unary_rm_reg(
                        crate::codegen::x86_64::opcode::UnaryOp::Neg,
                        crate::codegen::shared::asm_operand::Width::B64,
                        2,
                    )); // neg rdx
                },
                UnOp::BitNot => {
                    self.emit_insn(&Insn::unary_rm_reg(
                        crate::codegen::x86_64::opcode::UnaryOp::Not,
                        crate::codegen::shared::asm_operand::Width::B64,
                        0,
                    )); // not rax
                    self.emit_insn(&Insn::unary_rm_reg(
                        crate::codegen::x86_64::opcode::UnaryOp::Not,
                        crate::codegen::shared::asm_operand::Width::B64,
                        2,
                    )); // not rdx
                },
                UnOp::Not => {
                    // P11: or rax, rdx via typed opcode.
                    self.emit_insn(&Insn::new(
                        crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                            op: crate::codegen::x86_64::opcode::AluOp::Or,
                            width: crate::codegen::shared::asm_operand::Width::B64,
                            dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                0,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                            src: crate::codegen::x86_64::opcode::Operand::Reg(
                                2,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                        },
                        &[],
                    ));
                    self.emit_insn(&Insn::test_rm_reg(
                        crate::codegen::shared::asm_operand::Width::B64,
                        0,
                        0,
                    )); // test rax, rax
                    self.emit_insn(&Insn::setcc_reg(
                        crate::codegen::x86_64::opcode::CondCode::Eq,
                        0,
                    )); // sete al
                    // P11: movzbq al, rax via MovExt typed.
                    self.emit_insn(&Insn::mov_ext(
                        false,
                        crate::codegen::shared::asm_operand::Width::B8,
                        crate::codegen::shared::asm_operand::Width::B64,
                        0,
                        0,
                    ));
                    // P11: xor rdx, rdx via typed opcode (high = 0).
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
                },
                UnOp::Plus => {},
            }
            if dst != 0 {
                self.mov(dst, 0);
            }
            return;
        }
        // 浮点取负: 翻转符号位 (XOR with sign mask)
        // 否则在 gen_expr 之后 emit neg r64 会错把 rax 取反, 而浮点结果在 xmm0.
        if matches!(op, UnOp::Neg)
            && (matches!(e.ty(), Type::Double | Type::Float | Type::LongDouble)
                || matches!(e.ty(), Type::Imaginary(_)))
        {
            let e_ty = e.ty();
            let neg_ty = if let Type::Imaginary(ref inner) = e_ty {
                &inner.base
            } else {
                &e_ty
            };
            // LD/imaginary LD 走 x87: gen_expr 后值在 st0, fchs 直接翻
            // 80-bit 符号位 (旧路径 XOR xmm0 bit63 只动 8 字节, LD 根本
            // 不在 xmm0, 结果无效)。
            if matches!(neg_ty, Type::LongDouble) {
                self.gen_expr(e, Self::EVAL_BASE);
                self.emit_x87_chs();
                if dst != 0 {
                    // 标量 LD rvalue: rax 取 gen_expr 已交付的地址
                    self.mov(dst, 0);
                }
                return;
            }
            self.gen_expr(e, Self::EVAL_BASE); // xmm0 = e
            match neg_ty {
                Type::Float => {
                    // 翻转 32 位符号位: movabs rax, 0x8000_0000
                    //   66 48 0f 6e c8 movq xmm1, rax
                    //   0f 57 c1       xorps xmm0, xmm1
                    self.emit_insn(&Insn::new(
                        crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                            rip_relative: false,
                            width: crate::codegen::shared::asm_operand::Width::B64,
                            dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                0,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                            src: crate::codegen::x86_64::opcode::Operand::Imm(0x80000000u64 as i64),
                        },
                        &[],
                    ));
                    self.emit_insn(&Insn::gpr_xmm(
                        true,
                        crate::codegen::shared::asm_operand::Width::B64,
                        0,
                        1,
                    ));
                    self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Xorps, SimdWidth::PS, 0, 1));
                },
                Type::Double => {
                    // 翻转 64 位符号位
                    self.emit_insn(&Insn::new(
                        crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                            rip_relative: false,
                            width: crate::codegen::shared::asm_operand::Width::B64,
                            dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                0,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                            src: crate::codegen::x86_64::opcode::Operand::Imm(
                                0x8000000000000000u64 as i64,
                            ),
                        },
                        &[],
                    ));
                    self.emit_insn(&Insn::gpr_xmm(
                        true,
                        crate::codegen::shared::asm_operand::Width::B64,
                        0,
                        1,
                    ));
                    self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Xorpd, SimdWidth::PD, 0, 1));
                },
                _ => unreachable!(),
            }
            if dst != 0 {
                self.mov(dst, 0);
            }
            return;
        }
        // _Complex 取负: 实部/虚部分别翻转符号位 (结果布局同
        // gen_complex_binop: 实部 xmm0, 分量对在 [r13+result_off])
        if matches!(op, UnOp::Neg) && matches!(e.ty(), Type::Complex(_)) {
            let inner = match &e.ty() {
                Type::Complex(i) => i.base.clone(),
                _ => Type::Double,
            };
            let inner_sz = inner.size() as i64;
            let result_off = self.complex_result_off();
            // complex LD: 两分量各 fchs (值通道是 x87), 物化到帧槽,
            // rax=result_off (LD rvalue 约定; 旧路径 movsd 只读 8 字节)。
            if matches!(inner, Type::LongDouble) {
                // 源只求值一次物化成 tbyte 对，real/imag 各 fchs 后写 result_off。
                let src = self.gen_complex_ld_materialize(e);
                self.emit_x87_fld_at(13, src as i32, &inner);
                self.emit_x87_chs();
                self.emit_x87_fstp_at(13, result_off as i32, &inner);
                self.emit_x87_fld_at(13, src as i32 + 16, &inner);
                self.emit_x87_chs();
                self.emit_x87_fstp_at(13, (result_off + inner_sz) as i32, &inner);
                self.emit_lea_r13(0, result_off);
                if dst != 0 {
                    self.mov(dst, 0);
                }
                return;
            }
            let pfx = if matches!(inner, Type::Float) { 0xF3u8 } else { 0xF2u8 };
            // 1. 实部 → xmm0, 虚部 → [r13+result_off+inner_sz]
            if self.gen_lvalue_addr(e, Self::EVAL_BASE) {
                // lvalue (变量/字段/下标): 用 rcx 保存地址 (flip_sign_xmm
                // 会破坏 rax 但不动 rcx), 逐分量加载/flip/store
                self.emit_insn(&Insn::new(
                    crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                        rip_relative: false,
                        width: crate::codegen::shared::asm_operand::Width::B64,
                        dst: crate::codegen::x86_64::opcode::Operand::Reg(
                            1,
                            crate::codegen::shared::asm_operand::Width::B64,
                        ),
                        src: crate::codegen::x86_64::opcode::Operand::Reg(
                            0,
                            crate::codegen::shared::asm_operand::Width::B64,
                        ),
                    },
                    &[],
                )); // mov rcx, rax
                let imag_off = inner_sz; // 变量内存虚部槽 (连续布局)
                let swidth = if matches!(inner, Type::Float) {
                    SimdWidth::SS
                } else {
                    SimdWidth::SD
                };
                // 实部: [rcx] → xmm0 → flip → [result_off]
                self.emit_insn(&Insn::simd_mov_mem_base(false, swidth, 0, 1, 0)); // movss/movsd xmm0, [rcx]
                self.flip_sign_xmm(0, &inner);
                self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x11], result_off);
                // 虚部: [rcx+off] → xmm0 → flip → [result_off+inner_sz]
                self.emit_insn(&Insn::simd_mov_mem_base(false, swidth, 0, 1, imag_off as i32));
                self.flip_sign_xmm(0, &inner);
                self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x11], result_off + inner_sz);
            } else {
                // rvalue: gen_expr 后 xmm0=实部
                self.gen_expr(e, Self::EVAL_BASE);
                if matches!(e, Expr::Call { .. }) {
                    // SSE 返回: 虚部在 xmm1, 物化到临时区
                    self.emit_insn(&Insn::raw_bytes(&[pfx, 0x41, 0x0F, 0x11]));
                    if (result_off + inner_sz).abs() < 128 {
                        self.emit_insn(&Insn::raw_bytes(&[0x4D, (result_off + inner_sz) as u8]));
                    } else {
                        self.emit_insn(&Insn::raw_bytes(&[0x8D]));
                        self.emit_insn(&Insn::raw_bytes(
                            &((result_off + inner_sz) as i32).to_le_bytes(),
                        ));
                    }
                }
                // 实部翻转符号位并写回临时区
                self.flip_sign_xmm(0, &inner);
                self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x11], result_off);
                // 虚部翻转符号位并写回临时区
                self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x10], result_off + inner_sz);
                self.flip_sign_xmm(0, &inner);
                self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x11], result_off + inner_sz);
            }
            // 重载实部到 xmm0 (complex rvalue 约定: xmm0=实部)
            self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x10], result_off);
            if dst != 0 {
                self.mov(dst, 0);
            }
            return;
        }
        // _Complex 取共轭 (~): 实部不变, 虚部翻转符号。
        // 浮点 inner: flip_sign 翻浮点符号位; 整数 inner: 整数取负
        // (旧实现统一 movsd+flip_sign, 整数 _Complex 得到
        // (0, 原虚部) — complex-6 test_int)。
        if matches!(op, UnOp::BitNot) && matches!(e.ty(), Type::Complex(_)) {
            let inner = match &e.ty() {
                Type::Complex(i) => i.base.clone(),
                _ => Type::Double,
            };
            let inner_sz = inner.size() as i64;
            let result_off = self.complex_result_off();
            // complex LD 共轭: 源只求值一次物化成 tbyte 对, real 直写,
            // imag fchs 后写。
            if matches!(inner, Type::LongDouble) {
                let src = self.gen_complex_ld_materialize(e);
                self.emit_x87_fld_at(13, src as i32, &inner);
                self.emit_x87_fstp_at(13, result_off as i32, &inner);
                self.emit_x87_fld_at(13, src as i32 + 16, &inner);
                self.emit_x87_chs();
                self.emit_x87_fstp_at(13, (result_off + inner_sz) as i32, &inner);
                self.emit_lea_r13(0, result_off);
                if dst != 0 {
                    self.mov(dst, 0);
                }
                return;
            }
            if matches!(inner, Type::Float | Type::Double) {
                let pfx = if matches!(inner, Type::Float) { 0xF3u8 } else { 0xF2u8 };
                if self.gen_lvalue_addr(e, Self::EVAL_BASE) {
                    self.emit_insn(&Insn::new(
                        crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                            rip_relative: false,
                            width: crate::codegen::shared::asm_operand::Width::B64,
                            dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                1,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                            src: crate::codegen::x86_64::opcode::Operand::Reg(
                                0,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                        },
                        &[],
                    )); // mov rcx, rax
                    let imag_off = inner_sz;
                    let swidth = if matches!(inner, Type::Float) {
                        SimdWidth::SS
                    } else {
                        SimdWidth::SD
                    };
                    // 实部: [rcx] → xmm0 → [result_off] (不变)
                    self.emit_insn(&Insn::simd_mov_mem_base(false, swidth, 0, 1, 0)); // movss/movsd xmm0, [rcx]
                    self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x11], result_off);
                    // 虚部: [rcx+off] → xmm0 → flip → [result_off+inner_sz]
                    self.emit_insn(&Insn::simd_mov_mem_base(false, swidth, 0, 1, imag_off as i32));
                    self.flip_sign_xmm(0, &inner);
                    self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x11], result_off + inner_sz);
                } else {
                    self.gen_expr(e, Self::EVAL_BASE);
                    // 实部已经在 xmm0, 直接写回
                    self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x11], result_off);
                    // 虚部: 加载并翻转
                    self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x10], result_off + inner_sz);
                    self.flip_sign_xmm(0, &inner);
                    self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x11], result_off + inner_sz);
                }
                // 重载实部到 xmm0
                self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x10], result_off);
                // rax = result_off (dcc complex rvalue 约定, 供
                // spill_complex_operand 的 rep movsb / gen_return 读取)
                self.emit_lea_r13(0, result_off);
                if dst != 0 {
                    self.mov(dst, 0);
                }
                return;
            }
            // 整数 inner: 共轭 = 虚部整数取负。
            // 1. 源物化到 result_off (保二进制布局)
            if self.gen_lvalue_addr(e, Self::EVAL_BASE) {
                self.mov(6, 0); // rsi = &src
                self.emit_lea_r13(7, result_off); // rdi = r13+result_off
                self.mov_imm(1, (inner_sz * 2) as i128);
                self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Cld));
                self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::RepMovsb));
            // cld; rep movsb
            } else {
                self.gen_expr(e, Self::EVAL_BASE);
                // rvalue 已按统一约定物化到 result_off (rax=地址);
                // 保险起见若 rax 不指向 result_off 则重拷
                // (整数 complex rvalue 均遵守此约定)
            }
            // 2. 虚部取负 (按内型宽度)
            match inner_sz {
                8 => {
                    self.load_stack(0, result_off + 8);
                    self.emit_insn(&Insn::unary_rm_reg(
                        crate::codegen::x86_64::opcode::UnaryOp::Neg,
                        crate::codegen::shared::asm_operand::Width::B64,
                        0,
                    )); // neg rax
                    self.store_stack(0, result_off + 8);
                },
                4 => {
                    self.load_stack(0, result_off + 4);
                    self.emit_insn(&Insn::unary_rm_reg(
                        crate::codegen::x86_64::opcode::UnaryOp::Neg,
                        crate::codegen::shared::asm_operand::Width::B32,
                        0,
                    )); // neg eax
                    self.emit_store(0, result_off + 4, 4);
                },
                2 => {
                    self.load_stack(0, result_off + 2);
                    self.emit_insn(&Insn::unary_rm_reg(
                        crate::codegen::x86_64::opcode::UnaryOp::Neg,
                        crate::codegen::shared::asm_operand::Width::B16,
                        0,
                    )); // neg ax
                    self.emit_store(0, result_off + 2, 2);
                },
                _ => {
                    self.load_stack(0, result_off + 1);
                    self.emit_insn(&Insn::unary_rm_reg(
                        crate::codegen::x86_64::opcode::UnaryOp::Neg,
                        crate::codegen::shared::asm_operand::Width::B8,
                        0,
                    )); // neg al
                    self.emit_store(0, result_off + 1, 1);
                },
            }
            // 3. rax = result_off (统一 rvalue 约定: rax 指向
            // 物化结果, 消费端从内存对读取)
            self.emit_lea_r13(0, result_off);
            if dst != 0 {
                self.mov(dst, 0);
            }
            return;
        }
        // Vector 取负/按位非: 逐元素操作, 结果写入约定槽 0
        // (dcc vector rvalue 约定: 结果在 vector_temp_base(0),
        // 消费端 gen_aggregate_assign 等按此读取)
        if matches!(e.ty(), Type::Vector(_, _)) {
            if let Type::Vector(ref elem_ty, count) = e.ty() {
                self.gen_vector_unary_into_slot(*op, e, &elem_ty.base, count, 0);
                return;
            }
        }
        self.gen_expr(e, dst);
        match op {
            UnOp::Neg => {
                // neg r/m: 根据类型大小选择操作数尺寸。
                // 64 位类型用 REX.W; 32 位有符号 + trapv 用 32 位 neg
                // (检测 32 位溢出); 32 位有符号无 trapv 用 REX.W
                // (寄存器保存符号扩展值, 64 位 neg 保持上 32 位一致);
                // 32 位无符号用 REX (无 .W) 保持零扩展。
                let ty_size = e.ty().size();
                let is_signed = Self::is_signed_int(&e.ty());
                let use_32bit_neg = ty_size == 4 && is_signed && self.trapv;
                if ty_size >= 8 || (ty_size >= 4 && is_signed && !use_32bit_neg) {
                    // 64-bit neg: 对有符号 32+ 位类型 (非 trapv),
                    // 寄存器值已符号扩展到 64 位, 必须用
                    // REX.W neg 保持上 32 位一致
                    // P11: neg dst64 via UnaryRm typed.
                    self.emit_insn(&Insn::unary_rm_reg(
                        crate::codegen::x86_64::opcode::UnaryOp::Neg,
                        crate::codegen::shared::asm_operand::Width::B64,
                        dst,
                    ));
                } else if use_32bit_neg {
                    // 32-bit neg for trapv: 检测 32 位溢出
                    let rex = 0x40 | if dst >= 8 { 0x01 } else { 0 }; // 位序: REX.B: 该寄存器只在 ModRM.r/m 或 opcode 低 3 位, 判据见 docs/ssa-pass-upgrade-plan.md §7.21
                    self.emit_insn(&Insn::raw_bytes(&[rex, 0xf7, 0xd8 | (dst & 7)]));
                } else {
                    // 32 位无符号: REX 前缀无 .W
                    let rex = 0x40 | if dst >= 8 { 0x01 } else { 0 }; // 位序: REX.B: 该寄存器只在 ModRM.r/m 或 opcode 低 3 位, 判据见 docs/ssa-pass-upgrade-plan.md §7.21
                    self.emit_insn(&Insn::raw_bytes(&[rex, 0xf7, 0xd8 | (dst & 7)]));
                }
                // -ftrapv: 有符号取负溢出检查 (e.g. -INT_MIN)
                // neg 在操作数为类型最小值时置 OF=1
                if self.trapv && Self::is_signed_int(&e.ty()) {
                    let trap_label = self.fresh_label();
                    let ok = self.fresh_label();
                    self.jmp_rel32(&trap_label, JumpKind::Jo);
                    self.jmp_rel32(&ok, JumpKind::Jmp);
                    self.label(&trap_label);
                    self.emit_insn(&Insn::raw_bytes(&[0xe8, 0, 0, 0, 0])); // call rel32
                    let call_at = self.code.len() as u64 - 4;
                    self.pending_func_rels
                        .push((call_at, "__dcc_trapv_overflow".to_string()));
                    self.label(&ok);
                    // 32 位 trapv neg 后符号扩展: 32 位 neg 结果在 eax,
                    // 需要 movslq 到 rax 保持 dcc 的 64 位寄存器约定
                    if e.ty().size() == 4 {
                        // P11: movsxd dst, dst via MovExt typed.
                        self.emit_insn(&Insn::mov_ext(
                            true,
                            crate::codegen::shared::asm_operand::Width::B32,
                            crate::codegen::shared::asm_operand::Width::B64,
                            dst,
                            dst,
                        ));
                    }
                }
            },
            UnOp::Not => {
                // 逻辑非: !x = (x == 0) ? 1 : 0
                // P11: test dst, dst via AluRm typed.
                self.emit_insn(&Insn::test_rm_reg(
                    crate::codegen::shared::asm_operand::Width::B64,
                    dst,
                    dst,
                ));
                // sete al: 零 → 1, 非零 → 0 (0F 94 C0)
                self.emit_insn(&Insn::setcc_reg(crate::codegen::x86_64::opcode::CondCode::Eq, 0));
                // P11: movzbq al, dst via MovExt typed (from B8, to B64).
                // ModRM: mod=11, reg=dst, r/m=0(al) → 0xC0 | ((dst&7)<<3)
                // REX.R (0x04) when dst>=8 (reg field extension)
                self.emit_insn(&Insn::mov_ext(
                    false,
                    crate::codegen::shared::asm_operand::Width::B8,
                    crate::codegen::shared::asm_operand::Width::B64,
                    dst,
                    0,
                ));
            },
            UnOp::BitNot => {
                // P11: not dst64 via UnaryRm typed.
                self.emit_insn(&Insn::unary_rm_reg(
                    crate::codegen::x86_64::opcode::UnaryOp::Not,
                    crate::codegen::shared::asm_operand::Width::B64,
                    dst,
                ));
                // not flips all 64 bits. The result must be
                // truncated to the expression type width, then
                // re-extended. For types < int, C integer
                // promotion means the NOT result is at least
                // 32 bits wide.
                let result_ty = e.ty();
                let result_sz = result_ty.size();
                let effective_sz = result_sz.max(4); // int promotion
                if effective_sz <= 4 {
                    // Result fits in 32 bits: truncate to 32
                    // bits then zero/sign-extend to 64.
                    let is_signed =
                        Self::is_signed_int(&result_ty) || matches!(result_ty, Type::Char);
                    if is_signed {
                        // P11: movsxd dst, dst (sign-extend 32→64) via MovExt typed.
                        // REX.W=0x48 (dst<8) / 0x4D (dst>=8, REX.R+REX.B 双置位).
                        self.emit_insn(&Insn::mov_ext(
                            true,
                            crate::codegen::shared::asm_operand::Width::B32,
                            crate::codegen::shared::asm_operand::Width::B64,
                            dst,
                            dst,
                        ));
                    } else {
                        // P11: mov dst32, dst32 (zero-extend 32→64) via MovRm typed.
                        // 自反 ModRM: dst 同时在 ModRM.reg 与 r/m ⇒ >=8 要同时置 REX.R 与 REX.B(0x05)；
                        // <8 时一个前缀都不发.
                        self.emit_insn(&Insn::mov_rm_reg(
                            crate::codegen::shared::asm_operand::Width::B32,
                            dst,
                            dst,
                        ));
                    }
                } else if result_sz < 8 {
                    // 8-byte types: no fixup needed (not already
                    // produces correct 64-bit result)
                    unreachable!()
                }
            },
            UnOp::Plus => {},
        }
    }
}
