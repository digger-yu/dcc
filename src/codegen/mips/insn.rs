// src/codegen/mips/insn.rs
//
// MIPS (o32 / n32) 后端 - 指令枚举与类型化构造子 (统一发射入口).
//
// 第一性原理: 每一个 self.code.extend_from_slice(&inst.to_be_bytes()) 调用点
// 都应该被替换为 `self.emit_insn(Insn::raw(...))` 通路.
//
// 迁移纪律 (G11): 纯移动, 构造子编码 == 现有字节. 未覆盖的指令走 RawU32 透传.
//
// MIPS32 定长 4 字节, **大端**.

use crate::codegen::shared::asm_operand::{InsnRec, Operand};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum MipsOpcode {
    Nop = 0,
    Sync,
    /// 原始 u32 透传 (迁移期兜底).
    RawU32,
}

#[derive(Clone, Debug)]
pub struct Insn {
    pub op: MipsOpcode,
    pub ops: [Operand; 4],
}

impl Insn {
    pub fn new(op: MipsOpcode, ops: &[Operand]) -> Self {
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

    pub fn encode_bytes(&self) -> Option<Vec<u8>> {
        match self.op {
            MipsOpcode::Nop => Some(0u32.to_be_bytes().to_vec()), // sll zero, zero, 0
            MipsOpcode::Sync => Some(0x0000000Fu32.to_be_bytes().to_vec()),
            MipsOpcode::RawU32 => {
                let w = Self::imm_val(&self.ops[0]) as u32;
                Some(w.to_be_bytes().to_vec())
            },
        }
    }

    pub fn to_rec(&self, pc: u64) -> InsnRec {
        let opcode_id = self.op as u16;
        let mut rec = InsnRec::new(opcode_id, &self.ops[..]);
        rec.pc = pc;
        rec.bytes_len = 4;
        rec
    }
}

impl Insn {
    pub fn nop() -> Self {
        Insn::new(MipsOpcode::Nop, &[])
    }
    /// 直接用 u32 字面值构造 (迁移期兜底, 逐步替换为类型化构造子).
    pub fn raw(word: u32) -> Self {
        Insn::new(MipsOpcode::RawU32, &[Operand::uimm(word as i64, 32)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_be_passthrough() {
        // MIPS 大端
        assert_eq!(Insn::raw(0x0000000Fu32).encode_bytes(), Some(vec![0x00, 0x00, 0x00, 0x0F]));
    }
    #[test]
    fn nop_be() {
        assert_eq!(Insn::nop().encode_bytes(), Some(vec![0, 0, 0, 0]));
    }
}
