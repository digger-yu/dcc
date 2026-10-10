// src/codegen/x86_64/opcode.rs
//
// 类型化 x86_64 指令枚举 (P9 方案 C 步骤 1 打样).
//
// 第一性原理: 每个变体的 encode() 与 render() 都在此文件定义, 是单份真相:
// 同一 (kind, operands) 只在这里描述一次, 不再需要"手写 REX/ModRM 发字节 +
// disasm.rs 逐字节反向解码"两份表永远同步.
//
// 迁移纪律 (G11): 本步 (步骤 1) 只对 AluRm{Add/Sub, B64, Rsp, Imm32} 形态
// 输出与原手写逐字节相同的字节; 其余变体先定义不实现 (render 返回占位串,
// encode 走 None → 发射方必须先用上再切), 禁止一次性铺开.

/// GPR ALU 操作族. 共享同一 ModRM.reg 编码位段: /0 ADD, /1 OR, /2 ADC, /3 SBB,
/// /4 AND, /5 SUB, /6 XOR, /7 CMP. TEST 走 0x84/0x85 (无 ModRM.reg 语义).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AluOp {
    Add,
    Sub,
    And,
    Or,
    Xor,
    Cmp,
    Test,
}

/// 一元操作 (INC/DEC/NOT/NEG/MUL/IMUL/DIV/IDIV) — 步骤 1 不实现.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum UnaryOp {
    Inc,
    Dec,
    Not,
    Neg,
    Mul,
    Imul,
    Div,
    Idiv,
}

/// 移位族 — 步骤 1 不实现.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum ShiftOp {
    Shl,
    Shr,
    Sar,
    Sal,
    Rol,
    Ror,
    Rcl,
    Rcr,
}

/// PUSH/POP — 步骤 3 收敛 reg 形态.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StackOp {
    Push,
    Pop,
}

/// P11 Bucket I: 无操作数/固定前缀指令 (cld/mfence/rep movsb/rep stosb/rep movsq).
/// 所有形态均为固定字节序列, 无 ModRM/SIB/imm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum NoArgOp {
    /// cld: FC (清除方向标志).
    Cld,
    /// mfence: 0F AE F0 (Store-Load 屏障).
    Mfence,
    /// rep movsb: F3 A4 (ES:[rdi] → R:[rsi], rcx 字节计数).
    RepMovsb,
    /// rep stosb: F3 AA (AL → ES:[rdi], rcx 字节计数).
    RepStosb,
    /// rep movsq: F3 48 A5 (qword 版本, 带 REX.W).
    RepMovsq,
}

/// SIMD 算术 — 步骤 1 不实现.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum SimdArithOp {
    Add,
    Sub,
    Mul,
    Div,
    Max,
    Min,
    Sqrt,
    Ucomi,
    Comi,
    Xorps,
    Xorpd,
    /// P11 Bucket F: pand/por/pxor (66 0F DB/EB/EF /r).
    Pand,
    Por,
    Pxor,
    /// P11 Bucket C: cvtsd2ss/cvtss2sd (F2/F3 0F 5A /r).
    /// Convert between scalar double and scalar float in xmm.
    CvtSdSs,
    /// P11 Bucket J: andps/andpd (0F 54 /r / 66 0F 54 /r) — bitwise AND of packed
    /// single/double-precision floats. Used by isinf/fabs fast paths.
    Andps,
    Andpd,
    /// P11 Bucket A2: orps/orpd (0F 56 /r / 66 0F 56 /r) — bitwise OR packed.
    Orps,
    Orpd,
    /// P11 Bucket B: packed integer add/sub across widths (paddb/w/d/q).
    /// paddb=66 0F FC /r, paddw=66 0F FD /r, paddd=66 0F FE /r, paddq=66 0F D4 /r.
    /// psubb=66 0F F8 /r, psubw=66 0F F9 /r, psubd=66 0F FA /r, psubq=66 0F FB /r.
    PAdd,
    PSub,
    /// P11 Bucket B: packed float arithmetic (addps/subps/mulps/divps / pd variants).
    Addps,
    Subps,
    Mulps,
    Divps,
    Addpd,
    Subpd,
    Mulpd,
    Divpd,
}

/// SIMD 宽度 — 步骤 4: SimdMov 用来发 xmm store/load 到 rsp 槽.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SimdWidth {
    /// movss: F3 前缀, 0F 10/11
    SS,
    /// movsd: F2 前缀, 0F 10/11
    SD,
    /// movups: 无 legacy 前缀, 0F 10/11
    PS,
    /// movupd: 66 前缀, 0F 10/11
    PD,
    /// movq gp->xmm: 66 前缀, 0F 6E (reg-reg 形态, 步骤 4 不收敛)
    DQ,
    /// 128-bit 通用 (movups/movaps 无 legacy 前缀)
    O,
    /// P11 Bucket B: packed-integer element sizes (used by PAdd/PSub).
    B, // 8-bit elements (paddb/psubb)
    W, // 16-bit elements (paddw/psubw)
    D, // 32-bit elements (paddd/psubd)
    Q, // 64-bit elements (paddq/psubq)
}

/// x87 操作. 步骤 4: 覆盖固定 escap 字节 (无操作数) 与单内存操作数两类.
///
/// 无操作数固定 escap 序列: 直接给出 (escap, modrm) 两字节.
///   FldConst { escap, modrm }  如 fld1=D9 E8, fldz=D9 EE, fldpi=D9 EB
///   FstpSt0                   fstp st(0): DD D9
///   Fchs                      fchs: D9 E0
///   FcomPopBin { escap, modrm }  如 faddp=DE C1 / fsubrp=DE E9 / fmulp=DE C9 / fdivrp=DE F9
///   Fucomip                   fucomip st(0), st(1): DF E9
///
/// 单内存操作数: escap 字节 + reg 位段 (/0 /1 /2 /3 /4 /5 /6 /7) + ModRM/SIB/disp.
///   FldMem { width }   fld m32fp (D9 /0) / m64fp (DD /0) / m80fp (DB /5)
///   FstpMem { width }  fstp m32fp (D9 /3) / m64fp (DD /3) / m80fp (DB /7)
///   FildMem { width }  fild m32int (DB /0) / m64int (DB /0 带 REX.W)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum X87Op {
    /// 固定 escap 序列 (无内存). 例 fld1 = (0xD9, 0xE8).
    Fixed { escap: u8, modrm: u8 },
    /// fld m32fp / m64fp / m80fp. width: 4=m32fp, 8=m64fp, 10=m80fp.
    FldMem { width: u8 },
    /// fstp m32fp / m64fp / m80fp.
    FstpMem { width: u8 },
    /// fild m32int / m64int. width: 4=m32int, 8=m64int (带 REX.W).
    FildMem { width: u8 },
    /// P11 Bucket E: stmxcsr/ldmxcsr m32 — 0F AE /3 (store) or /2 (load).
    /// Encoding: 0F AE modrm(mod=00, reg=2|3, r/m=base) + SIB(disp8|disp32).
    MxcsrMem { store: bool },
    /// P11 Bucket E: fnstcw/fldcw m16 — D9 /7 (store) or /5 (load).
    ///   fnstcw [rsp]: D9 3C 24 (SIB mod=00, reg=7, rm=4, disp8=0).
    ///   fldcw  [rsp]: D9 2C 24 (SIB mod=00, reg=5, rm=4, disp8=0).
    FcwMem { store: bool },
}

/// 移位数量: CL 或 imm8 — 步骤 1 不实现.
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
pub(crate) enum ShiftAmount {
    Cl,
    Imm8(u8),
}

/// 条件码 — 步骤 3 先占位 (rel 形态 pending 暂不收敛).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum CondCode {
    Eq,
    Ne,
    Lt,
    Ge,
    Gt,
    Le,
    A,
    Ae,
    B,
    Be,
    S,
    Ns,
    P,
    Np,
    O,
    No,
}

/// 内存操作数 ({disp, base, index, seg}).
/// base/index 都是 Intel 寄存器号 (0=rax .. 15=r15).
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) struct MemOperand {
    pub disp: i32,
    pub base: Option<u8>,
    pub index: Option<(u8, u8)>,
    pub seg: Option<u8>,
}

impl MemOperand {
    /// 便捷构造: [base + disp] (无 index / seg). 用于 emit_store / emit_load 类发射点.
    pub(crate) fn bp_disp(base: u8, disp: i32) -> Self {
        MemOperand {
            disp,
            base: Some(base),
            index: None,
            seg: None,
        }
    }
}

/// x86_64 寄存器号 (Intel 编码): 0=rax, 1=rcx, 2=rdx, 3=rbx, 4=rsp, 5=rbp,
/// 6=rsi, 7=rdi, 8..15=r8..r15. 步骤 1 只把目标限定在 Reg(4=Rsp, B64) 上.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) enum Operand {
    None,
    Reg(u8, crate::codegen::shared::asm_operand::Width),
    Imm(i64),
    Mem(MemOperand),
    Sym(String),
    SymDisp(String, i32),
}

/// 编码机制族. 不按助记符展开 — add/sub/and/or/xor/cmp 共用 AluRm.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) enum X86_64Opcode {
    /// ADD/SUB/AND/OR/XOR/CMP/TEST reg/mem/imm, 按 width 发 REX+ModRM.
    ///
    /// 步骤 1 只打样: `AluRm{op: Add|Sub, width: B64, dst: Reg(4=Rsp), src: Imm(imm32)}`
    /// 形态 (即 emit_sub_rsp / emit_add_rsp 用的 `48 81 /5 /4 C4|EC <imm32 LE>`).
    /// 其余形态暂返回 None, 发射方继续走 raw_bytes 兜底.
    AluRm {
        op: AluOp,
        width: crate::codegen::shared::asm_operand::Width,
        dst: Operand,
        src: Operand,
    },

    /// MOV 族 — 步骤 1 不实现.
    MovRm {
        width: crate::codegen::shared::asm_operand::Width,
        dst: Operand,
        src: Operand,
    },

    /// MOVSX/MOVZX — 步骤 1 不实现.
    MovExt {
        sign: bool,
        from: crate::codegen::shared::asm_operand::Width,
        to: crate::codegen::shared::asm_operand::Width,
        dst: Operand,
        src: Operand,
    },

    /// IMUL r64, r/m64 (0F AF /r) — P11.
    ImulRm {
        width: crate::codegen::shared::asm_operand::Width,
        dst: Operand,
        src: Operand,
    },

    /// INC/DEC/NOT/NEG/MUL/IMUL/DIV/IDIV — 步骤 1 不实现.
    UnaryRm {
        op: UnaryOp,
        width: crate::codegen::shared::asm_operand::Width,
        dst: Operand,
    },

    /// SHL/SHR/SAR/... — 步骤 1 不实现.
    Shift {
        op: ShiftOp,
        width: crate::codegen::shared::asm_operand::Width,
        dst: Operand,
        amount: ShiftAmount,
    },

    /// LEA — 步骤 1 不实现.
    Lea {
        dst: Operand,
        mem: MemOperand,
        rip_relative: bool,
    },

    /// CALL — 步骤 1 不实现.
    Call { target: Operand, pending_rel: bool },

    /// JMP — 步骤 1 不实现.
    Jmp { target: Operand, pending_rel: bool },

    /// JCC — 步骤 1 不实现.
    Jcc { cc: CondCode, pending_rel: bool },

    /// RET — 步骤 1 不实现.
    Ret { add_imm: Option<u16> },

    /// PUSH/POP — 步骤 1 不实现.
    Stack {
        op: StackOp,
        width: crate::codegen::shared::asm_operand::Width,
        dst: Operand,
    },

    /// SSE/AVX 算术 — 步骤 1 不实现.
    SimdArith {
        op: SimdArithOp,
        width: SimdWidth,
        dst: Operand,
        src: Operand,
    },

    /// P11 Bucket H: setcc r/m8 (0F 9x /0, mod=11 reg8).
    /// dst: Reg(reg_idx, B8) — reg_idx is Intel reg number (0=al..15=r15b).
    Setcc { cc: CondCode, dst: Operand },

    /// SSE/AVX 移动 — 步骤 4: 收敛 xmm store/load 到 rsp 槽.
    /// width 决定 legacy 前缀 (F3=SS, F2=SD, 无=O/PS), opcode 决定 0F 10(load) / 0F 11(store).
    /// dst=Reg(xmm, Xmm) / src=Mem(rsp, disp) 或反向.
    SimdMov {
        width: SimdWidth,
        opcode10: bool,
        dst: Operand,
        src: Operand,
    },

    /// P11 Bucket 7: shld/shrd (double shift) reg-reg form with CL count.
    /// shld 0F A5 / shrd 0F AD. dst=Reg(dst), src=Reg(src), count=CL implicitly.
    ShldShrd {
        is_shld: bool,
        width: crate::codegen::shared::asm_operand::Width,
        dst: Operand,
        src: Operand,
    },

    /// P11 Bucket B: SSE 整数<->浮点转换 reg-reg.
    ///   cvtsi2sd: F2 REX.W 0F 2A /r (r64 -> xmm) — integer_to_scalar_double.
    ///   cvtsi2ss: F3 REX.W 0F 2A /r (r64 -> xmm) — integer_to_scalar_float.
    ///   cvttsd2si: F2 REX.W 0F 2C /r (xmm -> r64, truncate).
    ///   cvttss2si: F3 REX.W 0F 2C /r (xmm -> r64, truncate).
    /// to_xmm=true 表示 r64->xmm 形态 (opcode 2A), 否则 xmm->r64 (opcode 2C).
    /// width=SimdWidth::SD 用 F2 前缀, width=SimdWidth::SS 用 F3 前缀.
    Cvt2Sd {
        to_xmm: bool,
        width: SimdWidth,
        dst: Operand,
        src: Operand,
    },

    /// P11 Bucket D: roundsd/roundss reg-reg with imm8.
    ///   roundsd xmm0,xmm0,imm8: 66 0F 3A 0B /r ib
    ///   roundss xmm0,xmm0,imm8: 66 0F 3A 0A /r ib
    /// width=SimdWidth::SD → opc2=0x0B; SS → opc2=0x0A. imm8 = rounding mode.
    SimdRound {
        width: SimdWidth,
        imm8: u8,
        dst: Operand,
        src: Operand,
    },

    /// x87 — 步骤 4: 收敛固定 escap 无操作数 + 单内存操作数两种形态.
    /// op::Fixed 时 mem=None; op::FldMem/FstpMem/FildMem 时 mem=Some(MemOperand).
    X87 { op: X87Op, mem: Option<MemOperand> },

    /// 兜底: 内嵌字节序列 + (可选) mnemonic 供 render.
    /// 与现有 Insn::raw 兼容.
    Raw {
        bytes: Vec<u8>,
        mnemonic: Option<String>,
    },

    /// P11 Bucket I: 无操作数/固定前缀指令 (cld/mfence/rep movsb/stosb/movsq).
    NoArg { op: NoArgOp },

    /// P11 Bucket K: movd/movq GPR↔XMM reg-reg (0F 6E / 0F 7E, with 66 prefix +
    /// optional REX.W).
    ///   movq r64->xmm:  66 REX.W 0F 6E /r  (modrm.reg=xmm, r/m=gpr)
    ///   movq xmm->r64:  66 REX.W 0F 7E /r  (modrm.reg=xmm, r/m=gpr)
    ///   movd r32->xmm:    0F 6E /r (no 66, no REX.W)
    ///   movd xmm->r32:    0F 7E /r
    /// to_xmm=true: GPR→XMM (0x6E); false: XMM→GPR (0x7E). width=B64 emits REX.W.
    GprXmm {
        to_xmm: bool,
        width: crate::codegen::shared::asm_operand::Width,
        dst: Operand,
        src: Operand,
    },
}

