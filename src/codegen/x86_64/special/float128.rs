// src/codegen/x86_64/special/float128.rs
// IEEE binary128（`__float128` / `_Float128`）的 x86-64 软浮点实现。
//
// x86-64 没有任何 binary128 硬件：SSE 最宽只有 64 位双精度，x87 最宽是
// 80 位扩展精度（尾数 64 位，装不下 binary128 的 113 位尾数，指数范围也
// 更小）。所以算术、比较、与所有其它类型的转换**全部**委托 libgcc 的
// TFmode soft-fp 例程。
//
// 值通道约定（与 `float_in_st0` 一致）：
//   - binary128 的 rvalue 交付在 **xmm0 的完整 16 字节**；
//   - `long double` 的 rvalue 交付在 x87 `st(0)`；
//   - 整数 rvalue 在 rax（128 位整数低 64 在 rax、高 64 在 rdx）。
//
// SysV AMD64 ABI 的实测真值（`gcc -O0 -S`，见提交说明）：
//   - `__float128` 分类是 `[Sse, SseUp]` —— `SseUp` 是**前面那个** SSE 寄存器
//     的高 8 字节，所以一个实参只占**一个** xmm 寄存器：
//     `f(a,b,c,d)` 装 a→xmm0、b→xmm1、c→xmm2、d→xmm3。
//   - 返回值同样只在 xmm0（16 字节）。
//   - libgcc 的 TF 例程因此签名是 `__addtf3(xmm0, xmm1) → xmm0`。
//   - 例外是 XFmode（long double）：`__extendxftf2` 的实参走**栈**（16 字节），
//     `__trunctfxf2` 的结果回到 st(0)。

use crate::ast::*;
use crate::codegen::shared::asm_operand::Width;
use crate::codegen::x86_64::insn::Insn;
use crate::codegen::x86_64::opcode::{CondCode, SimdArithOp, SimdWidth};
use crate::codegen::x86_64::Codegen;

impl Codegen {
    /// 把当前 rvalue（按 `src_ty` 所在的值通道）转换成 binary128。
    ///
    /// 前置：整数在 rax（int128 低 64 在 rax、高 64 在 rdx）、float/double
    /// 在 xmm0（低 4/8 字节有效）、`long double` 在 st(0)。
    /// 后置：xmm0 = binary128，且 x87 栈为空（若入口时非空）。
    pub fn emit_to_float128(&mut self, src_ty: &Type) {
        match src_ty {
            // 已在 xmm0，无需动作。
            Type::Float128 => {},
            Type::Float => self.emit_call_extern("__extendsftf2"),
            Type::Double => self.emit_call_extern("__extenddftf2"),
            Type::LongDouble => {
                // XFmode 实参走栈：开一个 16 字节槽，把 tbyte 落进去再调用。
                // sub/add 成对，对 outgoing 参数区与嵌套求值不可见。
                self.emit_insn(&Insn::sub_rsp_imm(16));
                self.emit_x87_fstp_at(4, 0, &Type::LongDouble); // fstp tbyte (%rsp)
                self.emit_call_extern("__extendxftf2");
                self.emit_insn(&Insn::add_rsp_imm(16));
            },
            Type::Int128 | Type::UInt128 => {
                // TI 实参：rdi = 低 64 位、rsi = 高 64 位。
                self.mov(7, 0); // mov rdi, rax
                self.mov(6, 2); // mov rsi, rdx
                self.emit_call_extern(if matches!(src_ty, Type::UInt128) {
                    "__floatuntitf"
                } else {
                    "__floattitf"
                });
            },
            _ => {
                // 64 位及以下整数。libgcc 的 DI/SI 版本分别读 rdi 的 64/32 位；
                // 统一走 DI 版本可以避免 32 位实参的高半未定义问题，但调用前
                // 必须先把 rax 按目标符号性扩展到 64 位。
                let unsigned = matches!(
                    src_ty,
                    Type::Bool
                        | Type::UChar
                        | Type::UShort
                        | Type::UInt
                        | Type::Char16
                        | Type::Char32
                        | Type::ULong
                        | Type::ULongLong
                );
                let is_64 =
                    matches!(src_ty, Type::Long | Type::LongLong | Type::ULong | Type::ULongLong);
                if !is_64 {
                    if unsigned {
                        // mov %eax, %eax：32 位写自动清零高 32 位
                        self.emit_insn(&Insn::mov_rm_reg(Width::B32, 0, 0));
                    } else {
                        // movslq %eax → %rax：有符号 32 位符号扩展到 64 位
                        self.emit_insn(&Insn::mov_ext(true, Width::B32, Width::B64, 0, 0));
                    }
                }
                self.mov(7, 0); // mov rdi, rax
                self.emit_call_extern(if unsigned { "__floatunditf" } else { "__floatditf" });
            },
        }
    }

