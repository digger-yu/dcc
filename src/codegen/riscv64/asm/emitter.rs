// src/codegen/riscv64/asm/emitter.rs - RISC-V 指令发射器
// Phase B Day 3 - Extracted from riscv64.rs.bak + mini_asm (2026/9/22)
//
// Source locations in riscv64.rs.bak:
// - encode_riscv64() and related functions are in mini_asm.rs (~line 598-777)
// - This module provides a high-level API for building instructions programmatically

use std::collections::HashMap;

// RISC-V specific types
type u12 = u16;  // 12-bit unsigned immediate
type i12 = i16;  // 12-bit signed immediate

/// RISC-V instruction format types
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InstFormat {
    R,      // R-type: rd, rs1, rs2, funct3, funct7
    I,      // I-type: rd, rs1, imm12, funct3
    S,      // S-type: rs1, rs2, imm[4:0], imm[11:5], funct3
    B,      // B-type: rs1, rs2, imm[12|8|10:1|6:7|5], funct3
    U,      // U-type: rd, imm[31:12], funct0
    J,      // J-type: rd, imm[20|10:1|11|30:21|19], funct0
    Auipc,  // AUIPC: rd, imm[31:12]
    Lui,    // LUI: rd, imm[31:12]
}

/// Operand type for constraint validation
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OperandType {
    Register,
    Memory,
    Immediate,
    FloatRegister,
}

/// Constraint representation for inline assembly
#[derive(Debug, Clone)]
pub struct Constraint {
    pub typ: OperandType,
    pub rw: ConstraintRW,
    pub reg_class: Option<String>,
    pub modifiers: Vec<char>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConstraintRW {
    Read,
    Write,
    ReadWrite,
}

impl Default for Constraint {
    fn default() -> Self {
        Self {
            typ: OperandType::Register,
            rw: ConstraintRW::ReadWrite,
            reg_class: Some("r".to_string()),
            modifiers: Vec::new(),
        }
    }
}

impl Constraint {
    /// Parse constraint string like "=r", "+m", "&f"
    pub fn parse(constraint: &str) -> Option<Self> {
        let s = constraint.trim();
        if s.is_empty() {
            return None;
        }

        let mut modifiers = Vec::new();
        let mut rw = ConstraintRW::ReadWrite;
        let mut start_idx = 0;

        // Check for write-only prefix
        if s.starts_with('=') {
            rw = ConstraintRW::Write;
            start_idx = 1;
        } else if s.starts_with('+') {
            rw = ConstraintRW::ReadWrite;
            start_idx = 1;
        } else if s.starts_with('&') {
            // Early clobber - will be handled separately
            modifiers.push('&');
            start_idx = 1;
        }

        let main = s[start_idx..].chars().next()?;
        
        let (typ, reg_class) = match main {
            'r' => (OperandType::Register, Some("r".to_string())),
            'm' => (OperandType::Memory, None),
            'i' => (OperandType::Immediate, None),
            'n' => (OperandType::Immediate, None),
            'f' => (OperandType::FloatRegister, Some("f".to_string())),
            'x' => (OperandType::Register, Some("x".to_string())), // any register
            _ => return None,
        };

        Some(Self {
            typ,
            rw,
            reg_class,
            modifiers,
        })
    }

    /// Check if constraint matches expected operand type
    pub fn matches(&self, expected: OperandType) -> bool {
        match (self.typ, expected) {
            (OperandType::Register, OperandType::Register) => true,
            (OperandType::Memory, OperandType::Memory) => true,
            (OperandType::Immediate, OperandType::Immediate) |
            (OperandType::Immediate, OperandType::Register) => true, // i/n can accept regs
            (OperandType::FloatRegister, OperandType::FloatRegister) => true,
            _ => false,
        }
    }
}

/// RISC-V register encoder
pub struct RegEncoder;

impl RegEncoder {
    /// Encode general-purpose register number to instruction bits
    pub fn encode_reg(reg: u8) -> Option<u8> {
        if reg < 32 {
            Some(reg)
        } else {
            None
        }
    }

