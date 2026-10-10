// src/codegen/x86_64/insn.rs
//
// x86_64 后端 - 指令记录模型 (统一发射入口).
//
// 第一性原理: x86_64 指令**变长** (1~15 字节, 含 REX / ModRM / SIB / disp / imm / 前缀),
// 不能像定长 ISA (aarch64/mips/arm/riscv32) 那样用 `u32` 字面值装. 因此本模型用
// `Raw` 承接迁移期所有 `self.emit(&[..])` 调用点; P9 步骤 1 起, 常用族 (AluRm /
// MovRm / UnaryRm / Shift / Lea / Call / Jmp / Jcc / Ret / Stack / SimdArith /
// SimdMov / X87) 逐步抽为类型化构造子, 其 encode/render 真源集中在 `opcode.rs`.
//
// 迁移纪律 (G11): 纯移动, 构造子编码 == 现有字节. 不变更任何位型.

use crate::codegen::shared::asm_operand::{InsnRec, Operand};
use crate::codegen::x86_64::opcode::X86_64Opcode;

#[derive(Clone, Debug)]
pub struct Insn {
    pub op: X86_64Opcode,
    pub ops: [Operand; 4],
    /// 兼容字段: 迁移期内 Raw 变体的字节序.
    /// 类型化变体的字节由 op.encode() 现场算, 不存这里.
    pub bytes: Vec<u8>,
    /// P9 方案 C: 发射点携带的 -S GAS 渲染文本.
    ///
    /// 真源语义: 一旦某条指令在 emit_insn 时带了 gas_text, -S 渲染器就**直接**
    /// 拿这一行输出 (不含前导 tab / 换行), 不再跑 disasm 反推. 没有这个字段,
    /// 渲染器必须退回字节反推. 这就是最终删除 disasm.rs 的前置条件.
    ///
    /// 注意: 这个字段**只影响 -S 文本**, 不参与 .text 段字节生成. 所以 G10 快照
    /// (校验 .text sha256) 对它零敏感.
    pub gas_text: Option<String>,
}

impl Insn {
    pub fn new(op: X86_64Opcode, ops: &[Operand]) -> Self {
        let mut arr = [Operand::None, Operand::None, Operand::None, Operand::None];
        let n = ops.len().min(4);
        arr[..n].clone_from_slice(&ops[..n]);
        Insn {
            op,
            ops: arr,
            bytes: Vec::new(),
            gas_text: None,
        }
    }

    /// 直接用字节切片构造 (迁移期兜底, 逐步替换为类型化构造子).
    /// x86_64 变长, 因此直接保留字节切片, 不做 u32/u16 拼装.
    ///
    /// P10 收敛: 原生发射路径一律用 raw_bytes (语义等价于 raw), 保留 raw
    /// 这个名字仅供 asm/ 内联汇编编码器继续使用 (那里走 raw_with_mnemonic).
    /// 新代码不要直接调这个构造子, 用 raw_with_gas(bytes, "gas text") 携带
    /// 发射点已知的 GAS 文本, 或类型化构造子 Insn::new(X86_64Opcode::...).
    #[doc(hidden)]
    pub fn raw_bytes(b: &[u8]) -> Self {
        Insn::new(
            X86_64Opcode::Raw {
                bytes: b.to_vec(),
                mnemonic: None,
            },
            &[],
        )
    }

    /// P11 Bucket I: 无操作数/固定前缀指令 (cld/mfence/rep movsb/stosb/movsq).
    pub fn noarg(op: crate::codegen::x86_64::opcode::NoArgOp) -> Self {
        Insn::new(X86_64Opcode::NoArg { op }, &[])
    }

