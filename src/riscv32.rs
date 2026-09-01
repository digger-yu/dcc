//! RISC-V 32-bit instruction encoding framework.
//!
//! This module provides the foundation for encoding RISC-V RV32IM
//! instructions for bare-metal embedded targets.

/// RISC-V instruction encoding result (always 32-bit for base ISA)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RvInstruction(pub u32);

impl RvInstruction {
    /// Get the encoded bytes (little-endian)
    pub fn to_bytes(&self) -> Vec<u8> {
        let e = self.0;
        vec![
            (e & 0xFF) as u8,
            ((e >> 8) & 0xFF) as u8,
            ((e >> 16) & 0xFF) as u8,
            ((e >> 24) & 0xFF) as u8,
        ]
    }

    /// Get the instruction size in bytes
    pub fn size(&self) -> usize {
        4
    }
}

/// RISC-V opcodes
pub mod opcode {
    pub const LUI: u32 = 0x37;
    pub const AUIPC: u32 = 0x17;
    pub const JAL: u32 = 0x6F;
    pub const JALR: u32 = 0x67;
    pub const BRANCH: u32 = 0x63;
    pub const LOAD: u32 = 0x03;
    pub const STORE: u32 = 0x23;
    pub const OP_IMM: u32 = 0x13;
    pub const OP: u32 = 0x33;
    pub const SYSTEM: u32 = 0x73;
}

/// RISC-V instruction encoder
pub struct RvEncoder;

impl RvEncoder {
    /// Encode NOP (ADDI x0, x0, 0)
    pub fn nop() -> RvInstruction {
        RvInstruction(0x00000013)
    }

    /// Encode RET (JALR x0, x1, 0)
    pub fn ret() -> RvInstruction {
        RvInstruction(0x00008067)
    }

    /// Encode LUI rd, imm20
    pub fn lui(rd: u8, imm20: u32) -> RvInstruction {
        assert!(rd < 32);
        RvInstruction((imm20 & 0xFFFFF000) | ((rd as u32) << 7) | opcode::LUI)
    }

    /// Encode AUIPC rd, imm20
    pub fn auipc(rd: u8, imm20: u32) -> RvInstruction {
        assert!(rd < 32);
        RvInstruction((imm20 & 0xFFFFF000) | ((rd as u32) << 7) | opcode::AUIPC)
    }

    /// Encode JAL rd, offset
    pub fn jal(rd: u8, offset: i32) -> RvInstruction {
        assert!(rd < 32);
        let imm = offset as u32;
        let bit20 = (imm >> 20) & 1;
        let bits10_1 = (imm >> 1) & 0x3FF;
        let bit11 = (imm >> 11) & 1;
        let bits19_12 = (imm >> 12) & 0xFF;

        let encoding = (bit20 << 31)
            | (bits10_1 << 21)
            | (bit11 << 20)
            | (bits19_12 << 12)
            | ((rd as u32) << 7)
            | opcode::JAL;
        RvInstruction(encoding)
    }

    /// Encode JALR rd, rs1, imm12
    pub fn jalr(rd: u8, rs1: u8, imm12: i32) -> RvInstruction {
        assert!(rd < 32 && rs1 < 32);
        let imm = (imm12 as u32) & 0xFFF;
        RvInstruction((imm << 20) | ((rs1 as u32) << 15) | ((rd as u32) << 7) | opcode::JALR)
    }

    /// Encode ADDI rd, rs1, imm12
    pub fn addi(rd: u8, rs1: u8, imm12: i32) -> RvInstruction {
        assert!(rd < 32 && rs1 < 32);
        let imm = (imm12 as u32) & 0xFFF;
        RvInstruction((imm << 20) | ((rs1 as u32) << 15) | (0 << 12) | ((rd as u32) << 7) | opcode::OP_IMM)
    }

    /// Encode ADD rd, rs1, rs2
    pub fn add(rd: u8, rs1: u8, rs2: u8) -> RvInstruction {
        assert!(rd < 32 && rs1 < 32 && rs2 < 32);
        RvInstruction(((rs2 as u32) << 20) | ((rs1 as u32) << 15) | (0 << 12) | ((rd as u32) << 7) | opcode::OP)
    }

    /// Encode SUB rd, rs1, rs2
    pub fn sub(rd: u8, rs1: u8, rs2: u8) -> RvInstruction {
        assert!(rd < 32 && rs1 < 32 && rs2 < 32);
        RvInstruction((0x20 << 25) | ((rs2 as u32) << 20) | ((rs1 as u32) << 15) | (0 << 12) | ((rd as u32) << 7) | opcode::OP)
    }

    /// Encode MUL rd, rs1, rs2 (M extension)
    pub fn mul(rd: u8, rs1: u8, rs2: u8) -> RvInstruction {
        assert!(rd < 32 && rs1 < 32 && rs2 < 32);
        RvInstruction((0x02 << 25) | ((rs2 as u32) << 20) | ((rs1 as u32) << 15) | (0 << 12) | ((rd as u32) << 7) | opcode::OP)
    }