    /// Decode register name to number
    pub fn decode_reg(name: &str) -> Option<u8> {
        let name = name.trim().to_lowercase();
        match name.as_str() {
            "zero" | "x0" => Some(0),
            "ra" | "x1" => Some(1),
            "sp" | "x2" => Some(2),
            "gp" | "x3" => Some(3),
            "tp" | "x4" => Some(4),
            "t0" | "x5" => Some(5),
            "t1" | "x6" => Some(6),
            "t2" | "x7" => Some(7),
            "s0" | "fp" | "x8" => Some(8),
            "s1" | "x9" => Some(9),
            "a0" | "x10" => Some(10),
            "a1" | "x11" => Some(11),
            "a2" | "x12" => Some(12),
            "a3" | "x13" => Some(13),
            "a4" | "x14" => Some(14),
            "a5" | "x15" => Some(15),
            "a6" | "x16" => Some(16),
            "a7" | "x17" => Some(17),
            "s2" | "x18" => Some(18),
            "s3" | "x19" => Some(19),
            "s4" | "x20" => Some(20),
            "s5" | "x21" => Some(21),
            "s6" | "x22" => Some(22),
            "s7" | "x23" => Some(23),
            "s8" | "x24" => Some(24),
            "s9" | "x25" => Some(25),
            "s10" | "x26" => Some(26),
            "s11" | "x27" => Some(27),
            "t3" | "x28" => Some(28),
            "t4" | "x29" => Some(29),
            "t5" | "x30" => Some(30),
            "t6" | "x31" => Some(31),
            _ => name.strip_prefix('x').and_then(|r| r.parse::<u8>().ok()).filter(|&r| r < 32),
        }
    }
}

/// RISC-V immediate value encoder
pub struct ImmEncoder;

impl ImmEncoder {
    /// Encode 12-bit signed immediate (for I/S-type instructions)
    pub fn encode_imm12(imm: i32) -> Option<u12> {
        if imm >= -2048 && imm < 2048 {
            Some((imm as i32 & 0xfff) as u12)
        } else {
            None
        }
    }

    /// Encode 7-bit immediate (for funct7 fields)
    pub fn encode_funct7(imm: u8) -> Option<u8> {
        if imm < 128 {
            Some(imm)
        } else {
            None
        }
    }

    /// Sign-extend 12-bit value
    pub fn sign_extend_12(val: u12) -> i32 {
        ((val << 20) as i32 >> 20) as i32
    }

    /// Convert immediate to signed if it fits in 12 bits
    pub fn as_signed(imm: u12) -> i32 {
        Self::sign_extend_12(imm)
    }
}

/// High-level instruction builder for programmatic code generation
pub struct RiscvEmitter {
    pub code: Vec<u8>,
    labels: HashMap<String, u64>,
    label_counter: u32,
}

impl Default for RiscvEmitter {
    fn default() -> Self {
        Self::new()
    }
}

impl RiscvEmitter {
    pub fn new() -> Self {
        Self {
            code: Vec::new(),
            labels: HashMap::new(),
            label_counter: 0,
        }
    }

    /// Define a labeled position
    pub fn define_label(&mut self, name: &str) {
        self.labels.insert(name.to_string(), self.code.len() as u64);
    }

    /// Get label offset
    pub fn get_label_offset(&self, name: &str) -> Option<u64> {
        self.labels.get(name).cloned()
    }

    /// Generate unique label
    pub fn gen_label(&mut self, prefix: &str) -> String {
        self.label_counter += 1;
        format!("{}_{}", prefix, self.label_counter)
    }

    // ========================================================================
    // Instruction Encoders (extracted from mini_asm.rs logic)
    // ========================================================================

    /// Encode R-type instruction: rd, rs1, rs2, funct3, funct7
    /// Example: add x1, x2, x3 -> ADDI(rd=x1, rs1=x2, rs2=x3, funct3=0x0, funct7=0x0)
    pub fn encode_r_type(opcode: u32, rd: u8, rs1: u8, rs2: u8, funct3: u8, funct7: u8) -> [u8; 4] {
        let insn = opcode
            | (opcode << 8)  // Fixed for RISC-V encoding
            | ((rd as u32) << 7)
            | ((rs1 as u32) << 15)
            | ((rs2 as u32) << 20)
            | ((funct3 as u32) << 12)
            | ((funct7 as u32) << 25);
        
        insn.to_le_bytes()
    }

    /// Encode I-type instruction: rd, rs1, imm12, funct3
    /// Example: addi x1, x2, 5 -> ADDI(rd=x1, rs1=x2, imm=5, funct3=0x0)
    pub fn encode_i_type(opcode: u32, rd: u8, rs1: u8, imm12: u12, funct3: u8) -> [u8; 4] {
        let insn = opcode
            | ((imm12 as u32) << 20)
            | ((rs1 as u32) << 15)
            | ((rd as u32) << 7)
            | ((funct3 as u32) << 12);
        
        insn.to_le_bytes()
    }

