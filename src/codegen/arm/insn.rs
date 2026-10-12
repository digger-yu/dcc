// src/codegen/arm/insn.rs
//
// ARM (AArch32) 后端 - 指令枚举与类型化构造子 (统一发射入口).
//
// 第一性原理: 每一个 self.code.extend_from_slice(&inst.to_le_bytes()) 调用点
// 都应该被替换为 `self.emit_insn(Insn::raw(...))` 通路.
//
// 迁移纪律 (G11): 纯移动, 构造子编码 == 现有字节. 未覆盖的指令走 RawU32 透传.
//
// ARM A32 定长 4 字节, 小端.

use crate::codegen::shared::asm_operand::{InsnRec, Operand};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum ArmOpcode {
    Nop = 0,
    Svc,
    /// 原始 u32 透传 (迁移期兜底)
    RawU32,
}

#[derive(Clone, Debug)]
pub struct Insn {
    pub op: ArmOpcode,
    pub ops: [Operand; 4],
}

impl Insn {
    pub fn new(op: ArmOpcode, ops: &[Operand]) -> Self {
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
            ArmOpcode::Nop => Some(0xE1A00000u32.to_le_bytes().to_vec()), // mov r0, r0
            ArmOpcode::Svc => Some(0xEF000000u32.to_le_bytes().to_vec()), // svc #0
            ArmOpcode::RawU32 => {
                let w = Self::imm_val(&self.ops[0]) as u32;
                Some(w.to_le_bytes().to_vec())
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
        Insn::new(ArmOpcode::Nop, &[])
    }
    /// 直接用 u32 字面值构造 (迁移期兜底, 逐步替换为类型化构造子).
    pub fn raw(word: u32) -> Self {
        Insn::new(ArmOpcode::RawU32, &[Operand::uimm(word as i64, 32)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_passthrough() {
        assert_eq!(
            Insn::raw(0xEB000000).encode_bytes(),
            Some(0xEB000000u32.to_le_bytes().to_vec())
        );
    }
}