impl X86_64Opcode {
    /// 编码为字节序列. None 表示该形态尚未实现, 发射方继续走 raw_bytes 兜底.
    pub(crate) fn encode(&self) -> Option<Vec<u8>> {
        use crate::codegen::shared::asm_operand::Width;
        match self {
            X86_64Opcode::AluRm { op, width, dst, src } => {
                // 形态分支 A: (Add|Sub, B64, Rsp=Reg(4), Imm).
                // 手写约定: 所有 emit_sub_rsp / emit_add_rsp 走 0x81 / id (imm32 LE),
                // 即使 imm 很小也带 4 字节立即数 (G10 快照依赖此行为).
                //   sub rsp, imm32:  48 81 EC <imm32 LE>
                //   add rsp, imm32:  48 81 C4 <imm32 LE>
                if let (Operand::Reg(reg_idx, Width::B64), Operand::Imm(imm)) = (dst, src) {
                    if *reg_idx == 4 {
                        let (opcode, modrm_reg_field): (u8, u8) = match op {
                            AluOp::Add => (0x81, 0b000), // /0
                            AluOp::Sub => (0x81, 0b101), // /5
                            _ => return None,
                        };
                        let rex = 0x48u8;
                        let modrm = 0xC0 | (modrm_reg_field << 3) | (reg_idx & 7);
                        let imm32 = (*imm as i32).to_le_bytes();
                        let mut out = Vec::with_capacity(3 + 4);
                        out.push(rex);
                        out.push(opcode);
                        out.push(modrm);
                        out.extend_from_slice(&imm32);
                        return Some(out);
                    }
                }
                // 形态分支 A3 (P11 Bucket B): 非 RSP reg, imm32 — 走 0x81 / id 形式.
                //   手写形态: 48 81 Cx / Dx / Ex (mod=11, /0 /1 /4 /5 /6 /7) + imm32 LE.
                //   仅当 imm 超出 i8 范围或调用方明确用 imm32 形式时走这里;
                //   本步保留调用点显式选择 (raw_bytes 双字节模式: 先发 opcode 三元组,
                //   再单独发 imm32 LE), 故我们也提供一个等价的 AluRm encode 走相同字节.
                if let (Operand::Reg(reg_idx, w), Operand::Imm(imm)) = (dst, src) {
                    if !matches!(op, AluOp::Test) {
                        let i8v = i8::try_from(*imm);
                        // 仅在 imm 超出 i8 范围时走 0x81 / id (避免和分支 A2 的 0x83 冲突).
                        if i8v.is_err() {
                            let reg_field: u8 = match op {
                                AluOp::Add => 0,
                                AluOp::Or => 1,
                                AluOp::And => 4,
                                AluOp::Sub => 5,
                                AluOp::Xor => 6,
                                AluOp::Cmp => 7,
                                AluOp::Test => unreachable!(),
                            };
                            let is_b64 = matches!(w, Width::B64);
                            let is_b32 = matches!(w, Width::B32);
                            if is_b64 || is_b32 {
                                let mut rex: u8 = 0x40;
                                if is_b64 {
                                    rex |= 0x08; // REX.W
                                }
                                if *reg_idx >= 8 {
                                    rex |= 0x01;
                                }
                                let need_rex = is_b64 || *reg_idx >= 8;
                                let modrm: u8 = 0xC0 | (reg_field << 3) | (reg_idx & 7);
                                let mut out = Vec::with_capacity(7);
                                if need_rex {
                                    out.push(rex);
                                }
                                out.push(0x81);
                                out.push(modrm);
                                out.extend_from_slice(&(*imm as i32).to_le_bytes());
                                return Some(out);
                            }
                        }
                    }
                }
                // 形态分支 A2: 非 RSP 的 reg, imm. 只在 imm 可符号扩展到 i8 时收敛
                // (即与手写 0x83 / ib 分支逐字节一致); 否则回退手写 (避免和
                // 既有 emit_sub_rsp 的 0x81 / id 形态产生歧义).
                //   0x83 / ib:  REX.W|B (B64) + 0x83 + ModRM(mod=11,reg=op,r/m=reg) + imm8
                if let (Operand::Reg(reg_idx, w), Operand::Imm(imm)) = (dst, src) {
                    if !matches!(op, AluOp::Test) {
                        let i8v = i8::try_from(*imm);
                        if let Ok(v) = i8v {
                            let reg_field: u8 = match op {
                                AluOp::Add => 0,
                                AluOp::Or => 1,
                                AluOp::And => 4,
                                AluOp::Sub => 5,
                                AluOp::Xor => 6,
                                AluOp::Cmp => 7,
                                AluOp::Test => unreachable!(),
                            };
                            let is_b64 = matches!(w, Width::B64);
                            let is_b32 = matches!(w, Width::B32);
                            if is_b64 || is_b32 {
                                let mut rex: u8 = 0x40;
                                if is_b64 {
                                    rex |= 0x08;
                                }
                                if *reg_idx >= 8 {
                                    rex |= 0x01;
                                }
                                let need_rex = is_b64 || *reg_idx >= 8;
                                let modrm: u8 = 0xC0 | (reg_field << 3) | (reg_idx & 7);
                                let mut out = Vec::with_capacity(4);
                                if need_rex {
                                    out.push(rex);
                                }
                                out.push(0x83);
                                out.push(modrm);
                                out.push(v as u8);
                                return Some(out);
                            }
                        }
                    }
                }
                // 形态分支 A4 (P11 Bucket B): reg, [base+disp] (mem -> reg).
                //   常见手写: 48 03 4X XX (add r64, [reg+disp8])
                //             48 2B 4X XX (sub r64, [reg+disp8])
                //             48 23 4X XX (and r64, [reg+disp8])
                //   与 reg-reg 区别: 目的在 reg (ModRM.reg), 源在 mem (ModRM.r/m).
                //   opc 用 03/2B/23/0B/33/3B (reg <- r/m).
                if let (Operand::Reg(dst_idx, _), Operand::Mem(mem)) = (dst, src) {
                    if mem.index.is_some() || mem.seg.is_some() {
                        return None;
                    }
                    let base = match mem.base {
                        Some(b) => b,
                        None => return None,
                    };
                    let is_b64 = matches!(width, Width::B64);
                    let is_b32 = matches!(width, Width::B32);
                    if !is_b64 && !is_b32 {
                        return None;
                    }
                    // opcode: reg <- r/m 形态 (03/0B/23/2B/33/3B).
                    let opcode: u8 = match op {
                        AluOp::Add => 0x03,
                        AluOp::Or => 0x0B,
                        AluOp::And => 0x23,
                        AluOp::Sub => 0x2B,
                        AluOp::Xor => 0x33,
                        AluOp::Cmp => 0x3B,
                        AluOp::Test => unreachable!(),
                    };
                    let mut rex: u8 = 0x40;
                    if is_b64 { rex |= 0x08; }
                    if *dst_idx >= 8 { rex |= 0x04; }
                    if base >= 8 { rex |= 0x01; }
                    let need_rex = is_b64 || *dst_idx >= 8 || base >= 8;
                    // disp 长度: base=rbp(5) mod=00 → rip-relative, 需 disp=0 时走 mod=01.
                    // base=rsp(4) 必须 SIB (rm_field=4).
                    let disp = mem.disp;
                    let (modrm_mod, disp_bytes): (u8, &[u8]) = if disp == 0 && base != 5 && base != 4 {
                        (0, &[])
                    } else if let Ok(d) = i8::try_from(disp) {
                        (1, &[d as u8])
                    } else {
                        (2, &disp.to_le_bytes())
                    };
                    let rm_field = base & 7;
                    let modrm = (modrm_mod << 6) | ((*dst_idx & 7) << 3) | rm_field;
                    let mut out = Vec::with_capacity(12);
                    if need_rex { out.push(rex); }
                    out.push(opcode);
                    out.push(modrm);
                    if rm_field == 4 {
                        out.push(0x24); // SIB: no index.
                    }
                    out.extend_from_slice(disp_bytes);
                    return Some(out);
                }
                // 形态分支 B: reg, reg (reg-reg). 通用 ALU 族.
                //   位序: 00=ADD r/m, r   01=OR   02=ADC   03=SBB
                //         04=AND r/m, r  05=SUB r/m, r
                //         06=XOR r/m, r 07=CMP r/m, r
                //   0x20=ADD r, r/m      0x21=OR   0x28=AND   0x29=SUB
                //         0x30=XOR       0x31=CMP  (同一 op 的反向 0x08/0x09/0x21/...)
                //
                //   ModRM.reg 放 src (源寄存器), ModRM.r/m 放 dst (目的寄存器 r/m).
                //   对 AND/OR/XOR/SUB 等 (comm: 目的在 r/m 字段), opc=0x20/0x21/0x29/0x30/0x31.
                //   对 ADD, 目的在 r/m: 用 0x00 (add r/m, r).
                let (Operand::Reg(dst_idx, _), Operand::Reg(src_idx, _)) = (dst, src) else {
                    return None;
                };
                // TEST reg,reg:  85 /r (B32) / REX.W + 85 /r (B64).
                //   ModRM.reg 字段放 0 (/0, TEST 不读取该字段), r/m 放 dst; src 被忽略?
                //   但手写形态 modrm = 0xC0 | (src<<3) | dst —— 虽然 TEST 只按 r/m 做 AND
                //   并置 flags, ModRM.reg 字段仍按 dst 本身填 (常见手写: test rax, rax
                //   modrm=C0; test rcx, rdx modrm=C1|... 实际 dst=r/m, src=reg).
                //   本实现沿用: reg=src, r/m=dst, mod=11. opcode=85 (B32) 或 REX.W+85 (B64).
                if matches!(op, AluOp::Test) {
                    let is_b64 = matches!(width, Width::B64);
                    let is_byte8 = matches!(width, Width::B8);
                    let mut rex: u8 = 0x40;
                    if is_b64 {
                        rex |= 0x08;
                    }
                    if *src_idx >= 8 {
                        rex |= 0x04;
                    }
                    if *dst_idx >= 8 {
                        rex |= 0x01;
                    }
                    let need_high_byte = is_byte8 && (*src_idx >= 4 || *dst_idx >= 4);
                    let need_rex = is_b64 || *src_idx >= 8 || *dst_idx >= 8 || need_high_byte;
                    let mut out = Vec::with_capacity(4);
                    if matches!(width, Width::B16) {
                        out.push(0x66);
                    }
                    if need_rex {
                        out.push(rex);
                    }
                    // TEST r/m, r: opcode 0x84 (B8) / 0x85 (B16/B32/B64).
                    out.push(if is_byte8 { 0x84 } else { 0x85 });
                    out.push(0xC0 | ((*src_idx & 7) << 3) | (*dst_idx & 7));
                    return Some(out);
                }
                // opc_base: "目的在 r/m" 的 opc (00/08/21/29/31). modrm_reg_field 恒为 src.
                //   B8:   0x00/0x08/0x20/0x28/0x30/0x38
                //   B16+: 0x01/0x09/0x21/0x29/0x31/0x39
                let is_byte8 = matches!(width, Width::B8);
                let opc_base: u8 = match op {
                    AluOp::Add => if is_byte8 { 0x00 } else { 0x01 },
                    AluOp::Or => if is_byte8 { 0x08 } else { 0x09 },
                    AluOp::And => if is_byte8 { 0x20 } else { 0x21 },
                    AluOp::Sub => if is_byte8 { 0x28 } else { 0x29 },
                    AluOp::Xor => if is_byte8 { 0x30 } else { 0x31 },
                    AluOp::Cmp => if is_byte8 { 0x38 } else { 0x39 },
                    AluOp::Test => return None,
                };
                // REX: W=1 仅 B64; R=1 当 src>=8 (ModRM.reg 字段); B=1 当 dst>=8 (ModRM.r/m 字段).
                // 特殊: B8 (spl/bpl/sil/dil, src 或 dst ∈ {4,5,6,7}) 时必须发一个 REX 前缀
                // 让汇编器把这些认成高 8 字节而非 ah/ch/dh/bh. 参考 emit_neg 与 monolith
                // `and dl, sil` 手写的 0x40.
                let mut rex: u8 = 0x40;
                if matches!(width, Width::B64) {
                    rex |= 0x08;
                }
                if *src_idx >= 8 {
                    rex |= 0x04;
                }
                if *dst_idx >= 8 {
                    rex |= 0x01;
                }
                // 需要真发 REX: B64 或任一字段>=8, 或 B8 形态涉及 src/dst ∈ {4,5,6,7}.
                let need_high_byte =
                    is_byte8 && (*src_idx >= 4 || *dst_idx >= 4);
                let need_rex = matches!(width, Width::B64)
                    || *src_idx >= 8
                    || *dst_idx >= 8
                    || need_high_byte;
                let mut out = Vec::with_capacity(4);
                if matches!(width, Width::B16) {
                    out.push(0x66);
                }
                if need_rex {
                    out.push(rex);
                }
                out.push(opc_base);
                // ModRM: mod=11, reg=src, r/m=dst.
                out.push(0xC0 | ((*src_idx & 7) << 3) | (*dst_idx & 7));
                Some(out)
            }
            X86_64Opcode::Lea { dst, mem, rip_relative } => {
                // 步骤 2b: 收敛 emit_lea_r13. 形态限定为 LEA reg64, [base+disp];
                // 不支持 index/seg/rip_relative (rip 相对由 emit_lea_sym 单独处理).
                //
                // 原手写 (instructions.rs::emit_lea_r13):
                //   49|rex_r, 0x8D, <modrm>, [disp8 | disp32]
                // 其中 base 固定是 r13=13:
                //   disp8:  modrm = 0x45 | ((dst&7)<<3)            (mod=01, r/m=5)
                //   disp32: modrm = 0x85 | ((dst&7)<<3)            (mod=10, r/m=5)
                // REX.B=1 因为 base=r13 (>=8), REX.R=1 因为 dst>=8, REX.W=0 因为 LEA
                // 本身就是 64-bit 地址运算 (不需要 REX.W; 原代码就是 0x49 不带 W).
                let (Operand::Reg(dst_idx, Width::B64), mem) = (dst, mem) else {
                    return None;
                };
                if mem.index.is_some() || mem.seg.is_some() || *rip_relative {
                    return None;
                }
                let base = mem.base?;
                if base == 4 || base == 12 {
                    // rsp/r12 需要 SIB — 本步不支持, 回退手写.
                    return None;
                }
                let disp = mem.disp;
                // 复用 emit_lea_r13 分支: disp=0 且 base==rbp/13 时 mod=00 会被解释为
                // rip 相对, 但原代码只在 fits_disp32(off) 分支里发 modrm — 意味着 off=0
                // 且 base=5 时也走 mod=01 (disp=0). 我们保持与原代码一致: off=0 走
                // disp8(=0). 这对 rbp/r13 是必须的 (否则变成 rip 相对).
                let (modrm_mod, disp_bytes): (u8, &[u8]) = if i8::try_from(disp).is_ok() {
                    (1, &disp.to_le_bytes()[..1])
                } else {
                    (2, &disp.to_le_bytes()[..4])
                };
                let mut rex: u8 = 0x40; // 基础 REX 前缀. W 由调用方指定 (B64 → W=1).
                if *dst_idx >= 8 {
                    rex |= 0x04; // REX.R
                }
                if base >= 8 {
                    rex |= 0x01; // REX.B
                }
                // 对 B64 宽度补 REX.W=1 (匹配原 emit_lea_r13 手写 0x49).
                if matches!(dst, Operand::Reg(_, Width::B64)) {
                    rex |= 0x08;
                }
                let modrm = (modrm_mod << 6) | ((*dst_idx & 7) << 3) | (base & 7);
                let mut out = Vec::with_capacity(8);
                out.push(rex);
                out.push(0x8D);
                out.push(modrm);
                out.extend_from_slice(disp_bytes);
                Some(out)
            }
            X86_64Opcode::UnaryRm { op, width, dst } => {
                // 步骤 2c: 收敛 increment() / emit_neg() 这类"单 reg 目的"形态.
                //   INC/DEC: 8-bit 用 FE /0 / /1; 其余宽度用 FF /0 / /1.
                //   NOT/NEG: 8-bit 用 F6 /2 / /3; 其余宽度用 F7 /2 / /3.
                let Operand::Reg(reg_idx, _) = dst else {
                    return None;
                };
                let is_byte = matches!(width, Width::B8);
                let (opc_base, modrm_reg_field): (u8, u8) = match op {
                    UnaryOp::Inc => (if is_byte { 0xFE } else { 0xFF }, 0b000),
                    UnaryOp::Dec => (if is_byte { 0xFE } else { 0xFF }, 0b001),
                    UnaryOp::Not => (if is_byte { 0xF6 } else { 0xF7 }, 0b010),
                    UnaryOp::Neg => (if is_byte { 0xF6 } else { 0xF7 }, 0b011),
                    _ => return None,
                };
                let mut out = Vec::with_capacity(4);
                // 16-bit 需要 66 前缀 (必须在 REX 之前).
                if matches!(width, Width::B16) {
                    out.push(0x66);
                }
                // REX: 8-bit 操作数且 reg>=4 (spl/bpl/sil/dil) 时也必须发一个 REX 让
                // 汇编器把 b 寄存器认成高 8 字节. 原代码 sz==1 分支已经处理.
                // reg>=8 时置 REX.B (目的只出现在 ModRM.r/m).
                let mut rex: u8 = 0x40;
                let need_rex_w = matches!(width, Width::B64);
                if need_rex_w {
                    rex |= 0x08;
                }
                if *reg_idx >= 8 {
                    rex |= 0x01; // REX.B
                }
                // 是否真的发 REX: 8-bit 时即使 reg<4 也要发 (emit_neg sz=1 发 0x40);
                // 其余宽度只有在 REX.W=1 或 reg>=8 时才发.
                let need_rex = is_byte || need_rex_w || *reg_idx >= 8;
                if need_rex {
                    out.push(rex);
                }
                out.push(opc_base);
                // ModRM mod=11 (reg, 无内存), reg=modrm_reg_field, r/m=reg_idx&7.
                out.push(0xC0 | (modrm_reg_field << 3) | (reg_idx & 7));
                Some(out)
            }
            X86_64Opcode::Shift { op, width, dst, amount } => {
                // 步骤 2d: 收敛 emit/expr/dispatch.rs 里的 imm8/CL 移位.
                //   SHL/SAL: /4; SHR: /5; SAR: /7.
                //   imm8=1 时用 D1 /4 形式; 其余 imm8 用 C1 /4 + imm8; CL 用 D3 /4.
                let Operand::Reg(reg_idx, _) = dst else {
                    return None;
                };
                let modrm_reg_field: u8 = match op {
                    ShiftOp::Shl | ShiftOp::Sal => 0b100,
                    ShiftOp::Shr => 0b101,
                    ShiftOp::Sar => 0b111,
                    _ => return None,
                };
                let mut rex: u8 = 0x40;
                if matches!(width, Width::B64) {
                    rex |= 0x08;
                }
                if *reg_idx >= 8 {
                    rex |= 0x01; // REX.B (r/m 字段是目的寄存器)
                }
                let mut out = Vec::with_capacity(5);
                if matches!(width, Width::B16) {
                    out.push(0x66);
                }
                let need_rex = matches!(width, Width::B64) || *reg_idx >= 8;
                if need_rex {
                    out.push(rex);
                }
                let opc_byte: u8 = match amount {
                    ShiftAmount::Cl => 0xD3,
                    ShiftAmount::Imm8(1) => 0xD1, // imm=1 的短形式 (原代码一直这么用)
                    ShiftAmount::Imm8(_) => 0xC1,
                };
                out.push(opc_byte);
                out.push(0xC0 | (modrm_reg_field << 3) | (reg_idx & 7));
                if let ShiftAmount::Imm8(n) = amount {
                    if *n != 1 {
                        out.push(*n);
                    }
                }
                Some(out)
            }
            X86_64Opcode::Raw { bytes, .. } => Some(bytes.clone()),
            X86_64Opcode::MovRm { width, dst, src } => {
                // 步骤 5: 追加 reg->reg (mod=11) 形态.
                //   64-bit mov r64, r64:  REX.W=0x48 | [src>=8 ? 0x04] | [dst>=8 ? 0x01],
                //                         0x89, ModRM = mod=11 | src<<3 | dst.
                //   32-bit mov r32, r32:  无 REX.W, 若任一字段>=8 则 REX.B/R.
                //   16-bit: 66 前缀; 8-bit: 0x88/0x8A 对应形态 (spl/bpl/sil/dil 需 REX).
                //   位序与 mem 形态不同: 我们走 MOV r/m64, r64 (0x89), 即 dst 在 r/m,
                //   src 在 ModRM.reg. 与 0x8B (mov r64, r/m64) 是同一条指令的两种编码.
                if let (Operand::Reg(dst_idx, _), Operand::Reg(src_idx, _)) = (dst, src) {
                    let is_byte8 = matches!(width, Width::B8);
                    let is_word16 = matches!(width, Width::B16);
                    let is_b64 = matches!(width, Width::B64);
                    // 0x88=mov r/m8, r8 ; 0x89=mov r/m16/32/64, r16/32/64.
                    let opcode: u8 = if is_byte8 { 0x88 } else { 0x89 };
                    let mut rex: u8 = 0x40;
                    if is_b64 {
                        rex |= 0x08;
                    }
                    if *src_idx >= 8 {
                        rex |= 0x04; // ModRM.reg 字段是 src
                    }
                    if *dst_idx >= 8 {
                        rex |= 0x01; // ModRM.r/m 字段是 dst
                    }
                    // 8-bit 时 spl/bpl/sil/dil (4..7) 必须发 REX 让汇编器认成高字节.
                    let need_rex = is_b64
                        || *src_idx >= 8
                        || *dst_idx >= 8
                        || (is_byte8 && (*src_idx >= 4 || *dst_idx >= 4));
                    let mut out = Vec::with_capacity(5);
                    if is_word16 {
                        out.push(0x66);
                    }
                    if need_rex {
                        out.push(rex);
                    }
                    out.push(opcode);
                    // ModRM: mod=11, reg=src, r/m=dst.
                    out.push(0xC0 | ((*src_idx & 7) << 3) | (*dst_idx & 7));
                    return Some(out);
                }
                // 步骤 5: 追加 imm->reg 短形式 (B8+r id / BA+r id + imm64).
                //   mov r32, imm32:  B8+r (r<8) / 41 B8+(r&7) (r>=8) + imm32 LE.
                //   mov r64, imm64:  REX.W=0x48|B, B8+r + imm64 LE.
                if let (Operand::Reg(reg_idx, _), Operand::Imm(imm)) = (dst, src) {
                    let is_b64 = matches!(width, Width::B64);
                    let is_b32 = matches!(width, Width::B32);
                    if !is_b64 && !is_b32 {
                        // 8/16-bit 立即数 mov 是 C6/C7 形态, 不是 B8+r, 不在此步处理.
                        return None;
                    }
                    let mut out = Vec::with_capacity(11);
                    if is_b64 {
                        // P11: 若 imm 落在 32 位无符号范围 [0, 0xFFFFFFFF], 可以用
                        // 48 B8 + imm32 (零扩展) 编码为 7 字节, 而不是 10 字节 imm64.
                        let imm_u64 = *imm as i64 as u64;
                        let fits_imm32_zero_ext = imm_u64 <= 0xFFFF_FFFF;
                        let mut rex = 0x48u8;
                        if *reg_idx >= 8 {
                            rex |= 0x01;
                        }
                        out.push(rex);
                        out.push(0xB8 + (reg_idx & 7));
                        if fits_imm32_zero_ext {
                            out.extend_from_slice(&(imm_u64 as u32).to_le_bytes());
                        } else {
                            out.extend_from_slice(&(*imm as i64).to_le_bytes());
                        }
                    } else {
                        // B32
                        if *reg_idx >= 8 {
                            out.push(0x41);
                        }
                        out.push(0xB8 + (reg_idx & 7));
                        out.extend_from_slice(&(*imm as i32).to_le_bytes());
                    }
                    return Some(out);
                }
                // 步骤 2a: 实现 reg->mem (store: 89/88) 与 mem->reg (load: 8B/8A)
                // 两种形态. 其余 (imm->mem C6/C7) 留给后续.
                //
                // 位序参考:
                //   store: MOV r/m, r    (0x88=B8, 0x89=w; ModRM.reg=src, ModRM.r/m=mem)
                //   load:  MOV r, r/m   (0x8A=B8, 0x8B=w; ModRM.reg=dst, ModRM.r/m=mem)
                let (reg_idx, mem) = match (dst, src) {
                    (Operand::Mem(mem), Operand::Reg(reg, _)) => {
                        // store: reg -> mem
                        (*reg, mem)
                    }
                    (Operand::Reg(reg, _), Operand::Mem(mem)) => {
                        // load: mem -> reg
                        (*reg, mem)
                    }
                    _ => return None,
                };
                // 本步只支持 base-only (无 index / 无 seg). 有 index 就先回退手写.
                if mem.index.is_some() || mem.seg.is_some() {
                    return None;
                }
                let base = mem.base?;
                let is_store = matches!(dst, Operand::Mem(_));
                let (opcode, need_prefix66, need_rex_w) = match width {
                    Width::B8 => (if is_store { 0x88 } else { 0x8A }, false, false),
                    Width::B16 => (if is_store { 0x89 } else { 0x8B }, true, false),
                    Width::B32 => (if is_store { 0x89 } else { 0x8B }, false, false),
                    Width::B64 => (if is_store { 0x89 } else { 0x8B }, false, true),
                    _ => return None,
                };
                // REX: 本步只处理 store_rsp_qword / load_rsp_qword, 其 base=rsp(=4),
                // 所以 REX.B 只来自 base>=8; 但通用实现里两个字段都算.
                let mut rex: u8 = 0x40;
                if need_rex_w {
                    rex |= 0x08;
                }
                if reg_idx >= 8 {
                    rex |= 0x04; // REX.R
                }
                if base >= 8 {
                    rex |= 0x01; // REX.B
                }
                // 判断是否需要真的发 REX 前缀: 没有任何字段被扩展, 且 width!=B64,
                // 那就保持裸 opcode (避免多一个 0x40 前缀). B64 总是发 REX.W.
                let need_rex = need_rex_w || reg_idx >= 8 || base >= 8;

                // ModRM / SIB / disp: base=rsp(4) 必须发 SIB; 其余 base 走普通 ModRM.
                // base=rbp(5) 在 mod=00 时是 rip-relative, 但我们这里永远带 disp,
                // 所以 mod=01/10 时 base=rbp 是合法的 [rbp+disp] 不需要额外前缀.
                let disp = mem.disp;
                let modrm_mod: u8;
                let disp_bytes: [u8; 4];
                let disp_len: usize;
                if disp == 0 && base != 5 {
                    // mod=00, 无 disp
                    modrm_mod = 0;
                    disp_len = 0;
                    disp_bytes = [0; 4];
                } else if let Ok(d) = i8::try_from(disp) {
                    // mod=01, disp8
                    modrm_mod = 1;
                    disp_len = 1;
                    disp_bytes = [d as u8, 0, 0, 0];
                } else {
                    // mod=10, disp32
                    modrm_mod = 2;
                    disp_len = 4;
                    disp_bytes = disp.to_le_bytes();
                }
                let reg_field = reg_idx & 7;
                let rm_field = base & 7;

                let mut out = Vec::with_capacity(16);
                if need_prefix66 {
                    out.push(0x66);
                }
                if need_rex {
                    out.push(rex);
                }
                out.push(opcode);
                // ModRM 字节: mod | reg_field | rm_field.
                // rm_field==4 (rsp/r12) 时必须跟一个 SIB.
                let modrm = (modrm_mod << 6) | (reg_field << 3) | rm_field;
                out.push(modrm);
                if rm_field == 4 {
                    // SIB: scale=0 (无 index), index=4 (ESP 编码 = no index), base=rm_field.
                    out.push(0x24);
                }
                out.extend_from_slice(&disp_bytes[..disp_len]);
                Some(out)
            }
            X86_64Opcode::MovExt { sign, from, to, dst, src } => {
                // P11: 收敛 movzx/movsx/movsxd 的 reg-reg 与 reg-mem 形态.
                //   MOVZX r32, r/m8 (B8):  0F B6 /r
                //   MOVZX r32, r/m16 (B16): 0F B7 /r
                //   MOVSX r32, r/m8 (B8):  0F BE /r
                //   MOVSX r32, r/m16 (B16): 0F BF /r
                //   MOVSXD r64, r/m32 (B64): 63 /r (REX.W=1)
                // dst ModRM.reg; src ModRM.r/m (reg 或 mem).
                use crate::codegen::shared::asm_operand::Width as W;
                // 先确定 escap / 低 opcode / 是否需要 REX.W=1.
                let (escap, low, rex_w): (u8, u8, bool) = match (sign, from, to) {
                    // movzx 到 r32
                    (false, W::B8, W::B32) => (0x0F, 0xB6, false),
                    (false, W::B16, W::B32) => (0x0F, 0xB7, false),
                    // movsx 到 r32
                    (true, W::B8, W::B32) => (0x0F, 0xBE, false),
                    (true, W::B16, W::B32) => (0x0F, 0xBF, false),
                    // movsxd 到 r64 (from r/m32)
                    (true, W::B32, W::B64) => (0x00, 0x63, true),
                    // movsx/movzx 到 r64 (from B8/B16) 走同样 escap, REX.W=1
                    (false, W::B8, W::B64) => (0x0F, 0xB6, true),
                    (false, W::B16, W::B64) => (0x0F, 0xB7, true),
                    (true, W::B8, W::B64) => (0x0F, 0xBE, true),
                    (true, W::B16, W::B64) => (0x0F, 0xBF, true),
                    _ => return None,
                };
                match (dst, src) {
                    (Operand::Reg(dst_idx, _), Operand::Reg(src_idx, _)) => {
                        // REX: W=1 (B64 目标), R=dst>=8, B=src>=8.
                        // 额外: B8 源 (spl/bpl/sil/dil, src&7 ∈ {4,5,6,7}) 需 REX 让汇编器认成高字节.
                        let mut rex: u8 = 0x40;
                        if rex_w {
                            rex |= 0x08;
                        }
                        if *dst_idx >= 8 {
                            rex |= 0x04;
                        }
                        if *src_idx >= 8 {
                            rex |= 0x01;
                        }
                        let need_rex = rex_w
                            || *dst_idx >= 8
                            || *src_idx >= 8
                            || (*from == W::B8 && *src_idx >= 4);
                        let mut out = Vec::with_capacity(6);
                        if need_rex {
                            out.push(rex);
                        }
                        if escap != 0 {
                            out.push(escap);
                        }
                        out.push(low);
                        out.push(0xC0 | ((*dst_idx & 7) << 3) | (*src_idx & 7));
                        Some(out)
                    }
                    (Operand::Reg(dst_idx, _), Operand::Mem(mem)) => {
                        // mem -> reg (load). 有 index/seg 就回退手写 (本步只接 base+disp).
                        if mem.index.is_some() || mem.seg.is_some() {
                            return None;
                        }
                        let base = match mem.base {
                            Some(b) => b,
                            None => return None,
                        };
                        // REX: W (B64), R=dst>=8, B=base>=8.
                        let mut rex: u8 = 0x40;
                        if rex_w {
                            rex |= 0x08;
                        }
                        if *dst_idx >= 8 {
                            rex |= 0x04;
                        }
                        if base >= 8 {
                            rex |= 0x01;
                        }
                        let need_rex = rex_w || *dst_idx >= 8 || base >= 8;
                        // ModRM mod / disp 长度: base=rbp(5) 在 mod=00 时是 rip-relative,
                        // 这里只接显式 disp, 因此 disp=0 且 base=5 时走 disp32 (mod=10) 以
                        // 避免被解释成 [rip+disp32].
                        let disp = mem.disp;
                        let (modrm_mod, disp_len, disp_bytes): (u8, usize, [u8; 4]) =
                            if disp == 0 && base != 5 {
                                (0, 0, [0; 4])
                            } else if let Ok(d) = i8::try_from(disp) {
                                (1, 1, [d as u8, 0, 0, 0])
                            } else {
                                (2, 4, disp.to_le_bytes())
                            };
                        let reg_field = *dst_idx & 7;
                        let rm_field = base & 7;
                        let mut out = Vec::with_capacity(16);
                        if need_rex {
                            out.push(rex);
                        }
                        if escap != 0 {
                            out.push(escap);
                        }
                        out.push(low);
                        let modrm = (modrm_mod << 6) | (reg_field << 3) | rm_field;
                        out.push(modrm);
                        if rm_field == 4 {
                            // SIB: scale=0, index=4 (no index), base=rm_field.
                            out.push(0x24);
                        }
                        out.extend_from_slice(&disp_bytes[..disp_len]);
                        Some(out)
            }
            _ => None,
                }
            }
            X86_64Opcode::ImulRm { width, dst, src } => {
                // P11: IMUL r, r/m — 0F AF /r (reg-reg 形态).
                //   B32: 0F AF /r (无 REX.W)
                //   B64: REX.W(0x48) 0F AF /r
                // dst ModRM.reg; src ModRM.r/m. 内存形态暂不支持 (返回 None).
                let (Operand::Reg(dst_idx, _), Operand::Reg(src_idx, _)) = (dst, src) else {
                    return None;
                };
                let is_b64 = matches!(width, Width::B64);
                let is_b32 = matches!(width, Width::B32);
                if !is_b64 && !is_b32 {
                    return None;
                }
                let mut rex: u8 = 0x40;
                if is_b64 {
                    rex |= 0x08;
                }
                if *dst_idx >= 8 {
                    rex |= 0x04;
                }
                if *src_idx >= 8 {
                    rex |= 0x01;
                }
                let need_rex = is_b64 || *dst_idx >= 8 || *src_idx >= 8;
                let mut out = Vec::with_capacity(5);
                if need_rex {
                    out.push(rex);
                }
                out.push(0x0F);
                out.push(0xAF);
                out.push(0xC0 | ((*dst_idx & 7) << 3) | (*src_idx & 7));
                Some(out)
            }
            X86_64Opcode::Stack { op, width, dst } => {
                // 步骤 3: 收敛 push(reg) / pop(reg) — 硬件 PUSH/POP reg64 短形式.
                //   PUSH r64:  50+rd  (rd<8) / 41 50+(rd&7)  (rd>=8, REX.B=0x41)
                //   POP  r64:  51+rd  (rd<8) / 41 51+(rd&7)  (rd>=8, REX.B=0x41)
                // 原 push()/pop() 在 instructions.rs 里按 reg 号 if/else 展开 16 个分支,
                // 每分支一字节或两字节. 这里按同一规则合并: 只支持 B64 reg 形态
                // (其他形态/内存/立即数 push 留待后续, 本步不覆盖).
                let (Operand::Reg(reg_idx, _), Width::B64) = (dst, width) else {
                    return None;
                };
                if *reg_idx >= 16 {
                    return None;
                }
                let base_opc: u8 = match op {
                    StackOp::Push => 0x50,
                    StackOp::Pop => 0x58,
                };
                if *reg_idx >= 8 {
                    Some(vec![0x41, base_opc + (reg_idx & 7)])
                } else {
                    Some(vec![base_opc + reg_idx])
                }
            }
            X86_64Opcode::Ret { add_imm } => {
                // 步骤 3: RET / RET imm16.
                //   RET:     C3
                //   RET n:   C2 imm16-LE
                match add_imm {
                    None => Some(vec![0xC3]),
                    Some(n) => {
                        let mut out = Vec::with_capacity(3);
                        out.push(0xC2);
                        out.extend_from_slice(&n.to_le_bytes());
                        Some(out)
                    }
                }
            }
            X86_64Opcode::Jmp { target, pending_rel } => {
                // 步骤 3: 只收敛**间接**形态 `jmp *rm` (FF /4).
                // rel32 直接跳转 (E9 rel32) 涉及 PendingJump 回填, 本步跳过以降风险.
                if *pending_rel {
                    return None;
                }
                let Operand::Reg(reg_idx, _) = target else {
                    return None;
                };
                // FF /4 ModRM: mod=11, reg_field=4 (/4), r/m=reg&7.
                // reg>=8 需要 REX.B=0x41.
                let mut out = Vec::with_capacity(3);
                if *reg_idx >= 8 {
                    out.push(0x41);
                }
                out.push(0xFF);
                out.push(0xE0 | (reg_idx & 7)); // mod=11, reg=4 (/4), r/m=reg&7
                Some(out)
            }
            X86_64Opcode::Call { target, pending_rel } => {
                // 步骤 3: 只收敛**间接**形态 `call *rm` (FF /2).
                // rel32 直接 call (E8 rel32) 走 PendingJump/pending_func_rels, 本步跳过.
                if *pending_rel {
                    return None;
                }
                let Operand::Reg(reg_idx, _) = target else {
                    return None;
                };
                // FF /2 ModRM: mod=11, reg_field=2 (/2), r/m=reg&7.
                let mut out = Vec::with_capacity(3);
                if *reg_idx >= 8 {
                    out.push(0x41);
                }
                out.push(0xFF);
                out.push(0xD0 | (reg_idx & 7)); // mod=11, reg=2, r/m=reg&7
                Some(out)
            }
            X86_64Opcode::X87 { op, mem } => {
                // 步骤 4: 两类形态.
                //
                // (1) 无操作数固定 escap 序列: mem=None.
                //     例 fld1 = D9 E8, fldz = D9 EE, fchs = D9 E0,
                //     fstp st(0) = DD D9, fucomip st(0), st(1) = DF E9,
                //     faddp/fsubrp/fmulp/fdivrp = DE C1/E9/C9/F9.
                //
                // (2) 单内存操作数 (fld/fstp/fild [base+disp32]): mem=Some.
                //     位序按 x87_floating_point.rs::emit_x87_mem_at 逐字节对齐:
                //       REX (可选, base>=8 或 REX.W) + escap + ModRM/SIB + disp32
                match op {
                    X87Op::Fixed { escap, modrm } => {
                        if mem.is_some() {
                            return None;
                        }
                        Some(vec![*escap, *modrm])
                    }
                    X87Op::FldMem { width }
                    | X87Op::FstpMem { width }
                    | X87Op::FildMem { width } => {
                        // mem 是 &Option<MemOperand>, 不能用 ? 运算符解引用.
                        #[allow(clippy::question_mark)]
                        let mem = match mem {
                            Some(m) => m,
                            None => return None,
                        };
                        if mem.index.is_some() || mem.seg.is_some() {
                            return None;
                        }
                        let base = mem.base?;
                        if base == 12 {
                            // r12 需要 SIB, 原 emit_x87_mem_at 已处理但 r12 极少用;
                            // 本步跳过以降低复杂度.
                            return None;
                        }
                        // escap 字节 + reg 位段.
                        let (escap, reg_field, rexw): (u8, u8, bool) = match (op, *width) {
                            (X87Op::FldMem { .. }, 4) => (0xD9, 0, false),   // fld m32fp
                            (X87Op::FldMem { .. }, 8) => (0xDD, 0, false),   // fld m64fp
                            (X87Op::FldMem { .. }, 10) => (0xDB, 5, false),  // fld m80fp
                            (X87Op::FstpMem { .. }, 4) => (0xD9, 3, false),   // fstp m32fp
                            (X87Op::FstpMem { .. }, 8) => (0xDD, 3, false),   // fstp m64fp
                            (X87Op::FstpMem { .. }, 10) => (0xDB, 7, false),  // fstp m80fp
                            (X87Op::FildMem { .. }, 4) => (0xDB, 0, false),  // fild m32int
                            (X87Op::FildMem { .. }, 8) => (0xDB, 0, true),    // fild m64int
                            _ => return None,
                        };
                        // REX: base>=8 → REX.B; rexw → REX.W.
                        let mut rex: u8 = 0x40;
                        if base >= 8 {
                            rex |= 0x01;
                        }
                        if rexw {
                            rex |= 0x08;
                        }
                        let disp = mem.disp;
                        let mut out = Vec::with_capacity(16);
                        // 0x40 冗余 REX 不发 (与原 emit_x87_mem_at 一致).
                        if rex != 0x40 {
                            out.push(rex);
                        }
                        out.push(escap);
                        // rm==100 (rsp=4) 必须插 SIB; r13=13 (低3位=5) 在原代码里也走 SIB.
                        if base == 4 || base == 13 {
                            out.push(0x84 | (reg_field << 3));
                            out.push(0x20 | (base & 7));
                        } else {
                            out.push(0x80 | (reg_field << 3) | (base & 7));
                        }
                        out.extend_from_slice(&disp.to_le_bytes());
                        Some(out)
                    }
                    X87Op::MxcsrMem { store } | X87Op::FcwMem { store } => {
                        let _ = store;
                        #[allow(clippy::question_mark)]
                        let mem = match mem {
                            Some(m) => m,
                            None => return None,
                        };
                        if mem.index.is_some() || mem.seg.is_some() {
                            return None;
                        }
                        let base = mem.base?;
                        if base == 12 {
                            return None;
                        }
                        // escap + reg_field (mod=00, SIB if rm==100) + disp.
                        let (escap, reg_field): (u8, u8) = match op {
                            X87Op::MxcsrMem { store: true } => (0xAE, 3),   // stmxcsr
                            X87Op::MxcsrMem { store: false } => (0xAE, 2),  // ldmxcsr
                            X87Op::FcwMem { store: true } => (0xD9, 7),     // fnstcw
                            X87Op::FcwMem { store: false } => (0xD9, 5),    // fldcw
                            _ => return None,
                        };
                        // 0F AE needs a 0x0F escape byte before escap.
                        let mut out = Vec::with_capacity(16);
                        // REX.B 仅当基址是扩展寄存器 (r8-r15) 时需要；/2 /3 /5 /7
                        // 都是 opcode 扩展位（非寄存器），不影响 REX。
                        if base >= 8 {
                            out.push(0x41);
                        }
                        if escap == 0xAE {
                            out.push(0x0F);
                        }
                        out.push(escap);
                        // 统一按 mod=10 (disp32) 编码，与通用 X87 mem 分支一致：
                        // 旧实现误发 mod=00/10 混合 + 缺 rm=100 的 SIB，指令字节长度
                        // 与实际不符，导致 stmxcsr/ldmxcsr 之后整条指令流错位（SIGSEGV）。
                        let disp = mem.disp;
                        if base == 4 || base == 13 {
                            // mod=10, rm=100 → 有 SIB；SIB base=rsp(100)/rbp(101)
                            out.push(0x84 | (reg_field << 3));
                            out.push(0x20 | (base & 7));
                        } else {
                            out.push(0x80 | (reg_field << 3) | (base & 7));
                        }
                        out.extend_from_slice(&(disp as i32).to_le_bytes());
                        Some(out)
                    }
                }
            }
            X86_64Opcode::SimdMov { width, opcode10, dst, src } => {
                // P11 Bucket 6: reg↔reg xmm↔xmm (mod=11).
                //   dst=Reg(dst_xmm, Xmm), src=Reg(src_xmm, Xmm).
                //   opcode2 = 0x10 (load) / 0x11 (store direction).
                //   P11 Bucket G: opcode10=false, opcode2=0x6F (movdqu load) / 0x7F (movdqu store).
                //   prefix: F3=SS, F2=SD, 66=PD/DQ, none=PS/O.
                if let (Operand::Reg(dst_xmm, _), Operand::Reg(src_xmm, _)) = (dst, src) {
                    let (pfx, opc2): (u8, u8) = match (width, *opcode10) {
                        // movss/movsd reg-reg
                        (SimdWidth::SS, true) => (0xF3, 0x10),
                        (SimdWidth::SS, false) => (0xF3, 0x11),
                        (SimdWidth::SD, true) => (0xF2, 0x10),
                        (SimdWidth::SD, false) => (0xF2, 0x11),
                        // P11 Bucket G: movaps(PS/O)/movapd(PD)/movdqa(DQ) load/store reg-reg.
                        //   movaps: 无 pfx, 0F 28 (load) / 0F 29 (store).
                        //   movapd/movdqa: 66 pfx, 0F 28 / 0F 29.
                        //   movdqu load: F3 pfx, 0F 6F; store: F3 pfx, 0F 7F.
                        (SimdWidth::PS | SimdWidth::O, true) => (0x00, 0x28),
                        (SimdWidth::PS | SimdWidth::O, false) => (0x00, 0x29),
                        (SimdWidth::PD, true) => (0x66, 0x28),
                        (SimdWidth::PD, false) => (0x66, 0x29),
                        (SimdWidth::DQ, true) => (0x66, 0x6F),
                        (SimdWidth::DQ, false) => (0x66, 0x7F),
                        // P11 Bucket B: B/W/D/Q widths are only valid in SimdArith,
                        // not SimdMov; reject to keep this match exhaustive.
                        _ => return None,
                    };
                    let need_rex = *dst_xmm >= 8 || *src_xmm >= 8;
                    let mut out = Vec::with_capacity(6);
                    if pfx != 0 {
                        out.push(pfx);
                    }
                    if need_rex {
                        let mut rex: u8 = 0x40;
                        if *dst_xmm >= 8 { rex |= 0x04; }
                        if *src_xmm >= 8 { rex |= 0x01; }
                        out.push(rex);
                    }
                    out.push(0x0F);
                    out.push(opc2);
                    let modrm: u8 = 0xC0 | ((dst_xmm & 0x7) << 3) | (src_xmm & 0x7);
                    out.push(modrm);
                    return Some(out);
                }
                // P11 Bucket C: 收敛 xmm store/load 到 [base+disp] (base 任意, 含 rsp).
                //   load:  dst=Reg(xmm, B128), src=Mem(base, disp)
                //   store: dst=Mem(base, disp), src=Reg(xmm, B128)
                // 与 emit_xmm_store_rsp / emit_xmm_load_rsp 逐字节对齐.
                let (xmm_idx, mem, is_store) = match (dst, src) {
                    (Operand::Mem(mem), Operand::Reg(xmm, _)) => (*xmm, mem, true),
                    (Operand::Reg(xmm, _), Operand::Mem(mem)) => (*xmm, mem, false),
                    _ => return None,
                };
                if mem.index.is_some() || mem.seg.is_some() {
                    return None;
                }
                let base = mem.base?;
                // legacy 前缀: SS=F3, SD=F2, O/PS=无. PD/DQ 本路径按 66 处理.
                let pfx: u8 = match width {
                    SimdWidth::SS => 0xF3,
                    SimdWidth::SD => 0xF2,
                    SimdWidth::PD => 0x66,
                    SimdWidth::DQ => 0x66,
                    SimdWidth::O | SimdWidth::PS => 0x00,
                    _ => return None,
                };
                // REX: xmm>=8 → REX.R=0x04; base>=8 → REX.B=0x01.
                let mut rex: u8 = 0x40;
                let mut need_rex = false;
                if xmm_idx >= 8 {
                    rex |= 0x04;
                    need_rex = true;
                }
                if base >= 8 {
                    rex |= 0x01;
                    need_rex = true;
                }
                // is_store 决定 opcode2: store=0x11/0x29/0x7F, load=0x10/0x28/0x6F.
                //  (opcode10 字段仅用于 reg↔reg 分支; mem 分支靠 is_store 选择.)
                let opc2: u8 = if is_store {
                    match width {
                        SimdWidth::DQ => 0x7F,
                        SimdWidth::PD => 0x29,
                        _ => 0x11,
                    }
                } else {
                    match width {
                        SimdWidth::DQ => 0x6F,
                        SimdWidth::PD => 0x28,
                        _ => 0x10,
                    }
                };
                let disp = mem.disp;
                // ModRM mod / disp 长度. base=rbp(5) 在 mod=00 是 rip-relative,
                // 所以 disp=0 且 base=5 时走 mod=01 (disp8=0) 避免被解释成 rip.
                // base=rsp(4) 必须 SIB; 当 disp=0 时 SIB(base=rsp,index=none) 配合
                // mod=00 即编码为 `modrm=0x04|xmm<<3, sib=0x24` (G10 快照约定).
                let (modrm_mod, disp_bytes): (u8, &[u8]) = if disp == 0 && base != 5 {
                    (0, &[])
                } else if let Ok(d) = i8::try_from(disp) {
                    (1, &[d as u8])
                } else {
                    (2, &disp.to_le_bytes())
                };
                let rm_field = base & 7;
                let mut out = Vec::with_capacity(16);
                if pfx != 0 {
                    out.push(pfx);
                }
                if need_rex {
                    out.push(rex);
                }
                out.push(0x0F);
                out.push(opc2);
                // ModRM: mod | reg(xmm) | rm(base). rm==4 时必须 SIB.
                let modrm = (modrm_mod << 6) | ((xmm_idx & 7) << 3) | rm_field;
                out.push(modrm);
                if rm_field == 4 {
                    out.push(0x24); // SIB: scale=0, index=4 (none), base=rsp&7.
                }
                out.extend_from_slice(disp_bytes);
                Some(out)
            }
            X86_64Opcode::SimdArith { op, width, dst, src } => {
                // P11 Bucket C+E: SIMD scalar arith reg-reg (addsd/subss etc) + xorps/xorpd.
                //   addsd: F2 0F 58 /r   subsd: F2 0F 5C /r   mulsd: F2 0F 59 /r   divsd: F2 0F 5E /r
                //   addss: F3 0F 58 /r   subss: F3 0F 5C /r   mulss: F3 0F 59 /r   divss: F3 0F 5E /r
                //   xorps: 0F 57 /r (无 legacy 前缀)   xorpd: 66 0F 57 /r
                let (Operand::Reg(dst_xmm, _), Operand::Reg(src_xmm, _)) = (dst, src) else {
                    return None;
                };
                let (pfx, opc2): (u8, u8) = match (op, width) {
                    (SimdArithOp::Add, SimdWidth::SD) => (0xF2, 0x58),
                    (SimdArithOp::Sub, SimdWidth::SD) => (0xF2, 0x5C),
                    (SimdArithOp::Mul, SimdWidth::SD) => (0xF2, 0x59),
                    (SimdArithOp::Div, SimdWidth::SD) => (0xF2, 0x5E),
                    (SimdArithOp::Add, SimdWidth::SS) => (0xF3, 0x58),
                    (SimdArithOp::Sub, SimdWidth::SS) => (0xF3, 0x5C),
                    (SimdArithOp::Mul, SimdWidth::SS) => (0xF3, 0x59),
                    (SimdArithOp::Div, SimdWidth::SS) => (0xF3, 0x5E),
                    (SimdArithOp::Xorps, _) => (0x00, 0x57),
                    (SimdArithOp::Xorpd, _) => (0x66, 0x57),
                    // P11 Bucket A: ucomiss/ucomisd/comiss/comisd reg-reg.
                    //   ucomiss xmm1,xmm0:  0F 2E C8
                    //   ucomisd xmm1,xmm0:  66 0F 2E C8
                    //   comiss xmm1,xmm0:   0F 2F C8
                    //   comisd xmm1,xmm0:   66 0F 2F C8
                    (SimdArithOp::Ucomi, SimdWidth::SS) => (0x00, 0x2E),
                    (SimdArithOp::Ucomi, SimdWidth::SD) => (0x66, 0x2E),
                    (SimdArithOp::Comi, SimdWidth::SS) => (0x00, 0x2F),
                    (SimdArithOp::Comi, SimdWidth::SD) => (0x66, 0x2F),
                    // P11 Bucket F: pand/por/pxor reg-reg (all use 66 prefix + 128-bit).
                    //   pand xmm0,xmm1:  66 0F DB C1
                    //   por  xmm0,xmm1:  66 0F EB C1
                    //   pxor xmm0,xmm1:  66 0F EF C1
                    (SimdArithOp::Pand, _) => (0x66, 0xDB),
                    (SimdArithOp::Por, _) => (0x66, 0xEB),
                    (SimdArithOp::Pxor, _) => (0x66, 0xEF),
                    // P11 Bucket E: sqrtsd/sqrtss reg-reg.
                    //   sqrtsd xmm0,xmm1:  F2 0F 51 C1
                    //   sqrtss xmm0,xmm1:  F3 0F 51 C1
                    (SimdArithOp::Sqrt, SimdWidth::SD) => (0xF2, 0x51),
                    (SimdArithOp::Sqrt, SimdWidth::SS) => (0xF3, 0x51),
                    // P11 Bucket C: cvtsd2ss/cvtss2sd reg-reg.
                    //   cvtsd2ss xmm0,xmm1:  F2 0F 5A C1
                    //   cvtss2sd xmm0,xmm1:  F3 0F 5A C1
                    (SimdArithOp::CvtSdSs, SimdWidth::SD) => (0xF2, 0x5A),
                    (SimdArithOp::CvtSdSs, SimdWidth::SS) => (0xF3, 0x5A),
                    // P11 Bucket J: andps/andpd reg-reg.
                    //   andps xmm0,xmm1:  0F 54 C1
                    //   andpd xmm0,xmm1:  66 0F 54 C1
                    (SimdArithOp::Andps, _) => (0x00, 0x54),
                    (SimdArithOp::Andpd, _) => (0x66, 0x54),
                    // P11 Bucket A2: maxsd/maxss/minsd/minss scalar.
                    //   maxsd xmm0,xmm1: F2 0F 5F C1   maxss xmm0,xmm1: F3 0F 5F C1
                    //   minsd xmm0,xmm1: F2 0F 5D C1   minss xmm0,xmm1: F3 0F 5D C1
                    (SimdArithOp::Max, SimdWidth::SD) => (0xF2, 0x5F),
                    (SimdArithOp::Max, SimdWidth::SS) => (0xF3, 0x5F),
                    (SimdArithOp::Min, SimdWidth::SD) => (0xF2, 0x5D),
                    (SimdArithOp::Min, SimdWidth::SS) => (0xF3, 0x5D),
                    // P11 Bucket A2: orps/orpd bitwise OR.
                    //   orps xmm0,xmm1: 0F 56 C1   orpd xmm0,xmm1: 66 0F 56 C1
                    (SimdArithOp::Orps, _) => (0x00, 0x56),
                    (SimdArithOp::Orpd, _) => (0x66, 0x56),
                    // P11 Bucket B: packed float arith (addps/subps/mulps/divps / pd).
                    //   addps 0F 58, subps 0F 5C, mulps 0F 59, divps 0F 5E
                    //   addpd 66 0F 58, subpd 66 0F 5C, mulpd 66 0F 59, divpd 66 0F 5E
                    (SimdArithOp::Addps, _) => (0x00, 0x58),
                    (SimdArithOp::Subps, _) => (0x00, 0x5C),
                    (SimdArithOp::Mulps, _) => (0x00, 0x59),
                    (SimdArithOp::Divps, _) => (0x00, 0x5E),
                    (SimdArithOp::Addpd, _) => (0x66, 0x58),
                    (SimdArithOp::Subpd, _) => (0x66, 0x5C),
                    (SimdArithOp::Mulpd, _) => (0x66, 0x59),
                    (SimdArithOp::Divpd, _) => (0x66, 0x5E),
                    // P11 Bucket B: packed integer add/sub (paddb/w/d/q, psubb/w/d/q).
                    //   All use 66 prefix; opcode varies by element width.
                    (SimdArithOp::PAdd, SimdWidth::B) => (0x66, 0xFC),
                    (SimdArithOp::PAdd, SimdWidth::W) => (0x66, 0xFD),
                    (SimdArithOp::PAdd, SimdWidth::D) => (0x66, 0xFE),
                    (SimdArithOp::PAdd, SimdWidth::Q) => (0x66, 0xD4),
                    (SimdArithOp::PSub, SimdWidth::B) => (0x66, 0xF8),
                    (SimdArithOp::PSub, SimdWidth::W) => (0x66, 0xF9),
                    (SimdArithOp::PSub, SimdWidth::D) => (0x66, 0xFA),
                    (SimdArithOp::PSub, SimdWidth::Q) => (0x66, 0xFB),
                    _ => return None,
                };
                let mut rex: u8 = 0x40;
                if *dst_xmm >= 8 { rex |= 0x04; }
                if *src_xmm >= 8 { rex |= 0x01; }
                let need_rex = *dst_xmm >= 8 || *src_xmm >= 8;
                let mut out = Vec::with_capacity(6);
                if pfx != 0 { out.push(pfx); }
                if need_rex { out.push(rex); }
                out.push(0x0F);
                out.push(opc2);
                out.push(0xC0 | ((*dst_xmm & 7) << 3) | (*src_xmm & 7));
                Some(out)
            }
            X86_64Opcode::ShldShrd { is_shld, width, dst, src } => {
                // P11 Bucket 7: shld/shrd reg-reg with CL count.
                //  shld r64,r64,cl:  REX.W 0F A5 modrm(mod=11,reg=src,r/m=dst)
                //  shrd r64,r64,cl:  REX.W 0F AD modrm(mod=11,reg=src,r/m=dst)
                let (dst_idx, src_idx) = match (dst, src) {
                    (Operand::Reg(d, _), Operand::Reg(s, _)) => (d, s),
                    _ => return None,
                };
                let is_b64 = matches!(width, Width::B64);
                let mut rex: u8 = 0x40;
                if is_b64 { rex |= 0x08; }
                if *src_idx >= 8 { rex |= 0x04; }
                if *dst_idx >= 8 { rex |= 0x01; }
                let need_rex = is_b64 || *src_idx >= 8 || *dst_idx >= 8;
                let opc2: u8 = if *is_shld { 0xA5 } else { 0xAD };
                let modrm: u8 = 0xC0 | ((src_idx & 7) << 3) | (dst_idx & 7);
                let mut out = Vec::with_capacity(5);
                if need_rex { out.push(rex); }
                out.push(0x0F);
                out.push(opc2);
                out.push(modrm);
                Some(out)
            }
            X86_64Opcode::Cvt2Sd { to_xmm, width, dst, src } => {
                // P11 Bucket B: cvtsi2sd/cvtsi2ss (r64->xmm) / cvttsd2si/cvttss2si (xmm->r64).
                //   cvtsi2sd r64,xmm:  F2 REX.W 0F 2A /r  modrm: reg=xmm, r/m=r64
                //   cvtsi2ss r64,xmm:  F3 REX.W 0F 2A /r
                //   cvttsd2si r64,xmm: F2 REX.W 0F 2C /r  modrm: reg=xmm, r/m=r64
                //   cvttss2si r64,xmm: F3 REX.W 0F 2C /r
                // Intel: modrm.reg = SSE operand, modrm.r/m = GPR.
                let (Operand::Reg(dst_idx, _), Operand::Reg(src_idx, _)) = (dst, src) else {
                    return None;
                };
                let pfx: u8 = match width {
                    SimdWidth::SD => 0xF2,
                    SimdWidth::SS => 0xF3,
                    _ => return None,
                };
                // In both directions reg field (reg_xmm) goes in ModRM.reg slot,
                // r64 in r/m slot. Determine which operand is xmm vs r64 by to_xmm flag.
                let (xmm_idx, gpr_idx) = if *to_xmm {
                    // dst=xmm, src=r64. modrm.reg=xmm(dst), r/m=gpr(src).
                    (*dst_idx, *src_idx)
                } else {
                    // dst=r64, src=xmm. modrm.reg=xmm(src), r/m=gpr(dst).
                    (*src_idx, *dst_idx)
                };
                let opc2: u8 = if *to_xmm { 0x2A } else { 0x2C };
                let mut rex: u8 = 0x48; // REX.W always (64-bit GPR operand).
                if xmm_idx >= 8 { rex |= 0x04; }
                if gpr_idx >= 8 { rex |= 0x01; }
                let modrm: u8 = 0xC0 | ((xmm_idx & 7) << 3) | (gpr_idx & 7);
                let mut out = Vec::with_capacity(6);
                out.push(pfx);
                out.push(rex);
                out.push(0x0F);
                out.push(opc2);
                out.push(modrm);
                Some(out)
            }
            X86_64Opcode::SimdRound { width, imm8, dst, src } => {
                // P11 Bucket D: roundsd/roundss reg-reg with imm8.
                //   roundsd xmm0,xmm0,imm: 66 0F 3A 0B /r ib
                //   roundss xmm0,xmm0,imm: 66 0F 3A 0A /r ib
                let (Operand::Reg(dst_idx, _), Operand::Reg(src_idx, _)) = (dst, src) else {
                    return None;
                };
                let opc2: u8 = match width {
                    SimdWidth::SD => 0x0B,
                    SimdWidth::SS => 0x0A,
                    _ => return None,
                };
                // REX if xmm>=8 (reg field).
                let need_rex = *dst_idx >= 8 || *src_idx >= 8;
                let mut out = Vec::with_capacity(8);
                out.push(0x66);
                if need_rex {
                    let mut rex: u8 = 0x40;
                    if *dst_idx >= 8 { rex |= 0x04; }
                    if *src_idx >= 8 { rex |= 0x01; }
                    out.push(rex);
                }
                out.push(0x0F);
                out.push(0x3A);
                out.push(opc2);
                out.push(0xC0 | ((*dst_idx & 7) << 3) | (*src_idx & 7));
                out.push(*imm8);
                Some(out)
            }
            X86_64Opcode::Setcc { cc, dst } => {
                // P11 Bucket H: setcc r8 (0F 9x /0, mod=11).
                //   sete al = 0F 94 C0; setne bl = 0F 95 C3; setb cl = 0F 92 C1.
                let Operand::Reg(reg_idx, _) = dst else {
                    return None;
                };
                let opc2: u8 = match cc {
                    CondCode::Eq => 0x94,
                    CondCode::Ne => 0x95,
                    CondCode::B => 0x92,
                    CondCode::Ae => 0x93,
                    CondCode::Be => 0x96,
                    CondCode::A => 0x97,
                    CondCode::P => 0x9A,
                    CondCode::Np => 0x9B,
                    CondCode::Lt => 0x9C,
                    CondCode::Ge => 0x9D,
                    CondCode::Gt => 0x9F,
                    CondCode::Le => 0x9E,
                    _ => return None,
                };
                // Regs 8..15 (sil/dil/bpl etc) require REX prefix to avoid
                // interpreting as ah/ch/dh/bh. We always emit REX.B set iff idx>=8.
                let mut out = Vec::with_capacity(5);
                if *reg_idx >= 8 {
                    out.push(0x41);
                }
                out.push(0x0F);
                out.push(opc2);
                // mod=11, /0 reg field=0, r/m=reg_idx&7
                out.push(0xC0 | (reg_idx & 7));
                Some(out)
            }
            // P11 Bucket I: 无操作数/固定前缀指令.
            X86_64Opcode::NoArg { op } => {
                let bytes: &[u8] = match op {
                    NoArgOp::Cld => &[0xFC],
                    NoArgOp::Mfence => &[0x0F, 0xAE, 0xF0],
                    NoArgOp::RepMovsb => &[0xF3, 0xA4],
                    NoArgOp::RepStosb => &[0xF3, 0xAA],
                    NoArgOp::RepMovsq => &[0xF3, 0x48, 0xA5],
                };
                Some(bytes.to_vec())
            }
            // P11 Bucket K: movd/movq GPR↔XMM reg-reg.
            X86_64Opcode::GprXmm { to_xmm, width, dst, src } => {
                let (Operand::Reg(dst_idx, _), Operand::Reg(src_idx, _)) = (dst, src) else {
                    return None;
                };
                // to_xmm=true: GPR→XMM (0x6E). to_xmm=false: XMM→GPR (0x7E).
                // modrm.reg=xmm (dst if to_xmm else src), modrm.r/m=gpr.
                let (xmm_idx, gpr_idx) = if *to_xmm {
                    (*dst_idx, *src_idx)
                } else {
                    (*src_idx, *dst_idx)
                };
                let is_b64 = matches!(width, crate::codegen::shared::asm_operand::Width::B64);
                let opc2: u8 = if *to_xmm { 0x6E } else { 0x7E };
                // movq (B64) needs 66 legacy prefix + REX.W. movd (B32) needs neither.
                let mut rex: u8 = 0x40;
                if is_b64 { rex |= 0x08; }
                if xmm_idx >= 8 { rex |= 0x04; }
                if gpr_idx >= 8 { rex |= 0x01; }
                let need_rex = is_b64 || xmm_idx >= 8 || gpr_idx >= 8;
                let mut out = Vec::with_capacity(6);
                if is_b64 { out.push(0x66); }
                if need_rex { out.push(rex); }
                out.push(0x0F);
                out.push(opc2);
                out.push(0xC0 | ((xmm_idx & 7) << 3) | (gpr_idx & 7));
                Some(out)
            }
            _ => None,
        }
    }