    /// 把 xmm0 里的 binary128 转换成 `dst_ty` 的 rvalue（按目标的值通道）。
    ///
    /// 前置：xmm0 = binary128。
    /// 后置：float/double → xmm0；`long double` → st(0)；整数 → rax
    ///（128 位整数低 64 在 rax、高 64 在 rdx）。
    pub fn emit_from_float128(&mut self, dst_ty: &Type) {
        match dst_ty {
            Type::Float128 => {},
            Type::Float => self.emit_call_extern("__trunctfsf2"),
            Type::Double => self.emit_call_extern("__trunctfdf2"),
            // XFmode 返回值在 st(0)。
            Type::LongDouble => self.emit_call_extern("__trunctfxf2"),
            Type::Int128 => self.emit_call_extern("__fixtfti"),
            Type::UInt128 => self.emit_call_extern("__fixunstfti"),
            Type::Long | Type::LongLong => self.emit_call_extern("__fixtfdi"),
            Type::ULong | Type::ULongLong => self.emit_call_extern("__fixunstfdi"),
            // `_Bool`：C 的语义是「值 != 0」，不是「截断取整」——0.5 截断为 0
            // 但布尔值是真。所以跟 0 比一次，而不是走 fixtfsi。
            Type::Bool => {
                self.emit_f128_zero_in_xmm1();
                self.emit_call_extern("__netf2");
                // `__netf2` 返回 0/1；`setne` 读 FLAGS，故先 test（理由同
                // `gen_float128_cmp`）。
                self.emit_insn(&Insn::raw_with_gas(&[0x85, 0xC0], "testl %eax, %eax"));
                // al = (返回值 != 0)
                self.emit_insn(&Insn::setcc_reg(CondCode::Ne, 0).with_gas("setne %al".to_string()));
                self.emit_insn(
                    &Insn::mov_ext(false, Width::B8, Width::B64, 0, 0)
                        .with_gas("movzbq %al, %rax".to_string()),
                );
            },
            _ => {
                // 32 位及以下整数：先取 32 位结果，再按符号性扩展到 64 位
                //（libgcc 返回的 SItype 只保证 eax 的低 32 位）。
                let unsigned = matches!(
                    dst_ty,
                    Type::UChar | Type::UShort | Type::UInt | Type::Char16 | Type::Char32
                );
                self.emit_call_extern(if unsigned { "__fixunstfsi" } else { "__fixtfsi" });
                if unsigned {
                    self.emit_insn(&Insn::mov_rm_reg(Width::B32, 0, 0)); // mov %eax, %eax
                } else {
                    self.emit_insn(&Insn::mov_ext(true, Width::B32, Width::B64, 0, 0));
                }
            },
        }
    }