    /// P11 Bucket K: movd/movq GPR↔XMM reg-reg.
    /// to_xmm=true: GPR→XMM (0x6E). width=B64 → movq (66 REX.W); B32 → movd.
    pub fn gpr_xmm(
        to_xmm: bool,
        width: crate::codegen::shared::asm_operand::Width,
        gpr_idx: u8,
        xmm_idx: u8,
    ) -> Self {
        let (dst, src) = if to_xmm {
            (
                crate::codegen::x86_64::opcode::Operand::Reg(
                    xmm_idx,
                    crate::codegen::shared::asm_operand::Width::B128,
                ),
                crate::codegen::x86_64::opcode::Operand::Reg(gpr_idx, width),
            )
        } else {
            (
                crate::codegen::x86_64::opcode::Operand::Reg(gpr_idx, width),
                crate::codegen::x86_64::opcode::Operand::Reg(
                    xmm_idx,
                    crate::codegen::shared::asm_operand::Width::B128,
                ),
            )
        };
        Insn::new(
            X86_64Opcode::GprXmm { to_xmm, width, dst, src },
            &[],
        )
    }

    /// asm/ 内联汇编路径专用 (P10 起原生路径已改用 raw_bytes).
    /// 语义与 raw_bytes 一致, 仅名字不同 —— 保留以避免改动内联汇编编码器的
    /// 几百处字面调用.
    pub fn raw(b: &[u8]) -> Self {
        Insn::raw_bytes(b)
    }

    pub fn nop() -> Self {
        Insn::new(
            X86_64Opcode::Raw {
                bytes: vec![0x90],
                mnemonic: Some("nop".into()),
            },
            &[],
        )
    }

    /// P9 步骤 5: 内联汇编发射完一段字节后, 用 (bytes, mnemonic) 就地包装成 Raw。
    /// 与 raw_bytes 的差别只是 mnemonic 字段填 Some, 供 render() 出 GAS 文本。
    /// 不改变任何发射字节 —— 只影响 -S 文本渲染 (步骤 6 起读 asm_insn_log)。
    pub fn raw_with_mnemonic(bytes: &[u8], mnemonic: &str) -> Self {
        let ops = [Operand::None, Operand::None, Operand::None, Operand::None];
        Insn {
            op: X86_64Opcode::Raw {
                bytes: bytes.to_vec(),
                mnemonic: Some(mnemonic.to_string()),
            },
            ops,
            bytes: Vec::new(),
            gas_text: Some(mnemonic.to_string()),
        }
    }

    /// P9 方案 C: 发射点已经知道这条指令最终在 -S 里要渲染成哪一行 GAS 文本,
    /// 直接携带. 渲染器不再需要反推字节.
    pub fn raw_with_gas(bytes: &[u8], gas_line: &str) -> Self {
        let ops = [Operand::None, Operand::None, Operand::None, Operand::None];
        Insn {
            op: X86_64Opcode::Raw {
                bytes: bytes.to_vec(),
                mnemonic: None,
            },
            ops,
            bytes: Vec::new(),
            gas_text: Some(gas_line.to_string()),
        }
    }

    /// P11: 给已构造的类型化 Insn 附加 GAS 文本 (等价于 raw_with_gas, 但不走 Raw 兜底).
    pub fn with_gas(mut self, gas_line: String) -> Self {
        self.gas_text = Some(gas_line);
        self
    }

    /// P11: 构造 `sub rsp, imm8` (imm 可装进 i8, 走 0x83 短形式, 48 83 EC ib).
    /// 注意: 这个形态**不**走 AluRm.encode() (因为 AluRm 默认走 0x81 imm32),
    /// 直接内联手写字节以保持 G10 快照逐字节一致.
    pub fn sub_rsp_imm(imm: i64) -> Self {
        let v = imm as i8 as u8;
        Insn::raw_bytes(&[0x48, 0x83, 0xEC, v])
    }

    /// P11: 构造 `add rsp, imm8` (imm 可装进 i8, 走 0x83 短形式, 48 83 C4 ib).
    pub fn add_rsp_imm(imm: i64) -> Self {
        let v = imm as i8 as u8;
        Insn::raw_bytes(&[0x48, 0x83, 0xC4, v])
    }