    /// 渲染为 GAS AT&T 一行 (不含 \n).
    /// 步骤 1: AluRm 的打样形态给真实输出, 其余变体给占位 (render 路径在步骤 6
    /// 才切, 本步 -S 仍走 disasm.rs).
    pub(crate) fn render(&self) -> String {
        use crate::codegen::shared::asm_operand::Width;
        match self {
            X86_64Opcode::AluRm { op, width, dst, src } => {
                if let (Operand::Reg(reg, Width::B64), Operand::Imm(imm)) = (dst, src) {
                    let mnemonic = match op {
                        AluOp::Add => "addq",
                        AluOp::Sub => "subq",
                        _ => return String::from("/* unhandled AluRm */"),
                    };
                    if *reg == 4 {
                        return format!("\t{}\t${}, %rsp", mnemonic, imm);
                    }
                }
                let _ = width;
                String::from("/* unhandled AluRm */")
            }
            X86_64Opcode::Raw { mnemonic, .. } => {
                mnemonic.clone().unwrap_or_else(|| "/* raw */".into())
            }
            X86_64Opcode::NoArg { op } => {
                let s: &str = match op {
                    NoArgOp::Cld => "cld",
                    NoArgOp::Mfence => "mfence",
                    NoArgOp::RepMovsb => "rep movsb",
                    NoArgOp::RepStosb => "rep stosb",
                    NoArgOp::RepMovsq => "rep movsq",
                };
                String::from(s)
            }
            _ => String::from("/* unimplemented opcode variant */"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::shared::asm_operand::Width;

    fn reg(r: u8, w: Width) -> Operand {
        Operand::Reg(r, w)
    }

    #[test]
    fn alu_rm_rsp_imm32_sub_b64() {
        // sub rsp, 4096 (imm 超出 i8, 走 0x81 / id): 48 81 EC 00 10 00 00
        let o = X86_64Opcode::AluRm {
            op: AluOp::Sub,
            width: Width::B64,
            dst: reg(4, Width::B64),
            src: Operand::Imm(4096),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x81, 0xEC, 0x00, 0x10, 0, 0]));
    }

    #[test]
    fn alu_rm_rsp_imm32_add_b64() {
        // add rsp, 4096: 48 81 C4 00 10 00 00
        let o = X86_64Opcode::AluRm {
            op: AluOp::Add,
            width: Width::B64,
            dst: reg(4, Width::B64),
            src: Operand::Imm(4096),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x81, 0xC4, 0x00, 0x10, 0, 0]));
    }

    #[test]
    fn alu_rm_and_ecx_imm32_b32() {
        // and ecx, 0xFFFF9FFF (imm32): 81 E1 FF 9F FF FF (B32, no REX.W)
        let o = X86_64Opcode::AluRm {
            op: AluOp::And,
            width: Width::B32,
            dst: reg(1, Width::B32),
            src: Operand::Imm(0xFFFF_9FFF_u32 as i32 as i64),
        };
        assert_eq!(o.encode(), Some(vec![0x81, 0xE1, 0xFF, 0x9F, 0xFF, 0xFF]));
    }

    #[test]
    fn alu_rm_cmp_edx_imm32_b32() {
        // cmp edx, 0x7FFF_FFFF: 81 FA FF FF FF 7F (B32, imm32)
        let o = X86_64Opcode::AluRm {
            op: AluOp::Cmp,
            width: Width::B32,
            dst: reg(2, Width::B32),
            src: Operand::Imm(0x7FFF_FFFF),
        };
        assert_eq!(o.encode(), Some(vec![0x81, 0xFA, 0xFF, 0xFF, 0xFF, 0x7F]));
    }

    // ---- reg-reg form (P11 Batch 2 opening) ----

    #[test]
    fn alu_rm_reg_reg_and_b8_sil_dl() {
        // and dl, sil (sz=1): 40 20 F2  (from instructions.rs::emit_and_rdx_rsi)
        let o = X86_64Opcode::AluRm {
            op: AluOp::And,
            width: Width::B8,
            dst: reg(2, Width::B8), // dl (rdx=2)
            src: reg(6, Width::B8), // sil (rsi=6)
        };
        assert_eq!(o.encode(), Some(vec![0x40, 0x20, 0xF2]));
    }

    #[test]
    fn alu_rm_reg_reg_and_b16_dx_si() {
        // and dx, si (sz=2): 66 21 F2
        let o = X86_64Opcode::AluRm {
            op: AluOp::And,
            width: Width::B16,
            dst: reg(2, Width::B16),
            src: reg(6, Width::B16),
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x21, 0xF2]));
    }

    #[test]
    fn alu_rm_reg_reg_and_b32_edx_esi() {
        // and edx, esi (sz=4): 21 F2
        let o = X86_64Opcode::AluRm {
            op: AluOp::And,
            width: Width::B32,
            dst: reg(2, Width::B32),
            src: reg(6, Width::B32),
        };
        assert_eq!(o.encode(), Some(vec![0x21, 0xF2]));
    }

    #[test]
    fn alu_rm_reg_reg_and_b64_rdx_rsi() {
        // and rdx, rsi (sz=8): 48 21 F2
        let o = X86_64Opcode::AluRm {
            op: AluOp::And,
            width: Width::B64,
            dst: reg(2, Width::B64),
            src: reg(6, Width::B64),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x21, 0xF2]));
    }

    #[test]
    fn alu_rm_reg_reg_or_all_widths() {
        // or dl, sil:    40 08 F2
        // or dx, si:     66 09 F2
        // or edx, esi:   09 F2
        // or rdx, rsi:   48 09 F2
        for (w, expect) in [
            (Width::B8, vec![0x40, 0x08, 0xF2]),
            (Width::B16, vec![0x66, 0x09, 0xF2]),
            (Width::B32, vec![0x09, 0xF2]),
            (Width::B64, vec![0x48, 0x09, 0xF2]),
        ] {
            let o = X86_64Opcode::AluRm {
                op: AluOp::Or,
                width: w,
                dst: reg(2, w),
                src: reg(6, w),
            };
            assert_eq!(o.encode(), Some(expect), "width={:?}", w);
        }
    }

    #[test]
    fn alu_rm_reg_reg_xor_all_widths() {
        // xor dl, sil:    40 30 F2
        // xor dx, si:     66 31 F2
        // xor edx, esi:   31 F2
        // xor rdx, rsi:   48 31 F2
        for (w, expect) in [
            (Width::B8, vec![0x40, 0x30, 0xF2]),
            (Width::B16, vec![0x66, 0x31, 0xF2]),
            (Width::B32, vec![0x31, 0xF2]),
            (Width::B64, vec![0x48, 0x31, 0xF2]),
        ] {
            let o = X86_64Opcode::AluRm {
                op: AluOp::Xor,
                width: w,
                dst: reg(2, w),
                src: reg(6, w),
            };
            assert_eq!(o.encode(), Some(expect), "width={:?}", w);
        }
    }

    #[test]
    fn alu_rm_reg_reg_sub_b64_rax_rcx() {
        // sub rax, rcx (sz=8): 48 29 C8   (from emit_sub_rax_rcx)
        let o = X86_64Opcode::AluRm {
            op: AluOp::Sub,
            width: Width::B64,
            dst: reg(0, Width::B64),
            src: reg(1, Width::B64),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x29, 0xC8]));
    }

    #[test]
    fn alu_rm_reg_reg_sub_b32_rcx_rax() {
        // sub ecx, eax (sz=4): 29 C1   (from emit_sub_rcx_rax)
        let o = X86_64Opcode::AluRm {
            op: AluOp::Sub,
            width: Width::B32,
            dst: reg(1, Width::B32),
            src: reg(0, Width::B32),
        };
        assert_eq!(o.encode(), Some(vec![0x29, 0xC1]));
    }

    #[test]
    fn alu_rm_reg_reg_xor_b32_edx_edx() {
        // xor edx, edx (from emit_xor_edx): 31 D2
        let o = X86_64Opcode::AluRm {
            op: AluOp::Xor,
            width: Width::B32,
            dst: reg(2, Width::B32),
            src: reg(2, Width::B32),
        };
        assert_eq!(o.encode(), Some(vec![0x31, 0xD2]));
    }

    #[test]
    fn alu_rm_reg_reg_xor_b64_rdx_rdx() {
        // xor rdx, rdx (sz=8): 48 31 D2
        let o = X86_64Opcode::AluRm {
            op: AluOp::Xor,
            width: Width::B64,
            dst: reg(2, Width::B64),
            src: reg(2, Width::B64),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x31, 0xD2]));
    }

    // ---- reg, imm form (P11 Batch 2r) ----

    #[test]
    fn alu_rm_sub_rsp_imm8_b64() {
        // sub rsp, 16: AluRm 走 0x81 / id (imm32 形式): 48 81 EC 10 00 00 00
        let o = X86_64Opcode::AluRm {
            op: AluOp::Sub,
            width: Width::B64,
            dst: reg(4, Width::B64),
            src: Operand::Imm(16),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x81, 0xEC, 0x10, 0, 0, 0]));
    }

    #[test]
    fn alu_rm_sub_rsp_imm32_b64() {
        // sub rsp, 4096:  48 81 EC 00 10 00 00
        let o = X86_64Opcode::AluRm {
            op: AluOp::Sub,
            width: Width::B64,
            dst: reg(4, Width::B64),
            src: Operand::Imm(4096),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x81, 0xEC, 0x00, 0x10, 0, 0]));
    }

    #[test]
    fn alu_rm_add_rsp_imm8_b64() {
        // add rsp, 16: AluRm 走 0x81 / id: 48 81 C4 10 00 00 00
        let o = X86_64Opcode::AluRm {
            op: AluOp::Add,
            width: Width::B64,
            dst: reg(4, Width::B64),
            src: Operand::Imm(16),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x81, 0xC4, 0x10, 0, 0, 0]));
    }

    #[test]
    fn alu_rm_add_rax_imm8_b64() {
        // add rax, 15:  48 83 C0 0F
        let o = X86_64Opcode::AluRm {
            op: AluOp::Add,
            width: Width::B64,
            dst: reg(0, Width::B64),
            src: Operand::Imm(15),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x83, 0xC0, 0x0F]));
    }

    #[test]
    fn alu_rm_and_rax_negative16_b64() {
        // and rax, -16:  48 83 E0 F0
        let o = X86_64Opcode::AluRm {
            op: AluOp::And,
            width: Width::B64,
            dst: reg(0, Width::B64),
            src: Operand::Imm(-16),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x83, 0xE0, 0xF0]));
    }

    #[test]
    fn alu_rm_cmp_eax_imm8_b32() {
        // cmp $48, %eax:  83 F8 30   (va_arg_store.rs)
        let o = X86_64Opcode::AluRm {
            op: AluOp::Cmp,
            width: Width::B32,
            dst: reg(0, Width::B32),
            src: Operand::Imm(48),
        };
        assert_eq!(o.encode(), Some(vec![0x83, 0xF8, 0x30]));
    }

    #[test]
    fn alu_rm_add_r9_imm8_b64() {
        // add r9, 16:  49 83 C1 10
        let o = X86_64Opcode::AluRm {
            op: AluOp::Add,
            width: Width::B64,
            dst: reg(9, Width::B64),
            src: Operand::Imm(16),
        };
        assert_eq!(o.encode(), Some(vec![0x49, 0x83, 0xC1, 0x10]));
    }

    // ---- AluRm imm32 (0x81 / id) for arbitrary B64 reg (P11 Bucket B) ----

    #[test]
    fn alu_rm_add_rcx_imm32_b64() {
        // add rcx, 4096 (imm 超出 i8):  48 81 C1 00 10 00 00
        let o = X86_64Opcode::AluRm {
            op: AluOp::Add,
            width: Width::B64,
            dst: reg(1, Width::B64),
            src: Operand::Imm(4096),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x81, 0xC1, 0x00, 0x10, 0, 0]));
    }

    #[test]
    fn alu_rm_and_rcx_imm32_b64() {
        // and rcx, 4096:  48 81 E1 00 10 00 00
        let o = X86_64Opcode::AluRm {
            op: AluOp::And,
            width: Width::B64,
            dst: reg(1, Width::B64),
            src: Operand::Imm(4096),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x81, 0xE1, 0x00, 0x10, 0, 0]));
    }

    #[test]
    fn alu_rm_cmp_rcx_imm32_b64() {
        // cmp rcx, imm32:  48 81 F9 xx xx xx xx
        let o = X86_64Opcode::AluRm {
            op: AluOp::Cmp,
            width: Width::B64,
            dst: reg(1, Width::B64),
            src: Operand::Imm(-1000),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x81, 0xF9, 0x18, 0xFC, 0xFF, 0xFF]));
    }

    #[test]
    fn alu_rm_sub_rdi_imm32_b64() {
        // sub rdi, 0x1000:  48 81 EF 00 10 00 00
        let o = X86_64Opcode::AluRm {
            op: AluOp::Sub,
            width: Width::B64,
            dst: reg(7, Width::B64),
            src: Operand::Imm(0x1000),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x81, 0xEF, 0x00, 0x10, 0, 0]));
    }

    // ---- MovRm reg-reg (P11 Batch 2r) ----

    #[test]
    fn mov_rm_mov_rsi_rax_b64() {
        // mov rsi, rax:  48 89 C6   (float.rs / lvalue.rs handwritten)
        let o = X86_64Opcode::MovRm {
            width: Width::B64,
            dst: reg(6, Width::B64),
            src: reg(0, Width::B64),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x89, 0xC6]));
    }

    #[test]
    fn mov_rm_mov_rcx_rax_b64() {
        // mov rcx, rax:  48 89 C1
        let o = X86_64Opcode::MovRm {
            width: Width::B64,
            dst: reg(1, Width::B64),
            src: reg(0, Width::B64),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x89, 0xC1]));
    }

    #[test]
    fn mov_rm_mov_eax_eax_b32() {
        // mov eax, eax (zero-extended):  89 C0
        let o = X86_64Opcode::MovRm {
            width: Width::B32,
            dst: reg(0, Width::B32),
            src: reg(0, Width::B32),
        };
        assert_eq!(o.encode(), Some(vec![0x89, 0xC0]));
    }

    // ---- MovRm imm->reg (P11 Batch 2r) ----

    #[test]
    fn mov_rm_mov_eax_imm32_zero() {
        // mov eax, 0:  B8 00 00 00 00   (functions.rs handwritten)
        let o = X86_64Opcode::MovRm {
            width: Width::B32,
            dst: reg(0, Width::B32),
            src: Operand::Imm(0),
        };
        assert_eq!(o.encode(), Some(vec![0xB8, 0, 0, 0, 0]));
    }

    #[test]
    fn mov_rm_mov_rax_imm64() {
        // mov rax, 0x80000000:  fits 32-bit zero-ext imm, encode as 48 B8 00 00 00 80 (7 bytes)
        let o = X86_64Opcode::MovRm {
            width: Width::B64,
            dst: reg(0, Width::B64),
            src: Operand::Imm(0x80000000),
        };
        assert_eq!(o.encode(), Some(vec![
            0x48, 0xB8, 0x00, 0x00, 0x00, 0x80
        ]));
    }

    #[test]
    fn mov_rm_mov_rax_imm64_high() {
        // mov rax, 0x80000000_00000000: requires full imm64, 10 bytes.
        let o = X86_64Opcode::MovRm {
            width: Width::B64,
            dst: reg(0, Width::B64),
            src: Operand::Imm(0x8000_0000_0000_0000u64 as i64),
        };
        assert_eq!(o.encode(), Some(vec![
            0x48, 0xB8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80
        ]));
    }

    // ---- MovRm mem->reg [rax+0] load forms (P11 Batch 2x) ----

    #[test]
    fn mov_rm_mov_rax_load_rax_b64() {
        // mov rax, [rax]:  48 8B 00
        let o = X86_64Opcode::MovRm {
            width: Width::B64,
            dst: reg(0, Width::B64),
            src: Operand::Mem(MemOperand {
                disp: 0, base: Some(0), index: None, seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x8B, 0x00]));
    }

    #[test]
    fn mov_rm_mov_eax_load_rax_b32() {
        // mov eax, [rax]:  8B 00  (zero-extends to rax)
        let o = X86_64Opcode::MovRm {
            width: Width::B32,
            dst: reg(0, Width::B32),
            src: Operand::Mem(MemOperand {
                disp: 0, base: Some(0), index: None, seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x8B, 0x00]));
    }

    #[test]
    fn mov_rm_mov_rax_load_rsp_b64() {
        // mov rax, [rsp]:  48 8B 04 24  (SIB 跟随 rsp)
        let o = X86_64Opcode::MovRm {
            width: Width::B64,
            dst: reg(0, Width::B64),
            src: Operand::Mem(MemOperand {
                disp: 0, base: Some(4), index: None, seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x8B, 0x04, 0x24]));
    }

    #[test]
    fn mov_rm_mov_eax_load_rsp_b32() {
        // mov eax, [rsp]:  8B 04 24  (mod=00, reg=eax=0, SIB=24)
        let o = X86_64Opcode::MovRm {
            width: Width::B32,
            dst: reg(0, Width::B32),
            src: Operand::Mem(MemOperand {
                disp: 0, base: Some(4), index: None, seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x8B, 0x04, 0x24]));
    }

    // ---- P11 Bucket A: MovRm reg<->mem with disp8/disp32 ----

    #[test]
    fn mov_rm_load_r64_base_disp8() {
        // mov rcx, [r10+16]: 49 8B 4A 10 (REX.WB: B=1 r10=10; reg=rcx=1; disp=0x10)
        let o = X86_64Opcode::MovRm {
            width: Width::B64,
            dst: reg(1, Width::B64),
            src: Operand::Mem(MemOperand {
                disp: 16, base: Some(10), index: None, seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x49, 0x8B, 0x4A, 0x10]));
    }

    #[test]
    fn mov_rm_load_r64_base_disp32() {
        // mov rax, [r13+disp32]: 49 8B 85 XX XX XX XX (REX.WB; reg=rax=0, base=r13)
        let o = X86_64Opcode::MovRm {
            width: Width::B64,
            dst: reg(0, Width::B64),
            src: Operand::Mem(MemOperand {
                disp: 0x100, base: Some(13), index: None, seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x49, 0x8B, 0x85, 0x00, 0x01, 0, 0]));
    }

    #[test]
    fn mov_rm_load_r32_base_disp8() {
        // mov eax, [r10+4]: 41 8B 42 04 (REX.B; reg=eax=0)
        let o = X86_64Opcode::MovRm {
            width: Width::B32,
            dst: reg(0, Width::B32),
            src: Operand::Mem(MemOperand {
                disp: 4, base: Some(10), index: None, seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x41, 0x8B, 0x42, 0x04]));
    }

    #[test]
    fn mov_rm_store_r64_base_disp8() {
        // mov [r10+8], rcx: 49 89 4A 08 (REX.WB; reg=rcx=1)
        let o = X86_64Opcode::MovRm {
            width: Width::B64,
            dst: Operand::Mem(MemOperand {
                disp: 8, base: Some(10), index: None, seg: None,
            }),
            src: reg(1, Width::B64),
        };
        assert_eq!(o.encode(), Some(vec![0x49, 0x89, 0x4A, 0x08]));
    }

    #[test]
    fn mov_rm_store_r64_base_disp32() {
        // mov [r13+disp32], rax: 49 89 85 XX XX XX XX
        let o = X86_64Opcode::MovRm {
            width: Width::B64,
            dst: Operand::Mem(MemOperand {
                disp: 0x100, base: Some(13), index: None, seg: None,
            }),
            src: reg(0, Width::B64),
        };
        assert_eq!(o.encode(), Some(vec![0x49, 0x89, 0x85, 0x00, 0x01, 0, 0]));
    }

    #[test]
    fn mov_rm_store_r64_base_disp0() {
        // mov [rdi], r9: 4C 89 0F (REX.WR: reg=r9=9; r/m=rdi=7)
        let o = X86_64Opcode::MovRm {
            width: Width::B64,
            dst: Operand::Mem(MemOperand {
                disp: 0, base: Some(7), index: None, seg: None,
            }),
            src: reg(9, Width::B64),
        };
        assert_eq!(o.encode(), Some(vec![0x4C, 0x89, 0x0F]));
    }

    #[test]
    fn mov_rm_store_r64_base_disp8_sib_rsp() {
        // mov [rsp+disp8], r9: 4C 89 4C 24 XX (SIB follows mod=01 rm=4)
        let o = X86_64Opcode::MovRm {
            width: Width::B64,
            dst: Operand::Mem(MemOperand {
                disp: 16, base: Some(4), index: None, seg: None,
            }),
            src: reg(9, Width::B64),
        };
        assert_eq!(o.encode(), Some(vec![0x4C, 0x89, 0x4C, 0x24, 0x10]));
    }

    // ---- MovExt reg-reg (P11) ----

    #[test]
    fn mov_ext_movzx_r32_r8_no_rex() {
        // movzx eax, al:  0F B6 C0  (no REX, src=al=0)
        let o = X86_64Opcode::MovExt {
            sign: false,
            from: Width::B8,
            to: Width::B32,
            dst: reg(0, Width::B32),
            src: reg(0, Width::B8),
        };
        assert_eq!(o.encode(), Some(vec![0x0F, 0xB6, 0xC0]));
    }

    #[test]
    fn mov_ext_movzx_r32_r8_sil_needs_rex() {
        // movzx eax, sil:  40 0F B6 C6  (src=6=sil, 需要 REX)
        let o = X86_64Opcode::MovExt {
            sign: false,
            from: Width::B8,
            to: Width::B32,
            dst: reg(0, Width::B32),
            src: reg(6, Width::B8),
        };
        assert_eq!(o.encode(), Some(vec![0x40, 0x0F, 0xB6, 0xC6]));
    }

    #[test]
    fn mov_ext_movzx_r32_r16_no_rex() {
        // movzx eax, ax:  0F B7 C0
        let o = X86_64Opcode::MovExt {
            sign: false,
            from: Width::B16,
            to: Width::B32,
            dst: reg(0, Width::B32),
            src: reg(0, Width::B16),
        };
        assert_eq!(o.encode(), Some(vec![0x0F, 0xB7, 0xC0]));
    }

    #[test]
    fn mov_ext_movsx_r32_r8() {
        // movsx eax, al:  0F BE C0
        let o = X86_64Opcode::MovExt {
            sign: true,
            from: Width::B8,
            to: Width::B32,
            dst: reg(0, Width::B32),
            src: reg(0, Width::B8),
        };
        assert_eq!(o.encode(), Some(vec![0x0F, 0xBE, 0xC0]));
    }

    #[test]
    fn mov_ext_movsx_r32_r16() {
        // movsx eax, ax:  0F BF C0
        let o = X86_64Opcode::MovExt {
            sign: true,
            from: Width::B16,
            to: Width::B32,
            dst: reg(0, Width::B32),
            src: reg(0, Width::B16),
        };
        assert_eq!(o.encode(), Some(vec![0x0F, 0xBF, 0xC0]));
    }

    #[test]
    fn mov_ext_movsxd_r64_r32() {
        // movsxd rax, eax:  48 63 C0
        let o = X86_64Opcode::MovExt {
            sign: true,
            from: Width::B32,
            to: Width::B64,
            dst: reg(0, Width::B64),
            src: reg(0, Width::B32),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x63, 0xC0]));
    }

    #[test]
    fn mov_ext_movsx_r64_r16() {
        // movsx r64, r16 (movswq):  48 0F BF C0
        let o = X86_64Opcode::MovExt {
            sign: true,
            from: Width::B16,
            to: Width::B64,
            dst: reg(0, Width::B64),
            src: reg(0, Width::B16),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x0F, 0xBF, 0xC0]));
    }

    #[test]
    fn mov_ext_high_regs_r8_r15() {
        // movsx r8d, dil:  44 0F BE C7  (dst=r8=8 -> reg&7=0, src=dil=7)
        let o = X86_64Opcode::MovExt {
            sign: true,
            from: Width::B8,
            to: Width::B32,
            dst: reg(8, Width::B32),
            src: reg(7, Width::B8),
        };
        // REX.R=0x04 (dst>=8), src=7>=4 requires REX anyway.
        assert_eq!(o.encode(), Some(vec![0x44, 0x0F, 0xBE, 0xC7]));
    }

    #[test]
    fn mov_ext_self_ref_sxw_r64() {
        // movsx rax, al (self):  48 0F BE C0
        let o = X86_64Opcode::MovExt {
            sign: true,
            from: Width::B8,
            to: Width::B64,
            dst: reg(0, Width::B64),
            src: reg(0, Width::B8),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x0F, 0xBE, 0xC0]));
    }

    #[test]
    fn mov_ext_self_ref_sxw_r64_high() {
        // movsx r8, r8b (self, dst=src=r8):  4D 0F BE C0
        let o = X86_64Opcode::MovExt {
            sign: true,
            from: Width::B8,
            to: Width::B64,
            dst: reg(8, Width::B64),
            src: reg(8, Width::B8),
        };
        assert_eq!(o.encode(), Some(vec![0x4D, 0x0F, 0xBE, 0xC0]));
    }

    #[test]
    fn mov_ext_self_ref_sxw_r64_r16() {
        // movsx rax, ax (self):  48 0F BF C0
        let o = X86_64Opcode::MovExt {
            sign: true,
            from: Width::B16,
            to: Width::B64,
            dst: reg(0, Width::B64),
            src: reg(0, Width::B16),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x0F, 0xBF, 0xC0]));
    }

    // ---- MovExt reg-mem (P11) ----

    #[test]
    fn mov_ext_movzx_load_byte_b32_disp8() {
        // movzx ecx, byte [rax+disp8]:  0F B6 48 disp
        let o = X86_64Opcode::MovExt {
            sign: false,
            from: Width::B8,
            to: Width::B32,
            dst: reg(1, Width::B32),
            src: Operand::Mem(MemOperand {
                disp: 8, base: Some(0), index: None, seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x0F, 0xB6, 0x48, 0x08]));
    }

    #[test]
    fn mov_ext_movzx_load_byte_b32_disp32() {
        // movzx ecx, byte [rax+disp32]:  0F B6 88 disp32
        let o = X86_64Opcode::MovExt {
            sign: false,
            from: Width::B8,
            to: Width::B32,
            dst: reg(1, Width::B32),
            src: Operand::Mem(MemOperand {
                disp: 0x12345678, base: Some(0), index: None, seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![
            0x0F, 0xB6, 0x88,
            0x78, 0x56, 0x34, 0x12,
        ]));
    }

    #[test]
    fn mov_ext_movsx_load_word_b64_disp0() {
        // movswq rax, [rbx]:  48 0F BF 03  (mod=00, reg=rax=0, r/m=rbx=3)
        let o = X86_64Opcode::MovExt {
            sign: true,
            from: Width::B16,
            to: Width::B64,
            dst: reg(0, Width::B64),
            src: Operand::Mem(MemOperand {
                disp: 0, base: Some(3), index: None, seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x0F, 0xBF, 0x03]));
    }

    #[test]
    fn mov_ext_movsxd_load_b64_from_rsp() {
        // movsxd rax, dword [rsp]:  48 63 04 24  (SIB for rsp)
        let o = X86_64Opcode::MovExt {
            sign: true,
            from: Width::B32,
            to: Width::B64,
            dst: reg(0, Width::B64),
            src: Operand::Mem(MemOperand {
                disp: 0, base: Some(4), index: None, seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x63, 0x04, 0x24]));
    }

    #[test]
    fn mov_ext_movzx_load_byte_b64_with_rex_b() {
        // movzbq r8, [r11]:  4C 0F B6 03  (REX.W=1, REX.B=base=11&7=3 -> 0x0B=base>=8, dst=r8>=8)
        // REX = 0x48|0x04(R)|0x01(B) = 0x4D? Let's re-derive:
        //   rex_w=1 -> 0x48; dst=8>=8 -> REX.R=0x04; base=11>=8 -> REX.B=0x01. Total 0x4D.
        //   modrm = mod(0) | reg(0) | rm(3) = 0x03.
        let o = X86_64Opcode::MovExt {
            sign: false,
            from: Width::B8,
            to: Width::B64,
            dst: reg(8, Width::B64),
            src: Operand::Mem(MemOperand {
                disp: 0, base: Some(11), index: None, seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x4D, 0x0F, 0xB6, 0x03]));
    }

    // ---- TEST reg,reg (P11) ----

    #[test]
    fn test_rm_eax_eax_b32() {
        // test eax, eax:  85 C0
        let o = X86_64Opcode::AluRm {
            op: AluOp::Test,
            width: Width::B32,
            dst: reg(0, Width::B32),
            src: reg(0, Width::B32),
        };
        assert_eq!(o.encode(), Some(vec![0x85, 0xC0]));
    }

    #[test]
    fn test_rm_rax_rax_b64() {
        // test rax, rax:  48 85 C0
        let o = X86_64Opcode::AluRm {
            op: AluOp::Test,
            width: Width::B64,
            dst: reg(0, Width::B64),
            src: reg(0, Width::B64),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x85, 0xC0]));
    }

    #[test]
    fn test_rm_rcx_rcx_b32() {
        // test ecx, ecx:  85 C9
        let o = X86_64Opcode::AluRm {
            op: AluOp::Test,
            width: Width::B32,
            dst: reg(1, Width::B32),
            src: reg(1, Width::B32),
        };
        assert_eq!(o.encode(), Some(vec![0x85, 0xC9]));
    }

    #[test]
    fn test_rm_edx_edx_b32() {
        // test edx, edx:  85 D2 (self-ref: dst=src=edx=2)
        let o = X86_64Opcode::AluRm {
            op: AluOp::Test,
            width: Width::B32,
            dst: reg(2, Width::B32),
            src: reg(2, Width::B32),
        };
        assert_eq!(o.encode(), Some(vec![0x85, 0xD2]));
    }

    #[test]
    fn test_rm_r9_r9_b64_high() {
        // test r9, r9:  4D 85 C9  (REX.R+B since both r/m and reg >=8)
        let o = X86_64Opcode::AluRm {
            op: AluOp::Test,
            width: Width::B64,
            dst: reg(9, Width::B64),
            src: reg(9, Width::B64),
        };
        assert_eq!(o.encode(), Some(vec![0x4D, 0x85, 0xC9]));
    }

    // ---- IMUL reg,reg (P11) ----

    #[test]
    fn imul_rm_eax_ecx_b32() {
        // imul eax, ecx:  0F AF C1
        let o = X86_64Opcode::ImulRm {
            width: Width::B32,
            dst: reg(0, Width::B32),
            src: reg(1, Width::B32),
        };
        assert_eq!(o.encode(), Some(vec![0x0F, 0xAF, 0xC1]));
    }

    #[test]
    fn imul_rm_rax_rcx_b64() {
        // imul rax, rcx:  48 0F AF C1
        let o = X86_64Opcode::ImulRm {
            width: Width::B64,
            dst: reg(0, Width::B64),
            src: reg(1, Width::B64),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x0F, 0xAF, 0xC1]));
    }

    #[test]
    fn imul_rm_r8_r9_b64_high() {
        // imul r8, r9:  4D 0F AF C1  (REX.W|R|B; reg=r8&7=0, r/m=r9&7=1)
        let o = X86_64Opcode::ImulRm {
            width: Width::B64,
            dst: reg(8, Width::B64),
            src: reg(9, Width::B64),
        };
        assert_eq!(o.encode(), Some(vec![0x4D, 0x0F, 0xAF, 0xC1]));
    }

    #[test]
    fn imul_rm_r9_r8_b64_high_swap() {
        // imul r9, r8:  4D 0F AF C8  (reg=r9&7=1, r/m=r8&7=0)
        let o = X86_64Opcode::ImulRm {
            width: Width::B64,
            dst: reg(9, Width::B64),
            src: reg(8, Width::B64),
        };
        assert_eq!(o.encode(), Some(vec![0x4D, 0x0F, 0xAF, 0xC8]));
    }

    #[test]
    fn imul_rm_edx_ecx_b32() {
        // imul edx, ecx:  0F AF D1  (reg=edx=2, r/m=ecx=1)
        let o = X86_64Opcode::ImulRm {
            width: Width::B32,
            dst: reg(2, Width::B32),
            src: reg(1, Width::B32),
        };
        assert_eq!(o.encode(), Some(vec![0x0F, 0xAF, 0xD1]));
    }

    // ---- SimdArith reg-reg (P11 Bucket C+E) ----

    fn xmm(r: u8) -> Operand {
        Operand::Reg(r, Width::B128)
    }

    #[test]
    fn simd_addsd_xmm0_xmm1() {
        // addsd xmm0, xmm1:  F2 0F 58 C1  (reg=xmm1, r/m=xmm0)
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::Add,
            width: SimdWidth::SD,
            dst: xmm(0),
            src: xmm(1),
        };
        assert_eq!(o.encode(), Some(vec![0xF2, 0x0F, 0x58, 0xC1]));
    }

    #[test]
    fn simd_subsd_xmm0_xmm1() {
        // subsd xmm0, xmm1:  F2 0F 5C C1
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::Sub,
            width: SimdWidth::SD,
            dst: xmm(0),
            src: xmm(1),
        };
        assert_eq!(o.encode(), Some(vec![0xF2, 0x0F, 0x5C, 0xC1]));
    }

    #[test]
    fn simd_mulsd_xmm0_xmm1() {
        // mulsd xmm0, xmm1:  F2 0F 59 C1
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::Mul,
            width: SimdWidth::SD,
            dst: xmm(0),
            src: xmm(1),
        };
        assert_eq!(o.encode(), Some(vec![0xF2, 0x0F, 0x59, 0xC1]));
    }

    #[test]
    fn simd_divsd_xmm0_xmm1() {
        // divsd xmm0, xmm1:  F2 0F 5E C1
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::Div,
            width: SimdWidth::SD,
            dst: xmm(0),
            src: xmm(1),
        };
        assert_eq!(o.encode(), Some(vec![0xF2, 0x0F, 0x5E, 0xC1]));
    }

    #[test]
    fn simd_addss_xmm0_xmm1() {
        // addss xmm0, xmm1:  F3 0F 58 C1
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::Add,
            width: SimdWidth::SS,
            dst: xmm(0),
            src: xmm(1),
        };
        assert_eq!(o.encode(), Some(vec![0xF3, 0x0F, 0x58, 0xC1]));
    }

    #[test]
    fn simd_subss_xmm0_xmm1() {
        // subss xmm0, xmm1:  F3 0F 5C C1
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::Sub,
            width: SimdWidth::SS,
            dst: xmm(0),
            src: xmm(1),
        };
        assert_eq!(o.encode(), Some(vec![0xF3, 0x0F, 0x5C, 0xC1]));
    }

    #[test]
    fn simd_xorps_xmm0_xmm0() {
        // xorps xmm0, xmm0:  0F 57 C0  (zero xmm0)
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::Xorps,
            width: SimdWidth::PS,
            dst: xmm(0),
            src: xmm(0),
        };
        assert_eq!(o.encode(), Some(vec![0x0F, 0x57, 0xC0]));
    }

    #[test]
    fn simd_xorpd_xmm0_xmm0() {
        // xorpd xmm0, xmm0:  66 0F 57 C0
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::Xorpd,
            width: SimdWidth::PD,
            dst: xmm(0),
            src: xmm(0),
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x0F, 0x57, 0xC0]));
    }

    #[test]
    fn simd_xorps_xmm15_xmm15_high() {
        // xorps xmm15, xmm15:  45 0F 57 FF  (REX.R+B set)
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::Xorps,
            width: SimdWidth::PS,
            dst: xmm(15),
            src: xmm(15),
        };
        assert_eq!(o.encode(), Some(vec![0x45, 0x0F, 0x57, 0xFF]));
    }

    // ---- SimdArith max/min/orps/orpd (P11 Bucket A2) ----

    #[test]
    fn simd_minsd_xmm0_xmm2() {
        // minsd xmm0, xmm2:  F2 0F 5D C2
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::Min,
            width: SimdWidth::SD,
            dst: xmm(0),
            src: xmm(2),
        };
        assert_eq!(o.encode(), Some(vec![0xF2, 0x0F, 0x5D, 0xC2]));
    }

    #[test]
    fn simd_minss_xmm0_xmm2() {
        // minss xmm0, xmm2:  F3 0F 5D C2
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::Min,
            width: SimdWidth::SS,
            dst: xmm(0),
            src: xmm(2),
        };
        assert_eq!(o.encode(), Some(vec![0xF3, 0x0F, 0x5D, 0xC2]));
    }

    #[test]
    fn simd_maxsd_xmm0_xmm2() {
        // maxsd xmm0, xmm2:  F2 0F 5F C2
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::Max,
            width: SimdWidth::SD,
            dst: xmm(0),
            src: xmm(2),
        };
        assert_eq!(o.encode(), Some(vec![0xF2, 0x0F, 0x5F, 0xC2]));
    }

    #[test]
    fn simd_maxss_xmm0_xmm2() {
        // maxss xmm0, xmm2:  F3 0F 5F C2
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::Max,
            width: SimdWidth::SS,
            dst: xmm(0),
            src: xmm(2),
        };
        assert_eq!(o.encode(), Some(vec![0xF3, 0x0F, 0x5F, 0xC2]));
    }

    #[test]
    fn simd_orps_xmm0_xmm1() {
        // orps xmm0, xmm1:  0F 56 C1
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::Orps,
            width: SimdWidth::PS,
            dst: xmm(0),
            src: xmm(1),
        };
        assert_eq!(o.encode(), Some(vec![0x0F, 0x56, 0xC1]));
    }

    #[test]
    fn simd_orpd_xmm0_xmm1() {
        // orpd xmm0, xmm1:  66 0F 56 C1
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::Orpd,
            width: SimdWidth::PD,
            dst: xmm(0),
            src: xmm(1),
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x0F, 0x56, 0xC1]));
    }

    // ---- SimdArith packed integer (P11 Bucket B) ----

    #[test]
    fn simd_paddq_xmm0_xmm1() {
        // paddq xmm0, xmm1:  66 0F D4 C1
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::PAdd,
            width: SimdWidth::Q,
            dst: xmm(0),
            src: xmm(1),
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x0F, 0xD4, 0xC1]));
    }

    #[test]
    fn simd_psubw_xmm0_xmm1() {
        // psubw xmm0, xmm1:  66 0F F9 C1
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::PSub,
            width: SimdWidth::W,
            dst: xmm(0),
            src: xmm(1),
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x0F, 0xF9, 0xC1]));
    }

    // ---- SimdArith packed float (P11 Bucket B) ----

    #[test]
    fn simd_addps_xmm0_xmm1() {
        // addps xmm0, xmm1:  0F 58 C1
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::Addps,
            width: SimdWidth::PS,
            dst: xmm(0),
            src: xmm(1),
        };
        assert_eq!(o.encode(), Some(vec![0x0F, 0x58, 0xC1]));
    }

    #[test]
    fn simd_mulpd_xmm0_xmm1() {
        // mulpd xmm0, xmm1:  66 0F 59 C1
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::Mulpd,
            width: SimdWidth::PD,
            dst: xmm(0),
            src: xmm(1),
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x0F, 0x59, 0xC1]));
    }

    // ---- SimdArith ucomi/comi (P11 Bucket A) ----

    #[test]
    fn simd_ucomiss_xmm1_xmm0() {
        // ucomiss xmm1, xmm0:  0F 2E C8
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::Ucomi,
            width: SimdWidth::SS,
            dst: xmm(1),
            src: xmm(0),
        };
        assert_eq!(o.encode(), Some(vec![0x0F, 0x2E, 0xC8]));
    }

    #[test]
    fn simd_ucomisd_xmm1_xmm0() {
        // ucomisd xmm1, xmm0:  66 0F 2E C8
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::Ucomi,
            width: SimdWidth::SD,
            dst: xmm(1),
            src: xmm(0),
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x0F, 0x2E, 0xC8]));
    }

    #[test]
    fn simd_comiss_xmm0_xmm1() {
        // comiss xmm0, xmm1:  0F 2F C1
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::Comi,
            width: SimdWidth::SS,
            dst: xmm(0),
            src: xmm(1),
        };
        assert_eq!(o.encode(), Some(vec![0x0F, 0x2F, 0xC1]));
    }

    #[test]
    fn simd_comisd_xmm0_xmm1() {
        // comisd xmm0, xmm1:  66 0F 2F C1
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::Comi,
            width: SimdWidth::SD,
            dst: xmm(0),
            src: xmm(1),
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x0F, 0x2F, 0xC1]));
    }

    // ---- Cvt2Sd (P11 Bucket B) ----

    #[test]
    fn cvttsd2si_rax_xmm0() {
        // cvttsd2si rax, xmm0:  F2 48 0F 2C C0
        let o = X86_64Opcode::Cvt2Sd {
            to_xmm: false,
            width: SimdWidth::SD,
            dst: reg(0, Width::B64),
            src: xmm(0),
        };
        assert_eq!(o.encode(), Some(vec![0xF2, 0x48, 0x0F, 0x2C, 0xC0]));
    }

    #[test]
    fn cvttss2si_rax_xmm0() {
        // cvttss2si rax, xmm0:  F3 48 0F 2C C0
        let o = X86_64Opcode::Cvt2Sd {
            to_xmm: false,
            width: SimdWidth::SS,
            dst: reg(0, Width::B64),
            src: xmm(0),
        };
        assert_eq!(o.encode(), Some(vec![0xF3, 0x48, 0x0F, 0x2C, 0xC0]));
    }

    #[test]
    fn cvtsi2sd_xmm0_rax() {
        // cvtsi2sd xmm0, rax:  F2 48 0F 2A C0
        let o = X86_64Opcode::Cvt2Sd {
            to_xmm: true,
            width: SimdWidth::SD,
            dst: xmm(0),
            src: reg(0, Width::B64),
        };
        assert_eq!(o.encode(), Some(vec![0xF2, 0x48, 0x0F, 0x2A, 0xC0]));
    }

    #[test]
    fn cvtsi2ss_xmm0_rax() {
        // cvtsi2ss xmm0, rax:  F3 48 0F 2A C0
        let o = X86_64Opcode::Cvt2Sd {
            to_xmm: true,
            width: SimdWidth::SS,
            dst: xmm(0),
            src: reg(0, Width::B64),
        };
        assert_eq!(o.encode(), Some(vec![0xF3, 0x48, 0x0F, 0x2A, 0xC0]));
    }

    // ---- SimdArith pand/por/pxor (P11 Bucket F) ----

    #[test]
    fn pand_xmm0_xmm1() {
        // pand xmm0, xmm1:  66 0F DB C1
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::Pand,
            width: SimdWidth::O,
            dst: xmm(0),
            src: xmm(1),
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x0F, 0xDB, 0xC1]));
    }

    #[test]
    fn por_xmm0_xmm1() {
        // por xmm0, xmm1:  66 0F EB C1
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::Por,
            width: SimdWidth::O,
            dst: xmm(0),
            src: xmm(1),
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x0F, 0xEB, 0xC1]));
    }

    #[test]
    fn pxor_xmm0_xmm1() {
        // pxor xmm0, xmm1:  66 0F EF C1
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::Pxor,
            width: SimdWidth::O,
            dst: xmm(0),
            src: xmm(1),
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x0F, 0xEF, 0xC1]));
    }

    // ---- SimdArith sqrtsd/sqrtss (P11 Bucket E) ----

    #[test]
    fn sqrtsd_xmm0_xmm1() {
        // sqrtsd xmm0, xmm1:  F2 0F 51 C1
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::Sqrt,
            width: SimdWidth::SD,
            dst: xmm(0),
            src: xmm(1),
        };
        assert_eq!(o.encode(), Some(vec![0xF2, 0x0F, 0x51, 0xC1]));
    }

    #[test]
    fn sqrtss_xmm0_xmm1() {
        // sqrtss xmm0, xmm1:  F3 0F 51 C1
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::Sqrt,
            width: SimdWidth::SS,
            dst: xmm(0),
            src: xmm(1),
        };
        assert_eq!(o.encode(), Some(vec![0xF3, 0x0F, 0x51, 0xC1]));
    }

    // ---- SimdArith cvtsd2ss/cvtss2sd (P11 Bucket C) ----

    #[test]
    fn cvtsd2ss_xmm0_xmm1() {
        // cvtsd2ss xmm0, xmm1:  F2 0F 5A C1
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::CvtSdSs,
            width: SimdWidth::SD,
            dst: xmm(0),
            src: xmm(1),
        };
        assert_eq!(o.encode(), Some(vec![0xF2, 0x0F, 0x5A, 0xC1]));
    }

    #[test]
    fn cvtss2sd_xmm0_xmm1() {
        // cvtss2sd xmm0, xmm1:  F3 0F 5A C1
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::CvtSdSs,
            width: SimdWidth::SS,
            dst: xmm(0),
            src: xmm(1),
        };
        assert_eq!(o.encode(), Some(vec![0xF3, 0x0F, 0x5A, 0xC1]));
    }

    // ---- SimdRound roundsd/roundss (P11 Bucket D) ----

    #[test]
    fn roundsd_xmm0_xmm0_imm01() {
        // roundsd xmm0, xmm0, 0x01:  66 0F 3A 0B C0 01
        let o = X86_64Opcode::SimdRound {
            width: SimdWidth::SD,
            imm8: 0x01,
            dst: xmm(0),
            src: xmm(0),
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x0F, 0x3A, 0x0B, 0xC0, 0x01]));
    }

    #[test]
    fn roundss_xmm0_xmm0_imm02() {
        // roundss xmm0, xmm0, 0x02:  66 0F 3A 0A C0 02
        let o = X86_64Opcode::SimdRound {
            width: SimdWidth::SS,
            imm8: 0x02,
            dst: xmm(0),
            src: xmm(0),
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x0F, 0x3A, 0x0A, 0xC0, 0x02]));
    }

    // ---- P11 Bucket G: movaps/movapd reg-reg ----

    #[test]
    fn movaps_xmm1_xmm0() {
        // movaps xmm1, xmm0: 0F 28 C8
        let o = X86_64Opcode::SimdMov {
            width: SimdWidth::PS,
            opcode10: true,
            dst: xmm(1),
            src: xmm(0),
        };
        assert_eq!(o.encode(), Some(vec![0x0F, 0x28, 0xC8]));
    }

    #[test]
    fn movapd_xmm1_xmm0() {
        // movapd xmm1, xmm0: 66 0F 28 C8
        let o = X86_64Opcode::SimdMov {
            width: SimdWidth::PD,
            opcode10: true,
            dst: xmm(1),
            src: xmm(0),
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x0F, 0x28, 0xC8]));
    }

    // ---- Setcc r8 (P11 Bucket H) ----

    #[test]
    fn setcc_eq_al() {
        // sete al: 0F 94 C0
        let o = X86_64Opcode::Setcc { cc: CondCode::Eq, dst: reg(0, Width::B8) };
        assert_eq!(o.encode(), Some(vec![0x0F, 0x94, 0xC0]));
    }

    #[test]
    fn setcc_ne_cl() {
        // setne cl: 0F 95 C1
        let o = X86_64Opcode::Setcc { cc: CondCode::Ne, dst: reg(1, Width::B8) };
        assert_eq!(o.encode(), Some(vec![0x0F, 0x95, 0xC1]));
    }

    #[test]
    fn setcc_b_dl() {
        // setb dl: 0F 92 C2
        let o = X86_64Opcode::Setcc { cc: CondCode::B, dst: reg(2, Width::B8) };
        assert_eq!(o.encode(), Some(vec![0x0F, 0x92, 0xC2]));
    }

    #[test]
    fn setcc_a_bl() {
        // seta bl: 0F 97 C3
        let o = X86_64Opcode::Setcc { cc: CondCode::A, dst: reg(3, Width::B8) };
        assert_eq!(o.encode(), Some(vec![0x0F, 0x97, 0xC3]));
    }

    #[test]
    fn setcc_np_sil_high() {
        // setnp r14b: 41 0F 9B C6  (reg=14 >=8 needs REX.B; reg&7=6, modrm=0xC6)
        let o = X86_64Opcode::Setcc { cc: CondCode::Np, dst: reg(14, Width::B8) };
        assert_eq!(o.encode(), Some(vec![0x41, 0x0F, 0x9B, 0xC6]));
    }

    // ---- P11 Bucket I: NoArg (cld/mfence/rep movsb/stosb/movsq) ----
    #[test]
    fn noarg_cld() {
        let o = X86_64Opcode::NoArg { op: NoArgOp::Cld };
        assert_eq!(o.encode(), Some(vec![0xFC]));
    }

    #[test]
    fn noarg_mfence() {
        let o = X86_64Opcode::NoArg { op: NoArgOp::Mfence };
        assert_eq!(o.encode(), Some(vec![0x0F, 0xAE, 0xF0]));
    }

    #[test]
    fn noarg_rep_movsb() {
        let o = X86_64Opcode::NoArg { op: NoArgOp::RepMovsb };
        assert_eq!(o.encode(), Some(vec![0xF3, 0xA4]));
    }

    #[test]
    fn noarg_rep_stosb() {
        let o = X86_64Opcode::NoArg { op: NoArgOp::RepStosb };
        assert_eq!(o.encode(), Some(vec![0xF3, 0xAA]));
    }

    #[test]
    fn noarg_rep_movsq() {
        let o = X86_64Opcode::NoArg { op: NoArgOp::RepMovsq };
        assert_eq!(o.encode(), Some(vec![0xF3, 0x48, 0xA5]));
    }

    // ---- P11 Bucket J: andps/andpd reg-reg ----
    #[test]
    fn simd_andps_xmm0_xmm1() {
        // andps xmm0, xmm1: 0F 54 C1
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::Andps, width: SimdWidth::O,
            dst: Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B128),
            src: Operand::Reg(1, crate::codegen::shared::asm_operand::Width::B128),
        };
        assert_eq!(o.encode(), Some(vec![0x0F, 0x54, 0xC1]));
    }

    #[test]
    fn simd_andpd_xmm0_xmm1() {
        // andpd xmm0, xmm1: 66 0F 54 C1
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::Andpd, width: SimdWidth::O,
            dst: Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B128),
            src: Operand::Reg(1, crate::codegen::shared::asm_operand::Width::B128),
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x0F, 0x54, 0xC1]));
    }

    // ---- P11 Bucket K: movd/movq GPR↔XMM reg-reg ----
    #[test]
    fn gprxmm_movq_rax_to_xmm1() {
        // movq xmm1, rax: 66 48 0F 6E C8  (xmm=1, gpr=0; modrm=0xC8)
        let o = X86_64Opcode::GprXmm {
            to_xmm: true,
            width: crate::codegen::shared::asm_operand::Width::B64,
            dst: Operand::Reg(1, crate::codegen::shared::asm_operand::Width::B128),
            src: Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64),
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x48, 0x0F, 0x6E, 0xC8]));
    }

    #[test]
    fn gprxmm_movq_xmm0_to_rax() {
        // movq rax, xmm0: 66 48 0F 7E C0  (xmm=0, gpr=0; modrm=0xC0)
        let o = X86_64Opcode::GprXmm {
            to_xmm: false,
            width: crate::codegen::shared::asm_operand::Width::B64,
            dst: Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64),
            src: Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B128),
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x48, 0x0F, 0x7E, 0xC0]));
    }

    #[test]
    fn gprxmm_movd_eax_to_xmm0() {
        // movd xmm0, eax: 0F 6E C0  (no 66, no REX.W; B32)
        let o = X86_64Opcode::GprXmm {
            to_xmm: true,
            width: crate::codegen::shared::asm_operand::Width::B32,
            dst: Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B128),
            src: Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B32),
        };
        assert_eq!(o.encode(), Some(vec![0x0F, 0x6E, 0xC0]));
    }

    // ---- P11 Bucket B: AluRm reg, [base+disp] ----

    #[test]
    fn alu_rm_add_r64_rsp_disp8() {
        // add rax, [rsp+16]: 48 03 44 24 10 (reg=rax=0 modrm.reg; SIB)
        let o = X86_64Opcode::AluRm {
            op: AluOp::Add,
            width: Width::B64,
            dst: reg(0, Width::B64),
            src: Operand::Mem(MemOperand {
                disp: 16, base: Some(4), index: None, seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x03, 0x44, 0x24, 0x10]));
    }

    #[test]
    fn alu_rm_sub_r64_rax_disp8() {
        // sub rax, [rax+8]: 48 2B 40 08 (reg=rax=0; rm=rax=0)
        let o = X86_64Opcode::AluRm {
            op: AluOp::Sub,
            width: Width::B64,
            dst: reg(0, Width::B64),
            src: Operand::Mem(MemOperand {
                disp: 8, base: Some(0), index: None, seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x2B, 0x40, 0x08]));
    }

    #[test]
    fn alu_rm_and_r64_r13_disp8() {
        // and rax, [r13+16]: 49 23 45 10 (REX.WB: base=r13=13)
        let o = X86_64Opcode::AluRm {
            op: AluOp::And,
            width: Width::B64,
            dst: reg(0, Width::B64),
            src: Operand::Mem(MemOperand {
                disp: 16, base: Some(13), index: None, seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x49, 0x23, 0x45, 0x10]));
    }

    // ---- P11 Bucket C: SimdMov any base+disp ----

    #[test]
    fn simd_mov_store_sd_xmm0_rax_disp8() {
        // movsd [rax+8], xmm0: F2 0F 11 40 08
        let o = X86_64Opcode::SimdMov {
            width: SimdWidth::SD,
            opcode10: false,
            dst: Operand::Mem(MemOperand {
                disp: 8, base: Some(0), index: None, seg: None,
            }),
            src: Operand::Reg(0, Width::B128),
        };
        assert_eq!(o.encode(), Some(vec![0xF2, 0x0F, 0x11, 0x40, 0x08]));
    }

    #[test]
    fn simd_mov_load_ss_xmm0_rsp_disp0() {
        // movss xmm0, [rsp]: F3 0F 10 04 24 (mod=00, SIB base=rsp, 无 disp)
        let o = X86_64Opcode::SimdMov {
            width: SimdWidth::SS,
            opcode10: true,
            dst: Operand::Reg(0, Width::B128),
            src: Operand::Mem(MemOperand {
                disp: 0, base: Some(4), index: None, seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0xF3, 0x0F, 0x10, 0x04, 0x24]));
    }

    // ---- P11 Bucket E2: SimdMov [rsp+disp8] sweep ----

    #[test]
    fn simd_mov_store_sd_xmm0_rsp_disp8() {
        // movsd [rsp+8], xmm0: F2 0F 11 44 24 08 (mod=01, SIB base=rsp)
        let o = X86_64Opcode::SimdMov {
            width: SimdWidth::SD,
            opcode10: false,
            dst: Operand::Mem(MemOperand {
                disp: 8, base: Some(4), index: None, seg: None,
            }),
            src: Operand::Reg(0, Width::B128),
        };
        assert_eq!(o.encode(), Some(vec![0xF2, 0x0F, 0x11, 0x44, 0x24, 0x08]));
    }

    #[test]
    fn simd_mov_load_sd_xmm1_rsp_disp8() {
        // movsd xmm1, [rsp+8]: F2 0F 10 4C 24 08
        let o = X86_64Opcode::SimdMov {
            width: SimdWidth::SD,
            opcode10: true,
            dst: Operand::Reg(1, Width::B128),
            src: Operand::Mem(MemOperand {
                disp: 8, base: Some(4), index: None, seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0xF2, 0x0F, 0x10, 0x4C, 0x24, 0x08]));
    }

    #[test]
    fn simd_mov_store_ss_xmm0_rsp_disp0() {
        // movss [rsp], xmm0: F3 0F 11 04 24 (mod=00)
        let o = X86_64Opcode::SimdMov {
            width: SimdWidth::SS,
            opcode10: false,
            dst: Operand::Mem(MemOperand {
                disp: 0, base: Some(4), index: None, seg: None,
            }),
            src: Operand::Reg(0, Width::B128),
        };
        assert_eq!(o.encode(), Some(vec![0xF3, 0x0F, 0x11, 0x04, 0x24]));
    }

    #[test]
    fn simd_mov_store_dq_xmm0_rsp_disp8() {
        // movdqa [rsp+16], xmm0: 66 0F 7F 44 24 10
        let o = X86_64Opcode::SimdMov {
            width: SimdWidth::DQ,
            opcode10: false,
            dst: Operand::Mem(MemOperand {
                disp: 16, base: Some(4), index: None, seg: None,
            }),
            src: Operand::Reg(0, Width::B128),
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x0F, 0x7F, 0x44, 0x24, 0x10]));
    }

    #[test]
    fn simd_mov_load_dq_xmm2_rsp_disp8() {
        // movdqa xmm2, [rsp+16]: 66 0F 6F 54 24 10
        let o = X86_64Opcode::SimdMov {
            width: SimdWidth::DQ,
            opcode10: true,
            dst: Operand::Reg(2, Width::B128),
            src: Operand::Mem(MemOperand {
                disp: 16, base: Some(4), index: None, seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x0F, 0x6F, 0x54, 0x24, 0x10]));
    }

    // ---- P11 Bucket E3: SimdMov [rax+disp8] sweep ----

    #[test]
    fn simd_mov_load_ss_xmm0_rax_disp0() {
        // movss xmm0, [rax]: F3 0F 10 00 (mod=00, no SIB)
        let o = X86_64Opcode::SimdMov {
            width: SimdWidth::SS,
            opcode10: true,
            dst: Operand::Reg(0, Width::B128),
            src: Operand::Mem(MemOperand {
                disp: 0, base: Some(0), index: None, seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0xF3, 0x0F, 0x10, 0x00]));
    }

    #[test]
    fn simd_mov_load_ss_xmm0_rax_disp8() {
        // movss xmm0, [rax+4]: F3 0F 10 40 04 (mod=01, reg=xmm0<<3=0, rm=rax=0)
        let o = X86_64Opcode::SimdMov {
            width: SimdWidth::SS,
            opcode10: true,
            dst: Operand::Reg(0, Width::B128),
            src: Operand::Mem(MemOperand {
                disp: 4, base: Some(0), index: None, seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0xF3, 0x0F, 0x10, 0x40, 0x04]));
    }
}