    /// Encode S-type store instruction: rs1, rs2, imm12, funct3
    /// Example: sd a0, 8(a1) -> SD(rs1=a1, rs2=a0, imm=8, funct3=3)
    pub fn encode_s_type(rs1: u8, rs2: u8, imm12: u12, funct3: u8) -> [u8; 4] {
        let low = ((imm12 & 0xf) as u32) << 20;
        let high = (((imm12 >> 4) & 0xf) as u32) << 25;
        
        let insn = low | high 
            | ((rs1 as u32) << 15)
            | ((rs2 as u32) << 20)
            | ((funct3 as u32) << 12)
            | 0x23; // opcode for store
        
        insn.to_le_bytes()
    }

    /// Encode B-type branch instruction: rs1, rs2, imm12, funct3
    /// Branch encodings are special (bit-reversed intermediate fields)
    pub fn encode_b_type(rs1: u8, rs2: u8, imm12: i12, funct3: u8) -> [u8; 4] {
        let imm_low = (imm12 & 0xf) as u32;
        let imm_high = ((imm12 >> 4) & 0xf) as u32;
        
        // B-type has non-contiguous imm bits
        let insn = (imm_high << 25)
            | (imm_low << 8)
            | ((rs1 as u32) << 15)
            | ((rs2 as u32) << 20)
            | ((funct3 as u32) << 12)
            | 0x63; // opcode for branch
        
        insn.to_le_bytes()
    }

    /// Encode U-type upper immediate: rd, imm32
    /// Example: lui x1, 0x12345 -> LUI(rd=x1, imm=0x12345)
    pub fn encode_u_type(opcode: u32, rd: u8, imm32: u32) -> [u8; 4] {
        let imm = imm32 & 0xfffff000; // Upper 20 bits
        
        let insn = opcode
            | ((imm as u32) << 12)
            | ((rd as u32) << 7);
        
        insn.to_le_bytes()
    }

    /// Encode J-type jump: rd, imm32
    /// JAL has complex bit layout similar to B-type
    pub fn encode_j_type(opcode: u32, rd: u8, imm32: i32) -> [u8; 4] {
        let imm_bits = imm32 as u32;
        
        // J-type encoding: [20][10:1][11][30:21]
        let part1 = (imm_bits & 0x800000) >> 19;  // bit 20
        let part2 = (imm_bits & 0x7fe0) >> 4;     // bits 10:1
        let part3 = (imm_bits & 0x800) >> 11;     // bit 11
        let part4 = (imm_bits & 0xfffe000) >> 20; // bits 30:21
        
        let insn = part1
            | (part2 << 20)
            | (part3 << 21)
            | (part4 << 12)
            | ((rd as u32) << 7)
            | 0x6f; // opcode for jal
        
        insn.to_le_bytes()
    }

    /// Encode U-type with lower opcode (AUIPC)
    pub fn encode_auipc(rd: u8, imm32: u32) -> [u8; 4] {
        Self::encode_u_type(0x17, rd, imm32)
    }

    /// Encode LUI instruction
    pub fn encode_lui(rd: u8, imm32: u32) -> [u8; 4] {
        Self::encode_u_type(0x37, rd, imm32)
    }

    /// Helper: emit raw bytes
    pub fn emit(&mut self, bytes: &[u8]) {
        self.code.extend_from_slice(bytes);
    }

    /// Emit single instruction
    pub fn emit_inst(&mut self, inst: [u8; 4]) {
        self.emit(&inst);
    }

    /// Create load/store instruction helpers
    pub fn lbInst(&mut self, rd: u8, off: i32, rs1: u8) {
        let inst = self.create_lb(rd, off, rs1);
        self.emit_inst(inst);
    }

    pub fn lhInst(&mut self, rd: u8, off: i32, rs1: u8) {
        let inst = self.create_lh(rd, off, rs1);
        self.emit_inst(inst);
    }

    pub fn lwInst(&mut self, rd: u8, off: i32, rs1: u8) {
        let inst = self.create_lw(rd, off, rs1);
        self.emit_inst(inst);
    }

    pub fn ldInst(&mut self, rd: u8, off: i32, rs1: u8) {
        let inst = self.create_ld(rd, off, rs1);
        self.emit_inst(inst);
    }

    pub fn sbInst(&mut self, rs2: u8, off: i32, rs1: u8) {
        let inst = self.create_sb(rs2, off, rs1);
        self.emit_inst(inst);
    }

    pub fn shInst(&mut self, rs2: u8, off: i32, rs1: u8) {
        let inst = self.create_sh(rs2, off, rs1);
        self.emit_inst(inst);
    }

    pub fn swInst(&mut self, rs2: u8, off: i32, rs1: u8) {
        let inst = self.create_sw(rs2, off, rs1);
        self.emit_inst(inst);
    }

