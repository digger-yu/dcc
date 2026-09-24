//! ARM Thumb-2 instruction encoding framework.
//!
//! This module provides the foundation for encoding ARM Thumb-2
//! instructions for Cortex-M embedded targets. Thumb-2 is a
//! mixed 16/32-bit instruction set that provides code density
//! close to Thumb-1 with performance approaching ARM.

/// Thumb-2 instruction encoding result
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbInstruction {
    /// 16-bit Thumb instruction
    T16(u16),
    /// 32-bit Thumb-2 instruction
    T32(u32),
}

impl ThumbInstruction {
    /// Get the encoded bytes (little-endian)
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            ThumbInstruction::T16(encoding) => {
                vec![(encoding & 0xFF) as u8, (encoding >> 8) as u8]
            },
            ThumbInstruction::T32(encoding) => {
                let hw1 = ((encoding >> 16) & 0xFFFF) as u16;
                let hw2 = (encoding & 0xFFFF) as u16;
                vec![
                    (hw1 & 0xFF) as u8,
                    (hw1 >> 8) as u8,
                    (hw2 & 0xFF) as u8,
                    (hw2 >> 8) as u8,
                ]
            },
        }
    }

    /// Get the instruction size in bytes
    pub fn size(&self) -> usize {
        match self {
            ThumbInstruction::T16(_) => 2,
            ThumbInstruction::T32(_) => 4,
        }
    }
}

/// Thumb-2 instruction encoder
pub struct ThumbEncoder;

impl ThumbEncoder {
    /// Encode a NOP instruction (16-bit)
    pub fn nop() -> ThumbInstruction {
        ThumbInstruction::T16(0xBF00)
    }

    /// Encode a BX LR instruction (16-bit)
    /// Returns from function by branching to link register
    pub fn bx_lr() -> ThumbInstruction {
        ThumbInstruction::T16(0x4770)
    }

    /// Encode a MOV Rd, #imm8 instruction (16-bit)
    /// Move immediate value to register (0-255)
    pub fn mov_imm8(rd: u8, imm8: u8) -> ThumbInstruction {
        assert!(rd < 8, "rd must be 0-7 for 16-bit MOV");
        let encoding = 0x2000u16 | ((rd as u16) << 8) | (imm8 as u16);
        ThumbInstruction::T16(encoding)
    }

    /// Encode a MOVW Rd, #imm16 instruction (32-bit)
    /// Move 16-bit immediate to register
    pub fn movw_imm16(rd: u8, imm16: u16) -> ThumbInstruction {
        assert!(rd < 16, "rd must be 0-15 for MOVW");
        let imm4 = (imm16 >> 12) & 0xF;
        let i = (imm16 >> 11) & 0x1;
        let imm3 = (imm16 >> 8) & 0x7;
        let imm8 = imm16 & 0xFF;

        let hw1 = 0xF240u16 | (i << 10) | imm4;
        let hw2 = ((rd as u16) << 8) | (imm3 << 12) | imm8;
        ThumbInstruction::T32(((hw1 as u32) << 16) | (hw2 as u32))
    }

    /// Encode a MOVT Rd, #imm16 instruction (32-bit)
    /// Move immediate to top halfword of register
    pub fn movt_imm16(rd: u8, imm16: u16) -> ThumbInstruction {
        assert!(rd < 16, "rd must be 0-15 for MOVT");
        let imm4 = (imm16 >> 12) & 0xF;
        let i = (imm16 >> 11) & 0x1;
        let imm3 = (imm16 >> 8) & 0x7;
        let imm8 = imm16 & 0xFF;

        let hw1 = 0xF2C0u16 | (i << 10) | imm4;
        let hw2 = ((rd as u16) << 8) | (imm3 << 12) | imm8;
        ThumbInstruction::T32(((hw1 as u32) << 16) | (hw2 as u32))
    }

    /// Encode a PUSH instruction (16-bit)
    /// Push registers onto stack
    pub fn push(reg_list: u8, push_lr: bool) -> ThumbInstruction {
        let lr_bit = if push_lr { 0x100u16 } else { 0 };
        let encoding = 0xB400u16 | lr_bit | (reg_list as u16);
        ThumbInstruction::T16(encoding)
    }

    /// Encode a POP instruction (16-bit)
    /// Pop registers from stack
    pub fn pop(reg_list: u8, pop_pc: bool) -> ThumbInstruction {
        let pc_bit = if pop_pc { 0x100u16 } else { 0 };
        let encoding = 0xBC00u16 | pc_bit | (reg_list as u16);
        ThumbInstruction::T16(encoding)
    }

