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

    /// SSE/AVX 移动 — 步骤 4: 收敛 xmm store/load 到 rsp 槽.
    /// width 决定 legacy 前缀 (F3=SS, F2=SD, 无=O/PS), opcode 决定 0F 10(load) / 0F 11(store).
    /// dst=Reg(xmm, Xmm) / src=Mem(rsp, disp) 或反向.
    SimdMov {
        width: SimdWidth,
        opcode10: bool,
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
                // TEST 走 0x84/0x85 (无 ModRM.reg 语义), 暂不收敛.
                if matches!(op, AluOp::Test) {
                    return None;
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
                        let mut rex = 0x48u8;
                        if *reg_idx >= 8 {
                            rex |= 0x01;
                        }
                        out.push(rex);
                        out.push(0xB8 + (reg_idx & 7));
                        out.extend_from_slice(&(*imm as i64).to_le_bytes());
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
                // P11: 收敛 movzx/movsx/movsxd 的 reg-reg 形态.
                //   MOVZX r32, r/m8 (B8):  0F B6 /r
                //   MOVZX r32, r/m16 (B16): 0F B7 /r
                //   MOVSX r32, r/m8 (B8):  0F BE /r
                //   MOVSX r32, r/m16 (B16): 0F BF /r
                //   MOVSXD r64, r/m32 (B64): 63 /r (REX.W=1)
                // dst ModRM.reg; src ModRM.r/m.
                let (Operand::Reg(dst_idx, _), Operand::Reg(src_idx, _)) = (dst, src) else {
                    return None;
                };
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
                }
            }
            X86_64Opcode::SimdMov { width, opcode10, dst, src } => {
                // 步骤 4: 收敛 xmm store/load 到 rsp 槽.
                //   load:  dst=Reg(xmm, B128), src=Mem(rsp=4, disp)
                //   store: dst=Mem(rsp=4, disp), src=Reg(xmm, B128)
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
                if base != 4 {
                    // 只收敛 rsp 槽形态; 其他 base 走原手写.
                    return None;
                }
                let _ = is_store;
                // legacy 前缀: SS=F3, SD=F2, O/PS=无. PD/DQ 步骤 4 不用.
                let pfx: u8 = match width {
                    SimdWidth::SS => 0xF3,
                    SimdWidth::SD => 0xF2,
                    SimdWidth::O | SimdWidth::PS => 0x00,
                    _ => return None,
                };
                // REX: xmm>=8 → REX.R=0x04; base=rsp 已在 ModRM 里用 SIB 表达, 不发 REX.B.
                let rex: u8 = if xmm_idx >= 8 { 0x44 } else { 0 };
                let opc2: u8 = if *opcode10 { 0x10 } else { 0x11 };
                let disp = mem.disp;
                let mut out = Vec::with_capacity(12);
                if pfx != 0 {
                    out.push(pfx);
                }
                if rex != 0 {
                    out.push(rex);
                }
                out.push(0x0F);
                out.push(opc2);
                // ModRM/SIB: base=rsp=4 必须 SIB. disp8 vs disp32.
                if let Ok(d) = i8::try_from(disp) {
                    out.push(0x44 | ((xmm_idx & 7) << 3));
                    out.push(0x24);
                    out.push(d as u8);
                } else {
                    out.push(0x84 | ((xmm_idx & 7) << 3));
                    out.push(0x24);
                    out.extend_from_slice(&disp.to_le_bytes());
                }
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
        // mov rax, 0x80000000:  48 B8 00 00 00 80 00 00 00 00
        let o = X86_64Opcode::MovRm {
            width: Width::B64,
            dst: reg(0, Width::B64),
            src: Operand::Imm(0x80000000),
        };
        assert_eq!(o.encode(), Some(vec![
            0x48, 0xB8, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00
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
}

