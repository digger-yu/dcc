// src/codegen/aarch64/insn.rs
//
// AArch64 指令枚举与类型化构造子。
//
// 第一性原理: 每一个 self.emit(&[u8;4]) 调用点都应该被替换为
// `self.emit_insn(Insn::Xxx(...))`。构造子本身负责按 AArch64 手册把操作数
// 打包成 u32 机器字, 再交给共享引擎装填字节。
//
// 迁移纪律 (G11): 现阶段纯移动, 即 "构造子编码 == 现有 self.emit(...) 字节"。
// 发现 "意图 ⊻ 现字节" 只登记不修, 由 bug-fix 阶段单独处理。

use crate::codegen::shared::asm_operand::{InsnRec, Operand, Width};

/// AArch64 操作码枚举。只覆盖 dcc 后端实际会发射的指令 (不是全 ISA)。
/// 未知 mnemonic 的 inline asm 行已经走 asm_source 透传路径, 不在此枚举内。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum Aarch64Opcode {
    Nop = 0,
    Ret,
    Br,
    Blr,
    B,
    Bl,
    BCond,
    Cbz,
    Cbnz,
    Svc,
    Dmb,
    Dsb,
    Isb,
    AddImm,
    AddShiftedReg,
    SubImm,
    SubShiftedReg,
    MovImm,   // movz
    MovReg,   // orr rd, xzr, rm
    MvnReg,
    LdrImm,   // ldr rt, [rn, #imm]
    StrImm,
    LdrbImm,
    StrbImm,
    LdrhImm,
    StrhImm,
    Adr,
    Adrp,
    CmpImm,   // subs xzr, rn, #imm
    CmpReg,
    Cset,
    Csel,
    Csinc,
    CcmpImm,
    CcmpReg,
    FaddS,
    FaddD,
    FsubS,
    FsubD,
    FmulS,
    FmulD,
    FdivS,
    FdivD,
    FcmpS,
    FcmpD,
    FcmpSZero,
    FcmpDZero,
    FccmpS,
    FccmpD,
    FnegS,
    FmovS,
    ScvtfS,  // int -> float (signed 32-bit)
    ScvtfD,
    FcvtzsS, // float -> int (signed 32-bit)
    FcvtzsD,
    LdrS,
    StrS,
    LdrD,
    StrD,
    // 向量 / HFA
    StpS,
    LdpS,
    StpD,
    LdpD,
    AddVec4S,
    SubVec4S,
    MulVec4S,
    OrrVec,
    EorVec,
    DupVec,
    MovZeroVec,
    // 整型扩展 / 移位 / 除法
    Sxtb,
    Sxth,
    Uxtb,
    Uxth,
    Lslv,
    Lsrv,
    Asrv,
    Sdiv,
    Msub,
    // 简单算术寄存器 (binop_w0_w1 硬编码)
    AddRegW,
    SubRegW,
    MulRegW,
    AndRegW,
    OrrRegW,
    EorRegW,
    // 64 位 shift 常量 (lsl xN, xN, #sh)
    LslImmX,
    // TLS / special
    Mrs,
    Msr,
    Sev,
    Sevl,
    Wfe,
    Wfi,
    Yield,
    Clrex,
    IcIallu,
    IcIvau,
    Ptrue,
    Whilelt,
    Ld1w,
    St1w,
    Ld1,
    St1,
    // 未分类占位: 迁移期还没改造的发射点
    RawU32,
}

/// 一条 AArch64 指令: opcode + 操作数。`Insn` 本身不携带字节, 由
/// `encode()` 计算成 u32 (AArch64 定长 4 字节, 小端)。
#[derive(Clone, Debug)]
pub struct Insn {
    pub op: Aarch64Opcode,
    pub ops: [Operand; 4],
}

impl Insn {
    /// 构造一条指令 (nops <= 4)。
    pub fn new(op: Aarch64Opcode, ops: &[Operand]) -> Self {
        let mut arr = [Operand::None, Operand::None, Operand::None, Operand::None];
        let n = ops.len().min(4);
        arr[..n].clone_from_slice(&ops[..n]);
        Insn { op, ops: arr }
    }

    fn op(&self, i: usize) -> Operand {
        self.ops[i].clone()
    }

    fn reg_idx(op: &Operand) -> u8 {
        match op {
            Operand::Reg { idx, .. } => *idx,
            _ => 0,
        }
    }

    fn is_64(op: &Operand) -> bool {
        matches!(op, Operand::Reg { width: Width::B64, .. })
    }

    fn imm_val(op: &Operand) -> i64 {
        match op {
            Operand::Imm { value, .. } => *value,
            _ => 0,
        }
    }

