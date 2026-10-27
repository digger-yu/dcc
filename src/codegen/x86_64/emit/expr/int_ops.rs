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
        // xorps xmm1, xmm1；ucomis{s,d} xmm0, xmm1（ModRM 0xC1：reg=xmm0, r/m=xmm1）。
        // xmm1 是本后端浮点比较的约定 scratch（见 special/builtins.rs 同族站点）。
        self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Xorps, SimdWidth::PS, 1, 1));
        // P11: ucomiss/ucomisd xmm1, xmm0 via SimdArith typed (ModRM C1 matches handwritten).
        let ucomis_w = if matches!(float_ty, Type::Float) {
            SimdWidth::SS
        } else {
            SimdWidth::SD
        };
        let ucomis_gas = if matches!(float_ty, Type::Float) {
            "ucomiss %xmm1, %xmm0"
        } else {
            "ucomisd %xmm1, %xmm0"
        };
        let insn = Insn::simd_arith_reg(SimdArithOp::Ucomi, ucomis_w, 0, 1)
            .with_gas(ucomis_gas.to_string());
        self.emit_insn(&insn);

        let l_done = self.fresh_label();
        // CF=1（源 < 0，或 NaN 的无序比较）⇒ 结果要么是合法负数、要么是负溢出的
        // INT_MIN（NaN 同样保留 indefinite），都不需要动 dst。
        self.jmp_rel32(&l_done, JumpKind::Jb);

        // 源 >= 0。test dst, dst：SF=1（结果为负）即正溢出（正值截断不可能为负）。
        // test r/m, r 编码 85 /r：dst 同时在 reg 与 r/m 两字段，高 8 号时 REX.R 与
        // REX.B 必须各自独立置位（与 instructions.rs::mov 同一口径）。
        // P11: test dst, dst via AluRm typed.
        let test_width = if is_64bit {
            crate::codegen::shared::asm_operand::Width::B64
        } else {
            crate::codegen::shared::asm_operand::Width::B32
        };
        let test_gas_reg = crate::codegen::x86_64::abi::regnames::att_name_of_num(
            dst,
            if is_64bit { 64 } else { 32 },
        )
        .unwrap_or_else(|| "%eax".to_string());
        let test_gas = format!("test {}, {}", test_gas_reg, test_gas_reg);
        self.emit_insn(&Insn::test_rm_reg(test_width, dst, dst).with_gas(test_gas));
        // 结果非负 ⇒ 在范围内，保留。
        self.jmp_rel32(&l_done, JumpKind::Jns);

        // 正溢出：dst = INT_MAX。32 位目标用 mov $imm32, r32（0xC7 /0），64 位目标
        // i64::MAX 超出 imm32，走 mov_imm 的 movabs 路径。
        //
        // 注意: 手写固定 C7 /0 id 形式 (不是 B8+r accumulator 形式), G10 快照
        // 依赖此字节数, 不能用 MovRm B32+Imm (走 B8+r) 替换.
        if is_64bit {
            self.mov_imm(dst, i64::MAX as i128);
        } else {
            let dst_name = crate::codegen::x86_64::abi::regnames::att_name_of_num(dst, 32)
                .unwrap_or_else(|| "%eax".to_string());
            let gas = format!("mov $2147483647, {}", dst_name);
            // P11: 复用 mov_reg_imm32_b64 的 C7 /0 编码逻辑 —— 但手写是 B32
            // (无 REX.W), 仅在 dst>=8 时发 REX.B (0x41). 用专门的 C7 构造器.
            let rex: u8 = if dst >= 8 { 0x41 } else { 0 };
            let modrm = 0xC0 | (dst & 7);
            let imm = i32::MAX.to_le_bytes();
            let mut bytes = Vec::with_capacity(7);
            if rex != 0 {
                bytes.push(rex);
            }
            bytes.push(0xC7);
            bytes.push(modrm);
            bytes.extend_from_slice(&imm);
            self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
        }
        self.label(&l_done);
    }
    pub(crate) fn emit_implicit_float_to_int(&mut self, dst: u8, float_ty: &Type, int_ty: &Type) {
        // binary128 → 整数: 值通道是 xmm0 的 16 字节, 没有 cvttss2si 这类硬件指令
        // 可用, 整条转换交给 libgcc 的 `__fixtf*` / `__fixunstf*`。`emit_from_float128`
        // 已按有符号/无符号、32/64/128 位正确路由 (含 rdx 处理), 这里统一委托,
        // 不必再区分 signed/unsigned 两条分支。`dst` 的 rax→dst 收尾由
        // `emit_from_float128` 内部完成。
        if matches!(float_ty, Type::Float128) {
            self.emit_from_float128(int_ty);
            return;
        }
        // 无符号目标: 不能沿用「有符号截断转换 + INT_MAX 饱和」—— 见
        // `emit_float_to_unsigned` 的根因说明。值通道与有符号路径同一条
        // (long double 从 x87 st(0) 落 f64 到 xmm0), 因此先跳一次再分流。
        if matches!(
            int_ty,
            Type::Bool | Type::UChar | Type::UShort | Type::UInt | Type::ULong | Type::ULongLong
        ) {
            if matches!(float_ty, Type::LongDouble)
                && !matches!(int_ty, Type::ULong | Type::ULongLong)
            {
                // 无符号 64 位走 emit_x87_long_double_to_uint64，需保持 st(0) 不变；
                // 其余无符号型值可先落到 f64 (xmm0) 再走 xmm 通道。
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
                let dst_name = crate::codegen::x86_64::abi::regnames::att_name_of_num(
                    dst,
                    if wide { 64 } else { 32 },
                )
                .unwrap_or_else(|| "%eax".to_string());
                let gas = format!("cvttss2si %xmm0, {}", dst_name);
                // P11 Bucket AL: typed cvt2sd (gpr_64=wide) — encoder now omits bare REX
                // when no field is extended, matching handwritten byte layout.
                let insn = if wide {
                    Insn::cvt2sd_reg(false, crate::codegen::x86_64::opcode::SimdWidth::SS, dst, 0)
                } else {
                    Insn::cvt2sd_reg_b32(
                        false,
                        crate::codegen::x86_64::opcode::SimdWidth::SS,
                        dst,
                        0,
                    )
                }
                .with_gas(gas);
                self.emit_insn(&insn);
            },
            Type::Double => {
                // cvttsd2si dst, xmm0 (F2 REX 0F 2C /r)
                let dst_name = crate::codegen::x86_64::abi::regnames::att_name_of_num(
                    dst,
                    if wide { 64 } else { 32 },
                )
                .unwrap_or_else(|| "%eax".to_string());
                let gas = format!("cvttsd2si %xmm0, {}", dst_name);
                let insn = if wide {
                    Insn::cvt2sd_reg(false, crate::codegen::x86_64::opcode::SimdWidth::SD, dst, 0)
                } else {
                    Insn::cvt2sd_reg_b32(
                        false,
                        crate::codegen::x86_64::opcode::SimdWidth::SD,
                        dst,
                        0,
                    )
                }
                .with_gas(gas);
                self.emit_insn(&insn);
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
                // 例外：64 位有符号同样受 80→64 舍入影响 (2^63-2 会被舍入成
                // 2^63 再饱和成 2^63-1)，改走精确的 x87 整数转换。
                if wide {
                    self.emit_x87_long_double_to_int64(dst);
                    return;
                }
                self.emit_x87_to_xmm0();
                let dst_name = crate::codegen::x86_64::abi::regnames::att_name_of_num(
                    dst,
                    if wide { 64 } else { 32 },
                )
                .unwrap_or_else(|| "%eax".to_string());
                let gas = format!("cvttsd2si %xmm0, {}", dst_name);
                let insn = if wide {
                    Insn::cvt2sd_reg(false, crate::codegen::x86_64::opcode::SimdWidth::SD, dst, 0)
                } else {
                    Insn::cvt2sd_reg_b32(
                        false,
                        crate::codegen::x86_64::opcode::SimdWidth::SD,
                        dst,
                        0,
                    )
                }
                .with_gas(gas);
                self.emit_insn(&insn);
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
        // long double 交付在 x87 st(0)：经 f64 通道 (emit_float_to_ulong) 会因
        // 80→64 舍入丢失 >= 2^52 的精度 (例如 0xFFFFFFFFFFFFFFFF 四舍五入成 2^64)，
        // 故无符号 64 位走精确的 x87 整数转换 (st(0) 信道).
        if matches!(float_ty, Type::LongDouble) && matches!(int_ty, Type::ULong | Type::ULongLong) {
            self.emit_x87_long_double_to_uint64(dst);
            return;
        }
        if matches!(int_ty, Type::ULong | Type::ULongLong) {
            self.emit_float_to_ulong(dst, !is_single);
            return;
        }
        // cvttrunc 到 64 位有符号 (REX.W): [2^31, 2^32) 不会触发 indefinite
        let width = if is_single { SimdWidth::SS } else { SimdWidth::SD };
        self.emit_insn(&Insn::cvt2sd_reg(false, width, dst, 0));
        // 负值钳 0: 先把 scratch 清零 (xor 会破坏 flags), 再 test 设 SF,
        // 最后 cmovs 选 0。
        let zero_reg = if dst == 0 { 2 } else { 0 };
        // P11: xor reg,reg via typed opcode (zero_reg is always <8 here).
        self.emit_insn(&Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                op: crate::codegen::x86_64::opcode::AluOp::Xor,
                width: crate::codegen::shared::asm_operand::Width::B64,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    zero_reg,
                    crate::codegen::shared::asm_operand::Width::B64,
                ),
                src: crate::codegen::x86_64::opcode::Operand::Reg(
                    zero_reg,
                    crate::codegen::shared::asm_operand::Width::B64,
                ),
            },
            &[],
        ));
        // P11: test dst, dst via AluRm typed (B64 自反).
        self.emit_insn(&Insn::test_rm_reg(
            crate::codegen::shared::asm_operand::Width::B64,
            dst,
            dst,
        ));
        // P11: cmovs dst, zero_reg via cmovcc_reg_reg_b64 typed.
        self.emit_insn(&Insn::cmovcc_reg_reg_b64(0x48, dst, zero_reg));
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
                         // P11 Bucket AH: typed xchg rax, rcx B64 (ModRM form = 48 87 C1).
            self.emit_insn(&Insn::xchg_reg(
                crate::codegen::shared::asm_operand::Width::B64,
                0,
                1,
                false,
            ));
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
            1 => {
                // P11: movzx eax, byte [rax] via MovExt typed.
                self.emit_insn(&Insn::mov_ext_mem(
                    false,
                    crate::codegen::shared::asm_operand::Width::B8,
                    crate::codegen::shared::asm_operand::Width::B32,
                    0,
                    0,
                    0,
                ));
            }, // movzx eax, byte [rax]
            2 => {
                // P11: mov ax,[rax] via mov_rm_mem typed (B16, load=true).
                self.emit_insn(&Insn::mov_rm_mem(
                    true,
                    crate::codegen::shared::asm_operand::Width::B16,
                    0,
                    0,
                    0,
                ));
                // P11: movzx rax,ax via mov_ext typed (B16->B64, REX.W=1 → 4 bytes, matches handwritten [0x48,0x0F,0xB7,0xC0]).
                self.emit_insn(&Insn::mov_ext(
                    false,
                    crate::codegen::shared::asm_operand::Width::B16,
                    crate::codegen::shared::asm_operand::Width::B64,
                    0,
                    0,
                ));
            }, // mov ax,[rax]; movzx rax,ax
            4 => {
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
            }, // mov eax, [rax]
            8 => {
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
            }, // mov rax, [rax]
            _ => {
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
            }, // default: 8 bytes
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
                     // P11 Bucket AH: typed xchg rax, rcx B64 (ModRM form = 48 87 C1).
        self.emit_insn(&Insn::xchg_reg(
            crate::codegen::shared::asm_operand::Width::B64,
            0,
            1,
            false,
        ));
        let is_signed = Self::is_signed_int(&lv_ty);
        self.binop(binop_op, 0, 1, is_signed, &lv_ty, &rhs.ty());
        // rax = result
        // 7. 归约到左值类型：赋值表达式的值是左值转换后的左值 (C11 6.5.16p3)。
        //    旧写法只在 `lv_sz == 4 && 有符号 int` 时发一枚 `mov eax, eax`，
        //    1/2 字节左值完全不归约（`u8 *q; long v = (*q += 300)` 实测 gcc 44 /
        //    dcc 300），且 4 字节无符号左值漏掉零扩展。
        self.reduce_assign_value_to_lvalue(0, &lv_ty);
        // 8. 恢复地址并存储
        self.pop(1); // pop rcx (address)
        match lv_sz {
            1 => {
                // P11: mov [rcx], al via mov_rm_mem typed.
                self.emit_insn(&Insn::mov_rm_mem(
                    false,
                    crate::codegen::shared::asm_operand::Width::B8,
                    0,
                    1,
                    0,
                ));
            }, // mov [rcx], al
            2 => {
                // P11: mov [rcx], ax via mov_rm_mem typed.
                self.emit_insn(&Insn::mov_rm_mem(
                    false,
                    crate::codegen::shared::asm_operand::Width::B16,
                    0,
                    1,
                    0,
                ));
            }, // mov [rcx], ax
            4 => {
                self.emit_insn(&Insn::new(
                    crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                        rip_relative: false,
                        width: crate::codegen::shared::asm_operand::Width::B32,
                        dst: crate::codegen::x86_64::opcode::Operand::Mem(
                            crate::codegen::x86_64::opcode::MemOperand {
                                disp: 0,
                                base: Some(1),
                                index: None,
                                seg: None,
                            },
                        ),
                        src: crate::codegen::x86_64::opcode::Operand::Reg(
                            0,
                            crate::codegen::shared::asm_operand::Width::B32,
                        ),
                    },
                    &[],
                ));
            }, // mov [rcx], eax
            _ => {
                self.emit_insn(&Insn::new(
                    crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                        rip_relative: false,
                        width: crate::codegen::shared::asm_operand::Width::B64,
                        dst: crate::codegen::x86_64::opcode::Operand::Mem(
                            crate::codegen::x86_64::opcode::MemOperand {
                                disp: 0,
                                base: Some(1),
                                index: None,
                                seg: None,
                            },
                        ),
                        src: crate::codegen::x86_64::opcode::Operand::Reg(
                            0,
                            crate::codegen::shared::asm_operand::Width::B64,
                        ),
                    },
                    &[],
                ));
            }, // mov [rcx], rax
        }
        // 8b. 窄类型 (1/2 字节) 的结果必须是**存回内存的截断值**: C11 6.5.16p3
        // 让复合赋值的值等于左侧对象的新值, 而 rax 里此时还是 64 位运算和
        // (250+10=260)。`if( (buf[4] += 6)==0 )` 一类的判断因此永远不成立。
        // 与 ++/-- 的内存 lvalue 路径同构: 存回后按原类型重新加载。
        if lv_sz < 4 {
            self.mov(0, 1); // rax = 目标地址 (rcx 仍握着它)
            self.emit_load_mem_typed(lv_sz, &lv_ty);
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
                    self.emit_insn(&Insn::mov_ext(
                        true,
                        crate::codegen::shared::asm_operand::Width::B8,
                        crate::codegen::shared::asm_operand::Width::B64,
                        dst,
                        dst,
                    ));
                } else {
                    // movzbq dst_low8, dst: 截断到 8 位并零扩展
                    self.emit_insn(&Insn::mov_ext(
                        false,
                        crate::codegen::shared::asm_operand::Width::B8,
                        crate::codegen::shared::asm_operand::Width::B64,
                        dst,
                        dst,
                    ));
                }
            },
            Type::Short | Type::UShort => {
                if is_signed {
                    // movswq dst_low16, dst: 截断到 16 位并符号扩展
                    self.emit_insn(&Insn::mov_ext(
                        true,
                        crate::codegen::shared::asm_operand::Width::B16,
                        crate::codegen::shared::asm_operand::Width::B64,
                        dst,
                        dst,
                    ));
                } else {
                    // movzwl: 32 位 mov 零扩展高 32 位, 再 and 0xFFFF
                    // 自反 ModRM: dst 同时在 ModRM.reg 与 r/m ⇒ >=8 必须同时置 REX.R 与 REX.B(0x05)，
                    // <8 时一个前缀都不发.
                    // P11: mov dst32, dst32 via MovRm typed.
                    self.emit_insn(&Insn::mov_rm_reg(
                        crate::codegen::shared::asm_operand::Width::B32,
                        dst,
                        dst,
                    ));
                    // P11: and dst64, 0xFFFF via AluRm typed (imm32 超出 i8 走 0x81 /id).
                    self.emit_insn(&Insn::new(
                        crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                            op: crate::codegen::x86_64::opcode::AluOp::And,
                            width: crate::codegen::shared::asm_operand::Width::B64,
                            dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                dst,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                            src: crate::codegen::x86_64::opcode::Operand::Imm(0xFFFF),
                        },
                        &[],
                    ));
                }
            },
            Type::Int | Type::UInt => {
                if !is_signed {
                    // 无符号 int: mov dst32, dst32 零扩展到 64 位
                    // P11: mov dst32, dst32 via MovRm typed.
                    self.emit_insn(&Insn::mov_rm_reg(
                        crate::codegen::shared::asm_operand::Width::B32,
                        dst,
                        dst,
                    ));
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

    /// 把留在 `reg` 里的赋值结果归约到**左值类型**的取值域。
    ///
    /// C11 6.5.16p3: 赋值表达式的值是「左值转换后的左值」，即已经带上了左值类型的
    /// 宽度与符号性；而内存左值的 store 只写低 n 字节，寄存器里留着的仍是右值在
    /// int/更宽精度下的位模式。具名变量路径早就补了这一步（`dispatch.rs` 赋值分支
    /// 里的重扩展），下标 / 解引用 / 成员 / 单次求值复合赋值四条路径都没有，实测
    /// （-O0 与 -O2 同）：
    ///   `u8 a[8]; a[3]=0; long v = (a[3] = 300);`  gcc 44 / dcc 300
    ///   `u8 a[8]; long v = (a[3] += 300);`         gcc 44 / dcc 300
    ///   `unsigned int ui[8]; long v = (ui[0] = 0x100000005ULL);`  gcc 5 / dcc 4294967301
    /// 与 `gen_int_narrow_cast` 的 `Type::Int` 分支不同，这里不能「什么都不做」：
    /// 那一支假定后续按 4 字节重新读取，而本函数的结果会原样交给更宽的使用点。
    pub(crate) fn reduce_assign_value_to_lvalue(&mut self, reg: u8, lty: &Type) {
        if !Self::is_integer_type(lty) && !matches!(lty, Type::Enum(..)) {
            // 浮点 / 复数 / 聚合 / 指针左值各有自己的值模型（值通道不同），不在此列
            return;
        }
        let signed = match lty {
            Type::Enum(_, _, Some(u)) => Self::is_signed_int(&u.base),
            Type::Bool | Type::UChar | Type::UShort | Type::UInt => false,
            _ => true,
        };
        use crate::codegen::shared::asm_operand::Width;
        // 三条自反扩展都得挂 gas（理由同下面 `mov %e<reg>,%e<reg>` 那一段）：
        // luaV_execute 里 `movsxd r9,r9d` 有 403 条、`movsxd rax,r15d` 84 条，全部来
        // 自本函数，而窥孔的幂等移动判据只认带文本的指令。
        use crate::codegen::x86_64::emit::gas::{
            REG16 as GAS_REG16, REG32 as GAS_REG32, REG64 as GAS_REG64, REG8 as GAS_REG8,
        };
        let r8 = GAS_REG8[(reg & 15) as usize];
        let r16 = GAS_REG16[(reg & 15) as usize];
        let r32 = GAS_REG32[(reg & 15) as usize];
        let r64 = GAS_REG64[(reg & 15) as usize];
        let insn = match (lty.size(), signed) {
            (1, s) => {
                let m = if s { "movsbq" } else { "movzbq" };
                Insn::mov_ext(s, Width::B8, Width::B64, reg, reg)
                    .with_gas(format!("{m} {r8}, {r64}"))
            },
            (2, s) => {
                let m = if s { "movswq" } else { "movzwq" };
                Insn::mov_ext(s, Width::B16, Width::B64, reg, reg)
                    .with_gas(format!("{m} {r16}, {r64}"))
            },
            (4, true) => Insn::mov_ext(true, Width::B32, Width::B64, reg, reg)
                .with_gas(format!("movsxd {r32}, {r64}")),
            (4, false) => {
                // mov reg32, reg32: 32 位自拷贝即零扩展到高 32 位
                //
                // gas 文本不能省：发射层窥孔的「幂等移动」判据是以 gas 文本为凭的
                // （`instructions.rs::is_idempotent_move`），这里不挂文本，相邻同字节的
                // 自反 `mov %eax,%eax` 就永远看不见（上一轮 luaV_execute 里那 2 条尾巴
                // 就是这么漏下的）。文本与字节同源于 `cast.rs` 的同形窄化写法。
                use crate::codegen::x86_64::emit::gas::REG32 as GAS_REG32;
                let gas = format!(
                    "mov {}, {}",
                    GAS_REG32[(reg & 15) as usize],
                    GAS_REG32[(reg & 15) as usize]
                );
                self.emit_insn_expect(&Insn::mov_rm_reg(Width::B32, reg, reg).with_gas(gas));
                return;
            },
            _ => return,
        };
        self.emit_insn_expect(&insn);
    }

    /// 赋值右值进内存左值之前的完整归约：先把浮点右值按左值内型截断，再按左值宽度
    /// 归约结果。内存左值路径原先两者都缺，于是 `u8 a[1]; a[0] = 300.5;` 把 double
    /// 位模式的低字节当整数写进内存（实测 dcc 读出 0x18，gcc 读出 44）。
    pub(crate) fn convert_assign_rhs_in_reg(&mut self, reg: u8, lty: &Type, rty: &Type) {
        self.convert_assign_rhs_float_to_int(reg, lty, rty);
        self.reduce_assign_value_to_lvalue(reg, lty);
    }

    /// 只为**存进内存**而做的右值转换（阶段1：值没人要时省掉宽度归约）。
    ///
    /// 能省的只有按左值宽度归约那一段：store 自己就按左值字节数截断。浮点→整数那一
    /// 段不能省：那是**值语义**的一部分（`int i = 2.7;` 存的必须是 2 而不是 2.7 的位模式
    /// 低 4 字节），与后面有没有人读这个值无关。两个判据仍只有 `convert_assign_rhs_float_to_int`
    /// 一份，不在这里重写。
    pub(crate) fn convert_assign_rhs_in_reg_for_store(&mut self, reg: u8, lty: &Type, rty: &Type) {
        self.convert_assign_rhs_float_to_int(reg, lty, rty);
    }

    /// 浮点右值 → 整数左值的那一段转换（上面两个入口共用）。
    fn convert_assign_rhs_float_to_int(&mut self, reg: u8, lty: &Type, rty: &Type) {
        if matches!(rty, Type::Float | Type::Double | Type::LongDouble)
            && (Self::is_integer_type(lty) || matches!(lty, Type::Enum(..)))
        {
            self.emit_implicit_float_to_int(reg, rty, lty);
        }
    }

    pub(crate) fn gen_int_promote(&mut self, dst: u8, ty: &Type) {
        match ty {
            Type::Char | Type::SChar | Type::Bool => {
                // movsbq dst, dst_low8
                self.emit_insn(&Insn::mov_ext(
                    true,
                    crate::codegen::shared::asm_operand::Width::B8,
                    crate::codegen::shared::asm_operand::Width::B64,
                    dst,
                    dst,
                ));
            },
            Type::Short => {
                // movswq dst, dst_low16
                self.emit_insn(&Insn::mov_ext(
                    true,
                    crate::codegen::shared::asm_operand::Width::B16,
                    crate::codegen::shared::asm_operand::Width::B64,
                    dst,
                    dst,
                ));
            },
            Type::UChar => {
                // movzbq dst, dst_low8
                self.emit_insn(&Insn::mov_ext(
                    false,
                    crate::codegen::shared::asm_operand::Width::B8,
                    crate::codegen::shared::asm_operand::Width::B64,
                    dst,
                    dst,
                ));
            },
            Type::UShort => {
                // movzwq dst, dst_low16
                self.emit_insn(&Insn::mov_ext(
                    false,
                    crate::codegen::shared::asm_operand::Width::B16,
                    crate::codegen::shared::asm_operand::Width::B64,
                    dst,
                    dst,
                ));
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
            // P11: mov dst32, dst32 via MovRm typed.
            self.emit_insn(&Insn::mov_rm_reg(
                crate::codegen::shared::asm_operand::Width::B32,
                dst,
                dst,
            ));
        } else {
            // P11: and dst64, mask via AluRm typed.
            self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                    op: crate::codegen::x86_64::opcode::AluOp::And,
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        dst,
                        crate::codegen::shared::asm_operand::Width::B64,
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Imm(mask as i64),
                },
                &[],
            ));
        }
    }
}
