// src/codegen/riscv64/insn.rs
//
// RISC-V 64 指令枚举与类型化构造子 (统一发射入口)。
//
// 第一性原理: 每一个 self.emit(&[u8]) 调用点都应该被替换为
// `self.emit_insn(Insn::raw(word))` 或类型化构造子。构造子负责按 RISC-V 手册
// 把操作数打包成 u32 / u16 机器字, 再交给共享引擎装填字节 (小端)。
//
// 迁移纪律 (G11): 现阶段纯移动, 即 "构造子编码 == 现有 self.emit(...) 字节"。
// 发现 "意图 ⊻ 现字节" 只登记不修, 由后续 bug-fix 阶段单独处理。
//
// RISC-V 变长: 32/64 位指令固定 4 字节; 压缩 (C 扩展) 指令 2 字节.
// 因此同时提供 RawU32 与 RawU16 透传构造子.

use crate::codegen::shared::asm_operand::{InsnRec, Operand};

/// RISC-V 64 操作码枚举 (dcc 后端实际会发射的指令子集)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum Riscv64Opcode {
    Nop = 0,
    Ecall,
    Ebreak,
    Mret,
    Wfi,
    /// 原始 u32 透传 (32 位常规指令)
    RawU32,
    /// 原始 u16 透传 (RVC 压缩指令)
    RawU16,
}

/// 一条 RISC-V 64 指令: opcode + 操作数。
#[derive(Clone, Debug)]
pub struct Insn {
    pub op: Riscv64Opcode,
    pub ops: [Operand; 4],
}

impl Insn {
    pub fn new(op: Riscv64Opcode, ops: &[Operand]) -> Self {
        let mut arr = [Operand::None, Operand::None, Operand::None, Operand::None];
        let n = ops.len().min(4);
        arr[..n].clone_from_slice(&ops[..n]);
        Insn { op, ops: arr }
    }

    fn imm_val(op: &Operand) -> i64 {
        match op {
            Operand::Imm { value, .. } => *value,
            _ => 0,
        }
    }

    /// 编码为字节序列: 32 位指令 4 字节小端; 16 位指令 2 字节小端.
    /// 未覆盖的操作码返回 None, 调用方自行走旧路径.
    pub fn encode_bytes(&self) -> Option<Vec<u8>> {
        match self.op {
            Riscv64Opcode::Nop => Some(0x0000_13u32.to_le_bytes().to_vec()),
            Riscv64Opcode::Ecall => Some(0x0000_73u32.to_le_bytes().to_vec()),
            Riscv64Opcode::Ebreak => Some(0x0010_0073u32.to_le_bytes().to_vec()),
            Riscv64Opcode::Mret => Some(0x3020_0073u32.to_le_bytes().to_vec()),
            Riscv64Opcode::Wfi => Some(0x1050_0073u32.to_le_bytes().to_vec()),
            Riscv64Opcode::RawU32 => {
                let w = Self::imm_val(&self.ops[0]) as u32;
                Some(w.to_le_bytes().to_vec())
            },
            Riscv64Opcode::RawU16 => {
                let w = Self::imm_val(&self.ops[0]) as u16;
                Some(w.to_le_bytes().to_vec())
            },
        }
    }

    pub fn to_rec(&self, pc: u64) -> InsnRec {
        let opcode_id = self.op as u16;
        let mut rec = InsnRec::new(opcode_id, &self.ops[..]);
        rec.pc = pc;
        rec.bytes_len = if matches!(self.op, Riscv64Opcode::RawU16) { 2 } else { 4 };
        rec
    }
}

// ===== 便捷构造子 =====
impl Insn {
    pub fn nop() -> Self {
        Insn::new(Riscv64Opcode::Nop, &[])
    }
    pub fn ecall() -> Self {
        Insn::new(Riscv64Opcode::Ecall, &[])
    }
    /// 直接用 u32 字面值构造 (迁移期兜底, 逐步替换为类型化构造子).
    pub fn raw(word: u32) -> Self {
        Insn::new(Riscv64Opcode::RawU32, &[Operand::uimm(word as i64, 32)])
    }
    /// 直接用 u16 字面值构造 (RVC 压缩指令).
    pub fn raw16(word: u16) -> Self {
        Insn::new(Riscv64Opcode::RawU16, &[Operand::uimm(word as i64, 16)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nop_encode() {
        assert_eq!(Insn::nop().encode_bytes(), Some(vec![0x13, 0x00, 0x00, 0x00]));
    }

    #[test]
    fn ecall_encode() {
        assert_eq!(Insn::ecall().encode_bytes(), Some(vec![0x73, 0x00, 0x00, 0x00]));
    }

    #[test]
    fn raw32_passthrough() {
        assert_eq!(
            Insn::raw(0xC010F573).encode_bytes(),
            Some(0xC010F573u32.to_le_bytes().to_vec())
        );
    }

    #[test]
    fn raw16_passthrough() {
        assert_eq!(Insn::raw16(0x0001).encode_bytes(), Some(vec![0x01, 0x00]));
    }
}