    /// 编码为 32-bit 机器字 (小端内存表示)。
    /// 迁移期内: 只覆盖已经有构造子的指令; 未覆盖的返回 None,
    /// 发射点继续走旧 `self.emit(&[u8])` 路径 (纯移动, 不改字节)。
    pub fn encode(&self) -> Option<u32> {
        let sf = if self.op(0) != Operand::None && Self::is_64(&self.op(0)) {
            1u32
        } else {
            0
        };
        let word = match self.op {
            Aarch64Opcode::Nop => 0xd503201f,
            Aarch64Opcode::Ret => 0xd65f03c0, // ret x30
            Aarch64Opcode::Br => 0xd61f0000 | ((Self::reg_idx(&self.op(0)) as u32) << 5),
            Aarch64Opcode::Blr => 0xd63f0000 | ((Self::reg_idx(&self.op(0)) as u32) << 5),
            Aarch64Opcode::MovReg => {
                // orr rd, xzr, rm. 顶部字节已含 sf: 64位=0xaa, 32位=0x2a
                let rd = Self::reg_idx(&self.op(0)) as u32;
                let rm = Self::reg_idx(&self.op(1)) as u32;
                let top = if sf != 0 { 0xaa } else { 0x2a };
                (top << 24) | (rm << 16) | (31 << 5) | rd
            }
            Aarch64Opcode::MovImm => {
                // movz rd, #imm16. 顶部字节已含 sf: 64位=0xd2, 32位=0x52, opc=10
                let rd = Self::reg_idx(&self.op(0)) as u32;
                let imm = Self::imm_val(&self.op(1)) as u32;
                let hw = Self::imm_val(&self.op(2)) as u32 & 3;
                let top = if sf != 0 { 0xd2 } else { 0x52 };
                (top << 24) | (1 << 23) | (hw << 21) | ((imm & 0xffff) << 5) | rd
            }
            Aarch64Opcode::AddImm => {
                // 顶部字节已含 sf: 64位=0x91, 32位=0x11
                let rd = Self::reg_idx(&self.op(0)) as u32;
                let rn = Self::reg_idx(&self.op(1)) as u32;
                let imm = Self::imm_val(&self.op(2)) as u32;
                let top = if sf != 0 { 0x91 } else { 0x11 };
                (top << 24) | ((imm & 0xfff) << 10) | (rn << 5) | rd
            }
            Aarch64Opcode::SubImm => {
                // 顶部字节已含 sf: 64位=0xd1, 32位=0x51
                let rd = Self::reg_idx(&self.op(0)) as u32;
                let rn = Self::reg_idx(&self.op(1)) as u32;
                let imm = Self::imm_val(&self.op(2)) as u32;
                let top = if sf != 0 { 0xd1 } else { 0x51 };
                (top << 24) | ((imm & 0xfff) << 10) | (rn << 5) | rd
            }
            Aarch64Opcode::CmpImm => {
                // subs xzr, rn, #imm. 顶部字节已含 sf: 64位=0xf1, 32位=0x71
                let rn = Self::reg_idx(&self.op(0)) as u32;
                let imm = Self::imm_val(&self.op(1)) as u32;
                let top = if sf != 0 { 0xf1 } else { 0x71 };
                (top << 24) | ((imm & 0xfff) << 10) | (rn << 5) | 31
            }
            Aarch64Opcode::Svc => {
                let imm = Self::imm_val(&self.op(0)) as u32;
                0xd4000001 | ((imm & 0xffff) << 5)
            }
            Aarch64Opcode::Dmb => {
                // GAS 真值: dmb sy = d5033fbf;  dmb ish = d5033bbf
                // 位布局: 0xd5030000 | (CRm=0x3 << 12) | (opt << 8) | 0xbf
                let opt = Self::imm_val(&self.op(0)) as u32;
                0xd5030000 | (0x3 << 12) | ((opt & 0xf) << 8) | 0xbf
            }
            Aarch64Opcode::Dsb => {
                // GAS 真值: dsb sy = d5033f9f;  dsb ish = d5033b9f
                // 位布局: 0xd5030000 | (CRm=0x3 << 12) | (opt << 8) | 0x9f
                let opt = Self::imm_val(&self.op(0)) as u32;
                0xd5030000 | (0x3 << 12) | ((opt & 0xf) << 8) | 0x9f
            }
            Aarch64Opcode::Isb => 0xd5033fdf,
            Aarch64Opcode::Wfe => 0xd503205f,
            Aarch64Opcode::Wfi => 0xd503207f,
            Aarch64Opcode::Yield => 0xd503203f,
            Aarch64Opcode::Sev => 0xd503209f,
            Aarch64Opcode::Sevl => 0xd50320bf,
            Aarch64Opcode::Clrex => 0xd5033f5f,
            // fcmp s/d s0, s1; fcmp s/d s0, #0.0
            Aarch64Opcode::FcmpS => 0x1e202000,
            Aarch64Opcode::FcmpD => 0x1e602000,
            Aarch64Opcode::FcmpSZero => 0x1e280008,
            Aarch64Opcode::FcmpDZero => 0x1e680008,
            Aarch64Opcode::RawU32 => Self::imm_val(&self.op(0)) as u32,
            // 未实现构造子: 返回 None, 由调用方走旧路径
            _ => return None,
        };
        Some(word)
    }