    /// P11 Bucket A: mov reg, reg (mod=11). width controls REX.W (B64) + REX.B/R (reg>=8).
    /// Encoding: MOV r/m, r (0x89 for non-byte; 0x88 for B8).
    pub fn mov_rm_reg(
        width: crate::codegen::shared::asm_operand::Width,
        dst_reg: u8,
        src_reg: u8,
    ) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                width,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(dst_reg, width),
                src: crate::codegen::x86_64::opcode::Operand::Reg(src_reg, width),
            },
            &[],
        )
    }

    /// P11 Bucket A: MovRm reg <-> mem [base+disp] (load or store).
    /// load=true: mov reg, [base+disp] (0x8B / 0x8A). load=false: mov [base+disp], reg (0x89 / 0x88).
    /// width: B8/B16/B32/B64. base: Intel reg number 0..15. disp: byte displacement (sign-ext).
    pub fn mov_rm_mem(
        load: bool,
        width: crate::codegen::shared::asm_operand::Width,
        reg_idx: u8,
        base: u8,
        disp: i32,
    ) -> Self {
        use crate::codegen::x86_64::opcode::{MemOperand, Operand, X86_64Opcode};
        let reg = Operand::Reg(reg_idx, width);
        let mem = Operand::Mem(MemOperand {
            disp,
            base: Some(base),
            index: None,
            seg: None,
        });
        let (dst, src) = if load { (reg, mem) } else { (mem, reg) };
        Insn::new(X86_64Opcode::MovRm { width, dst, src }, &[])
    }

    /// P11 Bucket B: AluRm reg, reg (mod=11) — add/sub/and/or/xor/cmp.
    pub fn alu_rm_reg_reg(
        op: crate::codegen::x86_64::opcode::AluOp,
        width: crate::codegen::shared::asm_operand::Width,
        dst_reg: u8,
        src_reg: u8,
    ) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                op,
                width,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(dst_reg, width),
                src: crate::codegen::x86_64::opcode::Operand::Reg(src_reg, width),
            },
            &[],
        )
    }

    /// P11 Bucket C: AluRm reg, [base+disp] (reg <- mem) — add/sub/and/or/xor/cmp.
    /// width: B64 (REX.W) or B32. base: Intel reg number 0..15. disp: byte displacement.
    pub fn alu_rm_mem(
        op: crate::codegen::x86_64::opcode::AluOp,
        width: crate::codegen::shared::asm_operand::Width,
        dst_reg: u8,
        base: u8,
        disp: i32,
    ) -> Self {
        use crate::codegen::x86_64::opcode::{MemOperand, Operand, X86_64Opcode};
        Insn::new(
            X86_64Opcode::AluRm {
                op,
                width,
                dst: Operand::Reg(dst_reg, width),
                src: Operand::Mem(MemOperand {
                    disp,
                    base: Some(base),
                    index: None,
                    seg: None,
                }),
            },
            &[],
        )
    }

    /// P11 Bucket D: push/pop reg64 via Stack typed opcode (50+rd / 41 50+(rd&7)).
    pub fn stack_op_reg(
        op: crate::codegen::x86_64::opcode::StackOp,
        reg_idx: u8,
    ) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::Stack {
                op,
                width: crate::codegen::shared::asm_operand::Width::B64,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    reg_idx,
                    crate::codegen::shared::asm_operand::Width::B64,
                ),
            },
            &[],
        )
    }

    /// P11: 构造 B32 `and/eor/or/cmp eax|ecx|edx, imm8` (0x83 / ib, mod=11).
    /// modrm_reg 字段编码 AluOp, modrm_rm 字段编码低 3 位寄存器索引.
    /// 与 rsp 不同, 非 rsp 寄存器 0x83 形态不常带 REX.W (G10 快照逐字节约定).
    pub fn alu_rm_reg_imm8(op: u8, reg_idx: u8, imm: i64) -> Self {
        let modrm = 0xC0 | ((op & 0x7) << 3) | (reg_idx & 0x7);
        let v = imm as i8 as u8;
        Insn::raw_bytes(&[0x83, modrm, v])
    }

    /// P11 Bucket B: 构造 B64 `add/or/and/sub/xor/cmp reg, imm32` (0x81 / id 形式).
    /// 与 rsp 的 0x81 imm32 一致, 但任意 reg; 手写常拆成两条 emit_insn (opcode 三元组 + imm32).
    /// op 编码: /0=add, /1=or, /4=and, /5=sub, /6=xor, /7=cmp.
    pub fn alu_rm_reg_imm32_b64(op: u8, reg_idx: u8, imm: i64) -> Self {
        let mut rex: u8 = 0x48;
        if reg_idx >= 8 {
            rex |= 0x01;
        }
        let modrm = 0xC0 | ((op & 0x7) << 3) | (reg_idx & 0x7);
        let imm32 = (imm as i32).to_le_bytes();
        let mut out = Vec::with_capacity(7);
        out.push(rex);
        out.push(0x81);
        out.push(modrm);
        out.extend_from_slice(&imm32);
        Insn::raw_bytes(&out)
    }

    /// P11: 构造 `movsd xmm_dst, xmm_src` (F2 0F 10 mod=11).
    /// 源/目的都在 xmm0..xmm15 (>=8 时带 REX.R/B 由 ModRM 低 3 位 + 前缀编码).
    pub fn movsd_xmm(dst_xmm: u8, src_xmm: u8) -> Self {
        // F2 0F 10 modrm; mod=11, reg=dst, r/m=src.
        Self::simd_xmm_reg_reg(0xF2, 0x10, dst_xmm, src_xmm)
    }

    /// P11: 构造 `movss xmm_dst, xmm_src` (F3 0F 10 mod=11).
    pub fn movss_xmm(dst_xmm: u8, src_xmm: u8) -> Self {
        Self::simd_xmm_reg_reg(0xF3, 0x10, dst_xmm, src_xmm)
    }

    /// P11: 构造 `movsd xmm_dst, xmm_src` 存储方向 (F2 0F 11 mod=11):
    ///   mod=11, reg=src (源 xmm), r/m=dst (目的 xmm).
    pub fn movsd_xmm_store(dst_xmm: u8, src_xmm: u8) -> Self {
        Self::simd_xmm_reg_reg(0xF2, 0x11, dst_xmm, src_xmm)
    }

    /// P11: 构造 `movss xmm_dst, xmm_src` 存储方向 (F3 0F 11 mod=11).
    pub fn movss_xmm_store(dst_xmm: u8, src_xmm: u8) -> Self {
        Self::simd_xmm_reg_reg(0xF3, 0x11, dst_xmm, src_xmm)
    }

    /// P11 Bucket B/E3: SimdMov [base+disp] store/load.
    /// store=true: movss/movsd/movups [base+disp], xmm (0F 11).
    /// store=false: movss/movsd/movups xmm, [base+disp] (0F 10).
    /// width: SimdWidth::{SS, SD, O/PS, PD, DQ}. base=gpr(4=rsp), disp=i32.
    pub fn simd_mov_mem_base(
        store: bool,
        width: crate::codegen::x86_64::opcode::SimdWidth,
        xmm_reg: u8,
        base: u8,
        disp: i32,
    ) -> Self {
        use crate::codegen::x86_64::opcode::{MemOperand, Operand, X86_64Opcode};
        let mem = Operand::Mem(MemOperand {
            disp,
            base: Some(base),
            index: None,
            seg: None,
        });
        let reg = Operand::Reg(xmm_reg, crate::codegen::shared::asm_operand::Width::B128);
        let (dst, src) = if store { (mem, reg) } else { (reg, mem) };
        Insn::new(
            X86_64Opcode::SimdMov {
                width,
                opcode10: store,
                dst,
                src,
            },
            &[],
        )
    }

    /// P11 Bucket B: SimdMov [rsp+disp] store/load. base 固定=4 (rsp).
    pub fn simd_mov_mem(
        store: bool,
        width: crate::codegen::x86_64::opcode::SimdWidth,
        xmm_reg: u8,
        disp: i32,
    ) -> Self {
        Self::simd_mov_mem_base(store, width, xmm_reg, 4, disp)
    }

    /// P11 Bucket G: movapd xmm_dst, xmm_src (66 0F 28 mod=11, load direction).
    pub fn movapd_xmm(dst_xmm: u8, src_xmm: u8) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::SimdMov {
                width: crate::codegen::x86_64::opcode::SimdWidth::PD,
                opcode10: true,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    dst_xmm,
                    crate::codegen::shared::asm_operand::Width::B128,
                ),
                src: crate::codegen::x86_64::opcode::Operand::Reg(
                    src_xmm,
                    crate::codegen::shared::asm_operand::Width::B128,
                ),
            },
            &[],
        )
    }

    /// P11 Bucket G: movaps xmm_dst, xmm_src (0F 28 mod=11, load direction).
    pub fn movaps_xmm(dst_xmm: u8, src_xmm: u8) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::SimdMov {
                width: crate::codegen::x86_64::opcode::SimdWidth::PS,
                opcode10: true,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    dst_xmm,
                    crate::codegen::shared::asm_operand::Width::B128,
                ),
                src: crate::codegen::x86_64::opcode::Operand::Reg(
                    src_xmm,
                    crate::codegen::shared::asm_operand::Width::B128,
                ),
            },
            &[],
        )
    }

    /// P11 Bucket C+E: SimdArith reg-reg (addsd/subsd/mulsd/divsd/addss/subss/mulss/divss/xorps/xorpd).
    /// op: SimdArithOp::{Add, Sub, Mul, Div, Xorps, Xorpd}. width: SimdWidth::{SD, SS}.
    pub fn simd_arith_reg(
        op: crate::codegen::x86_64::opcode::SimdArithOp,
        width: crate::codegen::x86_64::opcode::SimdWidth,
        dst_xmm: u8,
        src_xmm: u8,
    ) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::SimdArith {
                op,
                width,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    dst_xmm,
                    crate::codegen::shared::asm_operand::Width::B128,
                ),
                src: crate::codegen::x86_64::opcode::Operand::Reg(
                    src_xmm,
                    crate::codegen::shared::asm_operand::Width::B128,
                ),
            },
            &[],
        )
    }

    /// P11 Bucket E: stmxcsr/ldmxcsr [base+disp] (0F AE /3 or /2).
    pub fn mxcsr_mem(store: bool, base: u8, disp: i32) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::X87 {
                op: crate::codegen::x86_64::opcode::X87Op::MxcsrMem { store },
                mem: Some(crate::codegen::x86_64::opcode::MemOperand {
                    disp,
                    base: Some(base),
                    index: None,
                    seg: None,
                }),
            },
            &[],
        )
    }

    /// P11 Bucket E: fnstcw/fldcw [base+disp] (D9 /7 or /5).
    pub fn fcw_mem(store: bool, base: u8, disp: i32) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::X87 {
                op: crate::codegen::x86_64::opcode::X87Op::FcwMem { store },
                mem: Some(crate::codegen::x86_64::opcode::MemOperand {
                    disp,
                    base: Some(base),
                    index: None,
                    seg: None,
                }),
            },
            &[],
        )
    }

    /// P11 Bucket H: setcc r8 (0F 9x /0, mod=11).
    /// reg_idx: Intel reg number (0=al,1=cl,2=dl,3=bl,4=ah..15=r15b).
    pub fn setcc_reg(
        cc: crate::codegen::x86_64::opcode::CondCode,
        reg_idx: u8,
    ) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::Setcc {
                cc,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    reg_idx,
                    crate::codegen::shared::asm_operand::Width::B8,
                ),
            },
            &[],
        )
    }

    /// P11 Bucket B: cvtsi2sd/cvtsi2ss (r64->xmm) or cvttsd2si/cvttss2si (xmm->r64).
    /// to_xmm=true: r64->xmm (cvtsi2sd/cvtsi2ss). to_xmm=false: xmm->r64 (cvttsd2si/cvttss2si).
    /// width=SimdWidth::SD uses F2 prefix, width=SimdWidth::SS uses F3 prefix.
    pub fn cvt2sd_reg(
        to_xmm: bool,
        width: crate::codegen::x86_64::opcode::SimdWidth,
        dst_idx: u8,
        src_idx: u8,
    ) -> Self {
        use crate::codegen::shared::asm_operand::Width;
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::Cvt2Sd {
                to_xmm,
                width,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(dst_idx, Width::B128),
                src: crate::codegen::x86_64::opcode::Operand::Reg(src_idx, Width::B128),
            },
            &[],
        )
    }

    /// P11 Bucket D: roundsd/roundss reg-reg with imm8 (rounding mode).
    /// width=SimdWidth::SD → roundsd (66 0F 3A 0B /r ib).
    /// width=SimdWidth::SS → roundss (66 0F 3A 0A /r ib).
    pub fn simd_round_reg(
        width: crate::codegen::x86_64::opcode::SimdWidth,
        imm8: u8,
        dst_idx: u8,
        src_idx: u8,
    ) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::SimdRound {
                width,
                imm8,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    dst_idx,
                    crate::codegen::shared::asm_operand::Width::B128,
                ),
                src: crate::codegen::x86_64::opcode::Operand::Reg(
                    src_idx,
                    crate::codegen::shared::asm_operand::Width::B128,
                ),
            },
            &[],
        )
    }

    /// Common reg↔reg xmm↔xmm encoder.
    /// opcode2 = 0x10 (mov load direction dst<-src) or 0x11 (store direction dst<-src).
    /// Both dst/src are register fields in ModRM: reg field = dst_xmm, r/m = src_xmm.
    fn simd_xmm_reg_reg(pfx: u8, opcode2: u8, dst_xmm: u8, src_xmm: u8) -> Self {
        use crate::codegen::x86_64::opcode::{Operand, SimdWidth, X86_64Opcode};
        let width = match pfx {
            0xF3 => SimdWidth::SS,
            0xF2 => SimdWidth::SD,
            _ => SimdWidth::PS,
        };
        Insn::new(
            X86_64Opcode::SimdMov {
                width,
                opcode10: opcode2 == 0x10,
                dst: Operand::Reg(dst_xmm, crate::codegen::shared::asm_operand::Width::B128),
                src: Operand::Reg(src_xmm, crate::codegen::shared::asm_operand::Width::B128),
            },
            &[],
        )
    }

    /// P11: cqo / cdq / cwd sign-extend rax→rdx:rax.
    /// width=B64 → 48 99 (cqo); width=B32 → 99 (cdq); width=B16 → 66 99 (cwd).
    pub fn sign_extend_rax_to_rdx(width: crate::codegen::shared::asm_operand::Width) -> Self {
        let mut out = Vec::with_capacity(3);
        match width {
            crate::codegen::shared::asm_operand::Width::B64 => out.push(0x48),
            crate::codegen::shared::asm_operand::Width::B16 => out.push(0x66),
            _ => {},
        }
        out.push(0x99);
        Insn::raw_bytes(&out)
    }

    /// P11: movzx/movsx/movsxd via MovExt typed opcode (reg-reg form).
    /// sign: true=movsx/movsxd, false=movzx.
    /// from: source width (B8/B16/B32). to: dest width (B32/B64).
    pub fn mov_ext(
        sign: bool,
        from: crate::codegen::shared::asm_operand::Width,
        to: crate::codegen::shared::asm_operand::Width,
        dst_reg: u8,
        src_reg: u8,
    ) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                sign,
                from,
                to,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(dst_reg, to),
                src: crate::codegen::x86_64::opcode::Operand::Reg(src_reg, from),
            },
            &[],
        )
    }

    /// P11 Bucket 1: movzx/movsx/movsxd via MovExt typed opcode (mem-load form).
    /// sign: true=movsx/movsxd, false=movzx.
    /// from: source memory width (B8/B16/B32). to: dest width (B32/B64).
    /// base: x86_64 physical register index 0..15. disp: byte displacement.
    pub fn mov_ext_mem(
        sign: bool,
        from: crate::codegen::shared::asm_operand::Width,
        to: crate::codegen::shared::asm_operand::Width,
        dst_reg: u8,
        base: u8,
        disp: i32,
    ) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                sign,
                from,
                to,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(dst_reg, to),
                src: crate::codegen::x86_64::opcode::Operand::Mem(
                    crate::codegen::x86_64::opcode::MemOperand {
                        disp,
                        base: Some(base),
                        index: None,
                        seg: None,
                    },
                ),
            },
            &[],
        )
    }

    /// P11: NOT / NEG single-register unary via UnaryRm typed opcode.
    /// op: UnaryOp::Not or UnaryOp::Neg. width: B16/B32/B64. reg_idx: x86_64
    /// physical register index 0..15.
    pub fn unary_rm_reg(
        op: crate::codegen::x86_64::opcode::UnaryOp,
        width: crate::codegen::shared::asm_operand::Width,
        reg_idx: u8,
    ) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::UnaryRm {
                op,
                width,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(reg_idx, width),
            },
            &[],
        )
    }

    /// P11: TEST reg, reg via AluRm typed opcode.
    /// width: B32/B64 (common forms). dst/src: x86_64 physical register indices 0..15.
    pub fn test_rm_reg(
        width: crate::codegen::shared::asm_operand::Width,
        dst_reg: u8,
        src_reg: u8,
    ) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                op: crate::codegen::x86_64::opcode::AluOp::Test,
                width,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(dst_reg, width),
                src: crate::codegen::x86_64::opcode::Operand::Reg(src_reg, width),
            },
            &[],
        )
    }

    /// P11: IMUL reg, reg (0F AF /r) via ImulRm typed opcode.
    /// width: B32/B64. dst/src: x86_64 physical register indices 0..15.
    pub fn imul_rm_reg(
        width: crate::codegen::shared::asm_operand::Width,
        dst_reg: u8,
        src_reg: u8,
    ) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::ImulRm {
                width,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(dst_reg, width),
                src: crate::codegen::x86_64::opcode::Operand::Reg(src_reg, width),
            },
            &[],
        )
    }

    /// P11 Bucket G: unpcklps xmm_dst, xmm_src (0F 14 /r).
    pub fn unpcklps_xmm(dst_xmm: u8, src_xmm: u8) -> Self {
        Insn::simd_arith_reg(
            crate::codegen::x86_64::opcode::SimdArithOp::Unpcklps,
            crate::codegen::x86_64::opcode::SimdWidth::PS,
            dst_xmm,
            src_xmm,
        )
    }

    pub fn encode_bytes(&self) -> Option<Vec<u8>> {
        // 类型化变体: 让 opcode.rs 现场算字节. 尚未实现的变体返回 None →
        // 发射方必须切回 raw_bytes 兜底; 当前没有任何调用点依赖 None 分支.
        self.op.encode()
    }

    pub fn to_rec(&self, pc: u64) -> InsnRec {
        // 迁移期: opcode_id 统一记 0 (架构中性 InsnRec 只在跨后端通道用,
        // 生产路径尚未切到). 步骤 6 完成后再按变体映射到 u16 空间.
        let mut rec = InsnRec::new(0, &self.ops[..]);
        rec.pc = pc;
        rec.bytes_len = self.bytes.len() as u8;
        rec
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_bytes_passthrough() {
        // x86_64 本机小端, 字节原样落盘
        assert_eq!(
            Insn::raw_bytes(&[0x48, 0x8B, 0x05, 0x34, 0x12, 0x00, 0x00]).encode_bytes(),
            Some(vec![0x48, 0x8B, 0x05, 0x34, 0x12, 0x00, 0x00]),
        );
    }

    #[test]
    fn nop_encode() {
        assert_eq!(Insn::nop().encode_bytes(), Some(vec![0x90]));
    }

    #[test]
    fn raw_bytes_empty() {
        assert_eq!(Insn::raw_bytes(&[]).encode_bytes(), Some(Vec::new()));
    }
}
