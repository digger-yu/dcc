// src/codegen/x86_64/special/int128.rs
// __int128 / unsigned __int128 与 128 位向量元素的 x86_64 codegen。
//
// 本文件由 mod.rs 逐字搬迁而来 (batch B)：`gen_int128_binop` / `gen_int128_divmod`
// / `gen_int128_cmp_ordering` / `gen_int128_load` / `gen_int128_store`
// / `gen_vector_binop_int128` 原样剪切进 `impl Codegen` 块，函数体一字未改，
// 仅把 `fn` 提为 `pub fn`（mod.rs 是祖先模块，需要可见性）。
// 原影子自由函数 `emit_int128_load` / `emit_int128_store`（手写 ModRM 编码、
// 与生产 `gen_int128_load/store` 行为不一致）已按"以生产代码为准"作废删除。

use crate::ast::*;
use crate::codegen::shared::asm_operand::Width;
use crate::codegen::x86_64::insn::{BoolExpect, Insn};
use crate::codegen::x86_64::opcode::{
    AluOp, MemOperand, NoArgOp, Operand as OpcodeOperand, ShiftAmount, ShiftOp, UnaryOp,
    X86_64Opcode,
};
use crate::codegen::x86_64::{Codegen, JumpKind};

impl Codegen {
    /// P11 helper: emit reg-reg ALU (B64) via typed opcode.
    pub(crate) fn emit_alu_rm_reg64(&mut self, op: AluOp, dst: u8, src: u8) {
        let insn = Insn::new(
            X86_64Opcode::AluRm {
                op,
                width: Width::B64,
                dst: OpcodeOperand::Reg(dst, Width::B64),
                src: OpcodeOperand::Reg(src, Width::B64),
            },
            &[],
        );
        self.emit_insn(&insn);
    }

    /// P11 Bucket M: emit unary op (B64) via typed UnaryRm.
    pub(crate) fn emit_unary64(&mut self, op: UnaryOp, dst: u8) {
        let insn = Insn::new(
            X86_64Opcode::UnaryRm {
                op,
                width: Width::B64,
                dst: OpcodeOperand::Reg(dst, Width::B64),
            },
            &[],
        );
        self.emit_insn(&insn);
    }

    /// P11 Bucket M: emit shift (B64) via typed Shift.
    fn emit_shift64(&mut self, op: ShiftOp, dst: u8, amount: ShiftAmount) {
        let insn = Insn::new(
            X86_64Opcode::Shift {
                op,
                width: Width::B64,
                dst: OpcodeOperand::Reg(dst, Width::B64),
                amount,
            },
            &[],
        );
        self.emit_insn(&insn);
    }

    /// P11 Bucket M: emit B64 mov reg ← [base+disp] (load) or [base+disp] → reg (store).
    fn emit_mov_mem64(&mut self, dst: u8, src: u8, mem: MemOperand) {
        // dst=reg, src=Mem => load; dst=Mem, src=reg => store.  Here we pass both regs
        // via parameters: load_reg = reg receiving; store_reg = reg stored.
        let (dst_op, src_op) = if mem.base.is_some() && dst == u8::MAX {
            // store: dst=Mem(r/m), src=reg(store_reg).
            (OpcodeOperand::Mem(mem), OpcodeOperand::Reg(src, Width::B64))
        } else {
            (OpcodeOperand::Reg(dst, Width::B64), OpcodeOperand::Mem(mem))
        };
        let insn = Insn::new(
            X86_64Opcode::MovRm {
                rip_relative: false,
                width: Width::B64,
                dst: dst_op,
                src: src_op,
            },
            &[],
        );
        self.emit_insn(&insn);
    }

    /// P11 Bucket M: emit reg,imm8 ALU (B64) via typed opcode.
    fn emit_alu_rm_reg_imm8_b64(&mut self, op: AluOp, dst: u8, imm: i64) {
        let insn = Insn::new(
            X86_64Opcode::AluRm {
                op,
                width: Width::B64,
                dst: OpcodeOperand::Reg(dst, Width::B64),
                src: OpcodeOperand::Imm(imm),
            },
            &[],
        );
        self.emit_insn(&insn);
    }

    /// P11 Bucket M: emit load rax/rdx = [r11+disp] (global var low/high).
    fn emit_load_r11_disp64(&mut self, reg: u8, disp: i32) {
        let mem = MemOperand {
            disp,
            base: Some(11),
            index: None,
            seg: None,
        };
        self.emit_mov_mem64(reg, 0, mem);
    }

    /// P11 Bucket M: emit store [r11+disp] = reg (global var low/high).
    fn emit_store_r11_disp64(&mut self, reg: u8, disp: i32) {
        let mem = MemOperand {
            disp,
            base: Some(11),
            index: None,
            seg: None,
        };
        // dst=Mem, src=reg. Use u8::MAX as sentinel since reg=0..15 is real.
        self.emit_mov_mem64(u8::MAX, reg, mem);
    }

    /// P11 Bucket M: emit mov r32, imm32 (B32 short form B8+r + imm32).
    fn emit_mov_imm32_b32(&mut self, reg: u8, imm: i64) {
        let insn = Insn::new(
            X86_64Opcode::MovRm {
                rip_relative: false,
                width: Width::B32,
                dst: OpcodeOperand::Reg(reg, Width::B32),
                src: OpcodeOperand::Imm(imm),
            },
            &[],
        );
        self.emit_insn(&insn);
    }

    /// P11 Bucket M: emit AluRm reg,reg (B8).
    fn emit_alu_rm_reg8(&mut self, op: AluOp, dst: u8, src: u8) {
        let insn = Insn::new(
            X86_64Opcode::AluRm {
                op,
                width: Width::B8,
                dst: OpcodeOperand::Reg(dst, Width::B8),
                src: OpcodeOperand::Reg(src, Width::B8),
            },
            &[],
        );
        self.emit_insn(&insn);
    }