    /// xmm1 = +0.0（binary128 的全零位型）。
    fn emit_f128_zero_in_xmm1(&mut self) {
        self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Xorpd, SimdWidth::PD, 1, 1));
    }

    /// 把 16 字节的 binary128 常量放进 `.rodata`，并把它装进 **xmm0**（完整的 16 字节）。
    ///
    /// 用**两条 8 字节指令**而不是一条 `movups`：`movups xmm0, [rip+disp32]`
    /// 的编码是 `F3 REX.W 0F 10 05 disp32`（9 字节，disp32 在 +5）—— 没有
    /// REX.W 时 `F3 0F 10` 是 **movss**（只读 4 字节，静默取到半个常量）。
    /// 而 `float_refs` 通道假定的是「指令 8 字节、disp32 在 +4」
    /// （见 `elf/resolve.rs` 的回填与 `compile_rel` 的 addend `off-4`），
    /// 9 字节的形状会让 RIP 基准与重定位位置都错位 1 字节。
    ///
    /// 拆成两条后每条都正好 8 字节、disp32 都在 +4，直接复用该通道：
    ///   `F3 0F 7E 05 disp32`  movq   xmm0, [rip+disp32]  低 8 字节（高半清零）
    ///   `66 0F 16 05 disp32`  movhpd xmm0, [rip+disp32]  高 8 字节
    ///
    /// （高半用 movhpd 而不是 movhps：`movhps` 是 `0F 16`，7 字节、disp32 在
    /// +3，与通道的 +4 约定不符；`movhpd` 是 `66 0F 16`，8 字节、disp32 在 +4。
    /// 另外 `movups` 虽只有 7 字节（disp32 在 +3），同样不合通道约定。）
    ///
    /// 返回值在 xmm0（不是 rax）—— binary128 的值通道就是 xmm0 的 16 字节。
    pub(crate) fn emit_f128_const_rip(&mut self, bytes: &[u8; 16]) {
        let off = self.rodata.len() as u64;
        self.rodata.extend_from_slice(bytes);
        // 低半
        let p_lo = self.pos();
        self.emit_insn(&Insn::raw_with_gas(
            &[0xF3, 0x0F, 0x7E, 0x05, 0, 0, 0, 0],
            "movq 0x0(%rip), %xmm0",
        ));
        self.float_refs.push((p_lo, off));
        // 高半
        let p_hi = self.pos();
        self.emit_insn(&Insn::raw_with_gas(
            &[0x66, 0x0F, 0x16, 0x05, 0, 0, 0, 0],
            "movhpd 0x0(%rip), %xmm0",
        ));
        self.float_refs.push((p_hi, off + 8));
    }

    /// 把 xmm0 里的 binary128 拆成 **rax = 位 0..63** / **rdx = 位 64..127**。
    ///
    /// binary128 在 x86-64 上是**小端内存镜像**（与 `float80::Bits` 的约定一致）：
    /// 字节 0..7 = 位 0..63，字节 8..15 = 位 64..127。于是高 64 位里：
    ///   - 位 0..47  = 尾数的高 48 位（位 64..111）
    ///   - 位 48..62 = 15 位指数（位 112..126），偏置 16383
    ///   - 位 63     = 符号位（位 127）
    ///
    /// 走内存中转（`movups` + 两次 64 位载入）而不是 `pextrq`：后者要 SSE4.1，
    /// 而 dcc 的 x86-64 基线是 SSE2。
    pub(crate) fn emit_f128_split(&mut self) {
        use crate::codegen::shared::asm_operand::Width as W;
        use crate::codegen::x86_64::opcode::MemOperand;
        use crate::codegen::x86_64::opcode::Operand;
        use crate::codegen::x86_64::opcode::X86_64Opcode;
        self.emit_insn(&Insn::sub_rsp_imm(16));
        // 注意 `simd_mov_mem` 的第一个参数是 **store**: true = xmm → 内存。
        self.emit_insn(&Insn::simd_mov_mem(true, SimdWidth::PS, 0, 0)); // movups %xmm0, (%rsp)
        for (reg, disp) in [(0u8, 0i32), (2u8, 8i32)] {
            self.emit_insn(&Insn::new(
                X86_64Opcode::MovRm {
                    rip_relative: false,
                    width: W::B64,
                    dst: Operand::Reg(reg, W::B64),
                    src: Operand::Mem(MemOperand {
                        disp,
                        base: Some(4),
                        index: None,
                        seg: None,
                    }),
                },
                &[],
            ));
        }
        self.emit_insn(&Insn::add_rsp_imm(16));
    }

    /// 由 [`Self::emit_f128_split`] 取到的 binary128 位型（rax/rdx）合成谓词结果。
    ///
    /// `kind`：0 = isnan，1 = isinf（返回 **±1**），2 = isfinite，
    /// 3 = isnormal，4 = signbit。结果在 **rax**（0/1；isinf 为 ±1）。
    ///
    /// 位型判据（bias 16383，指数 15 位，尾数 113 位）：
    ///   - isnan    = 指数 == 0x7FFF ∧ 尾数非零
    ///   - isinf    = 指数 == 0x7FFF ∧ 尾数全零（按符号返回 ±1）
    ///   - isfinite = 指数 != 0x7FFF
    ///   - isnormal = 指数 ∈ [1, 0x7FFE]
    ///   - signbit  = 位 127
    ///
    /// 尾数非零的判定：rax（位 0..63）非零，**或** rdx 的低 48 位（尾数高半）
    /// 非零。rdx 的低 48 位左移 16 位后高位仍是尾数、低 16 位补 0，因此
    /// `shl` + `test` 即可，无需 48 位立即数（imm32 装不下）。
    pub(crate) fn gen_f128_bit_predicate(&mut self, kind: u8) {
        use crate::codegen::shared::asm_operand::Width as W;
        use crate::codegen::x86_64::opcode::AluOp;
        use crate::codegen::x86_64::opcode::CondCode as CC;
        use crate::codegen::x86_64::opcode::UnaryOp;
        // r8 = 指数（rdx 的位 48..62）
        self.emit_insn(&Insn::mov_rm_reg(W::B64, 8, 2)); // mov r8, rdx
        self.emit_insn(&Insn::shift_reg_imm8_c1_b64(0b101, 8, 48)); // shr r8, 48
        self.emit_insn(&Insn::alu_rm_reg_imm32_b64(4, 8, 0x7FFF)); // and r8, 0x7FFF
        match kind {
            4 => {
                // signbit: 位 127 = rdx 的最高位
                self.emit_insn(&Insn::mov_rm_reg(W::B64, 8, 2)); // mov r8, rdx
                self.emit_insn(&Insn::shift_reg_imm8_c1_b64(0b101, 8, 63)); // shr r8, 63
                self.emit_insn(&Insn::mov_rm_reg(W::B64, 0, 8)); // mov rax, r8
                return;
            },
            2 => {
                // isfinite: 指数 != 0x7FFF
                self.emit_insn(&Insn::alu_rm_reg_imm32_b64(7, 8, 0x7FFF)); // cmp r8, 0x7FFF
                self.emit_insn(&Insn::setcc_reg(CC::Ne, 0)); // setne al
                self.emit_insn(&Insn::mov_ext(false, W::B8, W::B64, 0, 0)); // movzx rax, al
                return;
            },
            3 => {
                // isnormal: (指数-1) <= 0x7FFD ⇔ 指数 ∈ [1, 0x7FFE]
                self.emit_insn(&Insn::alu_rm_reg_imm32_b64(5, 8, 1)); // sub r8, 1
                self.emit_insn(&Insn::alu_rm_reg_imm32_b64(7, 8, 0x7FFD)); // cmp r8, 0x7FFD
                self.emit_insn(&Insn::setcc_reg(CC::Be, 0)); // setbe al
                self.emit_insn(&Insn::mov_ext(false, W::B8, W::B64, 0, 0)); // movzx rax, al
                return;
            },
            _ => {},
        }
        // isnan (0) / isinf (1): 指数全 1 时再用尾数区分。
        self.emit_insn(&Insn::mov_rm_reg(W::B64, 9, 2)); // mov r9, rdx
        self.emit_insn(&Insn::shift_reg_imm8_c1_b64(0b100, 9, 16)); // shl r9, 16
        self.emit_insn(&Insn::mov_rm_reg(W::B64, 10, 0)); // mov r10, rax
        self.emit_insn(&Insn::alu_rm_reg_reg(AluOp::Or, W::B64, 10, 9)); // or r10, r9
        self.emit_insn(&Insn::alu_rm_reg_imm32_b64(7, 8, 0x7FFF)); // cmp r8, 0x7FFF
        self.emit_insn(&Insn::setcc_reg(CC::Eq, 0)); // sete al（指数全 1）
        self.emit_insn(&Insn::test_rm_reg(W::B64, 10, 10)); // test r10, r10
        if kind == 0 {
            self.emit_insn(&Insn::setcc_reg(CC::Ne, 3)); // setne bl（尾数非零 ⇒ NaN）
        } else {
            self.emit_insn(&Insn::setcc_reg(CC::Eq, 3)); // sete bl（尾数全零 ⇒ Inf）
        }
        self.emit_insn(&Insn::alu_rm_reg_reg(AluOp::And, W::B8, 0, 3)); // and al, bl
        self.emit_insn(&Insn::mov_ext(false, W::B8, W::B32, 0, 0)); // movzx eax, al
        if kind == 1 {
            // isinf 返回 ±1: result = isinf * (1 - 2*sign)
            self.emit_insn(&Insn::mov_rm_reg(W::B64, 9, 0)); // mov r9, rax
            self.emit_insn(&Insn::unary_rm_reg(UnaryOp::Neg, W::B64, 9)); // neg r9
            self.emit_insn(&Insn::mov_rm_reg(W::B64, 10, 2)); // mov r10, rdx
            self.emit_insn(&Insn::shift_reg_imm8_c1_b64(0b101, 10, 63)); // shr r10, 63
            self.emit_insn(&Insn::test_rm_reg(W::B64, 10, 10)); // test r10, r10
            self.emit_insn(&Insn::cmovcc_reg_reg_b64(0x45, 0, 9)); // cmovne rax, r9
        }
    }

    /// 求值两个 binary128 操作数并调用 libgcc 的 TFmode 比较例程。
    ///
    /// 返回值在 **eax**，约定见 [`Self::gen_float128_cmp`] 的注释
    /// （`__lttf2`/`__letf2` 无序返回 2，`__gttf2`/`__getf2` 无序返回 -2，
    /// `__eqtf2` 相等返回 0）。
    pub(crate) fn gen_float128_cmp2(&mut self, name: &str, l: &Expr, r: &Expr) {
        let l_ty = l.ty();
        let r_ty = r.ty();
        // 左操作数 → xmm0（binary128），溢出到栈槽
        self.gen_expr(l, Self::EVAL_BASE);
        self.emit_to_float128(&l_ty);
        self.emit_insn(&Insn::sub_rsp_imm(16));
        self.emit_insn(&Insn::simd_mov_mem(true, SimdWidth::PS, 0, 0)); // movups %xmm0, (%rsp)
                                                                        // 右操作数 → xmm0（binary128）
        self.expr_depth += 1;
        self.gen_expr(r, Self::EVAL_BASE);
        self.emit_to_float128(&r_ty);
        self.expr_depth -= 1;
        // xmm1 = 右，xmm0 = 左（恢复溢出槽）
        self.emit_insn(&Insn::movapd_xmm(1, 0)); // movapd xmm1, xmm0
        self.emit_insn(&Insn::simd_mov_mem(false, SimdWidth::PS, 0, 0)); // movups (%rsp), %xmm0
        self.emit_insn(&Insn::add_rsp_imm(16));
        self.emit_call_extern(name);
    }

    /// xmm0 里的 binary128 取负：翻转**最高**符号位（位 127）。
    ///
    /// binary128 的符号位在**高** 64 位的最高位（x86-64 上是小端内存镜像），
    /// 与 double（符号位在低 64 位）不同 ⇒ 不能复用 `flip_sign_xmm`，也不能
    /// 走整数 `neg`（那是 rax 的补码取负，与浮点值无关）。
    ///
    /// 做法：把「只有位 127 为 1」的 16 字节掩码常量落 .rodata，然后
    /// `xorpd xmm0, [rip+mask]` 一次翻转。
    ///
    /// 不能用寄存器搭掩码：`movq xmm1, rax` 只把掩码放进**低** 64 位，而
    /// `punpcklqdq xmm1, xmm1` 会把它**复制到两半** ⇒ 顺手把位 63 也翻了
    /// （实测 `-b` 得到「已取负再翻一位 63」的错值）。
    pub(crate) fn emit_float128_neg(&mut self) {
        // 掩码: 字节 15 的最高位 = 位 127 = binary128 的符号位。
        let mut mask = [0u8; 16];
        mask[15] = 0x80;
        // 内容键沿用 `"f_" + hex(bytes)` 形态（与 `gen_float128_imm_load` 一致），
        // `rodata_entry_bytes` 的十六进制反推对 16 字节同样成立。
        let key = format!(
            "f_{}",
            mask.iter()
                .map(|b| format!("{:02x}", b))
                .collect::<String>()
        );
        let key_bytes = key.into_bytes();
        if !self.string_pool.contains_key(&key_bytes) {
            let o = self.rodata.len() as u64;
            self.rodata.extend_from_slice(&mask);
            self.string_pool.insert(key_bytes.clone(), o);
        }
        let off = self.string_pool[&key_bytes];
        // xorpd xmm0, [RIP + disp32]  (66 0F 57 05 disp32)
        // 8 字节指令、disp32 在字节 4 ⇒ 复用 `float_refs` 的 RIP 相对修补通道。
        //
        // 起始 pc 在发射后取（发射入口可能在本条之前插入存出，发射前拿的 pos 会偏）。
        if let Some(at) = self.emit_insn_head(&Insn::raw_with_gas(
            &[0x66, 0x0F, 0x57, 0x05, 0, 0, 0, 0],
            "xorpd 0x0(%rip), %xmm0",
        )) {
            self.float_refs.push((at, off));
        }
    }

    /// binary128 的二元算术：求值两侧 → xmm0/xmm1 → 调 libgcc。
    ///
    /// 与浮点常规路径的分工：`gen_float_binop` 只在 `float_in_st0` 为真时走
    /// x87，否则落 SSE 的 addss/addsd 通路；binary128 两条都不适用，故在这里
    /// 单独分流（由其入口调用，见 `emit/expr/float.rs`）。
    pub(crate) fn gen_float128_binop(&mut self, op: BinOp, l: &Expr, r: &Expr) {
        let name = match op {
            BinOp::Add => "__addtf3",
            BinOp::Sub => "__subtf3",
            BinOp::Mul => "__multf3",
            BinOp::Div => "__divtf3",
            _ => return,
        };
        let l_ty = l.ty();
        let r_ty = r.ty();
        // 左操作数 → xmm0（binary128），溢出到栈槽
        self.gen_expr(l, Self::EVAL_BASE);
        self.emit_to_float128(&l_ty);
        self.emit_insn(&Insn::sub_rsp_imm(16));
        self.emit_insn(&Insn::simd_mov_mem(true, SimdWidth::PS, 0, 0)); // movups %xmm0, (%rsp)
                                                                        // 右操作数 → xmm0（binary128）
        self.expr_depth += 1;
        self.gen_expr(r, Self::EVAL_BASE);
        self.emit_to_float128(&r_ty);
        self.expr_depth -= 1;
        // xmm1 = 右，xmm0 = 左（恢复溢出槽）
        self.emit_insn(&Insn::movapd_xmm(1, 0)); // movapd xmm1, xmm0
        self.emit_insn(&Insn::simd_mov_mem(false, SimdWidth::PS, 0, 0)); // movups (%rsp), %xmm0
        self.emit_insn(&Insn::add_rsp_imm(16));
        self.emit_call_extern(name);
    }

    /// binary128 的比较：`__eqtf2` / `__lttf2` / `__letf2` / `__gttf2` /
    /// `__getf2` 的返回值约定（实测，见提交说明）：
    ///   - `__eqtf2` / `__netf2`：相等返回 0，否则返回 1（含 NaN）
    ///   - `__lttf2` / `__letf2`：a<b → -1，a==b → 0，a>b → 1，无序 → **2**
    ///   - `__gttf2` / `__getf2`：a<b → -1，a==b → 0，a>b → 1，无序 → **-2**
    ///
    /// 于是「对返回值做一次有符号条件判断」就同时拿到了 IEEE 的 NaN 语义：
    /// 无序时 `lt/le` 得 2（>0，假）、`gt/ge` 得 -2（<0，假）。
    pub(crate) fn gen_float128_cmp(&mut self, op: BinOp, l: &Expr, r: &Expr) {
        let (name, cc, mnem): (&str, CondCode, &str) = match op {
            BinOp::EqEq => ("__eqtf2", CondCode::Eq, "sete"),
            BinOp::Ne => ("__eqtf2", CondCode::Ne, "setne"),
            BinOp::Lt => ("__lttf2", CondCode::Lt, "setl"),
            BinOp::Le => ("__letf2", CondCode::Le, "setle"),
            BinOp::Gt => ("__gttf2", CondCode::Gt, "setg"),
            BinOp::Ge => ("__getf2", CondCode::Ge, "setge"),
            _ => return,
        };
        let l_ty = l.ty();
        let r_ty = r.ty();
        self.gen_expr(l, Self::EVAL_BASE);
        self.emit_to_float128(&l_ty);
        self.emit_insn(&Insn::sub_rsp_imm(16));
        self.emit_insn(&Insn::simd_mov_mem(true, SimdWidth::PS, 0, 0)); // movups %xmm0, (%rsp)
        self.expr_depth += 1;
        self.gen_expr(r, Self::EVAL_BASE);
        self.emit_to_float128(&r_ty);
        self.expr_depth -= 1;
        self.emit_insn(&Insn::movapd_xmm(1, 0)); // movapd xmm1, xmm0
        self.emit_insn(&Insn::simd_mov_mem(false, SimdWidth::PS, 0, 0)); // movups (%rsp), %xmm0
        self.emit_insn(&Insn::add_rsp_imm(16));
        self.emit_call_extern(name);
        // 返回值在 eax。**必须**先 test 再 setcc：`setcc` 读的是 FLAGS，
        // 而 `call` 之后 FLAGS 是 callee 最后一条指令留下的（与返回值无关）。
        // 漏掉 test 时 `Eq`/`Ne` 会碰巧正确（soft-fp 的 `__eqtf2` 尾部恰好
        // 按返回值置了 ZF），但 `Lt`/`Ge`/`Gt`/`Le` 依赖 SF/OF，全是错的
        // —— 实测 `2.0q < 3.5q` 判成 0。
        self.emit_insn(&Insn::raw_with_gas(&[0x85, 0xC0], "testl %eax, %eax"));
        // 按上面约定的有符号语义 setcc，再零扩展到 rax。
        self.emit_insn(&Insn::setcc_reg(cc, 0).with_gas(format!("{mnem} %al")));
        self.emit_insn(
            &Insn::mov_ext(false, Width::B8, Width::B64, 0, 0)
                .with_gas("movzbq %al, %rax".to_string()),
        );
    }

    /// 一元取负：`__negtf2(xmm0) → xmm0`（翻转符号位，NaN 也照翻）。
    pub(crate) fn gen_float128_neg(&mut self, e: &Expr) {
        let ty = e.ty();
        self.gen_expr(e, Self::EVAL_BASE);
        self.emit_to_float128(&ty);
        self.emit_call_extern("__negtf2");
    }
}