    /// Encode DIV rd, rs1, rs2 (M extension)
    pub fn div(rd: u8, rs1: u8, rs2: u8) -> RvInstruction {
        assert!(rd < 32 && rs1 < 32 && rs2 < 32);
        RvInstruction((0x02 << 25) | ((rs2 as u32) << 20) | ((rs1 as u32) << 15) | (4 << 12) | ((rd as u32) << 7) | opcode::OP)
    }

    /// Encode LW rd, rs1, offset
    pub fn lw(rd: u8, rs1: u8, offset: i32) -> RvInstruction {
        assert!(rd < 32 && rs1 < 32);
        let imm = (offset as u32) & 0xFFF;
        RvInstruction((imm << 20) | ((rs1 as u32) << 15) | (2 << 12) | ((rd as u32) << 7) | opcode::LOAD)
    }

    /// Encode SW rs2, rs1, offset
    pub fn sw(rs2: u8, rs1: u8, offset: i32) -> RvInstruction {
        assert!(rs1 < 32 && rs2 < 32);
        let imm = (offset as u32) & 0xFFF;
        let imm11_5 = (imm >> 5) & 0x7F;
        let imm4_0 = imm & 0x1F;
        RvInstruction((imm11_5 << 25) | ((rs2 as u32) << 20) | ((rs1 as u32) << 15) | (2 << 12) | (imm4_0 << 7) | opcode::STORE)
    }

    /// Encode BEQ rs1, rs2, offset
    pub fn beq(rs1: u8, rs2: u8, offset: i32) -> RvInstruction {
        assert!(rs1 < 32 && rs2 < 32);
        let imm = (offset as u32) & 0x1FFE;
        let bit12 = (imm >> 12) & 1;
        let bits10_5 = (imm >> 5) & 0x3F;
        let bits4_1 = (imm >> 1) & 0xF;
        let bit11 = (imm >> 11) & 1;

        let encoding = (bit12 << 31)
            | (bits10_5 << 25)
            | ((rs2 as u32) << 20)
            | ((rs1 as u32) << 15)
            | (0 << 12)
            | (bits4_1 << 8)
            | (bit11 << 7)
            | opcode::BRANCH;
        RvInstruction(encoding)
    }

    /// Encode ECALL
    pub fn ecall() -> RvInstruction {
        RvInstruction(0x00000073)
    }

    /// Encode EBREAK
    pub fn ebreak() -> RvInstruction {
        RvInstruction(0x00100073)
    }
}

/// RISC-V compressed (C extension) instruction encoding
pub mod compressed {
    use super::RvInstruction;

    /// Encode C.NOP (16-bit)
    pub fn c_nop() -> u16 {
        0x0001
    }

    /// Encode C.LI rd, imm (16-bit)
    pub fn c_li(rd: u8, imm: i8) -> u16 {
        assert!(rd < 32);
        let imm6 = (imm as u8) & 0x7F;
        let bit5 = ((imm6 >> 5) & 1) as u16;
        let bits4_0 = (imm6 & 0x1F) as u16;
        0x4001u16 | (bit5 << 12) | ((rd as u16) << 7) | bits4_0
    }

    /// Encode C.J offset (16-bit)
    pub fn c_j(offset: i32) -> u16 {
        let imm = offset as u16;
        let bit11 = ((imm >> 11) & 1) as u16;
        let bit4 = ((imm >> 4) & 1) as u16;
        let bit9_8 = ((imm >> 8) & 3) as u16;
        let bit10 = ((imm >> 10) & 1) as u16;
        let bit6 = ((imm >> 6) & 1) as u16;
        let bit7 = ((imm >> 7) & 1) as u16;
        let bit3_1 = ((imm >> 1) & 7) as u16;
        let bit5 = ((imm >> 5) & 1) as u16;

        0xA001u16
            | (bit11 << 12)
            | (bit4 << 11)
            | (bit9_8 << 9)
            | (bit10 << 8)
            | (bit6 << 7)
            | (bit7 << 6)
            | (bit3_1 << 3)
            | (bit5 << 2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nop_encoding() {
        let inst = RvEncoder::nop();
        assert_eq!(inst.0, 0x00000013);
        assert_eq!(inst.to_bytes(), vec![0x13, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn ret_encoding() {
        let inst = RvEncoder::ret();
        assert_eq!(inst.0, 0x00008067);
    }

    #[test]
    fn addi_encoding() {
        let inst = RvEncoder::addi(1, 0, 42); // addi ra, zero, 42
        assert_eq!(inst.size(), 4);
        let bytes = inst.to_bytes();
        assert_eq!(bytes.len(), 4);
    }

    #[test]
    fn lui_encoding() {
        let inst = RvEncoder::lui(1, 0x12345000); // lui ra, 0x12345
        let bytes = inst.to_bytes();
        assert_eq!(bytes.len(), 4);
    }

    #[test]
    fn mul_encoding() {
        let inst = RvEncoder::mul(1, 2, 3); // mul ra, s0, s1
        assert_eq!(inst.size(), 4);
    }
}
