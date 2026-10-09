//! emit/expr/int_ops.rs —— 自历史完整实现（git 28a9063 单文件 Codegen）按职责簇恢复的模块。
//! 仅移动/可见性调整，不改语义（backend_refactor_plan §3.1 原则 1）。
#![allow(clippy::all)]
#![allow(unused_imports)]
use crate::ast::*;
use crate::codegen::shared::elf as shared_elf;
use crate::codegen::shared::elf::*;
use crate::codegen::shared::*;
use crate::codegen::x86_64::asm::*;
use crate::codegen::x86_64::insn::Insn;
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
    pub(crate) fn emit_float_to_int_saturate(&mut self, dst: u8, float_ty: &Type, is_64bit: bool) {
        // 截断转换 (cvtt*2si) 已把结果写进 `dst`；范围外与 NaN 都得到整数 indefinite
        // (INT_MIN)。这里把「正溢出」夹到 INT_MAX —— 负溢出本就该是 INT_MIN，无需改。
        //
        // 判据（不依赖任何额外 GPR 暂存，因此 `dst` 是任意号都成立，含高 8 号）：
        //   * 源值 (xmm0) >= 0 而截断结果 (dst) < 0，只可能是正溢出 ⇒ dst = INT_MAX；
        //   * 源值 < 0 而结果 == INT_MIN ⇒ 负溢出，INT_MIN 正是期望值；
        //   * NaN：ucomisd 无序比较置 CF=1，落到下一条 Jb 出口 ⇒ 保留 INT_MIN
        //     （与旧实现 setc→负支→保留 indefinite 的结果一致）；
        //   * 其余情形结果在范围内，原样保留。
        //
        // 旧实现按 `dst == 1`（%rcx）分两支并把 r8/r9/r10 当空闲暂存：P16 把右操作数
        // 暂存号参数化以后 `dst` 可以是高 8 号（实测 method_overloads.cpp：dst=%r8 走进
        // else 支，r8 既是 dst 又被当暂存 ⇒ 2.5→int 得 0x80000004 而不是 10，§11.54-1）。
        let (ucomis_op, ucomis_prefix): (u8, &[u8]) = if matches!(float_ty, Type::Float) {
            (0x2E, &[][..]) // ucomiss：单精度无 66 前缀
        } else {
            (0x2E, &[0x66][..]) // ucomisd：double / long double(内部按 double)
        };
        // xorps xmm1, xmm1；ucomis{s,d} xmm0, xmm1（ModRM 0xC1：reg=xmm0, r/m=xmm1）。
        // xmm1 是本后端浮点比较的约定 scratch（见 special/builtins.rs 同族站点）。
        self.emit_insn(&Insn::raw_bytes(&[0x0F, 0x57, 0xC9]));
        let mut ucomis = ucomis_prefix.to_vec();
        ucomis.extend_from_slice(&[0x0F, ucomis_op, 0xC1]);
        self.emit_insn(&Insn::raw_bytes(&ucomis));

        let l_done = self.fresh_label();
        // CF=1（源 < 0，或 NaN 的无序比较）⇒ 结果要么是合法负数、要么是负溢出的
        // INT_MIN（NaN 同样保留 indefinite），都不需要动 dst。
        self.jmp_rel32(&l_done, JumpKind::Jb);

        // 源 >= 0。test dst, dst：SF=1（结果为负）即正溢出（正值截断不可能为负）。
        // test r/m, r 编码 85 /r：dst 同时在 reg 与 r/m 两字段，高 8 号时 REX.R 与
        // REX.B 必须各自独立置位（与 instructions.rs::mov 同一口径）。
        let test_rex = if is_64bit { 0x48 } else { 0x40 }
            | if dst >= 8 { 0x04 | 0x01 } else { 0 };
        let modrm = 0xC0 | ((dst & 7) << 3) | (dst & 7);
        if test_rex != 0x40 {
            self.emit_insn(&Insn::raw_bytes(&[test_rex, 0x85, modrm]));
        } else {
            self.emit_insn(&Insn::raw_bytes(&[0x85, modrm]));
        }
        // 结果非负 ⇒ 在范围内，保留。
        self.jmp_rel32(&l_done, JumpKind::Jns);

        // 正溢出：dst = INT_MAX。32 位目标用 mov $imm32, r32（0xC7 /0），64 位目标
        // i64::MAX 超出 imm32，走 mov_imm 的 movabs 路径。
        if is_64bit {
            self.mov_imm(dst, i64::MAX as i128);
        } else if dst >= 8 {
            self.emit_insn(&Insn::raw_bytes(&[0x41, 0xC7, 0xC0 | (dst & 7)]));
            self.emit_insn(&Insn::raw_bytes(&i32::MAX.to_le_bytes()));
        } else {
            self.emit_insn(&Insn::raw_bytes(&[0xC7, 0xC0 | (dst & 7)]));
            self.emit_insn(&Insn::raw_bytes(&i32::MAX.to_le_bytes()));
        }
        self.label(&l_done);
    }
    pub(crate) fn emit_implicit_float_to_int(&mut self, dst: u8, float_ty: &Type, int_ty: &Type) {
        // 无符号目标: 不能沿用「有符号截断转换 + INT_MAX 饱和」—— 见
        // `emit_float_to_unsigned` 的根因说明。值通道与有符号路径同一条
        // (long double 从 x87 st(0) 落 f64 到 xmm0), 因此先跳一次再分流。
        if matches!(
            int_ty,
            Type::Bool | Type::UChar | Type::UShort | Type::UInt | Type::ULong | Type::ULongLong
        ) {
            if matches!(float_ty, Type::LongDouble) {
                self.emit_x87_to_xmm0();
            }
            self.emit_float_to_unsigned(dst, float_ty, int_ty);
            return;
        }
        let wide = matches!(int_ty, Type::Long | Type::ULong | Type::LongLong | Type::ULongLong);
        match float_ty {
            Type::Float => {
                // cvttss2si dst, xmm0 (F3 REX 0F 2C /r)
                // cvt 的 REX 只在真的扩了字段时才发：`0x40 | 0 | 0` 是什么也不扩的裸前缀，
                // 多占一字节并被 objdump 标成 `rex cvttsd2si`（多余位的唯一读数）。
                let rex = (if wide { 0x08 } else { 0 }) | (if dst >= 8 { 0x04 } else { 0 });
                if rex != 0 {
                    self.emit_insn(&Insn::raw_bytes(&[0xF3, 0x40 | rex, 0x0F, 0x2C, 0xC0 | ((dst & 7) << 3)]));
                } else {
                    self.emit_insn(&Insn::raw_bytes(&[0xF3, 0x0F, 0x2C, 0xC0 | ((dst & 7) << 3)]));
                }
            },
            Type::Double => {
                // cvttsd2si dst, xmm0 (F2 REX 0F 2C /r)
                // cvt 的 REX 只在真的扩了字段时才发：`0x40 | 0 | 0` 是什么也不扩的裸前缀，
                // 多占一字节并被 objdump 标成 `rex cvttsd2si`（多余位的唯一读数）。
                let rex = (if wide { 0x08 } else { 0 }) | (if dst >= 8 { 0x04 } else { 0 });
                if rex != 0 {
                    self.emit_insn(&Insn::raw_bytes(&[0xF2, 0x40 | rex, 0x0F, 0x2C, 0xC0 | ((dst & 7) << 3)]));
                } else {
                    self.emit_insn(&Insn::raw_bytes(&[0xF2, 0x0F, 0x2C, 0xC0 | ((dst & 7) << 3)]));
                }
            },
            Type::LongDouble => {
                // long double 的值通道是 **x87 st(0)** (80-bit 值模型, 见
                // `Codegen::float_in_st0`), 不在 xmm0 里。旧实现照搬 Double 的
                // `cvttsd2si %xmm0` —— 读的是一个从未写入的 xmm0, 于是所有
                // 「常见算术转换后隐式截断成整数」的表达式 (`int r = 2030 + 12.0L;`
                // `int q = ld;`) 静默得 0, 而显式 `(int)ld` 因为走
                // `gen_expr_cast` 的 st(0) 分流倒是正确。
                // 现在与显式转换共用同一个 Layer0 原语: 先把 st(0) 硬件舍入
                // 落成 f64 到 xmm0, 再走按 xmm0 取值的 cvttsd2si。
                self.emit_x87_to_xmm0();
                // cvttsd2si dst, xmm0 (F2 REX 0F 2C /r)
                // cvt 的 REX 只在真的扩了字段时才发：`0x40 | 0 | 0` 是什么也不扩的裸前缀，
                // 多占一字节并被 objdump 标成 `rex cvttsd2si`（多余位的唯一读数）。
                let rex = (if wide { 0x08 } else { 0 }) | (if dst >= 8 { 0x04 } else { 0 });
                if rex != 0 {
                    self.emit_insn(&Insn::raw_bytes(&[0xF2, 0x40 | rex, 0x0F, 0x2C, 0xC0 | ((dst & 7) << 3)]));
                } else {
                    self.emit_insn(&Insn::raw_bytes(&[0xF2, 0x0F, 0x2C, 0xC0 | ((dst & 7) << 3)]));
                }
            },
            _ => {},
        }
        // Saturate out-of-range results to INT_MAX/INT_MIN
        // instead of x86 "integer indefinite" value.
        self.emit_float_to_int_saturate(dst, float_ty, wide);
    }

    /// 浮点 → **无符号**整型的唯一实现 (显式转型 `cast.rs` 与初始化/赋值/实参/
    /// 返回值等隐式收敛转换都走这里)。
    ///
    /// 根因: 照带有符号目标那套 `cvttsd2si` (32 位目的地) + INT_MAX 饱和, 会把
    /// `[2^31, 2^32)` 区间当成溢出静默夹成 `2147483647` —— `unsigned u =
    /// 4000000000.0L;` 在 gcc 真值里是 `4000000000`, dcc 给 `2147483647`。
    /// 正解是**用比目标宽一档的有符号截断转换再按目标宽度截断**:
    ///   * ≤32 位目标 → 64 位 `cvttt*2si` (该区间能精确表示) → 截到低 32 位;
    ///   * 64 位目标 → 64 位 cvtt 后若为负 (原值 ≥ 2^63) 加 2^64 校正
    ///     ([`Codegen::emit_float_to_ulong`])。
    ///
    /// 调用前浮点值已在 xmm0。负源值转无符号在 C 里是 UB, 保持与旧显式路径
    /// 一致地钳到 0。
    pub(crate) fn emit_float_to_unsigned(&mut self, dst: u8, float_ty: &Type, int_ty: &Type) {
        let is_single = matches!(float_ty, Type::Float);
        if matches!(int_ty, Type::ULong | Type::ULongLong) {
            self.emit_float_to_ulong(dst, !is_single);
            return;
        }
        // cvttrunc 到 64 位有符号 (REX.W): [2^31, 2^32) 不会触发 indefinite
        let pfx = if is_single { 0xF3u8 } else { 0xF2u8 };
        let rex = 0x48 | if dst >= 8 { 0x04 } else { 0 };
        self.emit_insn(&Insn::raw_bytes(&[pfx, rex, 0x0F, 0x2C, 0xC0 | ((dst & 7) << 3)]));
        // 负值钳 0: 先把 scratch 清零 (xor 会破坏 flags), 再 test 设 SF,
        // 最后 cmovs 选 0。
        let zero_reg = if dst == 0 { 2 } else { 0 };
        self.emit_insn(&Insn::raw_bytes(&[0x48, 0x31, 0xC0 | ((zero_reg & 7) << 3) | (zero_reg & 7)]));
        let test_rex = 0x48 | if dst >= 8 { 0x05 } else { 0 };   // 位序: REX.R+B: 自反 ModRM, 同一个寄存器既在 reg 又在 r/m, 判据见 docs/ssa-pass-upgrade-plan.md §7.21
        self.emit_insn(&Insn::raw_bytes(&[test_rex, 0x85, 0xC0 | ((dst & 7) << 3) | (dst & 7)]));
        let rex_cmov = 0x48 | if dst >= 8 { 0x04 } else { 0 };
        self.emit_insn(&Insn::raw_bytes(&[
            rex_cmov,
            0x0F,
            0x48,
            0xC0 | ((dst & 7) << 3) | (zero_reg & 7),
        ]));
        self.emit_truncate_to_unsigned(dst, int_ty);
    }
    pub(crate) fn gen_compound_assign_single_eval(
        &mut self,
        target: &Expr,
        rhs: &Expr,
        binop_op: BinOp,
        dst: u8,
    ) {
        let lv_ty = target.ty();
        let lv_sz = lv_ty.size();
        // 1. 计算 lvalue 地址 → rax
        let got_addr = self.gen_lvalue_addr(target, Self::EVAL_BASE);
        if !got_addr {
            // 无法取地址, 退回普通路径 (直接走原有 Assign codegen)
            // 注意: 这里不克隆表达式, 而是让调用方走原始路径.
            // 由于我们已经匹配了 compound pattern, 但无法取地址,
            // 直接按普通赋值处理 (读取 target 值, 计算 binop, 存回).
            self.gen_expr(target, Self::EVAL_BASE); // 读取 target 当前值 → rax
            self.push(0); // 保存旧值
            self.gen_expr_decay(rhs, Self::EVAL_BASE); // RHS → rax
            self.pop(1); // rcx = 旧值 (pop to rcx, not rdx!)
                         // rax = rhs, rcx = old → binop(rax=old, rcx=rhs)
                         // 交换: rax=rcx(old), rcx=rax(rhs)
            self.emit_insn(&Insn::raw_bytes(&[0x48, 0x87, 0xc1])); // xchg rax, rcx → rax=old, rcx=rhs
            let is_signed = Self::is_signed_int(&lv_ty);
            self.binop(binop_op, 0, 1, is_signed, &lv_ty, &rhs.ty());
            if dst != 0 {
                self.mov(dst, 0);
            }
            return;
        }
        // rax = lvalue 地址
        // 2. 保存地址到栈
        self.push(0); // push rax (address)
                      // 3. 读取旧值
        match lv_sz {
            1 => { self.emit_insn(&Insn::raw_bytes(&[0x0F, 0xB6, 0x00])); }, // movzx eax, byte [rax]
            2 => {
                self.emit_insn(&Insn::raw_bytes(&[0x66, 0x8B, 0x00]));
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x0F, 0xB7, 0xC0]));
            }, // mov ax,[rax]; movzx eax,ax
            4 => { self.emit_insn(&Insn::raw_bytes(&[0x8B, 0x00])); },       // mov eax, [rax]
            8 => { self.emit_insn(&Insn::raw_bytes(&[0x48, 0x8B, 0x00])); }, // mov rax, [rax]
            _ => { self.emit_insn(&Insn::raw_bytes(&[0x48, 0x8B, 0x00])); }, // default: 8 bytes
        }
        // rax = old value
        // 4. 保存旧值, 求值 RHS
        self.push(0); // push rax (old value)
        self.expr_depth += 1;
        self.gen_expr_decay(rhs, dst); // RHS → dst (typically 0=rax)
        self.expr_depth -= 1;
        // rax = RHS value
        // 5. pop 旧值到 rcx (register 1, NOT register 2/rdx!)
        self.pop(1); // pop rcx (old value)
                     // rcx = old value, rax = RHS
                     // 6. 计算 binop: result = old_value op rhs
                     // 需要: rax = old, rcx = rhs for binop.
                     // 当前: rcx = old, rax = rhs.
                     // 交换: xchg rax, rcx → rax=old, rcx=rhs
        self.emit_insn(&Insn::raw_bytes(&[0x48, 0x87, 0xc1])); // xchg rax, rcx
                                        // Now rax = old, rcx = rhs. Compute binop(rax, rcx).
        let is_signed = Self::is_signed_int(&lv_ty);
        self.binop(binop_op, 0, 1, is_signed, &lv_ty, &rhs.ty());
        // rax = result
        // 7. 截断到目标类型宽度 (32位以下)
        if lv_sz == 4 && lv_ty.is_int() {
            self.emit_insn(&Insn::raw_bytes(&[0x89, 0xc0])); // mov eax, eax (truncate to 32 bits)
        }
        // 8. 恢复地址并存储
        self.pop(1); // pop rcx (address)
        match lv_sz {
            1 => { self.emit_insn(&Insn::raw_bytes(&[0x88, 0x01])); },       // mov [rcx], al
            2 => { self.emit_insn(&Insn::raw_bytes(&[0x66, 0x89, 0x01])); }, // mov [rcx], ax
            4 => { self.emit_insn(&Insn::raw_bytes(&[0x89, 0x01])); },       // mov [rcx], eax
            _ => { self.emit_insn(&Insn::raw_bytes(&[0x48, 0x89, 0x01])); }, // mov [rcx], rax
        }
        // 9. 结果到 dst
        if dst != 0 {
            self.mov(dst, 0);
        }
    }
    pub(crate) fn gen_int_narrow_cast(&mut self, dst: u8, target_ty: &Type) {
        let is_signed = Self::is_signed_int(target_ty) || matches!(target_ty, Type::Char);
        match target_ty {
            Type::Bool | Type::Char | Type::SChar | Type::UChar => {
                if is_signed {
                    // movsbq dst_low8, dst: 截断到 8 位并符号扩展
                    // 自反 ModRM: dst 同时在 ModRM.reg 与 r/m ⇒ >=8 必须同时置 REX.R 与 REX.B。
                    let rex = 0x48 | if dst >= 8 { 0x05 } else { 0 };   // 位序: REX.R+B: 自反 ModRM, 同一个寄存器既在 reg 又在 r/m, 判据见 docs/ssa-pass-upgrade-plan.md §7.21
                    self.emit_insn(&Insn::raw_bytes(&[rex, 0x0F, 0xBE, 0xC0 | ((dst & 7) << 3) | (dst & 7)]));
                } else {
                    // movzbl dst_low8, dst: 截断到 8 位并零扩展
                    // 同上：两个字段同一个号，只置 REX.B 会把 reg 留在低 8 号上。
                    let rex = 0x48 | if dst >= 8 { 0x05 } else { 0 };   // 位序: REX.R+B: 自反 ModRM, 同一个寄存器既在 reg 又在 r/m, 判据见 docs/ssa-pass-upgrade-plan.md §7.21
                    self.emit_insn(&Insn::raw_bytes(&[rex, 0x0F, 0xB6, 0xC0 | ((dst & 7) << 3) | (dst & 7)]));
                }
            },
            Type::Short | Type::UShort => {
                if is_signed {
                    // movswq dst_low16, dst: 截断到 16 位并符号扩展
                    // 同上：自反 ModRM 两个字段都要扩。
                    let rex = 0x48 | if dst >= 8 { 0x05 } else { 0 };   // 位序: REX.R+B: 自反 ModRM, 同一个寄存器既在 reg 又在 r/m, 判据见 docs/ssa-pass-upgrade-plan.md §7.21
                    self.emit_insn(&Insn::raw_bytes(&[rex, 0x0F, 0xBF, 0xC0 | ((dst & 7) << 3) | (dst & 7)]));
                } else {
                    // movzwl: 32 位 mov 零扩展高 32 位, 再 and 0xFFFF
                    // 自反 ModRM: dst 同时在 ModRM.reg 与 r/m ⇒ >=8 必须同时置 REX.R 与 REX.B(0x05)，
                    // 只置一边就把另一个字段留在低 8 个寄存器上（读/写的都是别人的 %eax）。
                    // <8 时一个前缀都不发：裸 0x40 什么都不扩，只多占一字节并被 objdump 标成 `rex mov`。
                    if dst >= 8 {
                        self.emit_insn(&Insn::raw_bytes(&[0x45]));
                    }
                    self.emit_insn(&Insn::raw_bytes(&[0x89, 0xC0 | ((dst & 7) << 3) | (dst & 7)]));
                    let rex2 = 0x48 | if dst >= 8 { 0x01 } else { 0 };   // 位序: REX.B: 该寄存器只在 ModRM.r/m 或 opcode 低 3 位, 判据见 docs/ssa-pass-upgrade-plan.md §7.21
                    self.emit_insn(&Insn::raw_bytes(&[rex2, 0x81, 0xE0 | (dst & 7)]));
                    self.emit_insn(&Insn::raw_bytes(&0xFFFFu32.to_le_bytes()));
                }
            },
            Type::Int | Type::UInt => {
                if !is_signed {
                    // 无符号 int: mov dst32, dst32 零扩展到 64 位
                    // 自反 ModRM: dst 同时在 ModRM.reg 与 r/m ⇒ >=8 必须同时置 REX.R 与 REX.B(0x05)，
                    // 只置一边就把另一个字段留在低 8 个寄存器上（读/写的都是别人的 %eax）。
                    // <8 时一个前缀都不发：裸 0x40 什么都不扩，只多占一字节并被 objdump 标成 `rex mov`。
                    if dst >= 8 {
                        self.emit_insn(&Insn::raw_bytes(&[0x45]));
                    }
                    self.emit_insn(&Insn::raw_bytes(&[0x89, 0xC0 | ((dst & 7) << 3) | (dst & 7)]));
                }
                // 有符号 int: 64 位寄存器低 32 位已正确, 无需操作
            },
            Type::Long | Type::ULong | Type::LongLong | Type::ULongLong if !is_signed => {
                // 无符号 long: 已经是 64 位, 无需操作
                // (64 位有符号和无符号的位模式相同)
            },
            // 有符号 long: 同上, 无需操作
            _ => {},
        }
    }
    pub(crate) fn gen_int_promote(&mut self, dst: u8, ty: &Type) {
        let rex = 0x48 | if dst >= 8 { 0x05 } else { 0 };   // 位序: REX.R+B: 自反 ModRM, 同一个寄存器既在 reg 又在 r/m, 判据见 docs/ssa-pass-upgrade-plan.md §7.21
        match ty {
            Type::Char | Type::SChar | Type::Bool => {
                // movsx dst, dst_low8: 48 0F BE C0+dst,dst
                self.emit_insn(&Insn::raw_bytes(&[rex, 0x0F, 0xBE, 0xC0 | ((dst & 7) << 3) | (dst & 7)]));
            },
            Type::Short => {
                // movsx dst, dst_low16: 48 0F BF C0+dst,dst
                self.emit_insn(&Insn::raw_bytes(&[rex, 0x0F, 0xBF, 0xC0 | ((dst & 7) << 3) | (dst & 7)]));
            },
            Type::UChar => {
                // movzx dst, dst_low8: 48 0F B6 C0+dst,dst
                self.emit_insn(&Insn::raw_bytes(&[rex, 0x0F, 0xB6, 0xC0 | ((dst & 7) << 3) | (dst & 7)]));
            },
            Type::UShort => {
                // movzx dst, dst_low16: 48 0F B7 C0+dst,dst
                self.emit_insn(&Insn::raw_bytes(&[rex, 0x0F, 0xB7, 0xC0 | ((dst & 7) << 3) | (dst & 7)]));
            },
            _ => {}, // int/long/ptr 等不需要提升
        }
    }
    pub(crate) fn zero_extend_for_cmp(&mut self, dst: u8, ty: &Type) {
        let (mask, is_32bit, is_signed) = match ty {
            Type::Bool | Type::UChar | Type::Char | Type::SChar => {
                (0xFFu32, false, matches!(ty, Type::Char | Type::SChar))
            },
            Type::UShort | Type::Short => (0xFFFFu32, false, matches!(ty, Type::Short)),
            Type::UInt | Type::Int => (0, true, matches!(ty, Type::Int)),
            _ => return, // Long/ULong/ULongLong/Ptr 已是 64 位, 无需扩展
        };
        // 有符号类型: 符号扩展已在存储时完成, 无需额外处理
        // 无符号类型: 需要零扩展清除高位符号位
        if is_signed {
            return;
        }
        if is_32bit {
            // mov dst32, dst32: 32 位写自动零扩展高 32 位
            // 自反 ModRM: dst 同时在 ModRM.reg 与 r/m ⇒ >=8 必须同时置 REX.R 与 REX.B(0x05)，
            // 只置一边就把另一个字段留在低 8 个寄存器上（读/写的都是别人的 %eax）。
            // <8 时一个前缀都不发：裸 0x40 什么都不扩，只多占一字节并被 objdump 标成 `rex mov`。
            if dst >= 8 {
                self.emit_insn(&Insn::raw_bytes(&[0x45]));
            }
            self.emit_insn(&Insn::raw_bytes(&[0x89, 0xC0 | ((dst & 7) << 3) | (dst & 7)]));
        } else {
            // and dst, mask: 清除高位
            let rex = 0x48 | if dst >= 8 { 0x01 } else { 0 };   // 位序: REX.B: 该寄存器只在 ModRM.r/m 或 opcode 低 3 位, 判据见 docs/ssa-pass-upgrade-plan.md §7.21
            self.emit_insn(&Insn::raw_bytes(&[rex, 0x81, 0xE0 | (dst & 7)]));
            self.emit_insn(&Insn::raw_bytes(&mask.to_le_bytes()));
        }
    }
}