    /// Encode a B (branch) instruction (16-bit, short)
    /// Branch with signed 8-bit offset (-256 to +254 bytes)
    pub fn b_short(offset: i32) -> ThumbInstruction {
        assert!((-256..=254).contains(&offset), "offset out of range for 16-bit branch");
        assert!(offset % 2 == 0, "offset must be even (halfword aligned)");
        let imm8 = ((offset >> 1) & 0xFF) as u16;
        let encoding = 0xE000u16 | imm8;
        ThumbInstruction::T16(encoding)
    }

    /// Encode a BL (branch with link) instruction (32-bit)
    /// Branch with link for function calls
    pub fn bl(offset: i32) -> ThumbInstruction {
        assert!(offset % 2 == 0, "offset must be even");
        let imm11 = ((offset >> 1) & 0x7FF) as u32;
        let imm10 = ((offset >> 12) & 0x3FF) as u32;
        let s = if offset < 0 { 1u32 } else { 0 };

        let hw1 = 0xF000u16 | (s << 10) as u16 | imm10 as u16;
        let hw2 = 0xF800u16 | imm11 as u16;
        ThumbInstruction::T32(((hw1 as u32) << 16) | (hw2 as u32))
    }

    /// Encode an LDR (literal) instruction (16-bit)
    /// Load from PC-relative address
    pub fn ldr_literal(rt: u8, imm8: u8) -> ThumbInstruction {
        assert!(rt < 8, "rt must be 0-7 for 16-bit LDR literal");
        let encoding = 0x4800u16 | ((rt as u16) << 8) | (imm8 as u16);
        ThumbInstruction::T16(encoding)
    }

    /// Encode a STR instruction (16-bit, register offset)
    /// Store register to memory
    pub fn str_reg(rt: u8, rn: u8, rm: u8) -> ThumbInstruction {
        assert!(rt < 8 && rn < 8 && rm < 8, "registers must be 0-7");
        let encoding = 0x5000u16 | ((rm as u16) << 6) | ((rn as u16) << 3) | (rt as u16);
        ThumbInstruction::T16(encoding)
    }

    /// Encode a LDR instruction (16-bit, register offset)
    /// Load register from memory
    pub fn ldr_reg(rt: u8, rn: u8, rm: u8) -> ThumbInstruction {
        assert!(rt < 8 && rn < 8 && rm < 8, "registers must be 0-7");
        let encoding = 0x5800u16 | ((rm as u16) << 6) | ((rn as u16) << 3) | (rt as u16);
        ThumbInstruction::T16(encoding)
    }

    /// Encode an ADD (register) instruction (16-bit)
    pub fn add_reg(rd: u8, rn: u8, rm: u8) -> ThumbInstruction {
        assert!(rd < 8 && rn < 8 && rm < 8, "registers must be 0-7");
        let encoding = 0x1800u16 | ((rm as u16) << 6) | ((rn as u16) << 3) | (rd as u16);
        ThumbInstruction::T16(encoding)
    }

    /// Encode a SUB (register) instruction (16-bit)
    pub fn sub_reg(rd: u8, rn: u8, rm: u8) -> ThumbInstruction {
        assert!(rd < 8 && rn < 8 && rm < 8, "registers must be 0-7");
        let encoding = 0x1A00u16 | ((rm as u16) << 6) | ((rn as u16) << 3) | (rd as u16);
        ThumbInstruction::T16(encoding)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn t16_size_and_bytes() {
        let inst = ThumbEncoder::nop();
        assert_eq!(inst.size(), 2);
        assert_eq!(inst.to_bytes(), vec![0x00, 0xBF]);
    }

    #[test]
    fn t32_size_and_bytes() {
        let inst = ThumbEncoder::bl(0x1000);
        assert_eq!(inst.size(), 4);
        let bytes = inst.to_bytes();
        assert_eq!(bytes.len(), 4);
    }

    #[test]
    fn bx_lr_encoding() {
        let inst = ThumbEncoder::bx_lr();
        assert_eq!(inst.to_bytes(), vec![0x70, 0x47]);
    }

    #[test]
    fn mov_imm8_encoding() {
        let inst = ThumbEncoder::mov_imm8(0, 42);
        // MOV R0, #42 = 0x202A
        assert_eq!(inst.to_bytes(), vec![0x2A, 0x20]);
    }

    #[test]
    fn push_pop_encoding() {
        let push = ThumbEncoder::push(0x0F, true); // push r0-r3, lr
        assert_eq!(push.to_bytes(), vec![0x0F, 0xB5]);

        let pop = ThumbEncoder::pop(0x0F, false); // pop r0-r3
        assert_eq!(pop.to_bytes(), vec![0x0F, 0xBC]);
    }
}
