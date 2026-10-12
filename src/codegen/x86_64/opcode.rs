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
    /// P11 Bucket M: ADC r/m64, r64 (0x11 /r) and r64, r/m64 (0x13 /r).
    Adc,
    /// P11 Bucket M: SBB r/m64, r64 (0x19 /r) and r64, r/m64 (0x1B /r).
    Sbb,
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

/// P11 Bucket M: UnaryOp::Mul uses /4 modrm_reg_field. Neg/Not are covered above.
// (no change to enum body; we just lift the #[allow(dead_code)] for Mul usage)

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
    /// rep stosq: F3 48 AB (RAX → ES:[rdi], rcx qword 计数).
    RepStosq,
    /// rep movsq: F3 48 A5 (qword 版本, 带 REX.W).
    RepMovsq,
    /// syscall: 0F 05 (fast system call into 64-bit kernel).
    Syscall,
    /// int3: CC (debugger breakpoint).
    Int3,
    /// xgetbv: 0F 01 D0 (x87 control/status -> EDX:EAX).
    Xgetbv,
    /// cpuid: 0F A2 (CPU feature identification).
    Cpuid,
    /// ret near: C3 (return to caller).
    RetNear,
    /// P11 Bucket M: ud2 = 0F 0B (explicit undefined for zero-div UB).
    Ud2,
    /// P11 Bucket AS: fwait = 9B (x87 synchronization prefix).
    Fwait,
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
    /// P11 Bucket G: unpcklps xmm_dst, xmm_src (0F 14 /r) — low-half interleave.
    Unpcklps,
    /// P11 Bucket G2: packed shift by imm8 (psllq/psrlq/psrad).
    ///   psllq: 66 0F F3 /r ib, psrlq: 66 0F D3 /r ib, psrad: 66 0F E2 /r ib.
    PShiftImm8,
    /// P11 Bucket G2: packed compare equal (pcmpeqb/w/d).
    ///   pcmpeqb: 66 0F 74 /r, pcmpeqw: 66 0F 75 /r, pcmpeqd: 66 0F 76 /r.
    PCmpEq,
    /// P11 Bucket G2: unsigned byte min/max (pminub/pmaxub).
    ///   pminub: 0F DA /r, pmaxub: 0F DE /r.
    PMinMaxUb,
    /// P11 Bucket G2: pmaddwd 66 0F F5 /r.
    PMaddwd,
    /// P11 Bucket G2: psadbw 66 0F F6 /r.
    PSadbw,
    /// P11 Bucket G2: signed/unsigned saturating add/sub word.
    ///   paddsw: 66 0F ED /r, psubsw: 66 0F E9 /r
    ///   paddusw: 66 0F DD /r, psubusw: 66 0F D9 /r.
    PSatW,
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
    /// P11 Bucket N8: fnstenv/fldenv m14/28 — D9 /7 (store) or /4 (load).
    ///  mem 为 base-only [base+disp0], base∈{rdi(7)} 本批使用. 无 SIB.
    ///   fnstenv [rdi]: D9 37
    ///   fldenv  [rdi]: D9 27
    EnvMem { store: bool },
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
        rip_relative: bool,
    },

    /// MOVSX/MOVZX — 步骤 1 不实现.
    MovExt {
        sign: bool,
        from: crate::codegen::shared::asm_operand::Width,
        to: crate::codegen::shared::asm_operand::Width,
        dst: Operand,
        src: Operand,
        rip_relative: bool,
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

    /// P11 Bucket G2: psllq/psrlq/psrad xmm, imm8.
    ///   psllq: 66 0F F3 /r ib, psrlq: 66 0F D3 /r ib, psrad: 66 0F E2 /r ib.
    /// op in {PShift::L/Q, R/Q, R/D} encoded via width.
    SimdShiftImm8 {
        op: SimdArithOp,
        width: SimdWidth,
        dst: Operand,
        imm8: u8,
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
        /// true → GPR operand is r64 (REX.W=1); false → r32 (no REX.W).
        gpr_64: bool,
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
    ///   movd r32->xmm:  66 0F 6E /r (66 -> SSE2 XMM; no REX.W)
    ///   movd xmm->r32:  66 0F 7E /r
    /// to_xmm=true: GPR→XMM (0x6E); false: XMM→GPR (0x7E). width=B64 emits REX.W.
    GprXmm {
        to_xmm: bool,
        width: crate::codegen::shared::asm_operand::Width,
        dst: Operand,
        src: Operand,
    },

    /// P11 Bucket N6: BSF/BSR reg, reg (0F BC / 0F BD).
    ///   BSF r64,r/m64:  REX.W 0F BC /r
    ///   BSR r64,r/m64:  REX.W 0F BD /r
    ///   BSF r32,r/m32:    0F BC /r
    ///   BSR r32,r/m32:    0F BD /r
    /// dst ModRM.reg (destination), src ModRM.r/m (source).
    BitScan {
        is_bsr: bool,
        width: crate::codegen::shared::asm_operand::Width,
        dst: Operand,
        src: Operand,
    },

    /// P11 Bucket N6: CMOVcc r64,r/m64 (0F 40..4F /r).
    ///  B64 强制 REX.W=0x48. dst ModRM.reg (destination), src ModRM.r/m (source).
    ///  B32 无 REX.W.
    Cmovcc {
        cc: CondCode,
        width: crate::codegen::shared::asm_operand::Width,
        dst: Operand,
        src: Operand,
    },

    /// P11 Bucket O3: LOCK CMPXCHG r/m8/16/32/64, r8/16/32/64.
    ///  手写约定 (instructions.rs emit_lock_cmpxchg_rdi_rdx/rsi):
    ///    B8:  F0 40 0F B0 ModRM   (REX=0x40 用于 sil/dil 高字节, W=0)
    ///    B16: 66 F0 0F B1 ModRM
    ///    B32: F0 0F B1 ModRM      (无 REX.W)
    ///    B64: F0 48 0F B1 ModRM   (REX.W=0x48)
    ///  mem 端为 [base+disp0] base-only 形态 (本批只覆盖 rdi 固定 base).
    LockCmpxchgRm {
        width: crate::codegen::shared::asm_operand::Width,
        reg_idx: u8,
        mem: MemOperand,
    },

    /// P11 Bucket O3: LOCK XCHG r/m8/16/32/64, r8/16/32/64.
    ///  手写约定 (emit_lock_xchg_rdi_rsi):
    ///    B8:  F0 40 86 ModRM
    ///    B16: 66 F0 87 ModRM
    ///    B32: F0 87 ModRM
    ///    B64: F0 48 87 ModRM
    LockXchgRm {
        width: crate::codegen::shared::asm_operand::Width,
        reg_idx: u8,
        mem: MemOperand,
    },

    /// P11 Bucket N8: LOCK XADD r/m8/16/32/64, r8/16/32/64.
    ///  手写约定 (emit_atomic_xadd in special/builtins.rs):
    ///    B8:  F0 40 0F C0 ModRM     (REX=0x40 用于 sil/dil 高字节)
    ///    B16: 66 F0 0F C1 ModRM
    ///    B32: F0 0F C1 ModRM        (无 REX.W)
    ///    B64: F0 48 0F C1 ModRM     (REX.W=0x48)
    ///  mem 端为 [base+disp0] base-only 形态.
    LockXaddRm {
        width: crate::codegen::shared::asm_operand::Width,
        reg_idx: u8,
        mem: MemOperand,
    },

    /// P11 Bucket AH: XCHG r8/16/32/64, r8/16/32/64 (reg↔reg only).
    ///  手写约定 (int_ops.rs / vector.rs):
    ///    B8:  [REX] 86 ModRM      (REX=0x40 on byte ops for sil/dil high-byte)
    ///    B16: 66 [REX] 87 ModRM
    ///    B32: [REX] 87 ModRM
    ///    B64: [REX.W] 87 ModRM
    ///  若 dst=rax(0) 则走 accumulator-shortcut 0x90+reg (无 ModRM):
    ///    B16: 66 90+reg   B32/B64: [REX.W] 90+reg.
    XchgRm {
        width: crate::codegen::shared::asm_operand::Width,
        dst: Operand,
        src: Operand,
        /// 若 true 且 dst=rax, 走 accumulator-shortcut (0x90+src).
        /// 若 false, 强制走 ModRM form (用于 G10 快照字节一致).
        prefer_shortcut: bool,
    },

    /// P11 Bucket AF: cmpss/cmpsd xmm_dst, xmm_src, imm8 (C2 /r ib).
    ///   cmpss: F3 0F C2 /r ib   cmpsd: F2 0F C2 /r ib.
    /// width=SimdWidth::SS → F3, SimdWidth::SD → F2. imm8 = comparison predicate.
    SimdCmpImm8 {
        width: SimdWidth,
        imm8: u8,
        dst: Operand,
        src: Operand,
    },

    /// P11 Bucket AF: pmulld xmm_dst, xmm_src (SSE4.1: 66 0F 38 40 /r).
    Pmulld { dst: Operand, src: Operand },

    /// P11 Bucket P11R: direct call rel32 (E8 id). Emits `E8 00 00 00 00`;
    /// the rel32 field is patched later via PendingJump/pending_func_rels.
    /// No REX, no ModRM — opcode is fixed.
    CallRel32,

    /// P11 Bucket P11R: direct jmp rel32 (E9 id). Emits `E9 00 00 00 00`;
    /// rel32 field patched later via PendingJump/pending_func_rels.
    JmpRel32,

    /// P11 Bucket P11R: jmp r/m64 (FF /4). Used for indirect jumps through
    /// memory [base+disp] (reg form already covered by Jmp).
    /// modrm_mod chosen by disp size (00/01/10); SIB emitted when base=rsp/r12.
    JmpRmMem { base: u8, disp: i32 },

    /// P11 Bucket P11R: push m64 [rsp+disp8] (FF /6). Only base=rsp(4) SIB
    /// form is encoded here (used by complex.rs spill pushes).
    PushMemDisp8 { base: u8, disp: i8 },

    /// P11 Bucket P11R: movss/movsd [rip+disp32], xmm or xmm, [rip+disp32].
    /// mod=00 r/m=101 (rip-relative). disp32 is patched via float_refs /
    /// pending_tls_rels / pending_data_rels.
    SimdMovRip {
        store: bool,
        width: SimdWidth,
        xmm: u8,
    },
}

impl X86_64Opcode {
    /// 编码为字节序列. None 表示该形态尚未实现, 发射方继续走 raw_bytes 兜底.
    pub(crate) fn encode(&self) -> Option<Vec<u8>> {
        use crate::codegen::shared::asm_operand::Width;
        match self {
            X86_64Opcode::AluRm {
                op,
                width,
                dst,
                src,
            } => {
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
                                AluOp::Adc => 2,
                                AluOp::Sbb => 3,
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
                                AluOp::Adc => 2,
                                AluOp::Sbb => 3,
                                AluOp::And => 4,
                                AluOp::Sub => 5,
                                AluOp::Xor => 6,
                                AluOp::Cmp => 7,
                                AluOp::Test => unreachable!(),
                            };
                            let is_b64 = matches!(w, Width::B64);
                            let is_b32 = matches!(w, Width::B32);
                            let is_b16 = matches!(w, Width::B16);
                            let is_b8 = matches!(w, Width::B8);
                            if is_b64 || is_b32 || is_b16 || is_b8 {
                                let mut rex: u8 = 0x40;
                                if is_b64 {
                                    rex |= 0x08;
                                }
                                if *reg_idx >= 8 {
                                    rex |= 0x01;
                                }
                                let need_rex = is_b64 || *reg_idx >= 8 || (is_b8 && *reg_idx >= 4);
                                let modrm: u8 = 0xC0 | (reg_field << 3) | (reg_idx & 7);
                                let mut out = Vec::with_capacity(6);
                                if is_b16 {
                                    out.push(0x66);
                                }
                                if need_rex {
                                    out.push(rex);
                                }
                                out.push(if is_b8 { 0x80 } else { 0x83 });
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
                        AluOp::Adc => 0x13,
                        AluOp::Sbb => 0x1B,
                        AluOp::And => 0x23,
                        AluOp::Sub => 0x2B,
                        AluOp::Xor => 0x33,
                        AluOp::Cmp => 0x3B,
                        AluOp::Test => unreachable!(),
                    };
                    let mut rex: u8 = 0x40;
                    if is_b64 {
                        rex |= 0x08;
                    }
                    if *dst_idx >= 8 {
                        rex |= 0x04;
                    }
                    if base >= 8 {
                        rex |= 0x01;
                    }
                    let need_rex = is_b64 || *dst_idx >= 8 || base >= 8;
                    // disp 长度: base=rbp(5) mod=00 → rip-relative, 需 disp=0 时走 mod=01.
                    // base=rsp(4) 必须 SIB (rm_field=4).
                    let disp = mem.disp;
                    let (modrm_mod, disp_bytes): (u8, &[u8]) =
                        if disp == 0 && base != 5 && base != 4 {
                            (0, &[])
                        } else if let Ok(d) = i8::try_from(disp) {
                            (1, &[d as u8])
                        } else {
                            (2, &disp.to_le_bytes())
                        };
                    let rm_field = base & 7;
                    let modrm = (modrm_mod << 6) | ((*dst_idx & 7) << 3) | rm_field;
                    let mut out = Vec::with_capacity(12);
                    if need_rex {
                        out.push(rex);
                    }
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
                    AluOp::Add => {
                        if is_byte8 {
                            0x00
                        } else {
                            0x01
                        }
                    },
                    AluOp::Or => {
                        if is_byte8 {
                            0x08
                        } else {
                            0x09
                        }
                    },
                    // P11 Bucket M: ADC r/m,r = 10/11; SBB r/m,r = 18/19.
                    AluOp::Adc => {
                        if is_byte8 {
                            0x10
                        } else {
                            0x11
                        }
                    },
                    AluOp::Sbb => {
                        if is_byte8 {
                            0x18
                        } else {
                            0x19
                        }
                    },
                    AluOp::And => {
                        if is_byte8 {
                            0x20
                        } else {
                            0x21
                        }
                    },
                    AluOp::Sub => {
                        if is_byte8 {
                            0x28
                        } else {
                            0x29
                        }
                    },
                    AluOp::Xor => {
                        if is_byte8 {
                            0x30
                        } else {
                            0x31
                        }
                    },
                    AluOp::Cmp => {
                        if is_byte8 {
                            0x38
                        } else {
                            0x39
                        }
                    },
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
                let need_high_byte = is_byte8 && (*src_idx >= 4 || *dst_idx >= 4);
                let need_rex =
                    matches!(width, Width::B64) || *src_idx >= 8 || *dst_idx >= 8 || need_high_byte;
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
            },
            X86_64Opcode::Lea {
                dst,
                mem,
                rip_relative,
            } => {
                // P11 Bucket P: 扩展 Lea.encode() 支持 SIB index.
                // 已有形态: LEA reg64, [base+disp] (无 index, base != rsp/r12).
                // 新增形态: LEA reg64, [base + index*scale + disp] (scale=1 无 disp).
                //   - REX: W=1(B64) R=dst>=8 X=index>=8 B=base>=8.
                //   - ModRM: mod=00(无 disp), reg=dst, r/m=100(=SIB follow).
                //   - SIB: scale<<6 | (index&7)<<3 | (base&7).
                let (Operand::Reg(dst_idx, Width::B64), mem) = (dst, mem) else {
                    return None;
                };
                if mem.seg.is_some() {
                    return None;
                }
                // P11: LEA reg64, [rip+disp32] (mod=00 r/m=101). disp32 reloc is patched
                // by callers (pending_data_rels). REX only carries REX.R for dst>=8;
                // REX.B is irrelevant (no base register).
                if *rip_relative {
                    if mem.base.is_some() || mem.index.is_some() {
                        return None;
                    }
                    let mut rex: u8 = 0x48; // REX.W=1 for B64
                    if *dst_idx >= 8 {
                        rex |= 0x04; // REX.R
                    }
                    let modrm = 0x05 | ((*dst_idx & 7) << 3); // mod=00, r/m=101 (rip-rel)
                    let disp = mem.disp;
                    let mut out = Vec::with_capacity(7);
                    out.push(rex);
                    out.push(0x8D);
                    out.push(modrm);
                    out.extend_from_slice(&disp.to_le_bytes());
                    return Some(out);
                }
                let base = mem.base?;
                let disp = mem.disp;
                let mut rex: u8 = 0x40;
                if *dst_idx >= 8 {
                    rex |= 0x04; // REX.R
                }
                if base >= 8 {
                    rex |= 0x01; // REX.B
                }
                rex |= 0x08; // REX.W=1 for B64 lea

                // 路径 A: 无 index, 且 base != rsp/r12 — 原有 bp+disp 逻辑.
                if mem.index.is_none() && base != 4 && base != 12 {
                    let (modrm_mod, disp_bytes): (u8, &[u8]) = if i8::try_from(disp).is_ok() {
                        (1, &disp.to_le_bytes()[..1])
                    } else {
                        (2, &disp.to_le_bytes()[..4])
                    };
                    let modrm = (modrm_mod << 6) | ((*dst_idx & 7) << 3) | (base & 7);
                    let mut out = Vec::with_capacity(8);
                    out.push(rex);
                    out.push(0x8D);
                    out.push(modrm);
                    out.extend_from_slice(disp_bytes);
                    Some(out)
                } else if mem.index.is_none() && (base == 4 || base == 12) {
                    // 路径 A2: base=rsp(4) 或 r12(12) 且无 index — 必须插 SIB
                    // (SIB base=base&7=4, index=4=ESP 编码=无 index). 支持任意 disp.
                    let (modrm_mod, disp_bytes): (u8, &[u8]) = if i8::try_from(disp).is_ok() {
                        (1, &disp.to_le_bytes()[..1])
                    } else {
                        (2, &disp.to_le_bytes()[..4])
                    };
                    // modrm: reg=dst, r/m=100 (SIB follows).
                    let modrm = (modrm_mod << 6) | ((*dst_idx & 7) << 3) | 0x04;
                    // SIB: scale=0, index=4(no index), base=base&7.
                    let sib = 0x20 | (base & 7);
                    let mut out = Vec::with_capacity(8);
                    out.push(rex);
                    out.push(0x8D);
                    out.push(modrm);
                    out.push(sib);
                    out.extend_from_slice(disp_bytes);
                    Some(out)
                } else if let Some((index_idx, scale)) = mem.index {
                    // 路径 B: 有 index — SIB. 本步支持 disp=0, scale∈{1,2,4,8}.
                    if disp != 0 {
                        return None;
                    }
                    let sc_bits = match scale {
                        1 => 0u8,
                        2 => 1,
                        4 => 2,
                        8 => 3,
                        _ => return None,
                    };
                    if index_idx >= 8 {
                        rex |= 0x02; // REX.X
                    }
                    // modrm: mod=00, reg=dst, r/m=100 (SIB follows).
                    let modrm = ((*dst_idx & 7) << 3) | 0x04;
                    let sib = (sc_bits << 6) | ((index_idx & 7) << 3) | (base & 7);
                    let mut out = Vec::with_capacity(6);
                    out.push(rex);
                    out.push(0x8D);
                    out.push(modrm);
                    out.push(sib);
                    Some(out)
                } else {
                    // base == rsp/r12 且无 index — 仍走手写兜底.
                    None
                }
            },
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
                    // P11 Bucket M: MUL r64 = F7 /4 (REX.W=1, modrm mod=11 reg=/4 r/m=reg).
                    // Uses RAX:RDX implicitly. No B8 form for MUL, so gate via is_byte.
                    UnaryOp::Mul => (0xF7, 0b100),
                    // P11 Bucket N5: IMUL r32/r64 (single-operand, RAX implicit) = F7 /5.
                    // B8 uses F6 like MUL/NOT/NEG (byte form).
                    UnaryOp::Imul => (if is_byte { 0xF6 } else { 0xF7 }, 0b101),
                    // P11 Bucket O: DIV r/m = F7 /6; IDIV r/m = F7 /7. B8 uses F6.
                    UnaryOp::Div => (if is_byte { 0xF6 } else { 0xF7 }, 0b110),
                    UnaryOp::Idiv => (if is_byte { 0xF6 } else { 0xF7 }, 0b111),
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
            },
            X86_64Opcode::Shift {
                op,
                width,
                dst,
                amount,
            } => {
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
                    // P11 Bucket M: RCL by imm8/CL — modrm_reg_field = /2.
                    ShiftOp::Rcl => 0b010,
                    // P11 Bucket N5: ROL= /0, ROR= /1 (D3/D1/C1 ModRM=11.. field).
                    ShiftOp::Rol => 0b000,
                    ShiftOp::Ror => 0b001,
                    ShiftOp::Rcr => 0b011,
                };
                let is_b8 = matches!(width, Width::B8);
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
                // 8-bit shifts: REX.B still required for r8-r15; no REX.W.
                // Also spl/bpl/sil/dil (4..7) require REX to select high-byte names.
                let need_rex =
                    matches!(width, Width::B64) || *reg_idx >= 8 || (is_b8 && *reg_idx >= 4);
                if need_rex {
                    out.push(rex);
                }
                let opc_byte: u8 = match amount {
                    ShiftAmount::Cl => {
                        if is_b8 {
                            0xD2
                        } else {
                            0xD3
                        }
                    },
                    ShiftAmount::Imm8(1) => {
                        if is_b8 {
                            0xD0
                        } else {
                            0xD1
                        }
                    },
                    ShiftAmount::Imm8(_) => {
                        if is_b8 {
                            0xC0
                        } else {
                            0xC1
                        }
                    },
                };
                out.push(opc_byte);
                out.push(0xC0 | (modrm_reg_field << 3) | (reg_idx & 7));
                if let ShiftAmount::Imm8(n) = amount {
                    if *n != 1 {
                        out.push(*n);
                    }
                }
                Some(out)
            },
            X86_64Opcode::Raw { bytes, .. } => Some(bytes.clone()),
            X86_64Opcode::MovRm {
                width,
                dst,
                src,
                rip_relative,
            } => {
                // P11 Bucket O8: rip-relative mov reg, [rip+disp32] / mov [rip+disp32], reg.
                //   load (dst=Reg, src=Mem): 0x8B, ModRM = 00_ddd_101 (reg=dst, r/m=101=rip).
                //   store (dst=Mem, src=Reg): 0x89, ModRM = 00_ddd_101 (reg=src, r/m=101=rip).
                //   B16: 0x66 prefix. B64: REX.W=0x48. B8: 0x8A/0x88, spl/bpl/sil/dil 需 REX.
                //   REX.R = reg>=8; REX.B 与 r/m=101 无关 (rip 无 base 寄存器).
                if *rip_relative {
                    let (reg_idx, mem, is_store) = match (dst, src) {
                        (Operand::Reg(r, _), Operand::Mem(m)) => (*r, m, false),
                        (Operand::Mem(m), Operand::Reg(r, _)) => (*r, m, true),
                        _ => return None,
                    };
                    if mem.base.is_some() || mem.index.is_some() || mem.seg.is_some() {
                        return None;
                    }
                    let (opcode, need_prefix66, need_rex_w) = match width {
                        Width::B8 => (if is_store { 0x88 } else { 0x8A }, false, false),
                        Width::B16 => (if is_store { 0x89 } else { 0x8B }, true, false),
                        Width::B32 => (if is_store { 0x89 } else { 0x8B }, false, false),
                        Width::B64 => (if is_store { 0x89 } else { 0x8B }, false, true),
                        _ => return None,
                    };
                    let mut rex: u8 = 0x40;
                    if need_rex_w {
                        rex |= 0x08;
                    }
                    if reg_idx >= 8 {
                        rex |= 0x04; // REX.R
                    }
                    let need_high_byte = matches!(width, Width::B8) && reg_idx >= 4;
                    let need_rex = need_rex_w || reg_idx >= 8 || need_high_byte;
                    let modrm = 0x05 | ((reg_idx & 7) << 3); // mod=00, r/m=101
                    let disp = mem.disp;
                    let mut out = Vec::with_capacity(8);
                    if need_prefix66 {
                        out.push(0x66);
                    }
                    if need_rex {
                        out.push(rex);
                    }
                    out.push(opcode);
                    out.push(modrm);
                    out.extend_from_slice(&disp.to_le_bytes());
                    return Some(out);
                }
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
                    },
                    (Operand::Reg(reg, _), Operand::Mem(mem)) => {
                        // load: mem -> reg
                        (*reg, mem)
                    },
                    _ => return None,
                };
                // 本步支持 base-only 或 [base+index*scale] (scale=1/2/4/8, disp=0).
                // seg 前缀 / 其他 scale 先回退手写.
                if mem.seg.is_some() {
                    return None;
                }
                let (index_reg, index_scale) = match mem.index {
                    None => (None, 0u8),
                    Some((ir, s)) => {
                        if s != 1 && s != 2 && s != 4 && s != 8 {
                            return None;
                        }
                        (Some(ir), s)
                    },
                };
                let base = mem.base?;
                let is_store = matches!(dst, Operand::Mem(_));
                let (opcode, need_prefix66, need_rex_w) = match width {
                    Width::B8 => (if is_store { 0x88 } else { 0x8A }, false, false),
                    Width::B16 => (if is_store { 0x89 } else { 0x8B }, true, false),
                    Width::B32 => (if is_store { 0x89 } else { 0x8B }, false, false),
                    Width::B64 => (if is_store { 0x89 } else { 0x8B }, false, true),
                    _ => return None,
                };
                // REX: W (B64), R=reg>=8, B=base>=8, X=index>=8.
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
                if let Some(ir) = index_reg {
                    if ir >= 8 {
                        rex |= 0x02; // REX.X
                    }
                }
                // 判断是否需要真的发 REX 前缀: 没有任何字段被扩展, 且 width!=B64,
                // 那就保持裸 opcode (避免多一个 0x40 前缀). B64 总是发 REX.W.
                // B8 特殊: reg 字段 ∈ {4,5,6,7} 是 spl/bpl/sil/dil 高 8 字节, 必须 REX.
                let need_high_byte = matches!(width, Width::B8) && (reg_idx >= 4 || base >= 4);
                let need_rex = need_rex_w
                    || reg_idx >= 8
                    || base >= 8
                    || index_reg.map_or(false, |ir| ir >= 8)
                    || need_high_byte;

                // ModRM / SIB / disp: 有 index 时必须发 SIB; 无 index 时仅 base=rsp(4)
                // 必须发 SIB; 其余 base 走普通 ModRM.
                // base=rbp(5) 在 mod=00 时是 rip-relative, 但当有 SIB 时 (rsp / index)
                // mod=00 仍合法. base=rbp(5) 且无 SIB 时必须发 disp (mod=01/10).
                let disp = mem.disp;
                let need_sib = index_reg.is_some() || (base & 7) == 4;
                let modrm_mod: u8;
                let disp_bytes: [u8; 4];
                let disp_len: usize;
                if disp == 0 && (base != 5 || need_sib) {
                    // mod=00, 无 disp. base=rbp(5) 且无 SIB 时 mod=00 = rip-relative,
                    // 必须走 disp.
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
                // ModRM 字节: mod | reg_field | rm_field. 有 SIB 时 rm_field 必须为 4
                // (R/M=100) 以标识后续 SIB 字节; 实际 base 号放在 SIB 的 base 字段.
                let modrm_rm = if need_sib { 4 } else { rm_field };
                let modrm = (modrm_mod << 6) | (reg_field << 3) | modrm_rm;
                out.push(modrm);
                if need_sib {
                    // SIB: scale = log2(scale) (0=1, 1=2, 2=4, 3=8);
                    // index=4 (ESP) 编码 = 无 index; base = rm_field.
                    let scale_bits = match index_scale {
                        1 => 0,
                        2 => 1,
                        4 => 2,
                        8 => 3,
                        _ => 0,
                    };
                    let index_field = index_reg.map(|ir| ir & 7).unwrap_or(4);
                    let sib = (scale_bits << 6) | (index_field << 3) | rm_field;
                    out.push(sib);
                }
                out.extend_from_slice(&disp_bytes[..disp_len]);
                Some(out)
            },
            X86_64Opcode::MovExt {
                sign,
                from,
                to,
                dst,
                src,
                rip_relative,
            } => {
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
                // P11 Bucket O8: rip-relative movzx/movsx/movsxd reg, [rip+disp32].
                //   dst=Reg, src=Mem. ModRM r/m=101 (rip). REX.R = dst>=8.
                //   手写约定: B8/B16 无 REX.W; movsxd 有 REX.W=0x48/0x4C.
                if *rip_relative {
                    let (Operand::Reg(dst_idx, _), Operand::Mem(mem)) = (dst, src) else {
                        return None;
                    };
                    if mem.base.is_some() || mem.index.is_some() || mem.seg.is_some() {
                        return None;
                    }
                    let mut rex: u8 = 0x40;
                    if rex_w {
                        rex |= 0x08;
                    }
                    if *dst_idx >= 8 {
                        rex |= 0x04;
                    }
                    // B8/B16 源是 mem (不是 reg), 无需 spl/bpl/sil/dil 特殊处理;
                    // 手写 emit_global_load sz=1/2 对 reg=4..7 不发 REX.
                    let need_rex = rex_w || *dst_idx >= 8;
                    let modrm = 0x05 | ((*dst_idx & 7) << 3);
                    let disp = mem.disp;
                    let mut out = Vec::with_capacity(8);
                    if need_rex {
                        out.push(rex);
                    }
                    if escap != 0 {
                        out.push(escap);
                    }
                    out.push(low);
                    out.push(modrm);
                    out.extend_from_slice(&disp.to_le_bytes());
                    return Some(out);
                }
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
                    },
                    (Operand::Reg(dst_idx, _), Operand::Mem(mem)) => {
                        // mem -> reg (load). 支持 base+disp / [base+index*scale+disp=0].
                        // 非 scale ∈ {1,2,4,8} 的 index / seg 前缀回退手写.
                        let (index_reg, index_scale) = match mem.index {
                            None => (None, 0u8),
                            Some((ir, s)) => {
                                if s != 1 && s != 2 && s != 4 && s != 8 {
                                    return None;
                                }
                                (Some(ir), s)
                            },
                        };
                        if mem.seg.is_some() && mem.index.is_none() {
                            return None;
                        }
                        let base = match mem.base {
                            Some(b) => b,
                            None => return None,
                        };
                        // REX: W (B64), R=dst>=8, B=base>=8, X=index>=8.
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
                        if let Some(ir) = index_reg {
                            if ir >= 8 {
                                rex |= 0x02;
                            }
                        }
                        let need_rex = rex_w
                            || *dst_idx >= 8
                            || base >= 8
                            || index_reg.map_or(false, |ir| ir >= 8);
                        // ModRM mod / disp 长度: base=rbp(5) 在 mod=00 时是 rip-relative,
                        // 这里只接显式 disp, 因此 disp=0 且 base=5 时走 disp32 (mod=10) 以
                        // 避免被解释成 [rip+disp32]. 但若有 SIB (index 或 rsp) 则 mod=00 合法.
                        let disp = mem.disp;
                        let need_sib = index_reg.is_some() || (base & 7) == 4;
                        let (modrm_mod, disp_len, disp_bytes): (u8, usize, [u8; 4]) =
                            if disp == 0 && (base != 5 || need_sib) {
                                (0, 0, [0; 4])
                            } else if disp == 0 && base == 5 {
                                // rbp/r13 且无 SIB: 强制 disp32 (mod=10) 避免 rip-relative.
                                (2, 4, [0; 4])
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
                        let modrm_rm = if need_sib { 4 } else { rm_field };
                        let modrm = (modrm_mod << 6) | (reg_field << 3) | modrm_rm;
                        out.push(modrm);
                        if need_sib {
                            // SIB: scale_bits = log2(scale) (0=1,1=2,2=4,3=8);
                            // index=index_reg&7 (无 index 则 4=ESP).
                            let scale_bits = match index_scale {
                                1 => 0,
                                2 => 1,
                                4 => 2,
                                8 => 3,
                                _ => 0,
                            };
                            let index_field = index_reg.map(|ir| ir & 7).unwrap_or(4);
                            out.push((scale_bits << 6) | (index_field << 3) | rm_field);
                        }
                        out.extend_from_slice(&disp_bytes[..disp_len]);
                        Some(out)
                    },
                    _ => None,
                }
            },
            X86_64Opcode::ImulRm { width, dst, src } => {
                // P11: IMUL r, r/m — 0F AF /r.
                //   B32: 0F AF /r (无 REX.W)
                //   B64: REX.W(0x48) 0F AF /r
                // dst ModRM.reg; src ModRM.r/m.
                let is_b64 = matches!(width, Width::B64);
                let is_b32 = matches!(width, Width::B32);
                if !is_b64 && !is_b32 {
                    return None;
                }
                // reg-reg 形态.
                if let (Operand::Reg(dst_idx, _), Operand::Reg(src_idx, _)) = (dst, src) {
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
                    return Some(out);
                }
                // mem 形态: dst=Reg(dst_idx), src=Mem(base=rsp(4), index=None, disp8=i8).
                //   手写约定: imul rax, [rsp+off] → (REX.W) 0F AF 44 24 disp8.
                //   base=rsp 必须 SIB (rm=100); mod=01 disp8.
                let (Operand::Reg(dst_idx, _), Operand::Mem(mem)) = (dst, src) else {
                    return None;
                };
                if mem.seg.is_some() || mem.index.is_some() {
                    return None;
                }
                let base = mem.base?;
                if base != 4 {
                    return None; // 当前调用点只用到 rsp SIB 形态
                }
                let disp = mem.disp;
                let Ok(d) = i8::try_from(disp) else {
                    return None;
                };
                let mut rex: u8 = 0x40;
                if is_b64 {
                    rex |= 0x08;
                }
                if *dst_idx >= 8 {
                    rex |= 0x04;
                }
                // base=rsp(4) 不需要 REX.B (高 4..7 已经表达); 这里 base<8 所以 REX.B=0.
                let need_rex = is_b64 || *dst_idx >= 8;
                let mut out = Vec::with_capacity(6);
                if need_rex {
                    out.push(rex);
                }
                out.push(0x0F);
                out.push(0xAF);
                // ModRM: mod=01, reg=dst, rm=100 (SIB).
                out.push(0x40 | ((*dst_idx & 7) << 3) | 0x04);
                // SIB: scale=00, index=100 (none), base=100 (rsp).
                out.push(0x24);
                out.push(d as u8);
                Some(out)
            },
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
            },
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
                    },
                }
            },
            X86_64Opcode::Jmp {
                target,
                pending_rel,
            } => {
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
            },
            X86_64Opcode::Call {
                target,
                pending_rel,
            } => {
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
            },
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
                    },
                    X87Op::FldMem { width }
                    | X87Op::FstpMem { width }
                    | X87Op::FildMem { width } => {
                        // mem 是 &Option<MemOperand>, 不能用 ? 运算符解引用.
                        #[allow(clippy::question_mark)]
                        let mem = match mem {
                            Some(m) => m,
                            None => return None,
                        };
                        if mem.seg.is_some() {
                            return None;
                        }
                        let (index_reg, index_scale) = match mem.index {
                            None => (None, 0u8),
                            Some((ir, s)) => {
                                if s != 1 && s != 2 && s != 4 && s != 8 {
                                    return None;
                                }
                                (Some(ir), s)
                            },
                        };
                        let base = mem.base?;
                        // escap 字节 + reg 位段.
                        let (escap, reg_field, rexw): (u8, u8, bool) = match (op, *width) {
                            (X87Op::FldMem { .. }, 4) => (0xD9, 0, false), // fld m32fp
                            (X87Op::FldMem { .. }, 8) => (0xDD, 0, false), // fld m64fp
                            (X87Op::FldMem { .. }, 10) => (0xDB, 5, false), // fld m80fp
                            (X87Op::FstpMem { .. }, 4) => (0xD9, 3, false), // fstp m32fp
                            (X87Op::FstpMem { .. }, 8) => (0xDD, 3, false), // fstp m64fp
                            (X87Op::FstpMem { .. }, 10) => (0xDB, 7, false), // fstp m80fp
                            (X87Op::FildMem { .. }, 4) => (0xDB, 0, false), // fild m32int
                            (X87Op::FildMem { .. }, 8) => (0xDB, 0, true), // fild m64int
                            _ => return None,
                        };
                        // REX: base>=8 → REX.B; index>=8 → REX.X; rexw → REX.W.
                        let mut rex: u8 = 0x40;
                        if base >= 8 {
                            rex |= 0x01;
                        }
                        if let Some(ir) = index_reg {
                            if ir >= 8 {
                                rex |= 0x02;
                            }
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
                        // P11 Bucket H2: 按 disp 大小选 mod, 支持任意 base (含 r12).
                        //   disp=0 且 base!=5(rbp) → mod=00; base=5 强制 disp8=0.
                        //   i8 范围 → mod=01 + disp8.
                        //   其余 → mod=10 + disp32.
                        // rm=4 (rsp/r12) 必须插 SIB (SIB base=base&7=4=rsp or 4=r12).
                        // rm=5 (rbp/r13) 在 mod=00 时是 rip-relative, 因此 disp=0 且 base=5
                        //   时强制 mod=01 disp8=0.
                        // P11 Bucket AE: 有 index 时必须发 SIB, modrm.rm=4 (SIB follow).
                        let rm = base & 7;
                        let need_sib = index_reg.is_some() || rm == 4;
                        let (modrm_mod, disp_bytes): (u8, Vec<u8>) =
                            if disp == 0 && (rm != 5 || need_sib) {
                                (0u8, Vec::new())
                            } else if disp == 0 && rm == 5 {
                                // rbp/r13: mod=00 会被解码成 rip-relative, 故强制 mod=01 disp8=0.
                                (1u8, vec![0u8])
                            } else if let Ok(d) = i8::try_from(disp) {
                                (1u8, vec![d as u8])
                            } else {
                                (2u8, disp.to_le_bytes().to_vec())
                            };
                        let modrm_rm = if need_sib { 4 } else { rm };
                        out.push((modrm_mod << 6) | (reg_field << 3) | modrm_rm);
                        if need_sib {
                            // SIB: scale<<6 | index<<3 | base. index=4(ESP) 表示无 index.
                            let scale_bits = match index_scale {
                                1 => 0,
                                2 => 1,
                                4 => 2,
                                8 => 3,
                                _ => 0,
                            };
                            let index_field = index_reg.map(|ir| ir & 7).unwrap_or(4);
                            out.push((scale_bits << 6) | (index_field << 3) | rm);
                        }
                        out.extend_from_slice(&disp_bytes);
                        Some(out)
                    },
                    X87Op::MxcsrMem { store }
                    | X87Op::FcwMem { store }
                    | X87Op::EnvMem { store } => {
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
                        // escap + reg_field + disp.
                        let (escap, reg_field): (u8, u8) = match op {
                            X87Op::MxcsrMem { store: true } => (0xAE, 3), // stmxcsr
                            X87Op::MxcsrMem { store: false } => (0xAE, 2), // ldmxcsr
                            X87Op::FcwMem { store: true } => (0xD9, 7),   // fnstcw
                            X87Op::FcwMem { store: false } => (0xD9, 5),  // fldcw
                            X87Op::EnvMem { store: true } => (0xD9, 6),   // fnstenv [mem]
                            X87Op::EnvMem { store: false } => (0xD9, 4),  // fldenv [mem]
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
                        // P11 Bucket H2: 按 disp 大小选 mod, 支持任意 base (含 r12).
                        let rm = base & 7;
                        let disp = mem.disp;
                        let (modrm_mod, disp_bytes): (u8, Vec<u8>) = if disp == 0 && rm != 5 {
                            (0u8, Vec::new())
                        } else if disp == 0 && rm == 5 {
                            (1u8, vec![0u8])
                        } else if let Ok(d) = i8::try_from(disp) {
                            (1u8, vec![d as u8])
                        } else {
                            (2u8, disp.to_le_bytes().to_vec())
                        };
                        out.push((modrm_mod << 6) | (reg_field << 3) | rm);
                        if rm == 4 {
                            out.push(0x24);
                        }
                        out.extend_from_slice(&disp_bytes);
                        Some(out)
                    },
                }
            },
            X86_64Opcode::SimdMov {
                width,
                opcode10,
                dst,
                src,
            } => {
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
                    // 0x10/0x28/0x6F (load direction, opcode10=true):
                    //   semantic dst <- src; ModRM.reg=dst, ModRM.r/m=src.
                    // 0x11/0x29/0x7F (store direction, opcode10=false):
                    //   Intel encoding places the *source* operand in ModRM.reg
                    //   and the *destination* in ModRM.r/m; REX.R/REX.B follow
                    //   the physical field positions (reg/r/m), not the
                    //   semantic dst/src naming.
                    let (reg_field, rm_field) = if *opcode10 {
                        (*dst_xmm, *src_xmm)
                    } else {
                        (*src_xmm, *dst_xmm)
                    };
                    let need_rex = reg_field >= 8 || rm_field >= 8;
                    let mut out = Vec::with_capacity(6);
                    if pfx != 0 {
                        out.push(pfx);
                    }
                    if need_rex {
                        let mut rex: u8 = 0x40;
                        if reg_field >= 8 {
                            rex |= 0x04; // REX.R extends ModRM.reg
                        }
                        if rm_field >= 8 {
                            rex |= 0x01; // REX.B extends ModRM.r/m
                        }
                        out.push(rex);
                    }
                    out.push(0x0F);
                    out.push(opc2);
                    let modrm: u8 = 0xC0 | ((reg_field & 0x7) << 3) | (rm_field & 0x7);
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
                if mem.seg.is_some() {
                    return None;
                }
                let (index_reg, index_scale) = match mem.index {
                    None => (None, 0u8),
                    Some((ir, s)) => {
                        if s != 1 && s != 2 && s != 4 && s != 8 {
                            return None;
                        }
                        (Some(ir), s)
                    },
                };
                let base = mem.base?;
                // legacy 前缀: SS=F3, SD=F2, O/PS=无, PD/DQ=66.
                // P11 Bucket G: DQ 在内存形态下 = movdqu (F3 pfx, 0F 6F/7F).
                //   reg-reg 分支仍走 66 pfx (movdqa).
                let pfx: u8 = match width {
                    SimdWidth::SS => 0xF3,
                    SimdWidth::SD => 0xF2,
                    SimdWidth::PD => 0x66,
                    SimdWidth::DQ => 0xF3,
                    SimdWidth::O | SimdWidth::PS => 0x00,
                    _ => return None,
                };
                // REX: xmm>=8 → REX.R=0x04; base>=8 → REX.B=0x01; index>=8 → REX.X=0x02.
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
                if let Some(ir) = index_reg {
                    if ir >= 8 {
                        rex |= 0x02;
                        need_rex = true;
                    }
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
                // 但当有 SIB (rsp / index) 时 mod=00 仍合法. base=rbp(5) 且无 SIB
                // 时必须走 mod=01 (disp8=0) 避免被解释成 rip.
                // base=rsp(4) 必须 SIB; 当 disp=0 时 SIB(base=rsp,index=none) 配合
                // mod=00 即编码为 `modrm=0x04|xmm<<3, sib=0x24` (G10 快照约定).
                let need_sib = index_reg.is_some() || (base & 7) == 4;
                let (modrm_mod, disp_bytes): (u8, &[u8]) = if disp == 0 && (base != 5 || need_sib) {
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
                // ModRM: mod | reg(xmm) | rm(base). rm==4 或有 index 时必须 SIB.
                // 有 SIB 时 rm 必须置 4 (R/M=100) 以标识 SIB 字节.
                let modrm_rm = if need_sib { 4 } else { rm_field };
                let modrm = (modrm_mod << 6) | ((xmm_idx & 7) << 3) | modrm_rm;
                out.push(modrm);
                if need_sib {
                    let scale_bits = match index_scale {
                        1 => 0,
                        2 => 1,
                        4 => 2,
                        8 => 3,
                        _ => 0,
                    };
                    let index_field = index_reg.map(|ir| ir & 7).unwrap_or(4);
                    out.push((scale_bits << 6) | (index_field << 3) | rm_field);
                }
                out.extend_from_slice(disp_bytes);
                Some(out)
            },
            X86_64Opcode::SimdArith {
                op,
                width,
                dst,
                src,
            } => {
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
                    // P11 Bucket G: unpcklps xmm_dst, xmm_src (0F 14 /r, no legacy prefix).
                    (SimdArithOp::Unpcklps, _) => (0x00, 0x14),
                    // P11 Bucket G2: pcmpeqb/w/d (66 0F 74/75/76 /r).
                    (SimdArithOp::PCmpEq, SimdWidth::B) => (0x66, 0x74),
                    (SimdArithOp::PCmpEq, SimdWidth::W) => (0x66, 0x75),
                    (SimdArithOp::PCmpEq, SimdWidth::D) => (0x66, 0x76),
                    // P11 Bucket G2: pminub/pmaxub (0F DA/DE /r, no legacy prefix).
                    (SimdArithOp::PMinMaxUb, SimdWidth::B) => (0x00, 0xDA),
                    (SimdArithOp::PMinMaxUb, SimdWidth::W) => (0x00, 0xDE),
                    // P11 Bucket G2: pmaddwd/psadbw (66 0F F5/F6 /r).
                    (SimdArithOp::PMaddwd, _) => (0x66, 0xF5),
                    (SimdArithOp::PSadbw, _) => (0x66, 0xF6),
                    // P11 Bucket G2: paddsw/psubsw/paddusw/psubusw (66 0F ED/E9/DD/D9 /r).
                    //   Encode signed/unsigned + add/sub via SimdWidth:
                    //   W=add signed, Q=sub signed, D=add unsigned, B=sub unsigned.
                    (SimdArithOp::PSatW, SimdWidth::W) => (0x66, 0xED), // paddsw
                    (SimdArithOp::PSatW, SimdWidth::Q) => (0x66, 0xE9), // psubsw
                    (SimdArithOp::PSatW, SimdWidth::D) => (0x66, 0xDD), // paddusw
                    (SimdArithOp::PSatW, SimdWidth::B) => (0x66, 0xD9), // psubusw
                    _ => return None,
                };
                let mut rex: u8 = 0x40;
                if *dst_xmm >= 8 {
                    rex |= 0x04;
                }
                if *src_xmm >= 8 {
                    rex |= 0x01;
                }
                let need_rex = *dst_xmm >= 8 || *src_xmm >= 8;
                let mut out = Vec::with_capacity(6);
                if pfx != 0 {
                    out.push(pfx);
                }
                if need_rex {
                    out.push(rex);
                }
                out.push(0x0F);
                out.push(opc2);
                out.push(0xC0 | ((*dst_xmm & 7) << 3) | (*src_xmm & 7));
                Some(out)
            },
            X86_64Opcode::SimdShiftImm8 {
                op,
                width,
                dst,
                imm8,
            } => {
                // P11 Bucket G2: psllq/psrlq/psrad xmm, imm8.
                //   psllq xmm0, imm8: 66 0F F3 C0 ib
                //   psrlq xmm0, imm8: 66 0F D3 C0 ib
                //   psrad xmm0, imm8: 66 0F E2 C0 ib
                let Operand::Reg(dst_xmm, _) = dst else {
                    return None;
                };
                // op=PShiftImm8, width selects: Q=psllq, D=psrlq, W=psrad.
                let opc2: u8 = match (op, width) {
                    (SimdArithOp::PShiftImm8, SimdWidth::Q) => 0xF3, // psllq
                    (SimdArithOp::PShiftImm8, SimdWidth::D) => 0xD3, // psrlq
                    (SimdArithOp::PShiftImm8, SimdWidth::W) => 0xE2, // psrad
                    _ => return None,
                };
                let need_rex = *dst_xmm >= 8;
                let mut out = Vec::with_capacity(7);
                out.push(0x66);
                if need_rex {
                    let mut rex: u8 = 0x40;
                    if *dst_xmm >= 8 {
                        rex |= 0x04;
                    }
                    out.push(rex);
                }
                out.push(0x0F);
                out.push(opc2);
                out.push(0xC0 | ((*dst_xmm & 7) << 3) | (*dst_xmm & 7));
                out.push(*imm8);
                Some(out)
            },
            X86_64Opcode::ShldShrd {
                is_shld,
                width,
                dst,
                src,
            } => {
                // P11 Bucket 7: shld/shrd reg-reg with CL count.
                //  shld r64,r64,cl:  REX.W 0F A5 modrm(mod=11,reg=src,r/m=dst)
                //  shrd r64,r64,cl:  REX.W 0F AD modrm(mod=11,reg=src,r/m=dst)
                let (dst_idx, src_idx) = match (dst, src) {
                    (Operand::Reg(d, _), Operand::Reg(s, _)) => (d, s),
                    _ => return None,
                };
                let is_b64 = matches!(width, Width::B64);
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
                let need_rex = is_b64 || *src_idx >= 8 || *dst_idx >= 8;
                let opc2: u8 = if *is_shld { 0xA5 } else { 0xAD };
                let modrm: u8 = 0xC0 | ((src_idx & 7) << 3) | (dst_idx & 7);
                let mut out = Vec::with_capacity(5);
                if need_rex {
                    out.push(rex);
                }
                out.push(0x0F);
                out.push(opc2);
                out.push(modrm);
                Some(out)
            },
            X86_64Opcode::Cvt2Sd {
                to_xmm,
                width,
                gpr_64,
                dst,
                src,
            } => {
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
                let mut rex: u8 = if *gpr_64 { 0x48 } else { 0x40 };
                if xmm_idx >= 8 {
                    rex |= 0x04;
                }
                if gpr_idx >= 8 {
                    rex |= 0x01;
                }
                let modrm: u8 = 0xC0 | ((xmm_idx & 7) << 3) | (gpr_idx & 7);
                let mut out = Vec::with_capacity(6);
                out.push(pfx);
                // P11: 只在真有字段扩位时才发 REX 字节 (B32 路径若两 idx 均<8 则裸 REX 是多余的).
                let needs_rex = *gpr_64 || xmm_idx >= 8 || gpr_idx >= 8;
                if needs_rex {
                    out.push(rex);
                }
                out.push(0x0F);
                out.push(opc2);
                out.push(modrm);
                Some(out)
            },
            X86_64Opcode::SimdRound {
                width,
                imm8,
                dst,
                src,
            } => {
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
                    if *dst_idx >= 8 {
                        rex |= 0x04;
                    }
                    if *src_idx >= 8 {
                        rex |= 0x01;
                    }
                    out.push(rex);
                }
                out.push(0x0F);
                out.push(0x3A);
                out.push(opc2);
                out.push(0xC0 | ((*dst_idx & 7) << 3) | (*src_idx & 7));
                out.push(*imm8);
                Some(out)
            },
            X86_64Opcode::SimdCmpImm8 {
                width,
                imm8,
                dst,
                src,
            } => {
                // P11 Bucket AF: cmpss/cmpsd xmm_dst, xmm_src, imm8.
                //   cmpss: F3 0F C2 /r ib   cmpsd: F2 0F C2 /r ib.
                let (Operand::Reg(dst_idx, _), Operand::Reg(src_idx, _)) = (dst, src) else {
                    return None;
                };
                let pfx: u8 = match width {
                    SimdWidth::SS => 0xF3,
                    SimdWidth::SD => 0xF2,
                    _ => return None,
                };
                let need_rex = *dst_idx >= 8 || *src_idx >= 8;
                let mut out = Vec::with_capacity(6);
                out.push(pfx);
                if need_rex {
                    let mut rex: u8 = 0x40;
                    if *dst_idx >= 8 {
                        rex |= 0x04;
                    }
                    if *src_idx >= 8 {
                        rex |= 0x01;
                    }
                    out.push(rex);
                }
                out.push(0x0F);
                out.push(0xC2);
                out.push(0xC0 | ((*dst_idx & 7) << 3) | (*src_idx & 7));
                out.push(*imm8);
                Some(out)
            },
            X86_64Opcode::Pmulld { dst, src } => {
                // P11 Bucket AF: pmulld xmm_dst, xmm_src (66 0F 38 40 /r).
                let (Operand::Reg(dst_idx, _), Operand::Reg(src_idx, _)) = (dst, src) else {
                    return None;
                };
                let need_rex = *dst_idx >= 8 || *src_idx >= 8;
                let mut out = Vec::with_capacity(6);
                out.push(0x66);
                if need_rex {
                    let mut rex: u8 = 0x40;
                    if *dst_idx >= 8 {
                        rex |= 0x04;
                    }
                    if *src_idx >= 8 {
                        rex |= 0x01;
                    }
                    out.push(rex);
                }
                out.push(0x0F);
                out.push(0x38);
                out.push(0x40);
                out.push(0xC0 | ((*dst_idx & 7) << 3) | (*src_idx & 7));
                Some(out)
            },
            // P11 Bucket P11R: direct call rel32 (E8 00 00 00 00).
            // 5 bytes total; rel32 field is patched later by pending_func_rels.
            X86_64Opcode::CallRel32 => Some(vec![0xE8, 0, 0, 0, 0]),
            // P11 Bucket P11R: direct jmp rel32 (E9 00 00 00 00).
            X86_64Opcode::JmpRel32 => Some(vec![0xE9, 0, 0, 0, 0]),
            // P11 Bucket P11R: jmp r/m64 (FF /4).
            // Used for indirect jump through memory [base+disp].
            // modrm_mod chosen by disp size; SIB emitted when base=rsp(4)/r12(12).
            X86_64Opcode::JmpRmMem { base, disp } => {
                let rm_field = base & 7;
                let need_sib = rm_field == 4;
                // mod=00 if disp=0 and base!=rbp(5); base=rbp(5) at mod=00 is
                // rip-relative, so force mod=01 disp8=0 to keep [rbp+disp].
                let (modrm_mod, disp_bytes): (u8, Vec<u8>) =
                    if *disp == 0 && rm_field != 5 {
                        (0u8, Vec::new())
                    } else if *disp == 0 && rm_field == 5 {
                        (1u8, vec![0u8])
                    } else if let Ok(d) = i8::try_from(*disp) {
                        (1u8, vec![d as u8])
                    } else {
                        (2u8, disp.to_le_bytes().to_vec())
                    };
                let mut out = Vec::with_capacity(8);
                if *base >= 8 {
                    out.push(0x41); // REX.B
                }
                out.push(0xFF);
                let modrm_rm = if need_sib { 4 } else { rm_field };
                // /4 = reg field 0b100.
                out.push((modrm_mod << 6) | (0b100 << 3) | modrm_rm);
                if need_sib {
                    // SIB: scale=0, index=4(no index), base=rm_field.
                    out.push(0x20 | rm_field);
                }
                out.extend_from_slice(&disp_bytes);
                Some(out)
            },
            // P11 Bucket P11R: push m64 [rsp+disp8] (FF /6).
            // Only base=rsp(4) SIB form is encoded (used by complex.rs spills).
            X86_64Opcode::PushMemDisp8 { base, disp } => {
                let rm_field = *base & 7;
                let need_sib = rm_field == 4;
                // /6 push mem. mod=01, disp8.
                let modrm_rm = if need_sib { 4 } else { rm_field };
                let mut out = Vec::with_capacity(4);
                if *base >= 8 {
                    out.push(0x41);
                }
                out.push(0xFF);
                out.push((1u8 << 6) | (0b110 << 3) | modrm_rm);
                if need_sib {
                    out.push(0x20 | rm_field);
                }
                out.push(*disp as u8);
                Some(out)
            },
            // P11 Bucket P11R: movss/movsd [rip+disp32] load/store.
            // pfx: SS=F3, SD=F2. store=true: 0F 11, false: 0F 10.
            // mod=00 r/m=101, reg=xmm (REX.R if xmm>=8).
            X86_64Opcode::SimdMovRip { store, width, xmm } => {
                let pfx: u8 = match width {
                    SimdWidth::SS => 0xF3,
                    SimdWidth::SD => 0xF2,
                    _ => return None,
                };
                let opc2: u8 = if *store { 0x11 } else { 0x10 };
                let mut out = Vec::with_capacity(8);
                out.push(pfx);
                if *xmm >= 8 {
                    out.push(0x44); // REX.R
                }
                out.push(0x0F);
                out.push(opc2);
                // mod=00, reg=xmm&7, r/m=101 (rip-rel).
                out.push(((*xmm & 7) << 3) | 0x05);
                out.extend_from_slice(&0i32.to_le_bytes());
                Some(out)
            },
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
                    CondCode::S => 0x98,
                    CondCode::Ns => 0x99,
                    CondCode::O => 0x90,
                    CondCode::No => 0x91,
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
            },
            // P11 Bucket I: 无操作数/固定前缀指令.
            X86_64Opcode::NoArg { op } => {
                let bytes: &[u8] = match op {
                    NoArgOp::Cld => &[0xFC],
                    NoArgOp::Mfence => &[0x0F, 0xAE, 0xF0],
                    NoArgOp::RepMovsb => &[0xF3, 0xA4],
                    NoArgOp::RepStosb => &[0xF3, 0xAA],
                    NoArgOp::RepStosq => &[0xF3, 0x48, 0xAB],
                    NoArgOp::RepMovsq => &[0xF3, 0x48, 0xA5],
                    NoArgOp::Syscall => &[0x0F, 0x05],
                    NoArgOp::Int3 => &[0xCC],
                    NoArgOp::Xgetbv => &[0x0F, 0x01, 0xD0],
                    NoArgOp::Cpuid => &[0x0F, 0xA2],
                    NoArgOp::RetNear => &[0xC3],
                    NoArgOp::Ud2 => &[0x0F, 0x0B],
                    NoArgOp::Fwait => &[0x9B],
                };
                Some(bytes.to_vec())
            },
            // P11 Bucket K: movd/movq GPR↔XMM reg-reg.
            X86_64Opcode::GprXmm {
                to_xmm,
                width,
                dst,
                src,
            } => {
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
                // dcc always transfers to/from an XMM register, so both the
                // SSE2 MOVD (B32) and MOVQ (B64) forms require the 66 legacy
                // prefix; without it 0F 6E/7E decode as the MMX MOVD (mm
                // registers). MOVQ additionally needs REX.W.
                let mut rex: u8 = 0x40;
                if is_b64 {
                    rex |= 0x08;
                }
                if xmm_idx >= 8 {
                    rex |= 0x04;
                }
                if gpr_idx >= 8 {
                    rex |= 0x01;
                }
                let need_rex = is_b64 || xmm_idx >= 8 || gpr_idx >= 8;
                let mut out = Vec::with_capacity(6);
                out.push(0x66);
                if need_rex {
                    out.push(rex);
                }
                out.push(0x0F);
                out.push(opc2);
                out.push(0xC0 | ((xmm_idx & 7) << 3) | (gpr_idx & 7));
                Some(out)
            },
            // P11 Bucket N6: BSF/BSR reg, reg.
            X86_64Opcode::BitScan {
                is_bsr,
                width,
                dst,
                src,
            } => {
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
                } // ModRM.reg = dst
                if *src_idx >= 8 {
                    rex |= 0x01;
                } // ModRM.r/m = src
                let need_rex = is_b64 || *dst_idx >= 8 || *src_idx >= 8;
                let opc2: u8 = if *is_bsr { 0xBD } else { 0xBC };
                let mut out = Vec::with_capacity(5);
                if need_rex {
                    out.push(rex);
                }
                out.push(0x0F);
                out.push(opc2);
                out.push(0xC0 | ((*dst_idx & 7) << 3) | (*src_idx & 7));
                Some(out)
            },
            // P11 Bucket N6: CMOVcc reg, reg (B64/B32).
            X86_64Opcode::Cmovcc {
                cc,
                width,
                dst,
                src,
            } => {
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
                let opc2: u8 = match cc {
                    CondCode::Eq => 0x44,
                    CondCode::Ne => 0x45,
                    CondCode::B => 0x42,
                    CondCode::Ae => 0x43,
                    CondCode::Be => 0x46,
                    CondCode::A => 0x47,
                    CondCode::Lt => 0x4C,
                    CondCode::Ge => 0x4D,
                    CondCode::Gt => 0x4F,
                    CondCode::Le => 0x4E,
                    CondCode::S => 0x48,
                    CondCode::Ns => 0x49,
                    CondCode::P => 0x4A,
                    CondCode::Np => 0x4B,
                    CondCode::O => 0x40,
                    CondCode::No => 0x41,
                };
                let mut out = Vec::with_capacity(5);
                if need_rex {
                    out.push(rex);
                }
                out.push(0x0F);
                out.push(opc2);
                out.push(0xC0 | ((*dst_idx & 7) << 3) | (*src_idx & 7));
                Some(out)
            },
            // P11 Bucket O3: LOCK CMPXCHG r/m, reg.
            //  手写约定: 立即数/内存形态为 [base+disp0] base-only.
            //  字节序: [66?] [REX?] F0 0F (B0|B1) ModRM.
            //    B8:  REX=0x40 强制发 (sil/dil 高字节); opc2=B0; 无 66.
            //    B16: 66 + opc2=B1; REX 仅当 reg>=8/base>=8 时发.
            //    B32: 无 66, opc2=B1; REX 仅当 reg>=8/base>=8 时发.
            //    B64: REX.W=0x48, opc2=B1.
            X86_64Opcode::LockCmpxchgRm {
                width,
                reg_idx,
                mem,
            } => {
                let base = mem.base?;
                if mem.seg.is_some() || mem.index.is_some() || mem.disp != 0 {
                    return None;
                }
                let is_b8 = matches!(width, Width::B8);
                let is_b16 = matches!(width, Width::B16);
                let is_b32 = matches!(width, Width::B32);
                let is_b64 = matches!(width, Width::B64);
                if !(is_b8 || is_b16 || is_b32 || is_b64) {
                    return None;
                }
                let opc2: u8 = if is_b8 { 0xB0 } else { 0xB1 };
                let mut rex: u8 = 0x40;
                if is_b64 {
                    rex |= 0x08;
                }
                if *reg_idx >= 8 {
                    rex |= 0x04;
                } // ModRM.reg = reg
                if base >= 8 {
                    rex |= 0x01;
                } // ModRM.r/m = base
                  // 手写: B8 强制发 0x40 REX (即使 reg=6=sil, base=7=rdi).
                  //       B64 强制发 REX.W.
                  //       B16/B32: 仅在 reg>=8 / base>=8 时才发 REX.
                  // 字节序 (手写约定): [66] F0 [REX] 0F opc2 ModRM.
                let need_rex = is_b8 || is_b64 || *reg_idx >= 8 || base >= 8;
                let mut out = Vec::with_capacity(8);
                if is_b16 {
                    out.push(0x66);
                }
                out.push(0xF0);
                if need_rex {
                    out.push(rex);
                }
                out.push(0x0F);
                out.push(opc2);
                // mod=00 (disp=0, base!=5), reg=reg_idx&7, rm=base&7.
                let modrm = ((*reg_idx & 7) << 3) | (base & 7);
                out.push(modrm);
                Some(out)
            },
            // P11 Bucket O3: LOCK XCHG r/m, reg.
            //  B8:  REX=0x40 强制发, opcode=0x86.
            //  B16: 66 + opcode=0x87.
            //  B32: opcode=0x87.
            //  B64: REX.W=0x48, opcode=0x87.
            X86_64Opcode::LockXchgRm {
                width,
                reg_idx,
                mem,
            } => {
                let base = mem.base?;
                if mem.seg.is_some() || mem.index.is_some() || mem.disp != 0 {
                    return None;
                }
                let is_b8 = matches!(width, Width::B8);
                let is_b16 = matches!(width, Width::B16);
                let is_b32 = matches!(width, Width::B32);
                let is_b64 = matches!(width, Width::B64);
                if !(is_b8 || is_b16 || is_b32 || is_b64) {
                    return None;
                }
                let opcode: u8 = if is_b8 { 0x86 } else { 0x87 };
                let mut rex: u8 = 0x40;
                if is_b64 {
                    rex |= 0x08;
                }
                if *reg_idx >= 8 {
                    rex |= 0x04;
                }
                if base >= 8 {
                    rex |= 0x01;
                }
                let need_rex = is_b8 || is_b64 || *reg_idx >= 8 || base >= 8;
                let mut out = Vec::with_capacity(8);
                if is_b16 {
                    out.push(0x66);
                }
                out.push(0xF0);
                if need_rex {
                    out.push(rex);
                }
                out.push(opcode);
                let modrm = ((*reg_idx & 7) << 3) | (base & 7);
                out.push(modrm);
                Some(out)
            },
            // P11 Bucket N8: LOCK XADD r/m, reg. 与 LockXchgRm 平行, 但 escap=0F,
            // opc2: B8=C0 / B16/B32/B64=C1. LOCK 前缀放在 REX 之前.
            X86_64Opcode::LockXaddRm {
                width,
                reg_idx,
                mem,
            } => {
                let base = mem.base?;
                if mem.seg.is_some() || mem.index.is_some() || mem.disp != 0 {
                    return None;
                }
                let is_b8 = matches!(width, Width::B8);
                let is_b16 = matches!(width, Width::B16);
                let is_b32 = matches!(width, Width::B32);
                let is_b64 = matches!(width, Width::B64);
                if !(is_b8 || is_b16 || is_b32 || is_b64) {
                    return None;
                }
                let opc2: u8 = if is_b8 { 0xC0 } else { 0xC1 };
                let mut rex: u8 = 0x40;
                if is_b64 {
                    rex |= 0x08;
                }
                if *reg_idx >= 8 {
                    rex |= 0x04;
                }
                if base >= 8 {
                    rex |= 0x01;
                }
                let need_rex = is_b8 || is_b64 || *reg_idx >= 8 || base >= 8;
                let mut out = Vec::with_capacity(8);
                if is_b16 {
                    out.push(0x66);
                }
                out.push(0xF0);
                if need_rex {
                    out.push(rex);
                }
                out.push(0x0F);
                out.push(opc2);
                let modrm = ((*reg_idx & 7) << 3) | (base & 7);
                out.push(modrm);
                Some(out)
            },
            // P11 Bucket AH: XCHG reg, reg.
            //  字节序: [66] [REX] (0x86|B8 | 0x87|else) [ModRM=11 reg rm].
            //  若 dst=rax(0) 则走 accumulator-shortcut 0x90+src (无 ModRM).
            X86_64Opcode::XchgRm {
                width,
                dst,
                src,
                prefer_shortcut,
            } => {
                let (Operand::Reg(dst_idx, _), Operand::Reg(src_idx, _)) = (dst, src) else {
                    return None;
                };
                let is_b8 = matches!(width, Width::B8);
                let is_b16 = matches!(width, Width::B16);
                let is_b32 = matches!(width, Width::B32);
                let is_b64 = matches!(width, Width::B64);
                if !(is_b8 || is_b16 || is_b32 || is_b64) {
                    return None;
                }
                let dst_idx = *dst_idx;
                let src_idx = *src_idx;
                // Accumulator-shortcut form: dst=rax(0), B16/B32/B64, prefer_shortcut=true.
                if *prefer_shortcut && dst_idx == 0 && !is_b8 {
                    let mut out = Vec::with_capacity(3);
                    if is_b16 {
                        out.push(0x66);
                    }
                    if is_b64 {
                        out.push(0x48);
                    }
                    out.push(0x90 + (src_idx & 7));
                    return Some(out);
                }
                // ModRM form: mod=11, reg=dst&7, rm=src&7.
                let opcode: u8 = if is_b8 { 0x86 } else { 0x87 };
                let mut rex: u8 = 0x40;
                if is_b64 {
                    rex |= 0x08;
                }
                if dst_idx >= 8 {
                    rex |= 0x04;
                }
                if src_idx >= 8 {
                    rex |= 0x01;
                }
                let need_rex = is_b8 || is_b64 || dst_idx >= 8 || src_idx >= 8;
                let modrm = 0xC0 | ((dst_idx & 7) << 3) | (src_idx & 7);
                let mut out = Vec::with_capacity(5);
                if is_b16 {
                    out.push(0x66);
                }
                if need_rex {
                    out.push(rex);
                }
                out.push(opcode);
                out.push(modrm);
                Some(out)
            },
            _ => None,
        }
    }

    /// 渲染为 GAS AT&T 一行 (不含 \n).
    /// 步骤 1: AluRm 的打样形态给真实输出, 其余变体给占位 (render 路径在步骤 6
    /// 才切, 本步 -S 仍走 disasm.rs).
    pub(crate) fn render(&self) -> String {
        use crate::codegen::shared::asm_operand::Width;
        match self {
            X86_64Opcode::AluRm {
                op,
                width,
                dst,
                src,
            } => {
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
            },
            X86_64Opcode::Raw { mnemonic, .. } => {
                mnemonic.clone().unwrap_or_else(|| "/* raw */".into())
            },
            X86_64Opcode::NoArg { op } => {
                let s: &str = match op {
                    NoArgOp::Cld => "cld",
                    NoArgOp::Mfence => "mfence",
                    NoArgOp::RepMovsb => "rep movsb",
                    NoArgOp::RepStosb => "rep stosb",
                    NoArgOp::RepStosq => "rep stosq",
                    NoArgOp::RepMovsq => "rep movsq",
                    NoArgOp::Syscall => "syscall",
                    NoArgOp::Int3 => "int3",
                    NoArgOp::Xgetbv => "xgetbv",
                    NoArgOp::Cpuid => "cpuid",
                    NoArgOp::RetNear => "ret",
                    NoArgOp::Ud2 => "ud2",
                    NoArgOp::Fwait => "fwait",
                };
                String::from(s)
            },
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
            rip_relative: false,
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
            rip_relative: false,
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
            rip_relative: false,
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
            rip_relative: false,
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
            rip_relative: false,
            width: Width::B64,
            dst: reg(0, Width::B64),
            src: Operand::Imm(0x80000000),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0xB8, 0x00, 0x00, 0x00, 0x80]));
    }

    #[test]
    fn mov_rm_mov_rax_imm64_high() {
        // mov rax, 0x80000000_00000000: requires full imm64, 10 bytes.
        let o = X86_64Opcode::MovRm {
            rip_relative: false,
            width: Width::B64,
            dst: reg(0, Width::B64),
            src: Operand::Imm(0x8000_0000_0000_0000u64 as i64),
        };
        assert_eq!(
            o.encode(),
            Some(vec![0x48, 0xB8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80])
        );
    }

    // ---- MovRm mem->reg [rax+0] load forms (P11 Batch 2x) ----

    #[test]
    fn mov_rm_mov_rax_load_rax_b64() {
        // mov rax, [rax]:  48 8B 00
        let o = X86_64Opcode::MovRm {
            rip_relative: false,
            width: Width::B64,
            dst: reg(0, Width::B64),
            src: Operand::Mem(MemOperand {
                disp: 0,
                base: Some(0),
                index: None,
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x8B, 0x00]));
    }

    #[test]
    fn mov_rm_mov_eax_load_rax_b32() {
        // mov eax, [rax]:  8B 00  (zero-extends to rax)
        let o = X86_64Opcode::MovRm {
            rip_relative: false,
            width: Width::B32,
            dst: reg(0, Width::B32),
            src: Operand::Mem(MemOperand {
                disp: 0,
                base: Some(0),
                index: None,
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x8B, 0x00]));
    }

    #[test]
    fn mov_rm_mov_rax_load_rsp_b64() {
        // mov rax, [rsp]:  48 8B 04 24  (SIB 跟随 rsp)
        let o = X86_64Opcode::MovRm {
            rip_relative: false,
            width: Width::B64,
            dst: reg(0, Width::B64),
            src: Operand::Mem(MemOperand {
                disp: 0,
                base: Some(4),
                index: None,
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x8B, 0x04, 0x24]));
    }

    #[test]
    fn mov_rm_mov_eax_load_rsp_b32() {
        // mov eax, [rsp]:  8B 04 24  (mod=00, reg=eax=0, SIB=24)
        let o = X86_64Opcode::MovRm {
            rip_relative: false,
            width: Width::B32,
            dst: reg(0, Width::B32),
            src: Operand::Mem(MemOperand {
                disp: 0,
                base: Some(4),
                index: None,
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x8B, 0x04, 0x24]));
    }

    // ---- P11 Bucket A: MovRm reg<->mem with disp8/disp32 ----

    #[test]
    fn mov_rm_load_r64_base_disp8() {
        // mov rcx, [r10+16]: 49 8B 4A 10 (REX.WB: B=1 r10=10; reg=rcx=1; disp=0x10)
        let o = X86_64Opcode::MovRm {
            rip_relative: false,
            width: Width::B64,
            dst: reg(1, Width::B64),
            src: Operand::Mem(MemOperand {
                disp: 16,
                base: Some(10),
                index: None,
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x49, 0x8B, 0x4A, 0x10]));
    }

    #[test]
    fn mov_rm_load_r64_base_disp32() {
        // mov rax, [r13+disp32]: 49 8B 85 XX XX XX XX (REX.WB; reg=rax=0, base=r13)
        let o = X86_64Opcode::MovRm {
            rip_relative: false,
            width: Width::B64,
            dst: reg(0, Width::B64),
            src: Operand::Mem(MemOperand {
                disp: 0x100,
                base: Some(13),
                index: None,
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x49, 0x8B, 0x85, 0x00, 0x01, 0, 0]));
    }

    #[test]
    fn mov_rm_load_r32_base_disp8() {
        // mov eax, [r10+4]: 41 8B 42 04 (REX.B; reg=eax=0)
        let o = X86_64Opcode::MovRm {
            rip_relative: false,
            width: Width::B32,
            dst: reg(0, Width::B32),
            src: Operand::Mem(MemOperand {
                disp: 4,
                base: Some(10),
                index: None,
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x41, 0x8B, 0x42, 0x04]));
    }

    #[test]
    fn mov_rm_store_r64_base_disp8() {
        // mov [r10+8], rcx: 49 89 4A 08 (REX.WB; reg=rcx=1)
        let o = X86_64Opcode::MovRm {
            rip_relative: false,
            width: Width::B64,
            dst: Operand::Mem(MemOperand {
                disp: 8,
                base: Some(10),
                index: None,
                seg: None,
            }),
            src: reg(1, Width::B64),
        };
        assert_eq!(o.encode(), Some(vec![0x49, 0x89, 0x4A, 0x08]));
    }

    #[test]
    fn mov_rm_store_r64_base_disp32() {
        // mov [r13+disp32], rax: 49 89 85 XX XX XX XX
        let o = X86_64Opcode::MovRm {
            rip_relative: false,
            width: Width::B64,
            dst: Operand::Mem(MemOperand {
                disp: 0x100,
                base: Some(13),
                index: None,
                seg: None,
            }),
            src: reg(0, Width::B64),
        };
        assert_eq!(o.encode(), Some(vec![0x49, 0x89, 0x85, 0x00, 0x01, 0, 0]));
    }

    #[test]
    fn mov_rm_store_r64_base_disp0() {
        // mov [rdi], r9: 4C 89 0F (REX.WR: reg=r9=9; r/m=rdi=7)
        let o = X86_64Opcode::MovRm {
            rip_relative: false,
            width: Width::B64,
            dst: Operand::Mem(MemOperand {
                disp: 0,
                base: Some(7),
                index: None,
                seg: None,
            }),
            src: reg(9, Width::B64),
        };
        assert_eq!(o.encode(), Some(vec![0x4C, 0x89, 0x0F]));
    }

    #[test]
    fn mov_rm_store_r64_base_disp8_sib_rsp() {
        // mov [rsp+disp8], r9: 4C 89 4C 24 XX (SIB follows mod=01 rm=4)
        let o = X86_64Opcode::MovRm {
            rip_relative: false,
            width: Width::B64,
            dst: Operand::Mem(MemOperand {
                disp: 16,
                base: Some(4),
                index: None,
                seg: None,
            }),
            src: reg(9, Width::B64),
        };
        assert_eq!(o.encode(), Some(vec![0x4C, 0x89, 0x4C, 0x24, 0x10]));
    }

    // ---- P11 Bucket J: SIB [base+index*1] forms ----

    #[test]
    fn mov_rm_load_r64_base_index_no_rex() {
        // mov r9, (%rcx,%rax): 4C 8B 0C 01 (REX.WR: reg=r9=9; SIB scale=0 idx=rax=0 base=rcx=1)
        let o = X86_64Opcode::MovRm {
            rip_relative: false,
            width: Width::B64,
            dst: reg(9, Width::B64),
            src: Operand::Mem(MemOperand {
                disp: 0,
                base: Some(1),
                index: Some((0, 1)),
                seg: None,
            }),
        };
        // reg field = r9 & 7 = 1, modrm = (0<<6)|(1<<3)|rm=4 (SIB) = 0x0C.
        // SIB = scale=0 idx=rax=0 base=rcx&7=1 = 0x01.
        assert_eq!(o.encode(), Some(vec![0x4C, 0x8B, 0x0C, 0x01]));
    }

    #[test]
    fn mov_rm_load_r64_base_index_rex_x() {
        // mov rax, (%rcx,%r8): 49 8B 04 01 (REX.WX: X=1 because index=r8>=8; SIB idx=r8&7=0)
        let o = X86_64Opcode::MovRm {
            rip_relative: false,
            width: Width::B64,
            dst: reg(0, Width::B64),
            src: Operand::Mem(MemOperand {
                disp: 0,
                base: Some(1),
                index: Some((8, 1)),
                seg: None,
            }),
        };
        // REX = 0x40 | W=0x08 | X=0x02 (index=r8>=8) = 0x4A. base=rcx=1 no B, reg=rax=0 no R.
        assert_eq!(o.encode(), Some(vec![0x4A, 0x8B, 0x04, 0x01]));
    }

    #[test]
    fn simd_movsd_load_base_index() {
        // movsd xmm0, (r11,rax), xmm0: F2 41 0F 10 04 03
        //   legacy F2 (SD), REX.B=0x41 (base=r11=11 &7=3), 0F 10 (load),
        //   ModRM mod=00 reg=xmm0=0 rm=3 (r11&7), SIB scale=0 idx=rax=0 base=r11&7=3.
        let o = X86_64Opcode::SimdMov {
            width: SimdWidth::SD,
            opcode10: false,
            dst: Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B128),
            src: Operand::Mem(MemOperand {
                disp: 0,
                base: Some(11),
                index: Some((0, 1)),
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0xF2, 0x41, 0x0F, 0x10, 0x04, 0x03]));
    }

    #[test]
    fn simd_movss_load_base_index() {
        // movss xmm0, (r11,rax), xmm0: F3 41 0F 10 04 03
        let o = X86_64Opcode::SimdMov {
            width: SimdWidth::SS,
            opcode10: false,
            dst: Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B128),
            src: Operand::Mem(MemOperand {
                disp: 0,
                base: Some(11),
                index: Some((0, 1)),
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0xF3, 0x41, 0x0F, 0x10, 0x04, 0x03]));
    }

    // ---- MovExt reg-reg (P11) ----

    #[test]
    fn mov_ext_movzx_r32_r8_no_rex() {
        // movzx eax, al:  0F B6 C0  (no REX, src=al=0)
        let o = X86_64Opcode::MovExt {
            rip_relative: false,
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
            rip_relative: false,
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
            rip_relative: false,
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
            rip_relative: false,
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
            rip_relative: false,
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
            rip_relative: false,
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
            rip_relative: false,
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
            rip_relative: false,
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
            rip_relative: false,
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
            rip_relative: false,
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
            rip_relative: false,
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
            rip_relative: false,
            sign: false,
            from: Width::B8,
            to: Width::B32,
            dst: reg(1, Width::B32),
            src: Operand::Mem(MemOperand {
                disp: 8,
                base: Some(0),
                index: None,
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x0F, 0xB6, 0x48, 0x08]));
    }

    #[test]
    fn mov_ext_movzx_load_byte_b32_disp32() {
        // movzx ecx, byte [rax+disp32]:  0F B6 88 disp32
        let o = X86_64Opcode::MovExt {
            rip_relative: false,
            sign: false,
            from: Width::B8,
            to: Width::B32,
            dst: reg(1, Width::B32),
            src: Operand::Mem(MemOperand {
                disp: 0x12345678,
                base: Some(0),
                index: None,
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x0F, 0xB6, 0x88, 0x78, 0x56, 0x34, 0x12,]));
    }

    #[test]
    fn mov_ext_movsx_load_word_b64_disp0() {
        // movswq rax, [rbx]:  48 0F BF 03  (mod=00, reg=rax=0, r/m=rbx=3)
        let o = X86_64Opcode::MovExt {
            rip_relative: false,
            sign: true,
            from: Width::B16,
            to: Width::B64,
            dst: reg(0, Width::B64),
            src: Operand::Mem(MemOperand {
                disp: 0,
                base: Some(3),
                index: None,
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x0F, 0xBF, 0x03]));
    }

    #[test]
    fn mov_ext_movsxd_load_b64_from_rsp() {
        // movsxd rax, dword [rsp]:  48 63 04 24  (SIB for rsp)
        let o = X86_64Opcode::MovExt {
            rip_relative: false,
            sign: true,
            from: Width::B32,
            to: Width::B64,
            dst: reg(0, Width::B64),
            src: Operand::Mem(MemOperand {
                disp: 0,
                base: Some(4),
                index: None,
                seg: None,
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
            rip_relative: false,
            sign: false,
            from: Width::B8,
            to: Width::B64,
            dst: reg(8, Width::B64),
            src: Operand::Mem(MemOperand {
                disp: 0,
                base: Some(11),
                index: None,
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x4D, 0x0F, 0xB6, 0x03]));
    }

    // ---- MovExt reg, [base+index*1] SIB form (P11 Bucket X) ----

    #[test]
    fn mov_ext_movsx_load_byte_b32_sib_rcx_rax() {
        // movsx eax, [rcx+rax]:  0F BE 04 01
        //   mod=00, reg=eax(0), rm=100(SIB), SIB scale=0 idx=rax(0) base=rcx(1).
        let o = X86_64Opcode::MovExt {
            rip_relative: false,
            sign: true,
            from: Width::B8,
            to: Width::B32,
            dst: reg(0, Width::B32),
            src: Operand::Mem(MemOperand {
                disp: 0,
                base: Some(1),
                index: Some((0, 1)),
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x0F, 0xBE, 0x04, 0x01]));
    }

    #[test]
    fn mov_ext_movzx_load_word_b32_sib_rcx_rax_rex() {
        // movzx eax, word [rcx+rax]:  0F B7 04 01
        let o = X86_64Opcode::MovExt {
            rip_relative: false,
            sign: false,
            from: Width::B16,
            to: Width::B32,
            dst: reg(0, Width::B32),
            src: Operand::Mem(MemOperand {
                disp: 0,
                base: Some(1),
                index: Some((0, 1)),
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x0F, 0xB7, 0x04, 0x01]));
    }

    #[test]
    fn mov_ext_movsxd_load_b64_sib_rcx_rax_rexw() {
        // movsxd rax, dword [rcx+rax]:  48 63 04 01
        let o = X86_64Opcode::MovExt {
            rip_relative: false,
            sign: true,
            from: Width::B32,
            to: Width::B64,
            dst: reg(0, Width::B64),
            src: Operand::Mem(MemOperand {
                disp: 0,
                base: Some(1),
                index: Some((0, 1)),
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x63, 0x04, 0x01]));
    }

    #[test]
    fn mov_ext_movzx_load_byte_b32_sib_r11_rax_rex() {
        // movzx eax, byte [r11+rax]:  41 0F B6 04 03
        //   REX.B=base(11&7=3)->B=1; 无 REX.W. SIB: scale=0 idx=rax(0) base=r11&7=3.
        let o = X86_64Opcode::MovExt {
            rip_relative: false,
            sign: false,
            from: Width::B8,
            to: Width::B32,
            dst: reg(0, Width::B32),
            src: Operand::Mem(MemOperand {
                disp: 0,
                base: Some(11),
                index: Some((0, 1)),
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x41, 0x0F, 0xB6, 0x04, 0x03]));
    }

    #[test]
    fn mov_ext_movsx_load_word_b32_sib_r11_rax_rex() {
        // movsx eax, word [r11+rax]:  41 0F BF 04 03
        let o = X86_64Opcode::MovExt {
            rip_relative: false,
            sign: true,
            from: Width::B16,
            to: Width::B32,
            dst: reg(0, Width::B32),
            src: Operand::Mem(MemOperand {
                disp: 0,
                base: Some(11),
                index: Some((0, 1)),
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x41, 0x0F, 0xBF, 0x04, 0x03]));
    }

    #[test]
    fn mov_ext_movsxd_load_b64_sib_r11_rax_rexwb() {
        // movsxd rax, dword [r11+rax]:  49 63 04 03
        //   REX.W=1, REX.B=base>=8.
        let o = X86_64Opcode::MovExt {
            rip_relative: false,
            sign: true,
            from: Width::B32,
            to: Width::B64,
            dst: reg(0, Width::B64),
            src: Operand::Mem(MemOperand {
                disp: 0,
                base: Some(11),
                index: Some((0, 1)),
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x49, 0x63, 0x04, 0x03]));
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

    #[test]
    fn imul_rm_rax_rsp_disp8_b64() {
        // imul rax, [rsp+8]:  48 0F AF 44 24 08
        let o = X86_64Opcode::ImulRm {
            width: Width::B64,
            dst: reg(0, Width::B64),
            src: Operand::Mem(MemOperand {
                disp: 8,
                base: Some(4),
                index: None,
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x0F, 0xAF, 0x44, 0x24, 0x08]));
    }

    #[test]
    fn imul_rm_eax_rsp_disp8_b32() {
        // imul eax, [rsp+8]:  0F AF 44 24 08
        let o = X86_64Opcode::ImulRm {
            width: Width::B32,
            dst: reg(0, Width::B32),
            src: Operand::Mem(MemOperand {
                disp: 8,
                base: Some(4),
                index: None,
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x0F, 0xAF, 0x44, 0x24, 0x08]));
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

    #[test]
    fn simd_unpcklps_xmm0_xmm1() {
        // unpcklps xmm0, xmm1:  0F 14 C1  (no legacy prefix)
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::Unpcklps,
            width: SimdWidth::PS,
            dst: xmm(0),
            src: xmm(1),
        };
        assert_eq!(o.encode(), Some(vec![0x0F, 0x14, 0xC1]));
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
            gpr_64: true,
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
            gpr_64: true,
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
            gpr_64: true,
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
            gpr_64: true,
            dst: xmm(0),
            src: reg(0, Width::B64),
        };
        assert_eq!(o.encode(), Some(vec![0xF3, 0x48, 0x0F, 0x2A, 0xC0]));
    }

    #[test]
    fn cvttss2si_eax_xmm0_b32() {
        // cvttss2si eax, xmm0: F3 0F 2C C0 (no REX.W)
        let o = X86_64Opcode::Cvt2Sd {
            to_xmm: false,
            width: SimdWidth::SS,
            gpr_64: false,
            dst: reg(0, Width::B32),
            src: xmm(0),
        };
        assert_eq!(o.encode(), Some(vec![0xF3, 0x0F, 0x2C, 0xC0]));
    }

    #[test]
    fn cvttss2si_r8d_xmm0_b32_highreg() {
        // cvttss2si r8d, xmm0: F3 41 0F 2C C0 (REX.B for r/m=r8)
        let o = X86_64Opcode::Cvt2Sd {
            to_xmm: false,
            width: SimdWidth::SS,
            gpr_64: false,
            dst: reg(8, Width::B32),
            src: xmm(0),
        };
        assert_eq!(o.encode(), Some(vec![0xF3, 0x41, 0x0F, 0x2C, 0xC0]));
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
        let o = X86_64Opcode::Setcc {
            cc: CondCode::Eq,
            dst: reg(0, Width::B8),
        };
        assert_eq!(o.encode(), Some(vec![0x0F, 0x94, 0xC0]));
    }

    #[test]
    fn setcc_ne_cl() {
        // setne cl: 0F 95 C1
        let o = X86_64Opcode::Setcc {
            cc: CondCode::Ne,
            dst: reg(1, Width::B8),
        };
        assert_eq!(o.encode(), Some(vec![0x0F, 0x95, 0xC1]));
    }

    #[test]
    fn setcc_b_dl() {
        // setb dl: 0F 92 C2
        let o = X86_64Opcode::Setcc {
            cc: CondCode::B,
            dst: reg(2, Width::B8),
        };
        assert_eq!(o.encode(), Some(vec![0x0F, 0x92, 0xC2]));
    }

    #[test]
    fn setcc_a_bl() {
        // seta bl: 0F 97 C3
        let o = X86_64Opcode::Setcc {
            cc: CondCode::A,
            dst: reg(3, Width::B8),
        };
        assert_eq!(o.encode(), Some(vec![0x0F, 0x97, 0xC3]));
    }

    #[test]
    fn setcc_np_sil_high() {
        // setnp r14b: 41 0F 9B C6  (reg=14 >=8 needs REX.B; reg&7=6, modrm=0xC6)
        let o = X86_64Opcode::Setcc {
            cc: CondCode::Np,
            dst: reg(14, Width::B8),
        };
        assert_eq!(o.encode(), Some(vec![0x41, 0x0F, 0x9B, 0xC6]));
    }

    #[test]
    fn setcc_s_al() {
        // sets al: 0F 98 C0
        let o = X86_64Opcode::Setcc {
            cc: CondCode::S,
            dst: reg(0, Width::B8),
        };
        assert_eq!(o.encode(), Some(vec![0x0F, 0x98, 0xC0]));
    }

    #[test]
    fn setcc_o_dl() {
        // seto dl: 0F 90 C2
        let o = X86_64Opcode::Setcc {
            cc: CondCode::O,
            dst: reg(2, Width::B8),
        };
        assert_eq!(o.encode(), Some(vec![0x0F, 0x90, 0xC2]));
    }

    // ---- P11 Bucket I: NoArg (cld/mfence/rep movsb/stosb/movsq) ----
    #[test]
    fn noarg_cld() {
        let o = X86_64Opcode::NoArg { op: NoArgOp::Cld };
        assert_eq!(o.encode(), Some(vec![0xFC]));
    }

    #[test]
    fn noarg_mfence() {
        let o = X86_64Opcode::NoArg {
            op: NoArgOp::Mfence,
        };
        assert_eq!(o.encode(), Some(vec![0x0F, 0xAE, 0xF0]));
    }

    #[test]
    fn noarg_rep_movsb() {
        let o = X86_64Opcode::NoArg {
            op: NoArgOp::RepMovsb,
        };
        assert_eq!(o.encode(), Some(vec![0xF3, 0xA4]));
    }

    #[test]
    fn noarg_rep_stosb() {
        let o = X86_64Opcode::NoArg {
            op: NoArgOp::RepStosb,
        };
        assert_eq!(o.encode(), Some(vec![0xF3, 0xAA]));
    }

    #[test]
    fn noarg_rep_stosq() {
        let o = X86_64Opcode::NoArg {
            op: NoArgOp::RepStosq,
        };
        assert_eq!(o.encode(), Some(vec![0xF3, 0x48, 0xAB]));
    }

    #[test]
    fn noarg_rep_movsq() {
        let o = X86_64Opcode::NoArg {
            op: NoArgOp::RepMovsq,
        };
        assert_eq!(o.encode(), Some(vec![0xF3, 0x48, 0xA5]));
    }

    // ---- P11 Bucket J: andps/andpd reg-reg ----
    #[test]
    fn simd_andps_xmm0_xmm1() {
        // andps xmm0, xmm1: 0F 54 C1
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::Andps,
            width: SimdWidth::O,
            dst: Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B128),
            src: Operand::Reg(1, crate::codegen::shared::asm_operand::Width::B128),
        };
        assert_eq!(o.encode(), Some(vec![0x0F, 0x54, 0xC1]));
    }

    #[test]
    fn simd_andpd_xmm0_xmm1() {
        // andpd xmm0, xmm1: 66 0F 54 C1
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::Andpd,
            width: SimdWidth::O,
            dst: Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B128),
            src: Operand::Reg(1, crate::codegen::shared::asm_operand::Width::B128),
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x0F, 0x54, 0xC1]));
    }

    // ---- P11 Bucket G2: pcmpeqb/w/d, pminub/pmaxub, pmaddwd/psadbw, sat w ----
    #[test]
    fn simd_pcmpeqb_xmm0_xmm1() {
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::PCmpEq,
            width: SimdWidth::B,
            dst: xmm(0),
            src: xmm(1),
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x0F, 0x74, 0xC1]));
    }

    #[test]
    fn simd_pcmpeqw_xmm0_xmm1() {
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::PCmpEq,
            width: SimdWidth::W,
            dst: xmm(0),
            src: xmm(1),
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x0F, 0x75, 0xC1]));
    }

    #[test]
    fn simd_pcmpeqd_xmm0_xmm1() {
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::PCmpEq,
            width: SimdWidth::D,
            dst: xmm(0),
            src: xmm(1),
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x0F, 0x76, 0xC1]));
    }

    #[test]
    fn simd_pminub_xmm0_xmm1() {
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::PMinMaxUb,
            width: SimdWidth::B,
            dst: xmm(0),
            src: xmm(1),
        };
        assert_eq!(o.encode(), Some(vec![0x0F, 0xDA, 0xC1]));
    }

    #[test]
    fn simd_pmaxub_xmm0_xmm1() {
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::PMinMaxUb,
            width: SimdWidth::W,
            dst: xmm(0),
            src: xmm(1),
        };
        assert_eq!(o.encode(), Some(vec![0x0F, 0xDE, 0xC1]));
    }

    #[test]
    fn simd_pmaddwd_xmm0_xmm1() {
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::PMaddwd,
            width: SimdWidth::O,
            dst: xmm(0),
            src: xmm(1),
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x0F, 0xF5, 0xC1]));
    }

    #[test]
    fn simd_psadbw_xmm0_xmm1() {
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::PSadbw,
            width: SimdWidth::O,
            dst: xmm(0),
            src: xmm(1),
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x0F, 0xF6, 0xC1]));
    }

    #[test]
    fn simd_paddsw_xmm0_xmm1() {
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::PSatW,
            width: SimdWidth::W,
            dst: xmm(0),
            src: xmm(1),
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x0F, 0xED, 0xC1]));
    }

    #[test]
    fn simd_psubsw_xmm0_xmm1() {
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::PSatW,
            width: SimdWidth::Q,
            dst: xmm(0),
            src: xmm(1),
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x0F, 0xE9, 0xC1]));
    }

    #[test]
    fn simd_paddusw_xmm0_xmm1() {
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::PSatW,
            width: SimdWidth::D,
            dst: xmm(0),
            src: xmm(1),
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x0F, 0xDD, 0xC1]));
    }

    #[test]
    fn simd_psubusw_xmm0_xmm1() {
        let o = X86_64Opcode::SimdArith {
            op: SimdArithOp::PSatW,
            width: SimdWidth::B,
            dst: xmm(0),
            src: xmm(1),
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x0F, 0xD9, 0xC1]));
    }

    #[test]
    fn simd_psllq_xmm0_imm8() {
        let o = X86_64Opcode::SimdShiftImm8 {
            op: SimdArithOp::PShiftImm8,
            width: SimdWidth::Q,
            dst: xmm(0),
            imm8: 1,
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x0F, 0xF3, 0xC0, 0x01]));
    }

    #[test]
    fn simd_psrlq_xmm0_imm8() {
        let o = X86_64Opcode::SimdShiftImm8 {
            op: SimdArithOp::PShiftImm8,
            width: SimdWidth::D,
            dst: xmm(0),
            imm8: 2,
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x0F, 0xD3, 0xC0, 0x02]));
    }

    #[test]
    fn simd_psrad_xmm0_imm8() {
        let o = X86_64Opcode::SimdShiftImm8 {
            op: SimdArithOp::PShiftImm8,
            width: SimdWidth::W,
            dst: xmm(0),
            imm8: 3,
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x0F, 0xE2, 0xC0, 0x03]));
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
        // movd xmm0, eax: 66 0F 6E C0  (66 prefix -> SSE2 XMM, no REX.W; B32)
        let o = X86_64Opcode::GprXmm {
            to_xmm: true,
            width: crate::codegen::shared::asm_operand::Width::B32,
            dst: Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B128),
            src: Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B32),
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x0F, 0x6E, 0xC0]));
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
                disp: 16,
                base: Some(4),
                index: None,
                seg: None,
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
                disp: 8,
                base: Some(0),
                index: None,
                seg: None,
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
                disp: 16,
                base: Some(13),
                index: None,
                seg: None,
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
                disp: 8,
                base: Some(0),
                index: None,
                seg: None,
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
                disp: 0,
                base: Some(4),
                index: None,
                seg: None,
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
                disp: 8,
                base: Some(4),
                index: None,
                seg: None,
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
                disp: 8,
                base: Some(4),
                index: None,
                seg: None,
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
                disp: 0,
                base: Some(4),
                index: None,
                seg: None,
            }),
            src: Operand::Reg(0, Width::B128),
        };
        assert_eq!(o.encode(), Some(vec![0xF3, 0x0F, 0x11, 0x04, 0x24]));
    }

    #[test]
    fn simd_mov_store_dq_xmm0_rsp_disp8() {
        // movdqu [rsp+16], xmm0: F3 0F 7F 44 24 10
        let o = X86_64Opcode::SimdMov {
            width: SimdWidth::DQ,
            opcode10: false,
            dst: Operand::Mem(MemOperand {
                disp: 16,
                base: Some(4),
                index: None,
                seg: None,
            }),
            src: Operand::Reg(0, Width::B128),
        };
        assert_eq!(o.encode(), Some(vec![0xF3, 0x0F, 0x7F, 0x44, 0x24, 0x10]));
    }

    #[test]
    fn simd_mov_load_dq_xmm2_rsp_disp8() {
        // movdqu xmm2, [rsp+16]: F3 0F 6F 54 24 10
        let o = X86_64Opcode::SimdMov {
            width: SimdWidth::DQ,
            opcode10: true,
            dst: Operand::Reg(2, Width::B128),
            src: Operand::Mem(MemOperand {
                disp: 16,
                base: Some(4),
                index: None,
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0xF3, 0x0F, 0x6F, 0x54, 0x24, 0x10]));
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
                disp: 0,
                base: Some(0),
                index: None,
                seg: None,
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
                disp: 4,
                base: Some(0),
                index: None,
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0xF3, 0x0F, 0x10, 0x40, 0x04]));
    }

    // ---- P11 Bucket H2: X87 fld/fstp/fild [base+disp] sweep ----

    #[test]
    fn x87_fld_m32_rsp_disp0() {
        // fld dword [rsp]: D9 04 24 (mod=00, SIB base=rsp)
        let o = X86_64Opcode::X87 {
            op: X87Op::FldMem { width: 4 },
            mem: Some(MemOperand {
                disp: 0,
                base: Some(4),
                index: None,
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0xD9, 0x04, 0x24]));
    }

    #[test]
    fn x87_fld_m32_rsp_disp8() {
        // fld dword [rsp+8]: D9 44 24 08 (mod=01, SIB base=rsp)
        let o = X86_64Opcode::X87 {
            op: X87Op::FldMem { width: 4 },
            mem: Some(MemOperand {
                disp: 8,
                base: Some(4),
                index: None,
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0xD9, 0x44, 0x24, 0x08]));
    }

    #[test]
    fn x87_fld_m64_rax_disp0() {
        // fld qword [rax]: DD 00 (mod=00, rm=rax, no SIB)
        let o = X86_64Opcode::X87 {
            op: X87Op::FldMem { width: 8 },
            mem: Some(MemOperand {
                disp: 0,
                base: Some(0),
                index: None,
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0xDD, 0x00]));
    }

    #[test]
    fn x87_fld_m64_rcx_disp8() {
        // fld qword [rcx+16]: DD 41 10 (mod=01, reg=0, rm=rcx=1)
        let o = X86_64Opcode::X87 {
            op: X87Op::FldMem { width: 8 },
            mem: Some(MemOperand {
                disp: 16,
                base: Some(1),
                index: None,
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0xDD, 0x41, 0x10]));
    }

    #[test]
    fn x87_fstp_m64_rsp_disp0() {
        // fstp qword [rsp]: DD 1C 24 (mod=00, SIB base=rsp, reg=3)
        let o = X86_64Opcode::X87 {
            op: X87Op::FstpMem { width: 8 },
            mem: Some(MemOperand {
                disp: 0,
                base: Some(4),
                index: None,
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0xDD, 0x1C, 0x24]));
    }

    #[test]
    fn x87_fstp_tbyte_rsp_disp0() {
        // fstp tbyte [rsp]: DB 3C 24 (mod=00, SIB base=rsp, reg=7)
        let o = X86_64Opcode::X87 {
            op: X87Op::FstpMem { width: 10 },
            mem: Some(MemOperand {
                disp: 0,
                base: Some(4),
                index: None,
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0xDB, 0x3C, 0x24]));
    }

    #[test]
    fn x87_fild_m64_rsp_disp8_rexw() {
        // fild qword [rsp+8]: 48 DB 04 24 08 (REX.W, mod=01, SIB)
        let o = X86_64Opcode::X87 {
            op: X87Op::FildMem { width: 8 },
            mem: Some(MemOperand {
                disp: 8,
                base: Some(4),
                index: None,
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0xDB, 0x44, 0x24, 0x08]));
    }

    #[test]
    fn x87_fcw_fnstcw_rsp_disp0() {
        // fnstcw [rsp]: D9 3C 24 (mod=00, SIB base=rsp, reg=7)
        let o = X86_64Opcode::X87 {
            op: X87Op::FcwMem { store: true },
            mem: Some(MemOperand {
                disp: 0,
                base: Some(4),
                index: None,
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0xD9, 0x3C, 0x24]));
    }

    #[test]
    fn x87_mxcsr_stmxcsr_rsp_disp0() {
        // stmxcsr [rsp]: 0F AE 1C 24 (mod=00, SIB base=rsp, reg=3)
        let o = X86_64Opcode::X87 {
            op: X87Op::MxcsrMem { store: true },
            mem: Some(MemOperand {
                disp: 0,
                base: Some(4),
                index: None,
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x0F, 0xAE, 0x1C, 0x24]));
    }

    #[test]
    fn x87_fld_m64_rdi_disp8() {
        // fld qword [rdi+0x1C]: DD 47 1C (mod=01, reg=0, rm=rdi=7)
        let o = X86_64Opcode::X87 {
            op: X87Op::FldMem { width: 8 },
            mem: Some(MemOperand {
                disp: 0x1C,
                base: Some(7),
                index: None,
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0xDD, 0x47, 0x1C]));
    }

    // ---- Lea SIB index (P11 Bucket P) ----

    #[test]
    fn lea_rax_rdx_rax_sib_scale1() {
        // lea rax, (%rdx,%rax): REX.W 48 8D 04 02
        // rex: W=1, R=0(dst=rax=0), X=0(index=rax=0), B=0(base=rdx=2)
        // modrm: mod=00, reg=0, r/m=100(SIB) → 0x04
        // sib: scale=0(×1), index=rax=0, base=rdx=2 → 0x02
        let o = X86_64Opcode::Lea {
            dst: reg(0, Width::B64),
            mem: MemOperand {
                disp: 0,
                base: Some(2),
                index: Some((0, 1)),
                seg: None,
            },
            rip_relative: false,
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x8D, 0x04, 0x02]));
    }

    #[test]
    fn lea_r11_r11_r10_sib_scale1() {
        // lea r11, [r11+r10]: REX.WRXB 4F 8D 1C 13
        // rex: W=1 R=1(dst=r11=11>8) X=1(index=r10=10>8) B=1(base=r11=11>8)
        //   = 0x40 | 0x08 | 0x04 | 0x02 | 0x01 = 0x4F
        // modrm: mod=00 reg=3(dst&7=3) r/m=100 → 0x1C
        // sib: scale=0, index=2(r10&7=2), base=3(r11&7=3) → 0x13
        // 注: nested.rs 里写死的 0x4D 对应 dst=r10(非 r11); 当 dst=r11 时 REX.R=1.
        let o = X86_64Opcode::Lea {
            dst: reg(11, Width::B64),
            mem: MemOperand {
                disp: 0,
                base: Some(11),
                index: Some((10, 1)),
                seg: None,
            },
            rip_relative: false,
        };
        let got = o.encode();
        assert_eq!(got, Some(vec![0x4F, 0x8D, 0x1C, 0x13]), "got {:?}", got);
    }

    #[test]
    fn lea_rax_r13_r11_sib_scale1() {
        // lea rax, [r13+r11]: REX.WXB 4B 8D 04 1D
        // rex: W=1 R=0(dst=rax=0) X=1(index=r11=11>8) B=1(base=r13=13>8)
        //   = 0x40 | 0x08 | 0x02 | 0x01 = 0x4B
        // modrm: mod=00 reg=0(dst=rax=0) r/m=100 → 0x04
        // sib: scale=0(×1), index=3(r11&7=3), base=5(r13&7=5) → 0x1D
        // 注: 原 nested.rs 里写死的 0x49/0x1C/D3 对应 base=r11 (非 r13),
        // 那里 dst=rax=0, base=r11=11, index=r10=10 — 是另一个形态.
        let o = X86_64Opcode::Lea {
            dst: reg(0, Width::B64),
            mem: MemOperand {
                disp: 0,
                base: Some(13),
                index: Some((11, 1)),
                seg: None,
            },
            rip_relative: false,
        };
        let got = o.encode();
        assert_eq!(got, Some(vec![0x4B, 0x8D, 0x04, 0x1D]), "got {:?}", got);
    }

    #[test]
    fn lea_rax_rsp_disp8() {
        // lea rax, [rsp+disp8]: REX.W 48 8D 44 24 <disp8>
        // rex: W=1, R=0(dst=rax=0), B=0(base=rsp=4<8)
        // modrm: mod=01(disp8), reg=0(dst=rax=0), r/m=100(SIB) → 0x44
        // sib: scale=0, index=4(no index), base=4(rsp) → 0x24
        let o = X86_64Opcode::Lea {
            dst: reg(0, Width::B64),
            mem: MemOperand {
                disp: 0x0F,
                base: Some(4),
                index: None,
                seg: None,
            },
            rip_relative: false,
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x8D, 0x44, 0x24, 0x0F]));
    }

    #[test]
    fn lea_eax_rip_relative() {
        // lea rax, [rip+disp32=0]: 48 8D 05 00 00 00 00
        // rex: W=1 R=0(dst=rax=0) modrm: mod=00 reg=0 r/m=101(rip-rel) → 0x05
        let o = X86_64Opcode::Lea {
            dst: reg(0, Width::B64),
            mem: MemOperand {
                disp: 0,
                base: None,
                index: None,
                seg: None,
            },
            rip_relative: true,
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x8D, 0x05, 0, 0, 0, 0]));
    }

    #[test]
    fn lea_r8_rip_relative() {
        // lea r8, [rip+disp32=0]: 4C 8D 05 00 00 00 00
        // rex: W=1 R=1(dst=r8=8>7) → 0x4C
        let o = X86_64Opcode::Lea {
            dst: reg(8, Width::B64),
            mem: MemOperand {
                disp: 0,
                base: None,
                index: None,
                seg: None,
            },
            rip_relative: true,
        };
        assert_eq!(o.encode(), Some(vec![0x4C, 0x8D, 0x05, 0, 0, 0, 0]));
    }

    // ---- P11 Bucket O8: MovRm / MovExt rip-relative (global load/store) ----

    #[test]
    fn mov_rm_load_b64_rip_rel() {
        // mov rax, [rip+disp32]: 48 8B 05 disp32  (手写 emit_global_load _ 分支)
        let o = X86_64Opcode::MovRm {
            rip_relative: true,
            width: Width::B64,
            dst: reg(0, Width::B64),
            src: Operand::Mem(MemOperand {
                disp: 0,
                base: None,
                index: None,
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x8B, 0x05, 0, 0, 0, 0]));
    }

    #[test]
    fn mov_rm_load_b64_rip_rel_r8() {
        // mov r8, [rip+disp32]: 4C 8B 05 disp32 (REX.W=0x48 REX.R=0x04)
        let o = X86_64Opcode::MovRm {
            rip_relative: true,
            width: Width::B64,
            dst: reg(8, Width::B64),
            src: Operand::Mem(MemOperand {
                disp: 0,
                base: None,
                index: None,
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x4C, 0x8B, 0x05, 0, 0, 0, 0]));
    }

    #[test]
    fn mov_rm_load_b32_rip_rel_no_rex_w() {
        // mov eax, [rip+disp32]: 8B 05 disp32 (B32 无 REX.W, dst=eax=0)
        let o = X86_64Opcode::MovRm {
            rip_relative: true,
            width: Width::B32,
            dst: reg(0, Width::B32),
            src: Operand::Mem(MemOperand {
                disp: 0,
                base: None,
                index: None,
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x8B, 0x05, 0, 0, 0, 0]));
    }

    #[test]
    fn mov_rm_store_b64_rip_rel() {
        // mov [rip+disp32], rax: 48 89 05 disp32 (store: 0x89)
        let o = X86_64Opcode::MovRm {
            rip_relative: true,
            width: Width::B64,
            dst: Operand::Mem(MemOperand {
                disp: 0,
                base: None,
                index: None,
                seg: None,
            }),
            src: reg(0, Width::B64),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x89, 0x05, 0, 0, 0, 0]));
    }

    #[test]
    fn mov_rm_store_b64_rip_rel_r8() {
        // mov [rip+disp32], r8: 4C 89 05 disp32
        let o = X86_64Opcode::MovRm {
            rip_relative: true,
            width: Width::B64,
            dst: Operand::Mem(MemOperand {
                disp: 0,
                base: None,
                index: None,
                seg: None,
            }),
            src: reg(8, Width::B64),
        };
        assert_eq!(o.encode(), Some(vec![0x4C, 0x89, 0x05, 0, 0, 0, 0]));
    }

    #[test]
    fn mov_rm_store_b16_rip_rel() {
        // mov [rip+disp32], ax: 66 89 05 disp32 (0x66 prefix)
        let o = X86_64Opcode::MovRm {
            rip_relative: true,
            width: Width::B16,
            dst: Operand::Mem(MemOperand {
                disp: 0,
                base: None,
                index: None,
                seg: None,
            }),
            src: reg(0, Width::B16),
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x89, 0x05, 0, 0, 0, 0]));
    }

    #[test]
    fn mov_rm_store_b16_rip_rel_r8() {
        // mov [rip+disp32], r8w: 66 44 89 05 disp32 (0x66 在 REX 之前)
        let o = X86_64Opcode::MovRm {
            rip_relative: true,
            width: Width::B16,
            dst: Operand::Mem(MemOperand {
                disp: 0,
                base: None,
                index: None,
                seg: None,
            }),
            src: reg(8, Width::B16),
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x44, 0x89, 0x05, 0, 0, 0, 0]));
    }

    #[test]
    fn mov_rm_store_b8_rip_rel() {
        // mov [rip+disp32], al: 88 05 disp32 (B8: 0x88)
        let o = X86_64Opcode::MovRm {
            rip_relative: true,
            width: Width::B8,
            dst: Operand::Mem(MemOperand {
                disp: 0,
                base: None,
                index: None,
                seg: None,
            }),
            src: reg(0, Width::B8),
        };
        assert_eq!(o.encode(), Some(vec![0x88, 0x05, 0, 0, 0, 0]));
    }

    #[test]
    fn mov_rm_store_b8_rip_rel_r8b() {
        // mov [rip+disp32], r8b: 44 88 05 disp32 (reg=r8=8 → REX.R=0x04)
        let o = X86_64Opcode::MovRm {
            rip_relative: true,
            width: Width::B8,
            dst: Operand::Mem(MemOperand {
                disp: 0,
                base: None,
                index: None,
                seg: None,
            }),
            src: reg(8, Width::B8),
        };
        assert_eq!(o.encode(), Some(vec![0x44, 0x88, 0x05, 0, 0, 0, 0]));
    }

    #[test]
    fn mov_rm_store_b8_rip_rel_spl() {
        // mov [rip+disp32], spl: reg=4 需 REX 选高字节; rm=101(rip)
        // modrm = 0x05 | ((4&7)<<3) = 0x05 | 0x20 = 0x25
        let o = X86_64Opcode::MovRm {
            rip_relative: true,
            width: Width::B8,
            dst: Operand::Mem(MemOperand {
                disp: 0,
                base: None,
                index: None,
                seg: None,
            }),
            src: reg(4, Width::B8),
        };
        assert_eq!(o.encode(), Some(vec![0x40, 0x88, 0x25, 0, 0, 0, 0]));
    }

    #[test]
    fn mov_ext_load_b8_rip_rel() {
        // movzx eax, byte [rip+disp32]: 0F B6 05 disp32
        let o = X86_64Opcode::MovExt {
            rip_relative: true,
            sign: false,
            from: Width::B8,
            to: Width::B32,
            dst: reg(0, Width::B32),
            src: Operand::Mem(MemOperand {
                disp: 0,
                base: None,
                index: None,
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x0F, 0xB6, 0x05, 0, 0, 0, 0]));
    }

    #[test]
    fn mov_ext_load_b8_signed_rip_rel() {
        // movsx eax, byte [rip+disp32]: 0F BE 05 disp32
        let o = X86_64Opcode::MovExt {
            rip_relative: true,
            sign: true,
            from: Width::B8,
            to: Width::B32,
            dst: reg(0, Width::B32),
            src: Operand::Mem(MemOperand {
                disp: 0,
                base: None,
                index: None,
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x0F, 0xBE, 0x05, 0, 0, 0, 0]));
    }

    #[test]
    fn mov_ext_load_b8_rip_rel_r8() {
        // movzx r8d, byte [rip+disp32]: 44 0F B6 05 disp32
        let o = X86_64Opcode::MovExt {
            rip_relative: true,
            sign: false,
            from: Width::B8,
            to: Width::B32,
            dst: reg(8, Width::B32),
            src: Operand::Mem(MemOperand {
                disp: 0,
                base: None,
                index: None,
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x44, 0x0F, 0xB6, 0x05, 0, 0, 0, 0]));
    }

    #[test]
    fn mov_ext_load_b16_rip_rel() {
        // movzx eax, word [rip+disp32]: 0F B7 05 disp32
        let o = X86_64Opcode::MovExt {
            rip_relative: true,
            sign: false,
            from: Width::B16,
            to: Width::B32,
            dst: reg(0, Width::B32),
            src: Operand::Mem(MemOperand {
                disp: 0,
                base: None,
                index: None,
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x0F, 0xB7, 0x05, 0, 0, 0, 0]));
    }

    #[test]
    fn mov_ext_movsxd_rip_rel() {
        // movsxd rax, dword [rip+disp32]: 48 63 05 disp32
        let o = X86_64Opcode::MovExt {
            rip_relative: true,
            sign: true,
            from: Width::B32,
            to: Width::B64,
            dst: reg(0, Width::B64),
            src: Operand::Mem(MemOperand {
                disp: 0,
                base: None,
                index: None,
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x63, 0x05, 0, 0, 0, 0]));
    }

    #[test]
    fn mov_ext_movsxd_rip_rel_r8() {
        // movsxd r8, dword [rip+disp32]: 4C 63 05 disp32
        let o = X86_64Opcode::MovExt {
            rip_relative: true,
            sign: true,
            from: Width::B32,
            to: Width::B64,
            dst: reg(8, Width::B64),
            src: Operand::Mem(MemOperand {
                disp: 0,
                base: None,
                index: None,
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0x4C, 0x63, 0x05, 0, 0, 0, 0]));
    }

    // ---- P11 Bucket M: Adc/Sbb/Mul/Rcl/Ud2 ----

    #[test]
    fn alu_rm_adc_r8_r11_b64() {
        // adc r8, r11: REX.W R=1(r11) B=1(r8) → 4D 11 D8
        // opcode ADC r/m,r = 0x11 (B64); modrm mod=11 reg=r11&7=3, r/m=r8&7=0 → 0xD8
        let o = X86_64Opcode::AluRm {
            op: AluOp::Adc,
            width: Width::B64,
            dst: reg(8, Width::B64),
            src: reg(11, Width::B64),
        };
        assert_eq!(o.encode(), Some(vec![0x4D, 0x11, 0xD8]));
    }

    #[test]
    fn alu_rm_sbb_r8_r11_b64() {
        // sbb r8, r11: 4D 19 D8
        let o = X86_64Opcode::AluRm {
            op: AluOp::Sbb,
            width: Width::B64,
            dst: reg(8, Width::B64),
            src: reg(11, Width::B64),
        };
        assert_eq!(o.encode(), Some(vec![0x4D, 0x19, 0xD8]));
    }

    #[test]
    fn alu_rm_adc_r8_imm0_b64() {
        // adc r8, 0: REX.W B=1(r8) → 49 83 D0 00
        // 0x83 /2 ib. modrm mod=11 reg=/2, r/m=r8&7=0 → 0xD0.
        let o = X86_64Opcode::AluRm {
            op: AluOp::Adc,
            width: Width::B64,
            dst: reg(8, Width::B64),
            src: Operand::Imm(0),
        };
        assert_eq!(o.encode(), Some(vec![0x49, 0x83, 0xD0, 0x00]));
    }

    #[test]
    fn unary_mul_r9_b64() {
        // mul r9: REX.W B=1(r9) → 49 F7 E1 (/4)
        let o = X86_64Opcode::UnaryRm {
            op: UnaryOp::Mul,
            width: Width::B64,
            dst: reg(9, Width::B64),
        };
        assert_eq!(o.encode(), Some(vec![0x49, 0xF7, 0xE1]));
    }

    #[test]
    fn unary_mul_r11_b64() {
        // mul r11: REX.W B=1(r11) → 49 F7 E3
        let o = X86_64Opcode::UnaryRm {
            op: UnaryOp::Mul,
            width: Width::B64,
            dst: reg(11, Width::B64),
        };
        assert_eq!(o.encode(), Some(vec![0x49, 0xF7, 0xE3]));
    }

    #[test]
    fn unary_imul_ecx_b32() {
        // imul ecx (single-operand): F7 /5 → F7 E9 (mod=11, reg=/5=101, r/m=ecx=1)
        let o = X86_64Opcode::UnaryRm {
            op: UnaryOp::Imul,
            width: Width::B32,
            dst: reg(1, Width::B32),
        };
        assert_eq!(o.encode(), Some(vec![0xF7, 0xE9]));
    }

    #[test]
    fn unary_imul_rcx_b64() {
        // imul rcx (single-operand): REX.W=48 F7 /5 → 48 F7 E9
        let o = X86_64Opcode::UnaryRm {
            op: UnaryOp::Imul,
            width: Width::B64,
            dst: reg(1, Width::B64),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0xF7, 0xE9]));
    }

    #[test]
    fn unary_imul_cl_b8() {
        // imul cl (single-operand): F6 /5 → REX 40 F6 E9 (8-bit always gets REX prefix per UnaryRm rule)
        let o = X86_64Opcode::UnaryRm {
            op: UnaryOp::Imul,
            width: Width::B8,
            dst: reg(1, Width::B8),
        };
        assert_eq!(o.encode(), Some(vec![0x40, 0xF6, 0xE9]));
    }

    #[test]
    fn unary_div_ecx_b64() {
        // div rcx: REX.W=48 F7 /6 (modrm = 0xC0 | 0x30 | 1 = 0xF1) → 48 F7 F1
        let o = X86_64Opcode::UnaryRm {
            op: UnaryOp::Div,
            width: Width::B64,
            dst: reg(1, Width::B64),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0xF7, 0xF1]));
    }

    #[test]
    fn unary_idiv_ecx_b32() {
        // idiv ecx: F7 /7 (modrm = 0xC0 | 0x38 | 1 = 0xF9) → F7 F9
        let o = X86_64Opcode::UnaryRm {
            op: UnaryOp::Idiv,
            width: Width::B32,
            dst: reg(1, Width::B32),
        };
        assert_eq!(o.encode(), Some(vec![0xF7, 0xF9]));
    }

    #[test]
    fn unary_idiv_edi_b64() {
        // idiv rdi: REX.W=48 F7 /7 (modrm = 0xC0 | 0x38 | 7 = 0xFF) → 48 F7 FF
        let o = X86_64Opcode::UnaryRm {
            op: UnaryOp::Idiv,
            width: Width::B64,
            dst: reg(7, Width::B64),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0xF7, 0xFF]));
    }

    #[test]
    fn shift_rcl_rdx_1_b64() {
        // rcl rdx, 1: REX.W=48 D1 /2 → 48 D1 D2
        let o = X86_64Opcode::Shift {
            op: ShiftOp::Rcl,
            width: Width::B64,
            dst: reg(2, Width::B64),
            amount: ShiftAmount::Imm8(1),
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0xD1, 0xD2]));
    }

    #[test]
    fn shift_rcl_r14_1_b64() {
        // rcl r14, 1: REX.W|B(r14) = 49 D1 D6 (/2)
        let o = X86_64Opcode::Shift {
            op: ShiftOp::Rcl,
            width: Width::B64,
            dst: reg(14, Width::B64),
            amount: ShiftAmount::Imm8(1),
        };
        assert_eq!(o.encode(), Some(vec![0x49, 0xD1, 0xD6]));
    }

    #[test]
    fn shift_rol_rax_cl_b64() {
        // rol rax, cl: REX.W 48 D3 C0 (/0)
        let o = X86_64Opcode::Shift {
            op: ShiftOp::Rol,
            width: Width::B64,
            dst: reg(0, Width::B64),
            amount: ShiftAmount::Cl,
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0xD3, 0xC0]));
    }

    #[test]
    fn shift_ror_r10_cl_b64() {
        // ror r10, cl: REX.W|B(r10)=49 D3 CA (/1; mod=11, reg=/1=001, r/m=r10&7=2)
        let o = X86_64Opcode::Shift {
            op: ShiftOp::Ror,
            width: Width::B64,
            dst: reg(10, Width::B64),
            amount: ShiftAmount::Cl,
        };
        assert_eq!(o.encode(), Some(vec![0x49, 0xD3, 0xCA]));
    }

    #[test]
    fn shift_rol_eax_cl_b32() {
        // rol eax, cl: D3 C0 (no REX, no REX.W)
        let o = X86_64Opcode::Shift {
            op: ShiftOp::Rol,
            width: Width::B32,
            dst: reg(0, Width::B32),
            amount: ShiftAmount::Cl,
        };
        assert_eq!(o.encode(), Some(vec![0xD3, 0xC0]));
    }

    #[test]
    fn shift_ror_eax_cl_b32() {
        // ror eax, cl: D3 C8
        let o = X86_64Opcode::Shift {
            op: ShiftOp::Ror,
            width: Width::B32,
            dst: reg(0, Width::B32),
            amount: ShiftAmount::Cl,
        };
        assert_eq!(o.encode(), Some(vec![0xD3, 0xC8]));
    }

    #[test]
    fn shift_rol_r16_cl_b16() {
        // rol r16, cl: 66 D3 C0 (r16=eax-like reg0=ax)
        let o = X86_64Opcode::Shift {
            op: ShiftOp::Rol,
            width: Width::B16,
            dst: reg(0, Width::B16),
            amount: ShiftAmount::Cl,
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0xD3, 0xC0]));
    }

    #[test]
    fn shift_rol_r8_cl_b8() {
        // rol r8b, cl: REX.B=41 D2 C0 (/0); r8 idx=8 (REX.B=1, no REX.W)
        let o = X86_64Opcode::Shift {
            op: ShiftOp::Rol,
            width: Width::B8,
            dst: reg(8, Width::B8),
            amount: ShiftAmount::Cl,
        };
        assert_eq!(o.encode(), Some(vec![0x41, 0xD2, 0xC0]));
    }

    #[test]
    fn shift_ror_al_cl_b8() {
        // ror al, cl: D2 C8 (no REX, al=0 not high-byte)
        let o = X86_64Opcode::Shift {
            op: ShiftOp::Ror,
            width: Width::B8,
            dst: reg(0, Width::B8),
            amount: ShiftAmount::Cl,
        };
        assert_eq!(o.encode(), Some(vec![0xD2, 0xC8]));
    }

    #[test]
    fn noarg_ud2() {
        // ud2: 0F 0B
        let o = X86_64Opcode::NoArg { op: NoArgOp::Ud2 };
        assert_eq!(o.encode(), Some(vec![0x0F, 0x0B]));
    }

    // ---- P11 Bucket O3: LOCK CMPXCHG / XCHG r/m ----

    #[test]
    fn lock_cmpxchg_rdi_rdx_b8() {
        // lock cmpxchg [rdi], dl: F0 40 0F B0 17
        let o = X86_64Opcode::LockCmpxchgRm {
            width: Width::B8,
            reg_idx: 2,
            mem: MemOperand {
                disp: 0,
                base: Some(7),
                index: None,
                seg: None,
            },
        };
        assert_eq!(o.encode(), Some(vec![0xF0, 0x40, 0x0F, 0xB0, 0x17]));
    }

    #[test]
    fn lock_cmpxchg_rdi_rdx_b16() {
        // lock cmpxchg [rdi], dx: 66 F0 0F B1 17
        let o = X86_64Opcode::LockCmpxchgRm {
            width: Width::B16,
            reg_idx: 2,
            mem: MemOperand {
                disp: 0,
                base: Some(7),
                index: None,
                seg: None,
            },
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0xF0, 0x0F, 0xB1, 0x17]));
    }

    #[test]
    fn lock_cmpxchg_rdi_rdx_b32() {
        // lock cmpxchg [rdi], edx: F0 0F B1 17
        let o = X86_64Opcode::LockCmpxchgRm {
            width: Width::B32,
            reg_idx: 2,
            mem: MemOperand {
                disp: 0,
                base: Some(7),
                index: None,
                seg: None,
            },
        };
        assert_eq!(o.encode(), Some(vec![0xF0, 0x0F, 0xB1, 0x17]));
    }

    #[test]
    fn lock_cmpxchg_rdi_rdx_b64() {
        // lock cmpxchg [rdi], rdx: F0 48 0F B1 17
        let o = X86_64Opcode::LockCmpxchgRm {
            width: Width::B64,
            reg_idx: 2,
            mem: MemOperand {
                disp: 0,
                base: Some(7),
                index: None,
                seg: None,
            },
        };
        assert_eq!(o.encode(), Some(vec![0xF0, 0x48, 0x0F, 0xB1, 0x17]));
    }

    #[test]
    fn lock_cmpxchg_rdi_rsi_b8() {
        // lock cmpxchg [rdi], sil: F0 40 0F B0 37
        let o = X86_64Opcode::LockCmpxchgRm {
            width: Width::B8,
            reg_idx: 6,
            mem: MemOperand {
                disp: 0,
                base: Some(7),
                index: None,
                seg: None,
            },
        };
        assert_eq!(o.encode(), Some(vec![0xF0, 0x40, 0x0F, 0xB0, 0x37]));
    }

    #[test]
    fn lock_cmpxchg_rdi_rsi_b64() {
        // lock cmpxchg [rdi], rsi: F0 48 0F B1 37
        let o = X86_64Opcode::LockCmpxchgRm {
            width: Width::B64,
            reg_idx: 6,
            mem: MemOperand {
                disp: 0,
                base: Some(7),
                index: None,
                seg: None,
            },
        };
        assert_eq!(o.encode(), Some(vec![0xF0, 0x48, 0x0F, 0xB1, 0x37]));
    }

    #[test]
    fn lock_xchg_rdi_rsi_b8() {
        // lock xchg [rdi], sil: F0 40 86 37
        let o = X86_64Opcode::LockXchgRm {
            width: Width::B8,
            reg_idx: 6,
            mem: MemOperand {
                disp: 0,
                base: Some(7),
                index: None,
                seg: None,
            },
        };
        assert_eq!(o.encode(), Some(vec![0xF0, 0x40, 0x86, 0x37]));
    }

    #[test]
    fn lock_xchg_rdi_rsi_b16() {
        // lock xchg [rdi], si: 66 F0 87 37
        let o = X86_64Opcode::LockXchgRm {
            width: Width::B16,
            reg_idx: 6,
            mem: MemOperand {
                disp: 0,
                base: Some(7),
                index: None,
                seg: None,
            },
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0xF0, 0x87, 0x37]));
    }

    #[test]
    fn lock_xchg_rdi_rsi_b32() {
        // lock xchg [rdi], esi: F0 87 37
        let o = X86_64Opcode::LockXchgRm {
            width: Width::B32,
            reg_idx: 6,
            mem: MemOperand {
                disp: 0,
                base: Some(7),
                index: None,
                seg: None,
            },
        };
        assert_eq!(o.encode(), Some(vec![0xF0, 0x87, 0x37]));
    }

    #[test]
    fn lock_xchg_rdi_rsi_b64() {
        // lock xchg [rdi], rsi: F0 48 87 37
        let o = X86_64Opcode::LockXchgRm {
            width: Width::B64,
            reg_idx: 6,
            mem: MemOperand {
                disp: 0,
                base: Some(7),
                index: None,
                seg: None,
            },
        };
        assert_eq!(o.encode(), Some(vec![0xF0, 0x48, 0x87, 0x37]));
    }

    #[test]
    fn lock_xadd_rdi_rsi_b8() {
        // lock xadd [rdi], sil: F0 40 0F C0 37
        let o = X86_64Opcode::LockXaddRm {
            width: Width::B8,
            reg_idx: 6,
            mem: MemOperand {
                disp: 0,
                base: Some(7),
                index: None,
                seg: None,
            },
        };
        assert_eq!(o.encode(), Some(vec![0xF0, 0x40, 0x0F, 0xC0, 0x37]));
    }

    #[test]
    fn lock_xadd_rdi_rsi_b16() {
        // lock xadd [rdi], si: 66 F0 0F C1 37
        let o = X86_64Opcode::LockXaddRm {
            width: Width::B16,
            reg_idx: 6,
            mem: MemOperand {
                disp: 0,
                base: Some(7),
                index: None,
                seg: None,
            },
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0xF0, 0x0F, 0xC1, 0x37]));
    }

    #[test]
    fn lock_xadd_rdi_rsi_b32() {
        // lock xadd [rdi], esi: F0 0F C1 37
        let o = X86_64Opcode::LockXaddRm {
            width: Width::B32,
            reg_idx: 6,
            mem: MemOperand {
                disp: 0,
                base: Some(7),
                index: None,
                seg: None,
            },
        };
        assert_eq!(o.encode(), Some(vec![0xF0, 0x0F, 0xC1, 0x37]));
    }

    #[test]
    fn lock_xadd_rdi_rsi_b64() {
        // lock xadd [rdi], rsi: F0 48 0F C1 37
        let o = X86_64Opcode::LockXaddRm {
            width: Width::B64,
            reg_idx: 6,
            mem: MemOperand {
                disp: 0,
                base: Some(7),
                index: None,
                seg: None,
            },
        };
        assert_eq!(o.encode(), Some(vec![0xF0, 0x48, 0x0F, 0xC1, 0x37]));
    }

    #[test]
    fn x87_fnstenv_rdi() {
        // fnstenv [rdi]: D9 37
        let o = X86_64Opcode::X87 {
            op: X87Op::EnvMem { store: true },
            mem: Some(MemOperand {
                disp: 0,
                base: Some(7),
                index: None,
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0xD9, 0x37]));
    }

    #[test]
    fn x87_fldenv_rdi() {
        // fldenv [rdi]: D9 27
        let o = X86_64Opcode::X87 {
            op: X87Op::EnvMem { store: false },
            mem: Some(MemOperand {
                disp: 0,
                base: Some(7),
                index: None,
                seg: None,
            }),
        };
        assert_eq!(o.encode(), Some(vec![0xD9, 0x27]));
    }

    // ---- P11 Bucket AH: xchg reg, reg ----

    #[test]
    fn xchg_rcx_rax_b64_modrm() {
        // xchg rcx, rax (ModRM form): 48 87 C8
        let o = X86_64Opcode::XchgRm {
            width: Width::B64,
            dst: reg(1, Width::B64),
            src: reg(0, Width::B64),
            prefer_shortcut: false,
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x87, 0xC8]));
    }

    #[test]
    fn xchg_rax_rcx_b64_modrm() {
        // xchg rax, rcx (ModRM form, prefer_shortcut=false): 48 87 C1
        // Matches handwritten form in int_ops.rs.
        let o = X86_64Opcode::XchgRm {
            width: Width::B64,
            dst: reg(0, Width::B64),
            src: reg(1, Width::B64),
            prefer_shortcut: false,
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x87, 0xC1]));
    }

    #[test]
    fn xchg_rax_rcx_b64_shortcut() {
        // xchg rax, rcx (accumulator-shortcut form, vector.rs): 48 91
        let o = X86_64Opcode::XchgRm {
            width: Width::B64,
            dst: reg(0, Width::B64),
            src: reg(1, Width::B64),
            prefer_shortcut: true,
        };
        assert_eq!(o.encode(), Some(vec![0x48, 0x91]));
    }

    #[test]
    fn xchg_rax_rcx_b32_no_rex() {
        // xchg eax, ecx (B32, accumulator-shortcut): 91
        let o = X86_64Opcode::XchgRm {
            width: Width::B32,
            dst: reg(0, Width::B32),
            src: reg(1, Width::B32),
            prefer_shortcut: true,
        };
        assert_eq!(o.encode(), Some(vec![0x91]));
    }

    #[test]
    fn xchg_rax_rcx_b16_shortcut() {
        // xchg ax, cx (B16, accumulator-shortcut): 66 91
        let o = X86_64Opcode::XchgRm {
            width: Width::B16,
            dst: reg(0, Width::B16),
            src: reg(1, Width::B16),
            prefer_shortcut: true,
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x91]));
    }

    #[test]
    fn xchg_dl_sil_b8_modrm() {
        // xchg dl, sil (B8, ModRM form with REX=0x40 for sil): 40 86 D6
        let o = X86_64Opcode::XchgRm {
            width: Width::B8,
            dst: reg(2, Width::B8),
            src: reg(6, Width::B8),
            prefer_shortcut: false,
        };
        assert_eq!(o.encode(), Some(vec![0x40, 0x86, 0xD6]));
    }

    #[test]
    fn xchg_rdx_rsi_b16_modrm() {
        // xchg dx, si (B16, ModRM): 66 87 D6
        let o = X86_64Opcode::XchgRm {
            width: Width::B16,
            dst: reg(2, Width::B16),
            src: reg(6, Width::B16),
            prefer_shortcut: false,
        };
        assert_eq!(o.encode(), Some(vec![0x66, 0x87, 0xD6]));
    }
}
