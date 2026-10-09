// src/codegen/x86_64/insn.rs
//
// x86_64 后端 - 指令记录模型 (统一发射入口).
//
// 第一性原理: x86_64 指令**变长** (1~15 字节, 含 REX / ModRM / SIB / disp / imm / 前缀),
// 不能像定长 ISA (aarch64/mips/arm/riscv32) 那样用 `u32` 字面值装. 因此本模型用
// `RawBytes(Vec<u8>)` 承接迁移期所有 `self.emit(&[..])` 调用点; 后续再逐步把常用
// 助记符抽为类型化构造子 (P9 后续).
//
// 迁移纪律 (G11): 纯移动, 构造子编码 == 现有字节. 不变更任何位型.

use crate::codegen::shared::asm_operand::{InsnRec, Operand};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum X86_64Opcode {
    /// 空操作 (1 字节 0x90).
    Nop = 0,
    /// 原始字节透传 (迁移期兜底; 承载变长 x86_64 指令).
    RawBytes,
}

#[derive(Clone, Debug)]
pub struct Insn {
    pub op: X86_64Opcode,
    pub ops: [Operand; 4],
    /// RawBytes 字节序 (小端; x86_64 本机即小端, 直接 `Vec<u8>` 原样落盘).
    pub bytes: Vec<u8>,
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
        }
    }

    /// 直接用字节切片构造 (迁移期兜底, 逐步替换为类型化构造子).
    /// x86_64 变长, 因此直接保留字节切片, 不做 u32/u16 拼装.
    pub fn raw_bytes(b: &[u8]) -> Self {
        let mut insn = Insn::new(X86_64Opcode::RawBytes, &[]);
        insn.bytes = b.to_vec();
        insn
    }

    pub fn nop() -> Self {
        Insn::new(X86_64Opcode::Nop, &[])
    }

    pub fn encode_bytes(&self) -> Option<Vec<u8>> {
        match self.op {
            X86_64Opcode::Nop => Some(vec![0x90]),
            X86_64Opcode::RawBytes => Some(self.bytes.clone()),
        }
    }

    pub fn to_rec(&self, pc: u64) -> InsnRec {
        let opcode_id = self.op as u16;
        let mut rec = InsnRec::new(opcode_id, &self.ops[..]);
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