    /// 转换为架构中性的 InsnRec (供共享引擎记录/渲染)。
    pub fn to_rec(&self, pc: u64) -> InsnRec {
        let opcode_id = self.op as u16;
        let mut rec = InsnRec::new(opcode_id, &self.ops[..]);
        rec.pc = pc;
        rec.bytes_len = 4;
        rec
    }
}

// ===== 便捷构造子 =====
impl Insn {
    pub fn nop() -> Self {
        Insn::new(Aarch64Opcode::Nop, &[])
    }
    pub fn ret() -> Self {
        Insn::new(Aarch64Opcode::Ret, &[])
    }
    pub fn br(xn: u8) -> Self {
        Insn::new(Aarch64Opcode::Br, &[Operand::reg64(xn)])
    }
    pub fn blr(xn: u8) -> Self {
        Insn::new(Aarch64Opcode::Blr, &[Operand::reg64(xn)])
    }
    pub fn mov_reg(rd: u8, rm: u8, is64: bool) -> Self {
        let w = if is64 { Width::B64 } else { Width::B32 };
        Insn::new(Aarch64Opcode::MovReg, &[
            Operand::Reg { idx: rd, width: w },
            Operand::Reg { idx: rm, width: w },
        ])
    }
    pub fn mov_imm(rd: u8, imm: i64, is64: bool) -> Self {
        let w = if is64 { Width::B64 } else { Width::B32 };
        Insn::new(Aarch64Opcode::MovImm, &[
            Operand::Reg { idx: rd, width: w },
            Operand::uimm(imm, 16),
            Operand::uimm(0, 2), // hw=0
        ])
    }
    pub fn add_imm(rd: u8, rn: u8, imm: i64, is64: bool) -> Self {
        let w = if is64 { Width::B64 } else { Width::B32 };
        Insn::new(Aarch64Opcode::AddImm, &[
            Operand::Reg { idx: rd, width: w },
            Operand::Reg { idx: rn, width: w },
            Operand::uimm(imm, 12),
        ])
    }
    pub fn sub_imm(rd: u8, rn: u8, imm: i64, is64: bool) -> Self {
        let w = if is64 { Width::B64 } else { Width::B32 };
        Insn::new(Aarch64Opcode::SubImm, &[
            Operand::Reg { idx: rd, width: w },
            Operand::Reg { idx: rn, width: w },
            Operand::uimm(imm, 12),
        ])
    }
    pub fn svc(imm: i64) -> Self {
        Insn::new(Aarch64Opcode::Svc, &[Operand::uimm(imm, 16)])
    }
    /// 直接用 u32 字面值构造指令 (迁移期兜底, 逐步替换为类型化构造子)。
    pub fn raw(word: u32) -> Self {
        Insn::new(Aarch64Opcode::RawU32, &[Operand::uimm(word as i64, 32)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nop_encode() {
        assert_eq!(Insn::nop().encode(), Some(0xd503201f));
    }

    #[test]
    fn mov_reg_64bit_orr_alias() {
        // mov x9, x10 -> GAS: aa0a03e9 (orr x9, xzr, x10)
        let i = Insn::mov_reg(9, 10, true);
        assert_eq!(i.encode(), Some(0xaa0a03e9));
    }

    #[test]
    fn mov_reg_32bit_orr_alias() {
        // mov w12, w0 -> GAS: 2a0003ec (orr w12, wzr, w0)
        let i = Insn::mov_reg(12, 0, false);
        assert_eq!(i.encode(), Some(0x2a0003ec));
    }

    #[test]
    fn add_imm_64() {
        // add x0, x1, #4 -> GAS: 91001020
        let i = Insn::add_imm(0, 1, 4, true);
        assert_eq!(i.encode(), Some(0x91001020));
    }

    #[test]
    fn ret_encode() {
        assert_eq!(Insn::ret().encode(), Some(0xd65f03c0));
    }

    #[test]
    fn raw_passthrough() {
        assert_eq!(Insn::raw(0xd503201f).encode(), Some(0xd503201f));
    }
}