    // P1-4: __int128 二元运算 codegen
    // 使用 rax:rdx = 右操作数, rcx:r8 = 左操作数, 结果 → rax:rdx
    pub fn gen_int128_binop(&mut self, op: BinOp, l: &Expr, r: &Expr, dst: u8) {
        // 1. 求值左操作数 → rax, push 两次 (低/高)
        self.gen_expr_decay(l, Self::EVAL_BASE);
        self.push(0); // push low
        if !matches!(l.ty(), Type::Int128 | Type::UInt128) {
            if matches!(
                l.ty(),
                Type::Bool
                    | Type::UChar
                    | Type::UShort
                    | Type::UInt
                    | Type::ULong
                    | Type::ULongLong
            ) {
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
                )); // unsigned zero extension
            } else {
                self.emit_insn(&Insn::sign_extend_rax_to_rdx(
                    crate::codegen::shared::asm_operand::Width::B64,
                )); // signed extension
            }
        }
        self.push(2); // push high
                      // 2. 求值右操作数 → rax:rdx
        self.gen_expr_decay(r, Self::EVAL_BASE);
        if !matches!(r.ty(), Type::Int128 | Type::UInt128) {
            if matches!(
                r.ty(),
                Type::Bool
                    | Type::UChar
                    | Type::UShort
                    | Type::UInt
                    | Type::ULong
                    | Type::ULongLong
            ) {
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
            } else {
                self.emit_insn(&Insn::sign_extend_rax_to_rdx(
                    crate::codegen::shared::asm_operand::Width::B64,
                ));
            }
        }
        // 保存右操作数
        self.push(0); // push rax (right_low)
        self.push(2); // push rdx (right_high)
                      // 3. 按与 push 相反的顺序恢复两个 128 位操作数。
        self.pop(11); // r11 = right_high
        self.pop(9); // r9 = right_low
        self.pop(8); // r8 = left_high
        self.pop(1); // rcx = left_low
                     // 现在: rcx=left_low, r8=left_high, r9=right_low, r11=right_high
                     // r14/r15 是 SysV 被调用者保存寄存器，临时使用前必须保护。
        self.push(14);
        self.push(15);
        match op {
            BinOp::Add => {
                // add rcx, r9 (low): REX.W+R(r9=9) = 0x4C
                self.emit_insn(&Insn::new(
                    X86_64Opcode::AluRm {
                        op: AluOp::Add,
                        width: Width::B64,
                        dst: OpcodeOperand::Reg(1, Width::B64),
                        src: OpcodeOperand::Reg(9, Width::B64),
                    },
                    &[],
                )); // add rcx, r9
                    // adc r8, r11 (high): REX.W+R(r11)+B(r8) = 0x4D
                self.emit_alu_rm_reg64(AluOp::Adc, 8, 11);
                self.mov(0, 1); // rax = result_low
                self.mov(2, 8); // rdx = result_high
            },
            BinOp::Sub => {
                // sub rcx, r9 (low): REX.W+R(r9) = 0x4C
                self.emit_insn(&Insn::new(
                    X86_64Opcode::AluRm {
                        op: AluOp::Sub,
                        width: Width::B64,
                        dst: OpcodeOperand::Reg(1, Width::B64),
                        src: OpcodeOperand::Reg(9, Width::B64),
                    },
                    &[],
                )); // sub rcx, r9
                    // sbb r8, r11 (high): REX.W+R(r11)+B(r8) = 0x4D
                self.emit_alu_rm_reg64(AluOp::Sbb, 8, 11);
                self.mov(0, 1);
                self.mov(2, 8);
            },
            BinOp::And => {
                self.emit_insn(&Insn::new(
                    X86_64Opcode::AluRm {
                        op: AluOp::And,
                        width: Width::B64,
                        dst: OpcodeOperand::Reg(1, Width::B64),
                        src: OpcodeOperand::Reg(9, Width::B64),
                    },
                    &[],
                )); // and rcx, r9
                self.emit_insn(&Insn::new(
                    X86_64Opcode::AluRm {
                        op: AluOp::And,
                        width: Width::B64,
                        dst: OpcodeOperand::Reg(8, Width::B64),
                        src: OpcodeOperand::Reg(11, Width::B64),
                    },
                    &[],
                )); // and r8, r11
                self.mov(0, 1);
                self.mov(2, 8);
            },
            BinOp::Or => {
                self.emit_insn(&Insn::new(
                    X86_64Opcode::AluRm {
                        op: AluOp::Or,
                        width: Width::B64,
                        dst: OpcodeOperand::Reg(1, Width::B64),
                        src: OpcodeOperand::Reg(9, Width::B64),
                    },
                    &[],
                )); // or rcx, r9
                self.emit_insn(&Insn::new(
                    X86_64Opcode::AluRm {
                        op: AluOp::Or,
                        width: Width::B64,
                        dst: OpcodeOperand::Reg(8, Width::B64),
                        src: OpcodeOperand::Reg(11, Width::B64),
                    },
                    &[],
                )); // or r8, r11
                self.mov(0, 1);
                self.mov(2, 8);
            },
            BinOp::Xor => {
                self.emit_insn(&Insn::new(
                    X86_64Opcode::AluRm {
                        op: AluOp::Xor,
                        width: Width::B64,
                        dst: OpcodeOperand::Reg(1, Width::B64),
                        src: OpcodeOperand::Reg(9, Width::B64),
                    },
                    &[],
                )); // xor rcx, r9
                self.emit_insn(&Insn::new(
                    X86_64Opcode::AluRm {
                        op: AluOp::Xor,
                        width: Width::B64,
                        dst: OpcodeOperand::Reg(8, Width::B64),
                        src: OpcodeOperand::Reg(11, Width::B64),
                    },
                    &[],
                )); // xor r8, r11
                self.mov(0, 1);
                self.mov(2, 8);
            },
            BinOp::Mul => {
                // 低 128 位 = a0*b0 + ((a0*b1 + a1*b0) << 64)。
                self.mov(0, 1); // rax = a0
                self.emit_unary64(UnaryOp::Mul, 9); // mul r9
                self.mov(14, 0); // 保存低 64 位
                self.mov(10, 2); // r10 = a0*b0 高位
                self.mov(0, 1);
                self.emit_unary64(UnaryOp::Mul, 11); // mul r11
                self.emit_insn(&Insn::new(
                    X86_64Opcode::AluRm {
                        op: AluOp::Add,
                        width: Width::B64,
                        dst: OpcodeOperand::Reg(10, Width::B64),
                        src: OpcodeOperand::Reg(0, Width::B64),
                    },
                    &[],
                )); // add r10, rax
                self.mov(0, 8);
                self.emit_unary64(UnaryOp::Mul, 9); // mul r9
                self.emit_insn(&Insn::new(
                    X86_64Opcode::AluRm {
                        op: AluOp::Add,
                        width: Width::B64,
                        dst: OpcodeOperand::Reg(10, Width::B64),
                        src: OpcodeOperand::Reg(0, Width::B64),
                    },
                    &[],
                )); // add r10, rax
                self.mov(0, 14);
                self.mov(2, 10);
            },
            BinOp::Div | BinOp::Mod => {
                let signed = !matches!(l.ty(), Type::UInt128) && !matches!(r.ty(), Type::UInt128);
                self.gen_int128_divmod(signed, matches!(op, BinOp::Mod));
            },
            BinOp::EqEq => {
                self.emit_alu_rm_reg64(AluOp::Xor, 0, 0); // xor rax, rax
                self.emit_insn(&Insn::new(
                    X86_64Opcode::AluRm {
                        op: AluOp::Cmp,
                        width: Width::B64,
                        dst: OpcodeOperand::Reg(1, Width::B64),
                        src: OpcodeOperand::Reg(9, Width::B64),
                    },
                    &[],
                )); // cmp rcx, r9 (low)
                let lne = self.fresh_label();
                self.jmp_rel32(&lne, JumpKind::Jne);
                self.emit_insn(&Insn::new(
                    X86_64Opcode::AluRm {
                        op: AluOp::Cmp,
                        width: Width::B64,
                        dst: OpcodeOperand::Reg(8, Width::B64),
                        src: OpcodeOperand::Reg(11, Width::B64),
                    },
                    &[],
                )); // cmp r8, r11 (high)
                self.jmp_rel32(&lne, JumpKind::Jne);
                self.mov_imm(0, 1);
                self.label(&lne);
                self.emit_alu_rm_reg64(AluOp::Xor, 2, 2); // xor rdx, rdx
            },
            BinOp::Ne => {
                self.emit_alu_rm_reg64(AluOp::Xor, 0, 0); // xor rax, rax
                self.mov_imm(0, 1);
                let leq = self.fresh_label();
                self.emit_insn(&Insn::new(
                    X86_64Opcode::AluRm {
                        op: AluOp::Cmp,
                        width: Width::B64,
                        dst: OpcodeOperand::Reg(1, Width::B64),
                        src: OpcodeOperand::Reg(9, Width::B64),
                    },
                    &[],
                )); // cmp rcx, r9 (low)
                self.jmp_rel32(&leq, JumpKind::Jne);
                self.emit_insn(&Insn::new(
                    X86_64Opcode::AluRm {
                        op: AluOp::Cmp,
                        width: Width::B64,
                        dst: OpcodeOperand::Reg(8, Width::B64),
                        src: OpcodeOperand::Reg(11, Width::B64),
                    },
                    &[],
                )); // cmp r8, r11 (high)
                self.jmp_rel32(&leq, JumpKind::Jne);
                self.mov_imm(0, 0);
                self.label(&leq);
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
            },
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                let is_signed =
                    !matches!(l.ty(), Type::UInt128) && !matches!(r.ty(), Type::UInt128);
                self.gen_int128_cmp_ordering(op, is_signed);
            },
            BinOp::Shl => {
                // rcx=left_low, r8=left_high, r9=shift_amount
                // 目标: rax:left_low, rdx:left_high, rcx:shift_count
                // 但 rcx 原来存 left_low, 需要先保存到其他地方
                // 重新组织: 先保存 shift 到 r9→已在那里, 用 rax=left_low, rdx=left_high
                // rcx 被覆盖前 left_low 已在 rax 中? 不, left_low 在 rcx 中
                // 解决: 先 mov rax, rcx (保存 left_low), 然后 mov rcx, r9 (shift)
                self.mov(0, 1); // rax = rcx = left_low
                self.mov(2, 8); // rdx = r8 = left_high
                self.mov(1, 9); // rcx = r9 = shift_amount
                let l_under_128 = self.fresh_label();
                let l_end = self.fresh_label();
                self.emit_insn(&Insn::alu_rm_reg_imm32_b64(7, 1, 128)); // cmp rcx, 128
                self.jmp_rel32(&l_under_128, JumpKind::Jl);
                self.emit_alu_rm_reg64(AluOp::Xor, 0, 0);
                self.emit_alu_rm_reg64(AluOp::Xor, 2, 2);
                self.jmp_rel32(&l_end, JumpKind::Jmp);
                self.label(&l_under_128);
                // cmp rcx, 64
                self.emit_insn(&Insn::new(
                    crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                        op: crate::codegen::x86_64::opcode::AluOp::Cmp,
                        width: crate::codegen::shared::asm_operand::Width::B64,
                        dst: crate::codegen::x86_64::opcode::Operand::Reg(
                            1,
                            crate::codegen::shared::asm_operand::Width::B64,
                        ),
                        src: crate::codegen::x86_64::opcode::Operand::Imm(64),
                    },
                    &[],
                )); // cmp rcx, 64
                let l_small = self.fresh_label();
                let l_eq64 = self.fresh_label();
                self.jmp_rel32(&l_small, JumpKind::Jl);
                // >= 64: n in [64,128). Result: lo=0, hi = (old_lo << (n-64)) | (old_hi >> (128-n)).
                // Incoming: rax=old_lo, rdx=old_hi, rcx=n.
                // If n == 64 (k=0): result_hi = old_lo, result_lo = 0.
                // (shr by 64 on a 64-bit reg degenerates to shr-by-0 under CL&63, so
                // we cannot rely on the generic formula when k==0.)
                self.emit_insn(&Insn::new(
                    crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                        op: crate::codegen::x86_64::opcode::AluOp::Cmp,
                        width: crate::codegen::shared::asm_operand::Width::B64,
                        dst: crate::codegen::x86_64::opcode::Operand::Reg(
                            1,
                            crate::codegen::shared::asm_operand::Width::B64,
                        ),
                        src: crate::codegen::x86_64::opcode::Operand::Imm(64),
                    },
                    &[],
                )); // cmp rcx, 64
                self.jmp_rel32(&l_eq64, JumpKind::Je);
                // n in (64,128): k = n-64 in (0,64).
                // 128 位结果 = (old_lo << (64+k)) mod 2^128: 低 64 位必为 0,
                // 高 64 位 = (old_lo << k) mod 2^64; old_hi 整体已左移 >=128 位,
                // 对低 128 位贡献为 0, 不可再并入 (旧实现错误地 OR 了 old_hi>>(64-k))。
                self.mov(8, 0); // r8 = old_lo
                self.emit_alu_rm_reg_imm8_b64(AluOp::Sub, 1, 64); // rcx = k = n-64
                self.mov(2, 8); // rdx = old_lo
                self.emit_shift64(ShiftOp::Shl, 2, ShiftAmount::Cl); // rdx = old_lo << k (高 64 位结果)
                self.emit_alu_rm_reg64(AluOp::Xor, 0, 0); // rax = 0 (低 64 位结果)
                self.jmp_rel32(&l_end, JumpKind::Jmp);
                // n == 64: swap halves: hi = old_lo, lo = 0.
                self.label(&l_eq64);
                self.mov(2, 0); // rdx = old_lo
                self.emit_alu_rm_reg64(AluOp::Xor, 0, 0); // rax = 0
                self.jmp_rel32(&l_end, JumpKind::Jmp);
                // < 64: shld rdx, rax, cl; shl rax, cl
                self.label(&l_small);
                self.emit_insn(&Insn::new(
                    crate::codegen::x86_64::opcode::X86_64Opcode::ShldShrd {
                        is_shld: true,
                        width: crate::codegen::shared::asm_operand::Width::B64,
                        dst: crate::codegen::x86_64::opcode::Operand::Reg(
                            2,
                            crate::codegen::shared::asm_operand::Width::B64,
                        ),
                        src: crate::codegen::x86_64::opcode::Operand::Reg(
                            0,
                            crate::codegen::shared::asm_operand::Width::B64,
                        ),
                    },
                    &[],
                )); // shld rdx, rax, cl
                self.emit_shift64(ShiftOp::Shl, 0, ShiftAmount::Cl); // shl rax, cl
                self.label(&l_end);
                // rax:rdx = result
            },
            BinOp::Shr => {
                // rcx=left_low, r8=left_high, r9=shift_amount
                self.mov(0, 1); // rax = rcx = left_low
                self.mov(2, 8); // rdx = r8 = left_high
                self.mov(1, 9); // rcx = r9 = shift_amount
                let signed = matches!(l.ty(), Type::Int128);
                let l_under_128 = self.fresh_label();
                let l_end = self.fresh_label();
                self.emit_insn(&Insn::alu_rm_reg_imm32_b64(7, 1, 128)); // cmp rcx, 128
                self.jmp_rel32(&l_under_128, JumpKind::Jl);
                if signed {
                    self.mov(0, 2);
                    self.emit_shift64(ShiftOp::Sar, 0, ShiftAmount::Imm8(63)); // sar rax, 63
                    self.mov(2, 0);
                } else {
                    self.emit_alu_rm_reg64(AluOp::Xor, 0, 0);
                    self.emit_alu_rm_reg64(AluOp::Xor, 2, 2);
                }
                self.jmp_rel32(&l_end, JumpKind::Jmp);
                self.label(&l_under_128);
                self.emit_insn(&Insn::new(
                    crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                        op: crate::codegen::x86_64::opcode::AluOp::Cmp,
                        width: crate::codegen::shared::asm_operand::Width::B64,
                        dst: crate::codegen::x86_64::opcode::Operand::Reg(
                            1,
                            crate::codegen::shared::asm_operand::Width::B64,
                        ),
                        src: crate::codegen::x86_64::opcode::Operand::Imm(64),
                    },
                    &[],
                )); // cmp rcx, 64
                let l_small = self.fresh_label();
                self.jmp_rel32(&l_small, JumpKind::Jl);
                // >= 64: rax = high >> (cl-64), high = sign/zero fill
                self.mov(8, 2); // r8 = rdx = left_high (保存)
                self.mov(0, 8); // rax = r8 = left_high
                if signed {
                    self.emit_shift64(ShiftOp::Sar, 0, ShiftAmount::Cl); // sar rax, cl
                    self.mov(2, 8);
                    self.emit_shift64(ShiftOp::Sar, 2, ShiftAmount::Imm8(63)); // sar rdx, 63
                } else {
                    self.emit_shift64(ShiftOp::Shr, 0, ShiftAmount::Cl); // shr rax, cl
                    self.emit_alu_rm_reg64(AluOp::Xor, 2, 2);
                    // high = 0
                }
                self.jmp_rel32(&l_end, JumpKind::Jmp);
                // < 64: shrd rax, rdx, cl; shift high with sign/zero fill
                self.label(&l_small);
                self.emit_insn(&Insn::new(
                    crate::codegen::x86_64::opcode::X86_64Opcode::ShldShrd {
                        is_shld: false,
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
                )); // shrd rax, rdx, cl
                self.emit_shift64(
                    if signed { ShiftOp::Sar } else { ShiftOp::Shr },
                    2,
                    ShiftAmount::Cl,
                );
                self.label(&l_end);
            },
            BinOp::AndAnd | BinOp::OrOr => {
                unreachable!("logical int128 ops are lowered separately")
            },
        }
        self.pop(15);
        self.pop(14);
        // 5. 移动到 dst
        if dst != 0 {
            self.mov(dst, 0);
        }
        // 结果的高 64 位在 rdx, 调用者需要存储到栈的第二个槽
    }

    /// 128 位 restoring division。
    /// 输入: r8:rcx dividend, r11:r9 divisor；输出: rdx:rax quotient/remainder。
    pub fn gen_int128_divmod(&mut self, signed: bool, want_remainder: bool) {
        // esi = 商符号，edi = 被除数符号（余数跟随被除数）。
        self.emit_insn(&Insn::alu_rm_reg_reg(
            crate::codegen::x86_64::opcode::AluOp::Xor,
            crate::codegen::shared::asm_operand::Width::B32,
            6,
            6,
        )); // xor esi, esi
        self.emit_insn(&Insn::alu_rm_reg_reg(
            crate::codegen::x86_64::opcode::AluOp::Xor,
            crate::codegen::shared::asm_operand::Width::B32,
            7,
            7,
        )); // xor edi, edi
        if signed {
            let lhs_positive = self.fresh_label();
            self.emit_insn(&Insn::test_rm_reg(
                crate::codegen::shared::asm_operand::Width::B64,
                8,
                8,
            )); // test r8, r8
            self.jmp_rel32(&lhs_positive, JumpKind::Jns);
            self.emit_insn(&Insn::unary_rm_reg(
                crate::codegen::x86_64::opcode::UnaryOp::Neg,
                crate::codegen::shared::asm_operand::Width::B64,
                1,
            )); // neg rcx
            self.emit_alu_rm_reg_imm8_b64(AluOp::Adc, 8, 0); // adc r8, 0
            self.emit_insn(&Insn::unary_rm_reg(
                crate::codegen::x86_64::opcode::UnaryOp::Neg,
                crate::codegen::shared::asm_operand::Width::B64,
                8,
            )); // neg r8
            self.emit_insn(&Insn::alu_rm_reg_imm8(6, 6, 1)); // xor esi, 1
            self.emit_mov_imm32_b32(7, 1); // mov edi, 1
            self.label(&lhs_positive);

            let rhs_positive = self.fresh_label();
            self.emit_insn(&Insn::test_rm_reg(
                crate::codegen::shared::asm_operand::Width::B64,
                11,
                11,
            )); // test r11, r11
            self.jmp_rel32(&rhs_positive, JumpKind::Jns);
            self.emit_insn(&Insn::unary_rm_reg(
                crate::codegen::x86_64::opcode::UnaryOp::Neg,
                crate::codegen::shared::asm_operand::Width::B64,
                9,
            )); // neg r9
            self.emit_alu_rm_reg_imm8_b64(AluOp::Adc, 11, 0); // adc r11, 0
            self.emit_insn(&Insn::unary_rm_reg(
                crate::codegen::x86_64::opcode::UnaryOp::Neg,
                crate::codegen::shared::asm_operand::Width::B64,
                11,
            )); // neg r11
            self.emit_insn(&Insn::alu_rm_reg_imm8(6, 6, 1)); // xor esi, 1
            self.label(&rhs_positive);
        }

        // C 对零除未定义；用 ud2 明确终止，绝不制造数值 0。
        self.mov(10, 9);
        self.emit_alu_rm_reg64(AluOp::Or, 10, 11); // or r10, r11
        let divisor_ok = self.fresh_label();
        self.emit_insn(&Insn::test_rm_reg(crate::codegen::shared::asm_operand::Width::B64, 10, 10)); // test r10, r10
        self.jmp_rel32(&divisor_ok, JumpKind::Jne);
        self.emit_insn(&Insn::new(X86_64Opcode::NoArg { op: NoArgOp::Ud2 }, &[])); // ud2
        self.label(&divisor_ok);

        self.mov(0, 1); // quotient/dividend low
        self.mov(2, 8); // quotient/dividend high
        self.emit_alu_rm_reg64(AluOp::Xor, 14, 14); // xor r14, r14
        self.emit_alu_rm_reg64(AluOp::Xor, 15, 15); // xor r15, r15
        self.emit_mov_imm32_b32(10, 128); // mov r10d, 128
        let loop_label = self.fresh_label();
        let subtract = self.fresh_label();
        let next = self.fresh_label();
        self.label(&loop_label);
        self.emit_insn(&Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::Shift {
                op: crate::codegen::x86_64::opcode::ShiftOp::Shl,
                width: crate::codegen::shared::asm_operand::Width::B64,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    0,
                    crate::codegen::shared::asm_operand::Width::B64,
                ),
                amount: crate::codegen::x86_64::opcode::ShiftAmount::Imm8(1),
            },
            &[],
        )); // shl rax, 1
        self.emit_shift64(ShiftOp::Rcl, 2, ShiftAmount::Imm8(1)); // rcl rdx, 1
        self.emit_shift64(ShiftOp::Rcl, 14, ShiftAmount::Imm8(1)); // rcl r14, 1
        self.emit_shift64(ShiftOp::Rcl, 15, ShiftAmount::Imm8(1)); // rcl r15, 1
        self.emit_alu_rm_reg64(AluOp::Cmp, 15, 11); // cmp r15, r11
        self.jmp_rel32(&next, JumpKind::Jb);
        self.jmp_rel32(&subtract, JumpKind::Ja);
        self.emit_alu_rm_reg64(AluOp::Cmp, 14, 9); // cmp r14, r9
        self.jmp_rel32(&next, JumpKind::Jb);
        self.label(&subtract);
        self.emit_alu_rm_reg64(AluOp::Sub, 14, 9); // sub r14, r9
        self.emit_alu_rm_reg64(AluOp::Sbb, 15, 11); // sbb r15, r11
        self.emit_insn(&Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                op: crate::codegen::x86_64::opcode::AluOp::Or,
                width: crate::codegen::shared::asm_operand::Width::B64,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    0,
                    crate::codegen::shared::asm_operand::Width::B64,
                ),
                src: crate::codegen::x86_64::opcode::Operand::Imm(1),
            },
            &[],
        )); // or rax, 1
        self.label(&next);
        self.emit_unary64(UnaryOp::Dec, 10); // dec r10
        self.jmp_rel32(&loop_label, JumpKind::Jne);

        if want_remainder {
            self.mov(0, 14);
            self.mov(2, 15);
            if signed {
                let positive = self.fresh_label();
                self.emit_insn(&Insn::test_rm_reg(
                    crate::codegen::shared::asm_operand::Width::B32,
                    7,
                    7,
                )); // test edi, edi
                self.jmp_rel32(&positive, JumpKind::Je);
                self.emit_insn(&Insn::unary_rm_reg(
                    crate::codegen::x86_64::opcode::UnaryOp::Neg,
                    crate::codegen::shared::asm_operand::Width::B64,
                    0,
                )); // neg rax
                self.emit_alu_rm_reg_imm8_b64(AluOp::Adc, 2, 0); // adc rdx, 0
                self.emit_insn(&Insn::unary_rm_reg(
                    crate::codegen::x86_64::opcode::UnaryOp::Neg,
                    crate::codegen::shared::asm_operand::Width::B64,
                    2,
                )); // neg rdx
                self.label(&positive);
            }
        } else if signed {
            let positive = self.fresh_label();
            self.emit_insn(&Insn::test_rm_reg(
                crate::codegen::shared::asm_operand::Width::B32,
                6,
                6,
            )); // test esi, esi
            self.jmp_rel32(&positive, JumpKind::Je);
            self.emit_insn(&Insn::unary_rm_reg(
                crate::codegen::x86_64::opcode::UnaryOp::Neg,
                crate::codegen::shared::asm_operand::Width::B64,
                0,
            )); // neg rax
            self.emit_alu_rm_reg_imm8_b64(AluOp::Adc, 2, 0); // adc rdx, 0
            self.emit_insn(&Insn::unary_rm_reg(
                crate::codegen::x86_64::opcode::UnaryOp::Neg,
                crate::codegen::shared::asm_operand::Width::B64,
                2,
            )); // neg rdx
            self.label(&positive);
        }
    }

    // P1-4: __int128 有序比较 (Lt/Le/Gt/Ge)
    // 输入: rcx=left_low, r8=left_high, r9=right_low, r11=right_high
    // 输出: rax = 0 或 1, rdx = 0
    pub fn gen_int128_cmp_ordering(&mut self, op: BinOp, is_signed: bool) {
        let l_end = self.fresh_label();
        let l_high = self.fresh_label();
        self.emit_alu_rm_reg64(AluOp::Xor, 0, 0); // xor rax, rax
                                                  // 比较高 64 位: cmp r8, r11
        self.emit_alu_rm_reg64(AluOp::Cmp, 8, 11); // cmp r8, r11
                                                   // P11 Bucket M: map (op, signed) to typed CondCode. cc 编码:
                                                   //   0x92=B, 0x93=Ae, 0x96=Be, 0x97=A, 0x9C=Lt, 0x9D=Ge, 0x9E=Le, 0x9F=Gt.
        use crate::codegen::x86_64::opcode::CondCode;
        let (cc_high, cc_low_eq): (CondCode, CondCode) = match op {
            BinOp::Lt if is_signed => (CondCode::Lt, CondCode::B),
            BinOp::Le if is_signed => (CondCode::Lt, CondCode::Be),
            BinOp::Gt if is_signed => (CondCode::Gt, CondCode::A),
            BinOp::Ge if is_signed => (CondCode::Gt, CondCode::Ae),
            BinOp::Lt => (CondCode::B, CondCode::B),
            BinOp::Le => (CondCode::B, CondCode::Be),
            BinOp::Gt => (CondCode::A, CondCode::A),
            BinOp::Ge => (CondCode::A, CondCode::Ae),
            _ => unreachable!(),
        };
        // 高半部不等时直接使用高半部比较结果。
        self.jmp_rel32(&l_high, JumpKind::Jne);
        // 高半部相等: 比较低半部 cmp rcx, r9
        self.emit_alu_rm_reg64(AluOp::Cmp, 1, 9); // cmp rcx, r9
        self.emit_insn(&Insn::new(
            X86_64Opcode::Setcc {
                cc: cc_low_eq,
                dst: OpcodeOperand::Reg(0, Width::B8),
            },
            &[],
        )); // setcc al
        self.jmp_rel32(&l_end, JumpKind::Jmp);
        // 高半部不等: 使用高半部比较结果
        self.label(&l_high);
        self.emit_insn(&Insn::new(
            X86_64Opcode::Setcc {
                cc: cc_high,
                dst: OpcodeOperand::Reg(0, Width::B8),
            },
            &[],
        )); // setcc al
        self.label(&l_end);
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

    // P1-4: __int128 加载变量到 rax:rdx (低:高)
    pub fn gen_int128_load(&mut self, name: &str) {
        if self.is_global_var(name) {
            self.emit_global_addr(11, name); // r11 = &global (resolve_global_key inside)
            self.emit_load_r11_disp64(0, 0); // rax = [r11]
            self.emit_load_r11_disp64(2, 8); // rdx = [r11+8]
        } else {
            let off = self.var_offset(name);
            // rax = [r13 + off] (low)
            self.load_stack(0, off);
            // rdx = [r13 + off + 8] (high)
            self.load_stack(2, off + 8);
        }
    }

    /// 从**地址寄存器** `addr_reg` 指向的 16 字节对象加载 __int128:
    /// 低 64 位 → dst, 高 64 位 → rdx (与 [`Self::gen_int128_load`] 同值模型)。
    ///
    /// 成员/下标/解引用形态的 `__int128` 左值 (如 `u.b`、`p->v`) 地址算完后
    /// 对象在内存里, 旧的成员读分支只认 1/2/4/8 字节形状, 对 16 字节不发
    /// 任何加载 —— dst 留着地址本身, 低半读成垃圾 (pr92904 根因之一)。
    /// `dst == rdx` 时借 rcx 周转, 防止高半覆盖刚取出的低半。
    pub fn gen_int128_load_at(&mut self, addr_reg: u8, dst: u8) {
        use crate::codegen::shared::asm_operand::Width;
        if dst == 2 {
            self.emit_insn(&Insn::mov_rm_mem(true, Width::B64, 1, addr_reg, 0)); // mov rcx, [addr]
            self.emit_insn(&Insn::mov_rm_mem(true, Width::B64, 2, addr_reg, 8)); // mov rdx, [addr+8]
            self.emit_insn(&Insn::mov_rm_reg(Width::B64, dst, 1)); // mov dst, rcx
        } else {
            self.emit_insn(&Insn::mov_rm_mem(true, Width::B64, 2, addr_reg, 8)); // mov rdx, [addr+8]
            self.emit_load_self_typed(8, false, dst); // mov dst, [addr]
        }
    }

    // P1-4: __int128 从 rax:rdx 存储到变量
    pub fn gen_int128_store(&mut self, name: &str) {
        if self.is_global_var(name) {
            self.emit_global_addr(11, name);
            self.emit_store_r11_disp64(0, 0); // [r11] = rax
            self.emit_store_r11_disp64(2, 8); // [r11+8] = rdx
        } else {
            let off = self.var_offset(name);
            self.store_stack(0, off); // low → [r13+off]
            self.store_stack(2, off + 8); // high → [r13+off+8]
        }
    }

    /// 标量整数 store 的**唯一**发射口：把值模型 `(rax = 低 64 位, rdx = 高 64 位)`
    /// 按 `size` 字节宽度写入 `[base + disp]`（可选段前缀紧贴每一笔访问）。
    ///
    /// 16 字节整型（`__int128` / `_BitInt(65..=128)`）装不进一个 GP 寄存器，必须发
    /// 两条 quad store。此前这套「按宽度选 store」的判据在赋值分发里被抄了七份，
    /// 其中解引用与通用左值回退两份只按 `_ => Width::B64` 发低半那一笔、高半留在
    /// rdx 里没人写 —— `u128 first = 0; *p = h1;` 之后调用方读到的 `first` 高 8
    /// 字节是栈上残值（long-mul-128.c 实测得到 `0xC << 64`）。成员/下标那几份各自
    /// 补了高半，但写法又不尽相同。这里收成单一真源，调用方不再自己拼字节。
    ///
    /// 浮点类型不走这里：它们的值通道是 xmm0 / st(0)，由
    /// [`Self::emit_store_scalar_float_at`] 负责。
    pub fn emit_store_scalar_int_at(&mut self, size: usize, base: u8, disp: i32, seg: Option<u8>) {
        let w = match size {
            1 => Width::B8,
            2 => Width::B16,
            4 => Width::B32,
            _ => Width::B64,
        };
        self.emit_seg(seg);
        self.emit_insn(&Insn::mov_rm_mem(false, w, 0, base, disp))
            .expect("typed-opcode: mov_rm_mem"); // 低半 → [base+disp]
        if size == 16 {
            // 高 64 位 → [base+disp+8]：段前缀只作用于紧随的一条指令，第二笔要重发。
            self.emit_seg(seg);
            self.emit_insn(&Insn::mov_rm_mem(false, Width::B64, 2, base, disp + 8))
                .expect("typed-opcode: mov_rm_mem");
        }
    }

    /// 128-bit 整数 (__int128/unsigned __int128) 向量 binop:
    /// GP 寄存器无法承载 16 字节元素, 必须用 XMM 打包指令处理.
    /// 支持比较 (EqEq/Ne) 和按位运算 (And/Or/Xor).
    /// 有标量操作数时, 标量通过 gen_expr 求值后存入临时槽再广播.
    pub fn gen_vector_binop_int128(
        &mut self,
        op: BinOp,
        l: &Expr,
        r: &Expr,
        result_slot: usize,
        result_off: i64,
        _elem_ty: &Type,
        count: usize,
        elem_size: usize,
        l_is_vec: bool,
        r_is_vec: bool,
        saved_op_slot: usize,
    ) {
        let vec_size = elem_size * count;
        self.vector_op_slot = result_slot + 1;

        // 物化操作数到临时槽
        let l_slot = result_slot + 1;
        let r_slot = result_slot + 2;
        let l_off: i64 = if l_is_vec {
            self.gen_vector_load_into_slot(l, l_slot);
            self.vector_temp_base(l_slot, vec_size)
        } else {
            self.gen_expr(l, Self::EVAL_BASE);
            let t_off = self.vector_temp_base(l_slot, 16);
            self.emit_store(0, t_off, 8); // [r13+t_off] = rax (low)
                                          // 标量可能只有 rax (如 int 常量), 需确保 rdx=0
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
            self.emit_store(2, t_off + 8, 8); // [r13+t_off+8] = rdx (high)
            t_off
        };

        let r_off: i64 = if r_is_vec {
            self.gen_vector_load_into_slot(r, r_slot);
            self.vector_temp_base(r_slot, vec_size)
        } else {
            self.gen_expr(r, Self::EVAL_BASE);
            let t_off = self.vector_temp_base(r_slot, 16);
            self.emit_store(0, t_off, 8);
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
            self.emit_store(2, t_off + 8, 8);
            t_off
        };

        for i in 0..count {
            // 逐元素 stride: 每个 __int128 元素占 elem_size 字节
            let eo = (i as i64) * (elem_size as i64);
            // 加载操作数到 GP 寄存器对:
            // rax:rdx = left (low:high), rcx:r8 = right (low:high)
            self.load_stack(0, l_off + eo); // rax = left low 64-bit
            self.load_stack(2, l_off + eo + 8); // rdx = left high 64-bit
            self.load_stack(1, r_off + eo); // rcx = right low 64-bit
            self.load_stack(8, r_off + eo + 8); // r8 = right high 64-bit

            match op {
                // === 比较操作 (EqEq/Ne): 顺序 setcc + AND, 无需跳转 ===
                BinOp::EqEq | BinOp::Ne => {
                    // 比较低 64-bit
                    self.emit_insn(&Insn::alu_rm_reg_reg(
                        crate::codegen::x86_64::opcode::AluOp::Cmp,
                        crate::codegen::shared::asm_operand::Width::B64,
                        1,
                        0,
                    )); // cmp rax, rcx
                    self.emit_insn(&Insn::setcc_reg(
                        crate::codegen::x86_64::opcode::CondCode::Eq,
                        0,
                    )); // sete al (al=1 if low equal)
                        // 比较高 64-bit
                    self.emit_alu_rm_reg64(AluOp::Cmp, 2, 8); // cmp rdx, r8
                    self.emit_insn(&Insn::setcc_reg(
                        crate::codegen::x86_64::opcode::CondCode::Eq,
                        4,
                    )); // sete ah (ah=1 if high equal)
                        // AND 两个结果: al &= ah (都为1 → 两 chunk 都相等)
                        // Intel: and al, ah → opcode 0x20 (and r/m8, r8), modrm=0xE0 (mod=11, reg=ah=100, r/m=al=000)
                        // 必须无 REX: 有 REX 时字段 4 会被重映射为 spl 而非 ah.
                        // 保持基线 raw_bytes 以避免 AluRm B8 编码器对 src>=4 强发 REX.
                    self.emit_insn(&Insn::raw_bytes(&[0x20, 0xE0]));
                    // 零扩展到 rax
                    self.emit_insn(&Insn::new(
                        crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                            rip_relative: false,
                            sign: false,
                            from: crate::codegen::shared::asm_operand::Width::B8,
                            to: crate::codegen::shared::asm_operand::Width::B64,
                            dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                0,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                            src: crate::codegen::x86_64::opcode::Operand::Reg(
                                0,
                                crate::codegen::shared::asm_operand::Width::B8,
                            ),
                        },
                        &[],
                    )); // movzbq rax, al
                        // rax = 1 (两 chunk 都相等 → a==b) 或 0 (至少一个不等 → a!=b)

                    if matches!(op, BinOp::Ne) {
                        // 取反: rax = !rax (0→1, 1→0)
                        self.emit_insn(&Insn::new(
                            crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                                op: crate::codegen::x86_64::opcode::AluOp::Xor,
                                width: crate::codegen::shared::asm_operand::Width::B64,
                                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                    0,
                                    crate::codegen::shared::asm_operand::Width::B64,
                                ),
                                src: crate::codegen::x86_64::opcode::Operand::Imm(1),
                            },
                            &[],
                        )); // xor rax, 1
                    }

                    // neg rax → 0/1 映射到 0/全1 (向量真值)
                    self.emit_insn(&Insn::unary_rm_reg(
                        crate::codegen::x86_64::opcode::UnaryOp::Neg,
                        crate::codegen::shared::asm_operand::Width::B64,
                        0,
                    )); // neg rax
                        // 复制 rax 到 rdx (让两个 64-bit 半部分都是相同值)
                    self.emit_insn(&Insn::new(
                        crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                            rip_relative: false,
                            width: crate::codegen::shared::asm_operand::Width::B64,
                            dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                2,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                            src: crate::codegen::x86_64::opcode::Operand::Reg(
                                0,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                        },
                        &[],
                    )); // mov rdx, rax
                        // 结果在 rax:rdx → 存回
                    self.store_stack(0, result_off + eo);
                    self.store_stack(2, result_off + eo + 8);
                },
                // === 按位运算: 逐 chunk GP 运算, 结果在 rcx (low), r8 (high) ===
                BinOp::And => {
                    self.emit_alu_rm_reg64(AluOp::And, 1, 0); // and rcx, rax (low &= left)
                    self.emit_alu_rm_reg64(AluOp::And, 8, 2); // and r8, rdx (high &= left)
                                                              // mov rax, rcx; mov rdx, r8
                    self.emit_insn(&Insn::new(
                        crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                            rip_relative: false,
                            width: crate::codegen::shared::asm_operand::Width::B64,
                            dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                0,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                            src: crate::codegen::x86_64::opcode::Operand::Reg(
                                1,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                        },
                        &[],
                    ));
                    self.emit_insn(&Insn::mov_rm_reg(
                        crate::codegen::shared::asm_operand::Width::B64,
                        2,
                        8,
                    ));
                    self.store_stack(0, result_off + eo);
                    self.store_stack(2, result_off + eo + 8);
                },
                BinOp::Or => {
                    self.emit_alu_rm_reg64(AluOp::Or, 1, 0); // or rcx, rax
                    self.emit_alu_rm_reg64(AluOp::Or, 8, 2); // or r8, rdx
                    self.emit_insn(&Insn::new(
                        crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                            rip_relative: false,
                            width: crate::codegen::shared::asm_operand::Width::B64,
                            dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                0,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                            src: crate::codegen::x86_64::opcode::Operand::Reg(
                                1,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                        },
                        &[],
                    ));
                    self.emit_insn(&Insn::mov_rm_reg(
                        crate::codegen::shared::asm_operand::Width::B64,
                        2,
                        8,
                    ));
                    self.store_stack(0, result_off + eo);
                    self.store_stack(2, result_off + eo + 8);
                },
                BinOp::Xor => {
                    self.emit_alu_rm_reg64(AluOp::Xor, 1, 0); // xor rcx, rax
                    self.emit_alu_rm_reg64(AluOp::Xor, 8, 2); // xor r8, rdx
                    self.emit_insn(&Insn::new(
                        crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                            rip_relative: false,
                            width: crate::codegen::shared::asm_operand::Width::B64,
                            dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                0,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                            src: crate::codegen::x86_64::opcode::Operand::Reg(
                                1,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                        },
                        &[],
                    ));
                    self.emit_insn(&Insn::mov_rm_reg(
                        crate::codegen::shared::asm_operand::Width::B64,
                        2,
                        8,
                    ));
                    self.store_stack(0, result_off + eo);
                    self.store_stack(2, result_off + eo + 8);
                },
                _ => {
                    // 暂不支持, 结果槽保持未定义
                },
            }
        }

        self.vector_op_slot = saved_op_slot;
    }
}
