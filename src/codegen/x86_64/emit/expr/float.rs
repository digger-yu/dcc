//! emit/expr/float.rs —— 自历史完整实现（git 28a9063 单文件 Codegen）按职责簇恢复的模块。
//! 仅移动/可见性调整，不改语义（backend_refactor_plan §3.1 原则 1）。
#![allow(clippy::all)]
#![allow(unused_imports)]
use crate::ast::*;
use crate::codegen::shared::elf as shared_elf;
use crate::codegen::shared::elf::*;
use crate::codegen::shared::*;
use crate::codegen::x86_64::asm::*;
use crate::codegen::x86_64::insn::Insn;
use crate::codegen::x86_64::opcode::{SimdArithOp, SimdWidth};
use crate::codegen::x86_64::Codegen;
use crate::codegen::x86_64::*;
use crate::const_eval::{self, ConstValue};
use crate::debug::*;
use crate::diagnostics::{Diagnostic, Severity, Span};
use crate::ir::{self, *};
use crate::semantics::{self, *};
use crate::target::Target;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;
impl Codegen {
    pub(crate) fn emit_float_convert(&mut self, from: &Type, to: &Type) {
        if matches!(from, Type::Float) && !matches!(to, Type::Float) {
            self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::CvtSdSs, SimdWidth::SS, 0, 0)); // cvtss2sd xmm0, xmm0
        } else if !matches!(from, Type::Float) && matches!(to, Type::Float) {
            self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::CvtSdSs, SimdWidth::SD, 0, 0)); // cvtsd2ss xmm0, xmm0
        }
    }
    pub(crate) fn emit_float_neg(&mut self, ty: &Type) {
        match ty {
            Type::Float => {
                // movabs rax, 0x80000000; movq xmm1, rax; xorps xmm0, xmm1
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64),
                    src: crate::codegen::x86_64::opcode::Operand::Imm(0x80000000u64 as i64),
                }, &[]));
                self.emit_insn(&Insn::raw_bytes(&[0x66, 0x48, 0x0F, 0x6E, 0xC8]));
                self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Xorps, SimdWidth::PS, 0, 1));
            },
            _ => {
                // movabs rax, 0x8000000000000000; movq xmm1, rax; xorpd xmm0, xmm1
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64),
                    src: crate::codegen::x86_64::opcode::Operand::Imm(0x8000000000000000u64 as i64),
                }, &[]));
                self.emit_insn(&Insn::raw_bytes(&[0x66, 0x48, 0x0F, 0x6E, 0xC8]));
                self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Xorpd, SimdWidth::PD, 0, 1));
            },
        }
    }
    pub(crate) fn emit_float_precision_convert(&mut self, src_ty: &Type, dst_ty: &Type) {
        // LongDouble is internally stored/computed as double in xmm0,
        // so treat LongDouble same as Double for conversion purposes.
        let src_is_single = matches!(src_ty, Type::Float);
        let dst_is_single = matches!(dst_ty, Type::Float);
        if src_is_single == dst_is_single {
            // Both single or both double/long-double: no conversion
            return;
        }
        if src_is_single && !dst_is_single {
            // float → double: cvtss2sd xmm0, xmm0
            self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::CvtSdSs, SimdWidth::SS, 0, 0));
        } else {
            // double/long-double → float: cvtsd2ss xmm0, xmm0
            self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::CvtSdSs, SimdWidth::SD, 0, 0));
        }
    }
    pub(crate) fn emit_float_to_ulong(&mut self, dst: u8, is_double: bool) {
        let l_done = self.fresh_label();
        let scratch = if dst == 0 { 2 } else { 0 }; // rdx or rax

        // cvttss2si/cvttsd2si dst, xmm0 (REX.W 64 位结果)
        if is_double {
            self.emit_insn(&Insn::cvt2sd_reg(false, SimdWidth::SD, dst, 0));
        } else {
            self.emit_insn(&Insn::cvt2sd_reg(false, SimdWidth::SS, dst, 0));
        }
        // test dst, dst
        let test_rex = 0x48 | if dst >= 8 { 0x05 } else { 0 };   // 位序: REX.R+B: 自反 ModRM, 同一个寄存器既在 reg 又在 r/m, 判据见 docs/ssa-pass-upgrade-plan.md §7.21
        use crate::codegen::x86_64::emit::gas::REG64 as GAS_REG64;
        let test_gas = format!("test {}, {}", GAS_REG64[(dst & 15) as usize], GAS_REG64[(dst & 15) as usize]);
        self.emit_insn(&Insn::raw_with_gas(&[test_rex, 0x85, 0xC0 | ((dst & 7) << 3) | (dst & 7)], &test_gas));
        // jns .done (非负 → 正确)
        self.jmp_rel32(&l_done, JumpKind::Jns);

        // --- 溢出路径: 原值 > INT64_MAX ---
        // 加载 2^63 浮点常量到 xmm1.
        // 注意: movq 将 GPR 64 位内容解释为 double 位模式,
        // 所以不能直接加载 float 32 位模式 (会被误解释为极小 double).
        // 统一使用 double 精度: 加载 2^63 as double,
        // float 情况下先将 xmm0 提升为 double 再减.
        // 2^63 as double = 0x43E0000000000000
        self.mov_imm(scratch, 0x43E0000000000000i128);
        // movq xmm1, scratch
        self.emit_movq_gp_to_xmm(1, scratch);
        if !is_double {
            // float 情况: 先将 xmm0 从 float 提升为 double
            // cvtss2sd xmm0, xmm0: F3 0F 5A C0
            self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::CvtSdSs, SimdWidth::SS, 0, 0));
        }
        // subsd xmm0, xmm1 (double 精度减 2^63)
        self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Sub, SimdWidth::SD, 0, 1));
        if !is_double {
            // float 情况: 减完后转回 float
            // cvtsd2ss xmm0, xmm0: F2 0F 5A C0
            self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::CvtSdSs, SimdWidth::SD, 0, 0));
        }
        // cvttss2si/cvttsd2si dst, xmm0 (余数, 现在 fits in int64)
        if is_double {
            self.emit_insn(&Insn::cvt2sd_reg(false, SimdWidth::SD, dst, 0));
        } else {
            self.emit_insn(&Insn::cvt2sd_reg(false, SimdWidth::SS, dst, 0));
        }
        // 加 2^63 整数校正
        self.mov_imm(scratch, 0x8000000000000000u64 as i128);
        // add dst, scratch: 01 /r (dst = r/m, scratch = reg)
        // P11: 收敛到 AluRm{Add, B64, dst, scratch}.
        {
            let insn = Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                    op: crate::codegen::x86_64::opcode::AluOp::Add,
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        dst,
                        crate::codegen::shared::asm_operand::Width::B64,
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        scratch,
                        crate::codegen::shared::asm_operand::Width::B64,
                    ),
                },
                &[],
            );
            if !self.emit_insn(&insn) {
                let add_rex = 0x48 | if scratch >= 8 { 0x04 } else { 0 } | if dst >= 8 { 0x01 } else { 0 };
                self.emit_insn(&Insn::raw_bytes(&[add_rex, 0x01, 0xC0 | ((scratch & 7) << 3) | (dst & 7)]));
            }
        }

        self.label(&l_done);
    }
    pub(crate) fn emit_scalar_float_convert(&mut self, src_ty: &Type, dst_ty: &Type) {
        let dst_is_float = matches!(dst_ty, Type::Float | Type::Double);
        if !dst_is_float {
            return;
        }
        let src_is_double = matches!(src_ty, Type::Double | Type::LongDouble);
        let src_is_float = matches!(src_ty, Type::Float);
        let src_is_int = Self::is_integer_type(src_ty);
        if src_is_double && matches!(dst_ty, Type::Float) {
            // cvtsd2ss xmm0, xmm0: F2 0F 5A C0
            self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::CvtSdSs, SimdWidth::SD, 0, 0));
        } else if src_is_float && matches!(dst_ty, Type::Double) {
            // cvtss2sd xmm0, xmm0: F3 0F 5A C0
            self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::CvtSdSs, SimdWidth::SS, 0, 0));
        } else if src_is_int {
            // rax = integer value, convert to float/double in xmm0
            // cvtsi2ss/cvtsi2sd xmm0, rax
            if matches!(dst_ty, Type::Float) {
                // cvtsi2ss xmm0, rax: F3 48 0F 2A C0
                self.emit_insn(&Insn::raw_with_gas(&[0xF3, 0x48, 0x0F, 0x2A, 0xC0], "cvtsi2ss %rax, %xmm0"));
            } else {
                // cvtsi2sd xmm0, rax: F2 48 0F 2A C0
                self.emit_insn(&Insn::raw_with_gas(&[0xF2, 0x48, 0x0F, 0x2A, 0xC0], "cvtsi2sd %rax, %xmm0"));
            }
        }
    }
    pub(crate) fn emit_truncate_to_unsigned(&mut self, dst: u8, target_ty: &Type) {
        let (mask, is_32bit) = match target_ty {
            Type::Bool | Type::UChar => (0xFFu32, false),
            Type::UShort => (0xFFFFu32, false),
            Type::UInt => (0, true),
            _ => return,
        };
        if is_32bit {
            // 自反 ModRM: dst 同时在 ModRM.reg 与 r/m ⇒ >=8 必须同时置 REX.R 与 REX.B(0x05)，
            // 只置一边就把另一个字段留在低 8 个寄存器上（读/写的都是别人的 %eax）。
            // <8 时一个前缀都不发：裸 0x40 什么都不扩，只多占一字节并被 objdump 标成 `rex mov`。
            if dst >= 8 {
                self.emit_insn(&Insn::raw_bytes(&[0x45]));
            }
            self.emit_insn(&Insn::raw_bytes(&[0x89, 0xC0 | ((dst & 7) << 3) | (dst & 7)]));
        } else {
            let rex = 0x48 | if dst >= 8 { 0x01 } else { 0 };   // 位序: REX.B: 该寄存器只在 ModRM.r/m 或 opcode 低 3 位, 判据见 docs/ssa-pass-upgrade-plan.md §7.21
            self.emit_insn(&Insn::raw_bytes(&[rex, 0x81, 0xE0 | (dst & 7)]));
            self.emit_insn(&Insn::raw_bytes(&mask.to_le_bytes()));
        }
    }
    pub(crate) fn emit_truth_flags(&mut self, ty: &Type) {
        if let Type::Complex(inner) = ty {
            let isz = inner.base.size();
            let total = isz * 2;
            match &inner.base {
                Type::Float => {
                    // 32 位浮点分量位模式: 清符号位后按位 or (±0.0 → 0)。
                    // 不能用 eax 作目的地 — 32 位 load 零扩展会破坏 rax
                    // 中的槽地址, 后续 [rax+isz] 变成对实部位模式解引用。
                    self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        1, crate::codegen::shared::asm_operand::Width::B32
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(0),
                            index: None,
                            seg: None,
                        }
                    ),
                },
                &[],
            )); // mov ecx, [rax] (实部)
                    self.emit_insn(&Insn::raw_bytes(&[0x8B, 0x50, isz as u8])); // mov edx, [rax+isz]
                    self.emit_insn(&Insn::new(
                        crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                            op: crate::codegen::x86_64::opcode::AluOp::And,
                            width: crate::codegen::shared::asm_operand::Width::B32,
                            dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                1, crate::codegen::shared::asm_operand::Width::B32
                            ),
                            src: crate::codegen::x86_64::opcode::Operand::Imm(0x7FFF_FFFF),
                        },
                        &[],
                    ));
                    self.emit_insn(&Insn::new(
                        crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                            op: crate::codegen::x86_64::opcode::AluOp::And,
                            width: crate::codegen::shared::asm_operand::Width::B32,
                            dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                2, crate::codegen::shared::asm_operand::Width::B32
                            ),
                            src: crate::codegen::x86_64::opcode::Operand::Imm(0x7FFF_FFFF),
                        },
                        &[],
                    ));
                    // P11: 收敛到 AluRm{Or, B32, rcx, rdx}.
                    {
                        let insn = Insn::new(
                            crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                                op: crate::codegen::x86_64::opcode::AluOp::Or,
                                width: crate::codegen::shared::asm_operand::Width::B32,
                                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                    1,
                                    crate::codegen::shared::asm_operand::Width::B32,
                                ),
                                src: crate::codegen::x86_64::opcode::Operand::Reg(
                                    2,
                                    crate::codegen::shared::asm_operand::Width::B32,
                                ),
                            },
                            &[],
                        );
                        if !self.emit_insn(&insn) {
                            self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::Or, crate::codegen::shared::asm_operand::Width::B32, 1, 2)); // or ecx, edx
                        }
                    }
                    self.emit_insn(&Insn::test_rm_reg(crate::codegen::shared::asm_operand::Width::B32, 1, 1)); // test ecx, ecx
                },
                Type::Double | Type::LongDouble => {
                    self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        1, crate::codegen::shared::asm_operand::Width::B64
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(0),
                            index: None,
                            seg: None,
                        }
                    ),
                },
                &[],
            )); // mov rcx, [rax]
                    self.emit_insn(&Insn::raw_bytes(&[0x48, 0x8B, 0x50, isz as u8])); // mov rdx, [rax+isz]
                    self.mov_imm(0, 0x7FFF_FFFF_FFFF_FFFFi128); // rax = 符号掩码
                    // P11: 收敛到 AluRm{And, B64, rcx, rax} / {And, B64, rdx, rax} / {Or, B64, rcx, rdx}.
                    {
                        let mk = |d: u8, s: u8, op: crate::codegen::x86_64::opcode::AluOp| {
                            Insn::new(
                                crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                                    op,
                                    width: crate::codegen::shared::asm_operand::Width::B64,
                                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                        d,
                                        crate::codegen::shared::asm_operand::Width::B64,
                                    ),
                                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                                        s,
                                        crate::codegen::shared::asm_operand::Width::B64,
                                    ),
                                },
                                &[],
                            )
                        };
                        if !self.emit_insn(&mk(1, 0, crate::codegen::x86_64::opcode::AluOp::And)) {
                            self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::And, crate::codegen::shared::asm_operand::Width::B64, 1, 0));
                        }
                        if !self.emit_insn(&mk(2, 0, crate::codegen::x86_64::opcode::AluOp::And)) {
                            self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::And, crate::codegen::shared::asm_operand::Width::B64, 2, 0));
                        }
                        if !self.emit_insn(&mk(1, 2, crate::codegen::x86_64::opcode::AluOp::Or)) {
                            self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::Or, crate::codegen::shared::asm_operand::Width::B64, 1, 2));
                        }
                    }
                    self.emit_insn(&Insn::test_rm_reg(crate::codegen::shared::asm_operand::Width::B64, 1, 1)); // test rcx, rcx
                },
                _ => {
                    // 整数分量: 任一分量非零即真。result_off 8 字节
                    // 对齐, ≤8 字节复数可一次整槽载入覆盖两分量。
                    match total {
                        16 => {
                            self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        1, crate::codegen::shared::asm_operand::Width::B64
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(0),
                            index: None,
                            seg: None,
                        }
                    ),
                },
                &[],
            )); // mov rcx, [rax]
                            self.emit_insn(&Insn::raw_bytes(&[0x48, 0x0B, 0x48, isz as u8])); // or rcx, [rax+isz]
                            self.emit_insn(&Insn::test_rm_reg(crate::codegen::shared::asm_operand::Width::B64, 1, 1)); // test rcx, rcx
                        },
                        8 => {
                            self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B64
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0, base: Some(0), index: None, seg: None,
                        }
                    ),
                },
                &[],
            )); // mov rax, [rax]
                            self.emit_insn(&Insn::test_rm_reg(crate::codegen::shared::asm_operand::Width::B64, 0, 0));
                        },
                        4 => {
                            self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0, base: Some(0), index: None, seg: None,
                        }
                    ),
                },
                &[],
            )); // mov eax, [rax]
                            self.emit_insn(&Insn::test_rm_reg(crate::codegen::shared::asm_operand::Width::B32, 0, 0)); // test eax, eax
                        },
                        _ => {
                            // total==2 (_Complex char 等)
                            self.emit_insn(&Insn::raw_bytes(&[0x0F, 0xB7, 0x00])); // movzx eax, word [rax]
                            self.emit_insn(&Insn::test_rm_reg(crate::codegen::shared::asm_operand::Width::B32, 0, 0)); // test eax, eax
                        },
                    }
                },
            }
            return;
        }
        // 根据条件类型宽度选择 test 指令: 64 位类型必须用
        // test %rax,%rax (48 85 C0), 否则高 32 位非零值会被
        // test %eax,%eax (85 C0) 误判为零.
        if ty.size() > 4 {
            self.emit_insn(&Insn::test_rm_reg(crate::codegen::shared::asm_operand::Width::B64, 0, 0)); // test rax, rax
        } else {
            self.emit_insn(&Insn::test_rm_reg(crate::codegen::shared::asm_operand::Width::B32, 0, 0)); // test eax, eax
        }
    }
    pub(crate) fn emit_ulong_to_float(&mut self, dst: u8, is_double: bool) {
        let l_pos = self.fresh_label();
        let l_done = self.fresh_label();

        // test dst, dst
        let test_rex = 0x48 | if dst >= 8 { 0x05 } else { 0 };   // 位序: REX.R+B: 自反 ModRM, 同一个寄存器既在 reg 又在 r/m, 判据见 docs/ssa-pass-upgrade-plan.md §7.21
        use crate::codegen::x86_64::emit::gas::REG64 as GAS_REG64;
        let test_gas = format!("test {}, {}", GAS_REG64[(dst & 15) as usize], GAS_REG64[(dst & 15) as usize]);
        self.emit_insn(&Insn::raw_with_gas(&[test_rex, 0x85, 0xC0 | ((dst & 7) << 3) | (dst & 7)], &test_gas));
        // jns .pos (bit63=0 → 正数, 直接转换)
        self.jmp_rel32(&l_pos, JumpKind::Jns);

        // --- bit63=1: 值 >= 2^63 ---
        // shr dst, 1: 无符号除以 2 (清除 bit63)
        self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Shift {
            op: crate::codegen::x86_64::opcode::ShiftOp::Shr,
            width: crate::codegen::shared::asm_operand::Width::B64,
            dst: crate::codegen::x86_64::opcode::Operand::Reg(dst, crate::codegen::shared::asm_operand::Width::B64),
            amount: crate::codegen::x86_64::opcode::ShiftAmount::Imm8(1),
        }, &[]));
        // cvtsi2ss/sd xmm0, dst (REX.W 64 位操作数)
        if is_double {
            self.emit_insn(&Insn::cvt2sd_reg(true, SimdWidth::SD, 0, dst));
            // addsd xmm0, xmm0: 乘 2.0
            self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Add, SimdWidth::SD, 0, 0));
        } else {
            self.emit_insn(&Insn::cvt2sd_reg(true, SimdWidth::SS, 0, dst));
            // addss xmm0, xmm0: 乘 2.0
            self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Add, SimdWidth::SS, 0, 0));
        }
        self.jmp_rel32(&l_done, JumpKind::Jmp);

        // --- .pos: 正数路径 ---
        self.label(&l_pos);
        if is_double {
            self.emit_insn(&Insn::cvt2sd_reg(true, SimdWidth::SD, 0, dst));
        } else {
            self.emit_insn(&Insn::cvt2sd_reg(true, SimdWidth::SS, 0, dst));
        }

        self.label(&l_done);
    }
    pub(crate) fn emit_xmm_rax_disp(&mut self, pfx: u8, xmm: u8, off: i64) {
        let rex = if xmm >= 8 { Some(0x44) } else { None };
        if let Some(rex) = rex {
            self.emit_insn(&Insn::raw_bytes(&[pfx, rex, 0x0F, 0x10]));
        } else {
            self.emit_insn(&Insn::raw_bytes(&[pfx, 0x0F, 0x10]));
        }
        let reg = (xmm & 7) << 3;
        if off == 0 {
            self.emit_insn(&Insn::raw_bytes(&[reg]));
        } else if (-128..=127).contains(&off) {
            self.emit_insn(&Insn::raw_bytes(&[0x40 | reg, off as i8 as u8]));
        } else {
            self.emit_insn(&Insn::raw_bytes(&[0x80 | reg]));
            self.emit_insn(&Insn::raw_bytes(&(off as i32).to_le_bytes()));
        }
    }
    pub(crate) fn emit_zero_extend(&mut self, dst: u8, src_ty: &Type) {
        let (mask, is_32bit) = match src_ty {
            Type::Bool | Type::UChar => (0xFFu32, false),
            Type::UShort => (0xFFFFu32, false),
            Type::UInt => (0, true), // 用 32 位 mov
            _ => return,
        };
        if is_32bit {
            // mov dst32, dst32: 89 /r (32 位操作数自动零扩展高 32 位)
            // 自反 ModRM: dst 同时在 ModRM.reg 与 r/m ⇒ >=8 必须同时置 REX.R 与 REX.B(0x05)，
            // 只置一边就把另一个字段留在低 8 个寄存器上（读/写的都是别人的 %eax）。
            // <8 时一个前缀都不发：裸 0x40 什么都不扩，只多占一字节并被 objdump 标成 `rex mov`。
            if dst >= 8 {
                self.emit_insn(&Insn::raw_bytes(&[0x45]));
            }
            self.emit_insn(&Insn::raw_bytes(&[0x89, 0xC0 | ((dst & 7) << 3) | (dst & 7)]));
        } else {
            // and dst, imm32: REX.W 81 /4 id
            let rex = 0x48 | if dst >= 8 { 0x01 } else { 0 };   // 位序: REX.B: 该寄存器只在 ModRM.r/m 或 opcode 低 3 位, 判据见 docs/ssa-pass-upgrade-plan.md §7.21
            self.emit_insn(&Insn::raw_bytes(&[rex, 0x81, 0xE0 | (dst & 7)]));
            self.emit_insn(&Insn::raw_bytes(&mask.to_le_bytes()));
        }
    }
    pub(crate) fn gen_float_binop(&mut self, op: BinOp, l: &Expr, r: &Expr) {
        // Determine the target float type per C usual arithmetic
        // conversions: the wider float type wins.  For mixed int/float
        // expressions like `a - step`, the float operand's type is used.
        let float_ty = Self::widest_float_type(&l.ty(), &r.ty());
        let is_float = matches!(float_ty, Type::Float);
        let is_long_double = matches!(float_ty, Type::LongDouble);
        // x87 通路唯一入口：`long double` 恒走 st(0)，`-mno-sse` 目标下
        // float/double 也走（判据集中在 `float_in_st0`，不在这里叠加条件）。
        if self.float_in_st0(&float_ty) {
            // 比较运算走特殊路径
            if matches!(op, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::EqEq | BinOp::Ne)
            {
                self.gen_float_cmp_x87(op, &float_ty, l, r);
                return;
            }
            // 取模: 软件实现, 需要 SSE 指令, 在 x87 通路下通过调用 fmod/fmodf 实现
            if matches!(op, BinOp::Mod) {
                // 回退到通用调用路径, 通过外部函数实现
                // 将左操作数存入临时槽, 计算右操作数, 调用 __dcc_x87_fmod
                self.gen_expr(l, Self::EVAL_BASE); // st(0) = l
                self.emit_operand_to_st0(&l.ty(), 0);
                // 保存 st(0) 到栈临时区 (固定 16 字节: ld 要装 tbyte 且保持对齐)
                self.emit_insn(&Insn::sub_rsp_imm(16)); // sub rsp, 16
                if is_long_double {
                    self.emit_insn(&Insn::raw_bytes(&[0xDB, 0x3C, 0x24])); // fstp tbyte [rsp]
                } else {
                    self.emit_insn(&Insn::raw_bytes(&[0xDD, 0x1C, 0x24])); // fstp qword [rsp]
                }
                self.expr_depth += 1;
                self.gen_expr(r, Self::EVAL_BASE); // st(0) = r
                self.emit_operand_to_st0(&r.ty(), 0);
                self.expr_depth -= 1;
                // 现在 st(0) = r, [rsp] = l
                // 交换: 需要 st(0) = l, st(1) = r 才能做 fdiv
                // 先加载 l 到 st(0)
                if is_long_double {
                    self.emit_insn(&Insn::raw_bytes(&[0xDB, 0x2C, 0x24])); // fld tbyte [rsp]
                } else {
                    self.emit_insn(&Insn::raw_bytes(&[0xDD, 0x04, 0x24])); // fld qword [rsp]
                }
                // 现在 st(0) = l, st(1) = r
                // 做 l/r
                self.emit_insn(&Insn::raw_bytes(&[0xDE, 0xF9])); // fdivrp: st(1) = st(0)/st(1) = l/r, pop
                                          // 现在 st(0) = l/r
                                          // trunc to int64
                self.emit_insn(&Insn::raw_bytes(&[0xDF, 0x7C, 0x24, 0x00])); // fistp qword [rsp] (truncate to int64)
                                                      // 重新加载 r
                self.emit_insn(&Insn::raw_bytes(&[0xDD, 0x04, 0x24])); // fld qword [rsp] -- 但已被 fistp 覆盖
                                                // 简化处理: 直接用 x87 做减法模拟 fmod
                                                // 实际上内核很少用浮点取模, 这里先简单处理
                                                // （C 的 `%` 本身不允许浮点操作数）
                self.emit_insn(&Insn::add_rsp_imm(16)); // add rsp, 16
                self.emit_x87_fldz(); // 结果置 0 (简化处理)
                return;
            }
            // 一般二元运算: 加载左到 st(0), 保存, 加载右到 st(0), 加载左到 st(0)
            // 最终: st(0) = 左, st(1) = 右
            self.gen_expr(l, Self::EVAL_BASE); // st(0) = l
                                 // 整数 / 宽浮点操作数经 Layer0 唯一入口搬到 st(0)
            self.emit_operand_to_st0(&l.ty(), 0);
            // 保存 l 到刚分配的 16 字节槽 (long double 用 tbyte, 否则 qword)
            self.emit_insn(&Insn::sub_rsp_imm(16)); // sub rsp, 16
            if is_long_double {
                self.emit_insn(&Insn::raw_bytes(&[0xDB, 0x3C, 0x24])); // fstp tbyte [rsp]
            } else {
                self.emit_insn(&Insn::raw_bytes(&[0xDD, 0x1C, 0x24])); // fstp qword [rsp]
            }
            self.expr_depth += 1;
            self.gen_expr(r, Self::EVAL_BASE); // st(0) = r
            self.emit_operand_to_st0(&r.ty(), 0);
            self.expr_depth -= 1;
            // 重新加载 l
            if is_long_double {
                self.emit_insn(&Insn::raw_bytes(&[0xDB, 0x2C, 0x24])); // fld tbyte [rsp]
            } else {
                self.emit_insn(&Insn::raw_bytes(&[0xDD, 0x04, 0x24])); // fld qword [rsp]
            }
            self.emit_insn(&Insn::add_rsp_imm(16)); // add rsp, 16
                                                // 执行运算: st(0) = 左, st(1) = 右
                                                // x87 的 fxxxp 是 st(1) op st(0), 然后 pop
                                                // 当前: st(0) = l, st(1) = r
                                                // 对于加法/乘法: 可交换, 直接做
                                                // 对于减法/除法: 需要用反向版本 fsubrp/fdivrp
            match op {
                BinOp::Add => { self.emit_insn(&Insn::raw_bytes(&[0xDE, 0xC1])); } // faddp: st(1) = st(1) + st(0), pop
                BinOp::Mul => { self.emit_insn(&Insn::raw_bytes(&[0xDE, 0xC9])); } // fmulp: st(1) = st(1) * st(0), pop
                BinOp::Sub => { self.emit_insn(&Insn::raw_bytes(&[0xDE, 0xE1])); } // fsubrp: st(1) = st(0) - st(1) = l - r, pop
                BinOp::Div => { self.emit_insn(&Insn::raw_bytes(&[0xDE, 0xF1])); } // fdivrp: st(1) = st(0) / st(1) = l / r, pop
                _ => {},
            }
            // 结果在 st(0) (x87 约定)
            return;
        }
        // 取模: 软件实现 fmod = l - trunc(l/r) * r
        // x86 无原生 float 模指令, dcc 无内置链接器 (非 -c 模式), 不能直接调
        // fmodf/fmod, 用 cvttsX2si 截断 + cvtsi2sX 回转 + sub/mul 实现.
        // 边界 (NaN/Inf/0) 由 cvttsX2si 返回 0x80000000, 测试用例覆盖不到.
        if matches!(op, BinOp::Mod) {
            // 复用上方 l→[rsp], 算 r, r→xmm1, l→xmm0 的代码段
            // 直接走通用路径, 这样节省重复 emit
            let save_bytes = (self.expr_depth as i32 + 1) * 8;
            self.gen_expr(l, Self::EVAL_BASE);
            self.emit_scalar_to_xmm0_if_needed(l, &float_ty);
            self.emit_insn(&Insn::sub_rsp_imm(save_bytes as i64));
            if is_float {
                self.emit_insn(&Insn::raw_bytes(&[0xF3, 0x0F, 0x11, 0x04, 0x24])); // movss [rsp], xmm0
            } else {
                self.emit_insn(&Insn::raw_bytes(&[0xF2, 0x0F, 0x11, 0x04, 0x24])); // movsd [rsp], xmm0
            }
            self.expr_depth += 1;
            self.gen_expr(r, Self::EVAL_BASE);
            self.expr_depth -= 1;
            self.emit_scalar_to_xmm0_if_needed(r, &float_ty);
            self.emit_insn(&Insn::raw_bytes(&[0x66, 0x0F, 0x28, 0xC8])); // movapd xmm1, xmm0 (r→xmm1)
            if is_float {
                self.emit_insn(&Insn::raw_bytes(&[0xF3, 0x0F, 0x10, 0x04, 0x24])); // movss xmm0, [rsp]
            } else {
                self.emit_insn(&Insn::raw_bytes(&[0xF2, 0x0F, 0x10, 0x04, 0x24])); // movsd xmm0, [rsp]
            }
            // 现在 xmm0=l, xmm1=r, [rsp]=l, save_bytes 字节尚未归还
            // r = l / r  (divss/divsd)
            if is_float {
                self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Div, SimdWidth::SS, 0, 1)); // divss xmm0, xmm1
            } else {
                self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Div, SimdWidth::SD, 0, 1)); // divsd xmm0, xmm1
            }
            // trunc 截断到 int64
            if is_float {
                self.emit_insn(&Insn::raw_with_gas(&[0xF3, 0x48, 0x0F, 0x2C, 0xC0], "cvttss2si %xmm0, %rax"));
            } else {
                self.emit_insn(&Insn::raw_with_gas(&[0xF2, 0x48, 0x0F, 0x2C, 0xC0], "cvttsd2si %xmm0, %rax"));
            }
            // int → float
            if is_float {
                self.emit_insn(&Insn::raw_with_gas(&[0xF3, 0x48, 0x0F, 0x2A, 0xC0], "cvtsi2ss %rax, %xmm0"));
            } else {
                self.emit_insn(&Insn::raw_with_gas(&[0xF2, 0x48, 0x0F, 0x2A, 0xC0], "cvtsi2sd %rax, %xmm0"));
            }
            // n * r
            if is_float {
                self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Mul, SimdWidth::SS, 0, 1)); // mulss xmm0, xmm1
            } else {
                self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Mul, SimdWidth::SD, 0, 1)); // mulsd xmm0, xmm1
            }
            // n*r 暂存 xmm1, l 回到 xmm0
            self.emit_insn(&Insn::raw_bytes(&[0x66, 0x0F, 0x28, 0xC8])); // movapd xmm1, xmm0
            if is_float {
                self.emit_insn(&Insn::raw_bytes(&[0xF3, 0x0F, 0x10, 0x04, 0x24])); // movss xmm0, [rsp]
            } else {
                self.emit_insn(&Insn::raw_bytes(&[0xF2, 0x0F, 0x10, 0x04, 0x24])); // movsd xmm0, [rsp]
            }
            // xmm0 = l - n*r
            if is_float {
                self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Sub, SimdWidth::SS, 0, 1)); // subss xmm0, xmm1
            } else {
                self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Sub, SimdWidth::SD, 0, 1)); // subsd xmm0, xmm1
            }
            self.emit_insn(&Insn::raw_with_gas(&[0x48, 0x83, 0xC4, save_bytes as u8], &format!("add ${save_bytes}, %rsp")));
            return;
        }
        // 保存左操作数到 rsp 下方 (红区)
        // 用 sub rsp, 8 分配空间, 避免 movq [rsp-8] 编码问题
        // 嵌套时每个深度用不同的偏移: [rsp - 8*(depth+1)]
        let save_bytes = (self.expr_depth as i32 + 1) * 8;
        // 计算左操作数到 xmm0
        self.gen_expr(l, Self::EVAL_BASE);
        // If LHS is integer, the result sits in eax; convert to
        // xmm0 before saving (e.g. `1.0 - x` where x is int).
        self.emit_scalar_to_xmm0_if_needed(l, &float_ty);
        // 保存 xmm0: sub rsp, save_bytes; movsd [rsp], xmm0
        self.emit_insn(&Insn::raw_with_gas(&[0x48, 0x83, 0xEC, save_bytes as u8], &format!("sub ${save_bytes}, %rsp")));
        if is_float {
            self.emit_insn(&Insn::raw_with_gas(&[0xF3, 0x0F, 0x11, 0x04, 0x24], "movss %xmm0, (%rsp)"));
        } else {
            self.emit_insn(&Insn::raw_with_gas(&[0xF2, 0x0F, 0x11, 0x04, 0x24], "movsd %xmm0, (%rsp)"));
        }
        // 计算右操作数到 xmm0
        self.expr_depth += 1;
        self.gen_expr(r, Self::EVAL_BASE);
        self.expr_depth -= 1;
        // 整数右操作数 (如 x * 2): 转换到 xmm0, 否则 xmm1 会残留左操作数
        self.emit_scalar_to_xmm0_if_needed(r, &float_ty);
        // xmm1 = 右操作数, 恢复左操作数到 xmm0
        self.emit_insn(&Insn::raw_with_gas(&[0x66, 0x0F, 0x28, 0xC8], "movapd %xmm0, %xmm1"));
        if is_float {
            self.emit_insn(&Insn::raw_with_gas(&[0xF3, 0x0F, 0x10, 0x04, 0x24], "movss (%rsp), %xmm0"));
        } else {
            self.emit_insn(&Insn::raw_with_gas(&[0xF2, 0x0F, 0x10, 0x04, 0x24], "movsd (%rsp), %xmm0"));
        }
        // 恢复 rsp
        self.emit_insn(&Insn::raw_with_gas(&[0x48, 0x83, 0xC4, save_bytes as u8], &format!("add ${save_bytes}, %rsp")));
                                                          // 现在 xmm0=左, xmm1=右 → xmm0 op xmm1 = 左 op 右
                                                          // 执行运算
        let (prefix, opcode, mnem) = match op {
            BinOp::Add => (if is_float { 0xF3 } else { 0xF2 }, 0x58, if is_float { "addss" } else { "addsd" }),
            BinOp::Sub => (if is_float { 0xF3 } else { 0xF2 }, 0x5C, if is_float { "subss" } else { "subsd" }),
            BinOp::Mul => (if is_float { 0xF3 } else { 0xF2 }, 0x59, if is_float { "mulss" } else { "mulsd" }),
            BinOp::Div => (if is_float { 0xF3 } else { 0xF2 }, 0x5E, if is_float { "divss" } else { "divsd" }),
            // 比较运算
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::EqEq | BinOp::Ne => {
                self.gen_float_cmp(op, &float_ty);
                return;
            },
            _ => return, // 不支持的运算
        };
        // xmm0 op xmm1 -> xmm0
        self.emit_insn(&Insn::raw_with_gas(&[prefix, 0x0F, opcode, 0xC1], &format!("{mnem} %xmm1, %xmm0")));
    }
    fn gen_float_cmp(&mut self, op: BinOp, ty: &Type) {
        let is_float = matches!(ty, Type::Float);
        // xmm0 = 左操作数, xmm1 = 右操作数
        // ucomiss/ucomisd xmm0, xmm1 (比较 左 vs 右)
        if is_float {
            self.emit_insn(&Insn::raw_with_gas(&[0x0F, 0x2E, 0xC1], "ucomiss %xmm1, %xmm0"));
        } else {
            self.emit_insn(&Insn::raw_with_gas(&[0x66, 0x0F, 0x2E, 0xC1], "ucomisd %xmm1, %xmm0"));
        }
        // 设置条件码
        let cc = match op {
            BinOp::Lt => (0x92, "jb"),
            BinOp::Le => (0x96, "jbe"),
            BinOp::Gt => (0x97, "ja"),
            BinOp::Ge => (0x93, "jae"),
            BinOp::EqEq => (0x94, "je"),
            BinOp::Ne => (0x95, "jne"),
            _ => (0x94, "je"),
        };
        // setcc al
        self.emit_insn(&Insn::raw_with_gas(&[0x0F, cc.0, 0xC0], &format!("set{al} %al", al = &cc.1[1..])));
        // movzx rax, al
        self.emit_insn(&Insn::raw_with_gas(&[0x48, 0x0F, 0xB6, 0xC0], "movzbq %al, %rax"));
    }
    pub(crate) fn gen_float_imm_load(&mut self, c: FloatConst, ty: &Type) {
        // x87 通路（long double 恒真 / -mno-sse）：真 80-bit 常量落 .rodata,
        // 用 `fld tbyte [rip+disp32]` 直接交付在 st(0)。
        if self.float_in_st0(ty) {
            self.emit_x87_float_imm(c, ty);
            return;
        }
        let is_float = matches!(ty, Type::Float);
        let bytes = if is_float {
            // float: 4 字节
            (c.as_f64() as f32).to_le_bytes().to_vec()
        } else {
            // double: 8 字节
            c.as_f64().to_le_bytes().to_vec()
        };
        let key = format!(
            "f_{}",
            bytes
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect::<String>()
        );
        let key_bytes = key.into_bytes();
        if !self.string_pool.contains_key(&key_bytes) {
            let o = self.rodata.len() as u64;
            self.rodata.extend_from_slice(&bytes);
            self.string_pool.insert(key_bytes.clone(), o);
        }
        let off = self.string_pool[&key_bytes];
        let at = self.pos();
        // movsd xmm0, [RIP + disp32]  (F2 0F 10 05 disp32)
        // movss xmm0, [RIP + disp32]  (F3 0F 10 05 disp32)
        let prefix = if is_float { 0xF3 } else { 0xF2 };
        let mnem = if is_float { "movss" } else { "movsd" };
        self.emit_insn(&Insn::raw_with_gas(&[prefix, 0x0F, 0x10, 0x05, 0, 0, 0, 0], &format!("{mnem} 0(%rip), %xmm0")));
        self.float_refs.push((at, off));
    }
    pub(crate) fn gen_float_load(&mut self, name: &str, ty: &Type) {
        // _Imaginary: 按内型取通道，与 gen_float_store 保持一致
        let effective_ty = if let Type::Imaginary(ref inner) = ty {
            &inner.base
        } else {
            ty
        };
        // x87 通路：`fld tbyte [r13+off]` 直接交付 st(0)，不再拆低 8 字节进 xmm0。
        if self.float_in_st0(effective_ty) {
            let off = self.var_offset(name);
            self.emit_x87_fld_mem(off, effective_ty);
            return;
        }
        self.load_float_to_xmm(name, ty, 0);
    }
    pub(crate) fn gen_float_store(&mut self, name: &str, ty: &Type) {
        let off = self.var_offset(name);
        // _Imaginary: 按内型存储
        let effective_ty = if let Type::Imaginary(ref inner) = ty {
            &inner.base
        } else {
            ty
        };
        // x87 通路: `fstp tbyte [r13+off]` 把 st(0) 的真 80-bit 对象表示原样落槽，
        // 不再写“低 8 字节 f64 位型 + 高 8 字节清零”。
        if self.float_in_st0(effective_ty) {
            self.emit_x87_fstp_mem_typed(off, effective_ty);
            return;
        }
        let is_float = matches!(effective_ty, Type::Float);
        if is_float {
            // float: movss [r13+off], xmm0 → F3 49 0F 11 45 off
            // P9 方案 C: 走统一 helper, 一次发射完整指令.
            self.emit_xmm_r13_disp(0xF3, 0x11, 0, off);
        } else {
            // double: movsd [r13+off], xmm0 → F2 49 0F 11 45 off
            self.emit_xmm_r13_disp(0xF2, 0x11, 0, off);
        }
    }
    pub(crate) fn gen_float_store_at(&mut self, off: i64, ty: &Type) {
        let is_float = matches!(ty, Type::Float);
        let is_ld = matches!(ty, Type::LongDouble);
        if is_ld {
            // LD 通道: 值在 st(0)，用 fstp tbyte 写全 10 字节（含 6 字节
            // 槽填充由布局保证），不再 movq 低 8 字节 + 清高 8 字节——
            // 那会丢失 80-bit 高位（指数/显式整数位），读回是 NaN/垃圾。
            self.emit_x87_fstp_at(13, off as i32, ty);
        } else if is_float {
            self.store_xmm32_stack(0, off);
        } else {
            self.store_xmm64_stack(0, off);
        }
    }
    pub(crate) fn load_bits_from_slot(&mut self, ty: &Type, src_off: i64, dst: u8) {
        match ty {
            Type::Vector(_, _) => {
                let toff = self.vector_temp_base(0, ty.size());
                if toff != src_off {
                    self.copy_r13_bytes(src_off, toff, ty.size());
                }
            },
            Type::Complex(inner) if matches!(&inner.base, Type::Float | Type::Double) => {
                let pfx = if matches!(&inner.base, Type::Float) { 0xF3 } else { 0xF2 };
                let inner_sz = inner.size() as i64;
                let result_off = self.complex_result_off();
                self.emit_xmm_r13_disp(pfx, 0x10, 0, src_off);
                self.emit_xmm_r13_disp(pfx, 0x11, 0, result_off);
                self.emit_xmm_r13_disp(pfx, 0x10, 1, src_off + inner_sz);
                self.emit_xmm_r13_disp(pfx, 0x11, 1, result_off + inner_sz);
                self.emit_xmm_r13_disp(pfx, 0x10, 0, result_off);
            },
            Type::Complex(_) | Type::Int128 | Type::UInt128 => {
                self.load_stack(dst, src_off);
                if ty.size() > 8 {
                    self.load_stack(2, src_off + 8);
                }
            },
            Type::Float => self.load_stack_xmm(0, src_off, 4, true),
            Type::Double | Type::LongDouble => self.load_stack_xmm(0, src_off, 8, false),
            _ => self.load_stack_sized(dst, src_off, ty.size().min(8)),
        }
    }
    pub(crate) fn load_float_to_xmm(&mut self, name: &str, ty: &Type, xmm: u8) {
        let off = self.var_offset(name);
        // _Imaginary: 按内型加载
        let effective_ty = if let Type::Imaginary(ref inner) = ty {
            inner.base.clone()
        } else {
            ty.clone()
        };
        let is_float = matches!(effective_ty, Type::Float);
        // `movss/movsd xmm, off(%r13)`：xmm 在 ModRM.reg ⇒ 扩它的是 REX.R，基址 %r13 要
        // REX.B，两个位必须在**同一枚**前缀里。以前这里先 `if xmm>=8 {emit 0x44}` 再
        // 固定发 `0x41`：x86 只认最后一条 REX，于是 0x44 被后面的 0x41 直接覆盖掉，
        // xmm 落在 8..15 时读的是 `xmm(xmm&7)`。单源入口 `emit_xmm_r13_disp` 按位算；
        // xmm<8 时两边字节逐字相同（前缀仍是 0x41）。
        let pfx = if is_float { 0xF3u8 } else { 0xF2u8 };
        self.emit_xmm_r13_disp(pfx, 0x10, xmm, off);
    }
    pub(crate) fn load_mem_xmm(&mut self, xmm: u8, elem_size: usize, is_float: bool) {
        if is_float {
            self.emit_insn(&Insn::raw_bytes(&[0xF3, 0x0F, 0x10, ((xmm & 7) << 3)]));
        } else if elem_size == 8 {
            self.emit_insn(&Insn::raw_bytes(&[0xF2, 0x0F, 0x10, ((xmm & 7) << 3)]));
        } else {
            self.emit_insn(&Insn::raw_bytes(&[0xF3, 0x0F, 0x10, ((xmm & 7) << 3)]));
        }
    }
    pub(crate) fn materialize_bits_to_slot(&mut self, e: &Expr, dst_off: i64) {
        let src_ty = e.ty();
        if matches!(src_ty, Type::Vector(_, _)) {
            self.gen_vector_to_temp(e, self.vector_op_slot);
            let src_off = self.vector_temp_base(self.vector_op_slot, src_ty.size());
            self.copy_r13_bytes(src_off, dst_off, src_ty.size());
            return;
        }
        if self.gen_lvalue_addr(e, Self::EVAL_BASE) {
            self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovRm { width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(6, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64)}, &[]));
            self.emit_lea_r13(7, dst_off);
            self.mov_imm(1, src_ty.size() as i128);
            self.emit_insn(&Insn::raw_bytes(&[0xFC, 0xF3, 0xA4]));
            return;
        }
        self.gen_expr(e, Self::EVAL_BASE);
        match src_ty {
            Type::Complex(ref inner) if matches!(&inner.base, Type::Float | Type::Double) => {
                // After gen_expr for complex returned via memory, rax (0) has the address
                // Just copy all bytes from *rax to the destination slot, preserves bit pattern
                let sz = src_ty.size();
                // rsi = source address (rax), rdi = destination address (r13 + dst_off)
                // rcx = size in bytes
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovRm { width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(6, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // mov rsi, rax
                self.emit_lea_r13(7, dst_off); // lea rdi, [r13 + dst_off]
                self.mov_imm(1, sz as i128); // mov rcx, sz
                self.emit_insn(&Insn::raw_bytes(&[0xFC, 0xF3, 0xA4])); // rep movsb
            },
            Type::Float => self.store_xmm32_stack(0, dst_off),
            Type::Double | Type::LongDouble => self.store_xmm64_stack(0, dst_off),
            Type::Complex(_) | Type::Int128 | Type::UInt128 => {
                self.store_stack(0, dst_off);
                if src_ty.size() > 8 {
                    self.store_stack(2, dst_off + 8);
                }
            },
            _ => self.emit_store(0, dst_off, src_ty.size().min(8)),
        }
    }
    pub(crate) fn mov_xmm(&mut self, dst: u8, src: u8) {
        if dst == src {
            return;
        }
        let mut rex = 0u8;
        if dst >= 8 {
            rex |= 0x04; // REX.R
        }
        if src >= 8 {
            rex |= 0x01; // REX.B
        }
        if rex != 0 {
            self.emit_insn(&Insn::raw_bytes(&[0x40 | rex]));
        }
        self.emit_insn(&Insn::raw_bytes(&[0x0F, 0x28, 0xC0 | ((dst & 7) << 3) | (src & 7)]));
    }
    pub(crate) fn mov_xmm_to_xmm(&mut self, dst: u8, src: u8) {
        if dst == src {
            return;
        }
        let mut rex = 0x40u8;
        if dst >= 8 {
            rex |= 0x04;
        }
        if src >= 8 {
            rex |= 0x01;
        }
        let modrm = 0xC0 | ((dst & 7) << 3) | (src & 7);
        self.emit_insn(&Insn::raw_bytes(&[rex, 0x0F, 0x28, modrm]));
    }
    pub(crate) fn try_gen_fp_contract(&mut self, op: BinOp, l: &Expr, r: &Expr, dst: u8) -> bool {
        if !matches!(op, BinOp::Add | BinOp::Sub) {
            return false;
        }
        let ty = l.ty();
        if !matches!(ty, Type::Float | Type::Double | Type::LongDouble) || r.ty() != ty {
            return false;
        }
        let (mul_l, mul_r, addend, add_neg) = match op {
            BinOp::Add => match (l, r) {
                (
                    Expr::Binop {
                        op: BinOp::Mul,
                        l: a,
                        r: b,
                        ..
                    },
                    c,
                ) => (a.as_ref(), b.as_ref(), c, false),
                (
                    c,
                    Expr::Binop {
                        op: BinOp::Mul,
                        l: a,
                        r: b,
                        ..
                    },
                ) => (a.as_ref(), b.as_ref(), c, false),
                _ => return false,
            },
            BinOp::Sub => match l {
                Expr::Binop {
                    op: BinOp::Mul,
                    l: a,
                    r: b,
                    ..
                } => (a.as_ref(), b.as_ref(), r, true),
                _ => return false,
            },
            _ => return false,
        };
        let mut args = vec![mul_l.clone(), mul_r.clone(), addend.clone()];
        if add_neg {
            args[2] = Expr::Unary {
                op: UnOp::Neg,
                e: Box::new(args[2].clone()),
                span: Self::expr_span(addend).clone(),
                ty: addend.ty(),
            };
        }
        let builtin = match ty {
            Type::Float => "__builtin_fmaf",
            Type::LongDouble => "__builtin_fmal",
            _ => "__builtin_fma",
        };
        self.gen_builtin(builtin, &args, dst)
    }
    fn widest_float_type(a: &Type, b: &Type) -> Type {
        let rank = |t: &Type| -> u8 {
            match t {
                Type::Float => 1,
                Type::Double => 2,
                Type::LongDouble => 3,
                _ => 0,
            }
        };
        if rank(a) >= rank(b) && rank(a) > 0 {
            a.clone()
        } else {
            b.clone()
        }
    }
}