    pub fn sdInst(&mut self, rs2: u8, off: i32, rs1: u8) {
        let inst = self.create_sd(rs2, off, rs1);
        self.emit_inst(inst);
    }

    // Load instructions
    fn create_lb(&self, rd: u8, off: i32, rs1: u8) -> [u8; 4] {
        let imm = self.zero_extend_i32(off, 12) as u12;
        RiscvEmitter::encode_i_type(0x03, rd, rs1, imm, 0) // lb uses funct3=0
    }

    fn create_lh(&self, rd: u8, off: i32, rs1: u8) -> [u8; 4] {
        let imm = self.zero_extend_i32(off, 12) as u12;
        RiscvEmitter::encode_i_type(0x03, rd, rs1, imm, 1) // lh uses funct3=1
    }

    fn create_lw(&self, rd: u8, off: i32, rs1: u8) -> [u8; 4] {
        let imm = self.zero_extend_i32(off, 12) as u12;
        RiscvEmitter::encode_i_type(0x03, rd, rs1, imm, 2) // lw uses funct3=2
    }

    fn create_ld(&self, rd: u8, off: i32, rs1: u8) -> [u8; 4] {
        let imm = self.zero_extend_i32(off, 12) as u12;
        RiscvEmitter::encode_i_type(0x03, rd, rs1, imm, 3) // ld uses funct3=3
    }

    // Store instructions
    fn create_sb(&self, rs2: u8, off: i32, rs1: u8) -> [u8; 4] {
        let imm = self.zero_extend_i32(off, 12) as u12;
        RiscvEmitter::encode_s_type(rs1, rs2, imm, 0)
    }

    fn create_sh(&self, rs2: u8, off: i32, rs1: u8) -> [u8; 4] {
        let imm = self.zero_extend_i32(off, 12) as u12;
        RiscvEmitter::encode_s_type(rs1, rs2, imm, 1)
    }

    fn create_sw(&self, rs2: u8, off: i32, rs1: u8) -> [u8; 4] {
        let imm = self.zero_extend_i32(off, 12) as u12;
        RiscvEmitter::encode_s_type(rs1, rs2, imm, 2)
    }

    fn create_sd(&self, rs2: u8, off: i32, rs1: u8) -> [u8; 4] {
        let imm = self.zero_extend_i32(off, 12) as u12;
        RiscvEmitter::encode_s_type(rs1, rs2, imm, 3)
    }

    /// Zero extend i32 to usize, capped at shift amount
    fn zero_extend_i32(&self, val: i32, shift: u32) -> u32 {
        if val >= 0 {
            val as u32
        } else {
            let mut v = (-val) as u32;
            v <<= (32 - shift) as i32;
            v >>= (32 - shift) as i32;
            v
        }
    }

    /// Return address offset calculation
    pub fn ret_addr_off(&self, _stack_base: i32, fp: u8, ra: u8) -> i32 {
        0 // Simplified version
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constraint_parsing() {
        let c = Constraint::parse("r").unwrap();
        assert_eq!(c.rw, ConstraintRW::ReadWrite);
        assert_eq!(c.typ, OperandType::Register);

        let c = Constraint::parse("=r").unwrap();
        assert_eq!(c.rw, ConstraintRW::Write);

        let c = Constraint::parse("+m").unwrap();
        assert_eq!(c.rw, ConstraintRW::ReadWrite);
        assert_eq!(c.typ, OperandType::Memory);
    }

    #[test]
    fn test_reg_encoders() {
        assert_eq!(RegEncoder::decode_reg("x1"), Some(1));
        assert_eq!(RegEncoder::decode_reg("ra"), Some(1));
        assert_eq!(RegEncoder::decode_reg("sp"), Some(2));
        assert_eq!(RegEncoder::decode_reg("a0"), Some(10));
    }

    #[test]
    fn test_imm_encoders() {
        let encoded = ImmEncoder::encode_imm12(100).unwrap();
        assert_eq!(encoded, 100);
        
        let decoded = ImmEncoder::as_signed(encoded);
        assert_eq!(decoded, 100);
    }

    #[test]
    fn test_encode_i_type() {
        let inst = RiscvEmitter::encode_i_type(0x13, 10, 11, 5, 0); // addi x10, x11, 5
        assert_eq!(inst[0], 0x05);
        assert_eq!(inst[1], 0x2b);
        assert_eq!(inst[2], 0x13);
        assert_eq!(inst[3], 0x00);
    }

    #[test]
    fn test_encoder_basic_flow() {
        let mut e = RiscvEmitter::new();
        e.define_label("test");
        assert_eq!(e.get_label_offset("test"), Some(0));
    }
}
