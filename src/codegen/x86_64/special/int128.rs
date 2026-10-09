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
use crate::codegen::x86_64::insn::Insn;
use crate::codegen::x86_64::{Codegen, JumpKind};

impl Codegen {
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
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x31, 0xD2])); // unsigned zero extension
            } else {
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x99])); // signed extension
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
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x31, 0xD2]));
            } else {
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x99]));
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
                self.emit_insn(&Insn::raw_bytes(&[0x4C, 0x01, 0xC9])); // add rcx, r9
                                                // adc r8, r11 (high): REX.W+R(r11)+B(r8) = 0x4D
                self.emit_insn(&Insn::raw_bytes(&[0x4D, 0x11, 0xD8])); // adc r8, r11
                self.mov(0, 1); // rax = result_low
                self.mov(2, 8); // rdx = result_high
            },
            BinOp::Sub => {
                // sub rcx, r9 (low): REX.W+R(r9) = 0x4C
                self.emit_insn(&Insn::raw_bytes(&[0x4C, 0x29, 0xC9])); // sub rcx, r9
                                                // sbb r8, r11 (high): REX.W+R(r11)+B(r8) = 0x4D
                self.emit_insn(&Insn::raw_bytes(&[0x4D, 0x19, 0xD8])); // sbb r8, r11
                self.mov(0, 1);
                self.mov(2, 8);
            },
            BinOp::And => {
                self.emit_insn(&Insn::raw_bytes(&[0x4C, 0x21, 0xC9])); // and rcx, r9
                self.emit_insn(&Insn::raw_bytes(&[0x4D, 0x21, 0xD8])); // and r8, r11
                self.mov(0, 1);
                self.mov(2, 8);
            },
            BinOp::Or => {
                self.emit_insn(&Insn::raw_bytes(&[0x4C, 0x09, 0xC9])); // or rcx, r9
                self.emit_insn(&Insn::raw_bytes(&[0x4D, 0x09, 0xD8])); // or r8, r11
                self.mov(0, 1);
                self.mov(2, 8);
            },
            BinOp::Xor => {
                self.emit_insn(&Insn::raw_bytes(&[0x4C, 0x31, 0xC9])); // xor rcx, r9
                self.emit_insn(&Insn::raw_bytes(&[0x4D, 0x31, 0xD8])); // xor r8, r11
                self.mov(0, 1);
                self.mov(2, 8);
            },
            BinOp::Mul => {
                // 低 128 位 = a0*b0 + ((a0*b1 + a1*b0) << 64)。
                self.mov(0, 1); // rax = a0
                self.emit_insn(&Insn::raw_bytes(&[0x49, 0xF7, 0xE1])); // mul r9
                self.mov(14, 0); // 保存低 64 位
                self.mov(10, 2); // r10 = a0*b0 高位
                self.mov(0, 1);
                self.emit_insn(&Insn::raw_bytes(&[0x49, 0xF7, 0xE3])); // mul r11
                self.emit_insn(&Insn::raw_bytes(&[0x49, 0x01, 0xC2])); // add r10, rax
                self.mov(0, 8);
                self.emit_insn(&Insn::raw_bytes(&[0x49, 0xF7, 0xE1])); // mul r9
                self.emit_insn(&Insn::raw_bytes(&[0x49, 0x01, 0xC2])); // add r10, rax
                self.mov(0, 14);
                self.mov(2, 10);
            },
            BinOp::Div | BinOp::Mod => {
                let signed = !matches!(l.ty(), Type::UInt128) && !matches!(r.ty(), Type::UInt128);
                self.gen_int128_divmod(signed, matches!(op, BinOp::Mod));
            },
            BinOp::EqEq => {
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x31, 0xC0])); // xor rax, rax
                self.emit_insn(&Insn::raw_bytes(&[0x4C, 0x39, 0xC9])); // cmp rcx, r9 (low)
                let lne = self.fresh_label();
                self.jmp_rel32(&lne, JumpKind::Jne);
                self.emit_insn(&Insn::raw_bytes(&[0x4D, 0x39, 0xD8])); // cmp r8, r11 (high)
                self.jmp_rel32(&lne, JumpKind::Jne);
                self.mov_imm(0, 1);
                self.label(&lne);
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x31, 0xD2])); // xor rdx, rdx
            },
            BinOp::Ne => {
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x31, 0xC0])); // xor rax, rax
                self.mov_imm(0, 1);
                let leq = self.fresh_label();
                self.emit_insn(&Insn::raw_bytes(&[0x4C, 0x39, 0xC9])); // cmp rcx, r9 (low)
                self.jmp_rel32(&leq, JumpKind::Jne);
                self.emit_insn(&Insn::raw_bytes(&[0x4D, 0x39, 0xD8])); // cmp r8, r11 (high)
                self.jmp_rel32(&leq, JumpKind::Jne);
                self.mov_imm(0, 0);
                self.label(&leq);
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x31, 0xD2])); // xor rdx, rdx
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
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x81, 0xF9]));
                self.emit_insn(&Insn::raw_bytes(&128u32.to_le_bytes())); // cmp rcx, 128
                self.jmp_rel32(&l_under_128, JumpKind::Jl);
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x31, 0xC0, 0x48, 0x31, 0xD2]));
                self.jmp_rel32(&l_end, JumpKind::Jmp);
                self.label(&l_under_128);
                // cmp rcx, 64
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x83, 0xF9, 64])); // cmp rcx, 64
                let l_small = self.fresh_label();
                self.jmp_rel32(&l_small, JumpKind::Jl);
                // >= 64: rdx = rax << (cl-64), rax = 0
                self.mov(8, 0); // r8 = rax = left_low (保存)
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x31, 0xC0])); // xor rax, rax (low = 0)
                self.mov(2, 8); // rdx = r8 = left_low
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0xD3, 0xE2])); // shl rdx, cl
                self.jmp_rel32(&l_end, JumpKind::Jmp);
                // < 64: shld rdx, rax, cl; shl rax, cl
                self.label(&l_small);
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x0F, 0xA5, 0xC2])); // shld rdx, rax, cl
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0xD3, 0xE0])); // shl rax, cl
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
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x81, 0xF9]));
                self.emit_insn(&Insn::raw_bytes(&128u32.to_le_bytes())); // cmp rcx, 128
                self.jmp_rel32(&l_under_128, JumpKind::Jl);
                if signed {
                    self.mov(0, 2);
                    self.emit_insn(&Insn::raw_bytes(&[0x48, 0xC1, 0xF8, 63])); // sar rax, 63
                    self.mov(2, 0);
                } else {
                    self.emit_insn(&Insn::raw_bytes(&[0x48, 0x31, 0xC0, 0x48, 0x31, 0xD2]));
                }
                self.jmp_rel32(&l_end, JumpKind::Jmp);
                self.label(&l_under_128);
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x83, 0xF9, 64])); // cmp rcx, 64
                let l_small = self.fresh_label();
                self.jmp_rel32(&l_small, JumpKind::Jl);
                // >= 64: rax = high >> (cl-64), high = sign/zero fill
                self.mov(8, 2); // r8 = rdx = left_high (保存)
                self.mov(0, 8); // rax = r8 = left_high
                if signed {
                    self.emit_insn(&Insn::raw_bytes(&[0x48, 0xD3, 0xF8])); // sar rax, cl
                    self.mov(2, 8);
                    self.emit_insn(&Insn::raw_bytes(&[0x48, 0xC1, 0xFA, 63])); // sar rdx, 63
                } else {
                    self.emit_insn(&Insn::raw_bytes(&[0x48, 0xD3, 0xE8])); // shr rax, cl
                    self.emit_insn(&Insn::raw_bytes(&[0x48, 0x31, 0xD2])); // high = 0
                }
                self.jmp_rel32(&l_end, JumpKind::Jmp);
                // < 64: shrd rax, rdx, cl; shift high with sign/zero fill
                self.label(&l_small);
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x0F, 0xAD, 0xD0])); // shrd rax, rdx, cl
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0xD3, if signed { 0xFA } else { 0xEA }]));
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
        self.emit_insn(&Insn::raw_bytes(&[0x31, 0xF6])); // xor esi, esi
        self.emit_insn(&Insn::raw_bytes(&[0x31, 0xFF])); // xor edi, edi
        if signed {
            let lhs_positive = self.fresh_label();
            self.emit_insn(&Insn::raw_bytes(&[0x4D, 0x85, 0xC0])); // test r8, r8
            self.jmp_rel32(&lhs_positive, JumpKind::Jns);
            self.emit_insn(&Insn::raw_bytes(&[0x48, 0xF7, 0xD9])); // neg rcx
            self.emit_insn(&Insn::raw_bytes(&[0x49, 0x83, 0xD0, 0x00])); // adc r8, 0
            self.emit_insn(&Insn::raw_bytes(&[0x49, 0xF7, 0xD8])); // neg r8
            self.emit_insn(&Insn::raw_bytes(&[0x83, 0xF6, 0x01])); // xor esi, 1
            self.emit_insn(&Insn::raw_bytes(&[0xBF, 1, 0, 0, 0])); // mov edi, 1
            self.label(&lhs_positive);

            let rhs_positive = self.fresh_label();
            self.emit_insn(&Insn::raw_bytes(&[0x4D, 0x85, 0xDB])); // test r11, r11
            self.jmp_rel32(&rhs_positive, JumpKind::Jns);
            self.emit_insn(&Insn::raw_bytes(&[0x49, 0xF7, 0xD9])); // neg r9
            self.emit_insn(&Insn::raw_bytes(&[0x49, 0x83, 0xD3, 0x00])); // adc r11, 0
            self.emit_insn(&Insn::raw_bytes(&[0x49, 0xF7, 0xDB])); // neg r11
            self.emit_insn(&Insn::raw_bytes(&[0x83, 0xF6, 0x01])); // xor esi, 1
            self.label(&rhs_positive);
        }

        // C 对零除未定义；用 ud2 明确终止，绝不制造数值 0。
        self.mov(10, 9);
        self.emit_insn(&Insn::raw_bytes(&[0x4D, 0x09, 0xDA])); // or r10, r11
        let divisor_ok = self.fresh_label();
        self.emit_insn(&Insn::raw_bytes(&[0x4D, 0x85, 0xD2])); // test r10, r10
        self.jmp_rel32(&divisor_ok, JumpKind::Jne);
        self.emit_insn(&Insn::raw_bytes(&[0x0F, 0x0B])); // ud2
        self.label(&divisor_ok);

        self.mov(0, 1); // quotient/dividend low
        self.mov(2, 8); // quotient/dividend high
        self.emit_insn(&Insn::raw_bytes(&[0x4D, 0x31, 0xF6])); // xor r14, r14 (remainder low)
        self.emit_insn(&Insn::raw_bytes(&[0x4D, 0x31, 0xFF])); // xor r15, r15 (remainder high)
        self.emit_insn(&Insn::raw_bytes(&[0x41, 0xBA]));
        self.emit_insn(&Insn::raw_bytes(&128u32.to_le_bytes())); // mov r10d, 128
        let loop_label = self.fresh_label();
        let subtract = self.fresh_label();
        let next = self.fresh_label();
        self.label(&loop_label);
        self.emit_insn(&Insn::raw_bytes(&[0x48, 0xD1, 0xE0])); // shl rax, 1
        self.emit_insn(&Insn::raw_bytes(&[0x48, 0xD1, 0xD2])); // rcl rdx, 1
        self.emit_insn(&Insn::raw_bytes(&[0x49, 0xD1, 0xD6])); // rcl r14, 1
        self.emit_insn(&Insn::raw_bytes(&[0x49, 0xD1, 0xD7])); // rcl r15, 1
        self.emit_insn(&Insn::raw_bytes(&[0x4D, 0x39, 0xDF])); // cmp r15, r11
        self.jmp_rel32(&next, JumpKind::Jb);
        self.jmp_rel32(&subtract, JumpKind::Ja);
        self.emit_insn(&Insn::raw_bytes(&[0x4D, 0x39, 0xCE])); // cmp r14, r9
        self.jmp_rel32(&next, JumpKind::Jb);
        self.label(&subtract);
        self.emit_insn(&Insn::raw_bytes(&[0x4D, 0x29, 0xCE])); // sub r14, r9
        self.emit_insn(&Insn::raw_bytes(&[0x4D, 0x19, 0xDF])); // sbb r15, r11
        self.emit_insn(&Insn::raw_bytes(&[0x48, 0x83, 0xC8, 0x01])); // or rax, 1
        self.label(&next);
        self.emit_insn(&Insn::raw_bytes(&[0x49, 0xFF, 0xCA])); // dec r10
        self.jmp_rel32(&loop_label, JumpKind::Jne);

        if want_remainder {
            self.mov(0, 14);
            self.mov(2, 15);
            if signed {
                let positive = self.fresh_label();
                self.emit_insn(&Insn::raw_bytes(&[0x85, 0xFF])); // test edi, edi
                self.jmp_rel32(&positive, JumpKind::Je);
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0xF7, 0xD8])); // neg rax
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x83, 0xD2, 0x00])); // adc rdx, 0
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0xF7, 0xDA])); // neg rdx
                self.label(&positive);
            }
        } else if signed {
            let positive = self.fresh_label();
            self.emit_insn(&Insn::raw_bytes(&[0x85, 0xF6])); // test esi, esi
            self.jmp_rel32(&positive, JumpKind::Je);
            self.emit_insn(&Insn::raw_bytes(&[0x48, 0xF7, 0xD8])); // neg rax
            self.emit_insn(&Insn::raw_bytes(&[0x48, 0x83, 0xD2, 0x00])); // adc rdx, 0
            self.emit_insn(&Insn::raw_bytes(&[0x48, 0xF7, 0xDA])); // neg rdx
            self.label(&positive);
        }
    }

    // P1-4: __int128 有序比较 (Lt/Le/Gt/Ge)
    // 输入: rcx=left_low, r8=left_high, r9=right_low, r11=right_high
    // 输出: rax = 0 或 1, rdx = 0
    pub fn gen_int128_cmp_ordering(&mut self, op: BinOp, is_signed: bool) {
        let l_end = self.fresh_label();
        let l_high = self.fresh_label();
        self.emit_insn(&Insn::raw_bytes(&[0x48, 0x31, 0xC0])); // xor rax, rax
                                        // 比较高 64 位: cmp r8, r11
        self.emit_insn(&Insn::raw_bytes(&[0x4D, 0x39, 0xD8])); // cmp r8, r11
        let (cc_high, cc_low_eq) = match op {
            BinOp::Lt if is_signed => (0x9C, 0x92),
            BinOp::Le if is_signed => (0x9C, 0x96),
            BinOp::Gt if is_signed => (0x9F, 0x97),
            BinOp::Ge if is_signed => (0x9F, 0x93),
            BinOp::Lt => (0x92, 0x92),
            BinOp::Le => (0x92, 0x96),
            BinOp::Gt => (0x97, 0x97),
            BinOp::Ge => (0x97, 0x93),
            _ => unreachable!(),
        };
        // 高半部不等时直接使用高半部比较结果。
        self.jmp_rel32(&l_high, JumpKind::Jne);
        // 高半部相等: 比较低半部 cmp rcx, r9
        self.emit_insn(&Insn::raw_bytes(&[0x4C, 0x39, 0xC9])); // cmp rcx, r9
        self.emit_insn(&Insn::raw_bytes(&[0x0F, cc_low_eq, 0xC0])); // setcc al
        self.jmp_rel32(&l_end, JumpKind::Jmp);
        // 高半部不等: 使用高半部比较结果
        self.label(&l_high);
        self.emit_insn(&Insn::raw_bytes(&[0x0F, cc_high, 0xC0])); // setcc al
        self.label(&l_end);
        self.emit_insn(&Insn::raw_bytes(&[0x48, 0x31, 0xD2])); // xor rdx, rdx
    }

    // P1-4: __int128 加载变量到 rax:rdx (低:高)
    pub fn gen_int128_load(&mut self, name: &str) {
        if self.is_global_var(name) {
            self.emit_global_addr(11, name); // r11 = &global (resolve_global_key inside)
            self.emit_insn(&Insn::raw_bytes(&[0x49, 0x8B, 0x03])); // rax = [r11]
            self.emit_insn(&Insn::raw_bytes(&[0x49, 0x8B, 0x53, 0x08])); // rdx = [r11+8]
        } else {
            let off = self.var_offset(name);
            // rax = [r13 + off] (low)
            self.load_stack(0, off);
            // rdx = [r13 + off + 8] (high)
            self.load_stack(2, off + 8);
        }
    }

    // P1-4: __int128 从 rax:rdx 存储到变量
    pub fn gen_int128_store(&mut self, name: &str) {
        if self.is_global_var(name) {
            self.emit_global_addr(11, name);
            self.emit_insn(&Insn::raw_bytes(&[0x49, 0x89, 0x03])); // [r11] = rax
            self.emit_insn(&Insn::raw_bytes(&[0x49, 0x89, 0x53, 0x08])); // [r11+8] = rdx
        } else {
            let off = self.var_offset(name);
            self.store_stack(0, off); // low → [r13+off]
            self.store_stack(2, off + 8); // high → [r13+off+8]
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
            self.emit_insn(&Insn::raw_bytes(&[0x48, 0x31, 0xD2])); // xor rdx, rdx
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
            self.emit_insn(&Insn::raw_bytes(&[0x48, 0x31, 0xD2])); // xor rdx, rdx
            self.emit_store(2, t_off + 8, 8);
            t_off
        };

        for _i in 0..count {
            // 加载操作数到 GP 寄存器对:
            // rax:rdx = left (low:high), rcx:r8 = right (low:high)
            self.load_stack(0, l_off); // rax = left low 64-bit
            self.load_stack(2, l_off + 8); // rdx = left high 64-bit
            self.load_stack(1, r_off); // rcx = right low 64-bit
            self.load_stack(8, r_off + 8); // r8 = right high 64-bit

            match op {
                // === 比较操作 (EqEq/Ne): 顺序 setcc + AND, 无需跳转 ===
                BinOp::EqEq | BinOp::Ne => {
                    // 比较低 64-bit
                    self.emit_insn(&Insn::raw_bytes(&[0x48, 0x39, 0xC1])); // cmp rax, rcx
                    self.emit_insn(&Insn::raw_bytes(&[0x0F, 0x94, 0xC0])); // sete al (al=1 if low equal)
                                                    // 比较高 64-bit
                    self.emit_insn(&Insn::raw_bytes(&[0x4C, 0x39, 0xC2])); // cmp rdx, r8
                    self.emit_insn(&Insn::raw_bytes(&[0x0F, 0x94, 0xC4])); // sete ah (ah=1 if high equal)
                                                    // AND 两个结果: al &= ah (都为1 → 两 chunk 都相等)
                                                    // Intel: and al, ah → opcode 0x20 (and r/m8, r8), modrm=0xE0 (mod=11, reg=ah=100, r/m=al=000)
                    self.emit_insn(&Insn::raw_bytes(&[0x20, 0xE0]));
                    // 零扩展到 rax
                    self.emit_insn(&Insn::raw_bytes(&[0x48, 0x0F, 0xB6, 0xC0])); // movzbq rax, al
                                                          // rax = 1 (两 chunk 都相等 → a==b) 或 0 (至少一个不等 → a!=b)

                    if matches!(op, BinOp::Ne) {
                        // 取反: rax = !rax (0→1, 1→0)
                        self.emit_insn(&Insn::raw_bytes(&[0x48, 0x83, 0xF0, 0x01])); // xor rax, 1
                    }

                    // neg rax → 0/1 映射到 0/全1 (向量真值)
                    self.emit_insn(&Insn::raw_bytes(&[0x48, 0xF7, 0xD8])); // neg rax
                                                    // 复制 rax 到 rdx (让两个 64-bit 半部分都是相同值)
                    self.emit_insn(&Insn::raw_bytes(&[0x48, 0x89, 0xC2])); // mov rdx, rax
                                                    // 结果在 rax:rdx → 存回
                    self.store_stack(0, result_off);
                    self.store_stack(2, result_off + 8);
                },
                // === 按位运算: 逐 chunk GP 运算, 结果在 rcx (low), r8 (high) ===
                BinOp::And => {
                    self.emit_insn(&Insn::raw_bytes(&[0x48, 0x21, 0xC1])); // and rcx, rax (low &= left)
                    self.emit_insn(&Insn::raw_bytes(&[0x4C, 0x21, 0xD0])); // and r8, rdx (high &= left)
                                                    // mov rax, rcx; mov rdx, r8
                    self.emit_insn(&Insn::raw_bytes(&[0x48, 0x89, 0xC8]));
                    self.emit_insn(&Insn::raw_bytes(&[0x4C, 0x89, 0xC2]));
                    self.store_stack(0, result_off);
                    self.store_stack(2, result_off + 8);
                },
                BinOp::Or => {
                    self.emit_insn(&Insn::raw_bytes(&[0x48, 0x09, 0xC1])); // or rcx, rax
                    self.emit_insn(&Insn::raw_bytes(&[0x4C, 0x09, 0xD0])); // or r8, rdx
                    self.emit_insn(&Insn::raw_bytes(&[0x48, 0x89, 0xC8]));
                    self.emit_insn(&Insn::raw_bytes(&[0x4C, 0x89, 0xC2]));
                    self.store_stack(0, result_off);
                    self.store_stack(2, result_off + 8);
                },
                BinOp::Xor => {
                    self.emit_insn(&Insn::raw_bytes(&[0x48, 0x31, 0xC1])); // xor rcx, rax
                    self.emit_insn(&Insn::raw_bytes(&[0x4C, 0x31, 0xD0])); // xor r8, rdx
                    self.emit_insn(&Insn::raw_bytes(&[0x48, 0x89, 0xC8]));
                    self.emit_insn(&Insn::raw_bytes(&[0x4C, 0x89, 0xC2]));
                    self.store_stack(0, result_off);
                    self.store_stack(2, result_off + 8);
                },
                _ => {
                    // 暂不支持, 结果槽保持未定义
                },
            }
        }

        self.vector_op_slot = saved_op_slot;
    }
}
