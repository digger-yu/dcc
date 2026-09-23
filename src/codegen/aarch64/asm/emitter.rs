// src/codegen/aarch64/asm/emitter.rs
// AArch64 Assembly Emitter - Unified Instruction Emission API
// 
// This module provides helper functions for emitting AArch64 instructions:
// - Register encoding helpers (convert register names to numbers)
// - Immediate value encoding (shifts, rotations)
// - Common instruction patterns (load/store, arithmetic, branch)
// - Placeholder handling for future use with monolith integration
//
// Phase A Day 4-5 - ASM Module Extraction (9/22/2026)

use std::fmt;

/// Error type for instruction emission
#[derive(Debug, Clone)]
pub enum AsmEmitError {
    /// Invalid register number
    InvalidRegister(u8),
    /// Invalid immediate value
    InvalidImmediate(i64),
    /// Invalid addressing mode
    InvalidAddressingMode(String),
    /// Instruction encoding error
    EncodingError(String),
}

impl fmt::Display for AsmEmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AsmEmitError::InvalidRegister(reg) => {
                write!(f, "invalid register number: {}", reg)
            }
            AsmEmitError::InvalidImmediate(imm) => {
                write!(f, "invalid immediate value: {}", imm)
            }
            AsmEmitError::InvalidAddressingMode(mode) => {
                write!(f, "invalid addressing mode: {}", mode)
            }
            AsmEmitError::EncodingError(instr) => {
                write!(f, "encoding error for '{}'", instr)
            }
        }
    }
}

impl std::error::Error for AsmEmitError {}

/// Result type for emission operations
pub type EmitResult<T> = Result<T, AsmEmitError>;

/// Context for the current assembly emission position
#[derive(Debug, Clone)]
pub struct EmitContext {
    /// Current position in output buffer
    pub pos: usize,
    /// Output code buffer
    pub buffer: Vec<u8>,
    /// Track pending branch relocations
    pub pending_branches: Vec<PendingBranch>,
}

impl Default for EmitContext {
    fn default() -> Self {
        Self::new()
    }
}

impl EmitContext {
    pub fn new() -> Self {
        Self {
            buffer: Vec::new(),
            pos: 0,
            pending_branches: Vec::new(),
        }
    }

    /// Add bytes to the output buffer
    pub fn emit(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
        self.pos += bytes.len();
    }

    /// Get current position
    pub fn current_pos(&self) -> usize {
        self.pos
    }

    /// Add a placeholder for a branch that needs relocation later
    pub fn add_branch_placeholder(&mut self) -> usize {
        let start_pos = self.buffer.len();
        // Reserve 4 bytes for branch target
        self.buffer.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        self.pos += 4;
        
        self.pending_branches.push(PendingBranch {
            position: start_pos,
            offset_adjustment: 0,
        });
        
        start_pos
    }

    /// Finalize all pending branches by resolving their targets
    pub fn finalize_pending_branches(&mut self, branch_targets: &[(usize, u32)]) {
        // Simple implementation: resolve based on offset tables
        for branch in &mut self.pending_branches {
            if let Some(&(target_offset, adjustment)) = branch_targets.get(branch.position) {
                let encoded_target = self.calculate_branch_target(target_offset, adjustment);
                let offset_bytes = encoded_target.to_le_bytes();
                
                self.buffer[branch.position] = offset_bytes[0];
                self.buffer[branch.position + 1] = offset_bytes[1];
                self.buffer[branch.position + 2] = offset_bytes[2];
                self.buffer[branch.position + 3] = offset_bytes[3];
            }
        }
    }

    fn calculate_branch_target(&self, target_offset: usize, _adjustment: u32) -> u32 {
        // Simplified: just return the offset
        ((target_offset - self.pos) / 4) as u32
    }
}

/// Information about a pending branch relocation
#[derive(Debug, Clone)]
pub struct PendingBranch {
    /// Position in code buffer where branch is located
    pub position: usize,
    /// Offset adjustment for branch calculation
    pub offset_adjustment: u32,
}

/// Register utilities for encoding registers into instruction fields
pub struct RegEncoder;

impl RegEncoder {
    /// Encode a register number into an instruction field (bits [4:0])
    pub fn encode_reg(reg: u8) -> u32 {
        if reg > 31 {
            Err(AsmEmitError::InvalidRegister(reg))
        } else {
            Ok(reg as u32)
        }
    }

    /// Encode a shift amount for LSL/LSR/ASR/ROR
    pub fn encode_shift(shift_type: char, amt: u8) -> Result<u32, AsmEmitError> {
        let shift_op = match shift_type {
            'l' | 'L' => 0b00, // LSL
            'r' | 'R' => 0b01, // LSR
            'a' | 'A' => 0b10, // ASR
            'r' | 'R' => 0b11, // ROR
            _ => return Err(AsmEmitError::InvalidImmediate(shift_type as i64)),
        };
        
        // Validate shift amount
        if amt == 0 || amt > 64 {
            return Err(AsmEmitError::InvalidImmediate(amt as i64));
        }
        
        Ok((shift_op << 6) | (amt as u32))
    }

    /// Encode an extend operation (SXTB, UXTH, etc.)
    pub fn encode_extend(ext_type: char, size: u8) -> Result<u32, AsmEmitError> {
        let extend_op = match ext_type {
            's' | 'S' => 0b00, // SXTB/SXTH/SXTW
            'u' | 'U' => 0b01, // UXTB/UXTH/UXTW
            _ => return Err(AsmEmitError::InvalidImmediate(ext_type as i64)),
        };
        
        Ok((extend_op << 5) | (size << 3))
    }
}

/// Load/Store instruction emitters
pub struct LdStEmitter;

impl LdStEmitter {
    /// Emit load register unsigned (ldr x0, [x1])
    pub fn emit_ldr_rt_rn(r_t: u8, r_n: u8, offset: i32) -> EmitResult<Vec<u8>> {
        let rt = RegEncoder::encode_reg(r_t)?;
        let rn = RegEncoder::encode_reg(r_n)?;
        
        // Validate register numbers
        if r_t > 31 || r_n > 31 {
            return Err(AsmEmitError::InvalidRegister(r_t));
        }
        
        // ldr Xt, [Xn, #offset] - L=1, opc=11, V=0, 01, 0
        let insn = 0xf9400000u32 | ((offset as u32 & 0xfff) << 10) | (rt << 5) | rn;
        
        Ok(insn.to_le_bytes().to_vec())
    }

    /// Emit store register (str x0, [x1, #8])
    pub fn emit_str_rt_rn(r_t: u8, r_n: u8, offset: i32) -> EmitResult<Vec<u8>> {
        let rt = RegEncoder::encode_reg(r_t)?;
        let rn = RegEncoder::encode_reg(r_n)?;
        
        if r_t > 31 || r_n > 31 {
            return Err(AsmEmitError::InvalidRegister(r_t));
        }
        
        // str Xt, [Xn, #offset] - L=0, opc=11, V=0, 01, 0
        let insn = 0xf9000000u32 | ((offset as u32 & 0xfff) << 10) | (rt << 5) | rn;
        
        Ok(insn.to_le_bytes().to_vec())
    }

    /// Emit load register signed (ldr w0, [x1], zero-extends)
    pub fn emit_ldrw_rt_rn(r_t: u8, r_n: u8, offset: i32) -> EmitResult<Vec<u8>> {
        let rt = RegEncoder::encode_reg(r_t)?;
        let rn = RegEncoder::encode_reg(r_n)?;
        
        if r_t > 31 || r_n > 31 {
            return Err(AsmEmitError::InvalidRegister(r_t));
        }
        
        // ldr Wt, [Xn, #offset] - L=1, opc=01, V=0, 01, 0
        let insn = 0xb9400000u32 | ((offset as u32 & 0xfff) << 10) | (rt << 5) | rn;
        
        Ok(insn.to_le_bytes().to_vec())
    }

    /// Emit pre-indexed load/store variants
    pub fn emit_pre_indexed(
        is_load: bool,
        is_unsigned: bool,
        r_t: u8,
        r_n: u8,
        r_m: Option<u8>,
        imm: u32,
        update_base: bool
    ) -> EmitResult<Vec<u8>> {
        let rt = RegEncoder::encode_reg(r_t)?;
        let rn = RegEncoder::encode_reg(r_n)?;
        let rn_val = rn?;
        
        if r_t > 31 || r_n > 31 {
            return Err(AsmEmitError::InvalidRegister(r_t));
        }
        
        // Check if using post-indexed or pre-indexed
        let base = if is_load { 0xf9400000 } else { 0xf9000000 };
        let l_bit = if is_load { 1 } else { 0 };
        let p_bit = if update_base { 1 } else { 0 };
        
        // For simple case without modifier register
        let offset_encoded = imm & 0xfff;
        let insn = base | (offset_encoded << 10) | (l_bit << 22) | (p_bit << 29) | (rn_val << 5) | rt;
        
        Ok(insn.to_le_bytes().to_vec())
    }
}

/// Arithmetic instruction emitters
pub struct ArithEmitter;

impl ArithEmitter {
    /// Emit move immediate (mov x0, #42)
    pub fn emit_mov_x_imm(r_t: u8, imm: u64) -> EmitResult<Vec<u8>> {
        let rt = RegEncoder::encode_reg(r_t)?;
        
        if r_t > 31 {
            return Err(AsmEmitError::InvalidRegister(r_t));
        }
        
        // mov x0, #imm - uses orr with zero register
        // ORR Xn, XZR, #imm16
        let shifted_imm = imm >> 16;
        let mask = (1u64 << (16 - shifted_imm.trailing_zeros())) - 1;
        let actual_imm = imm & mask;
        
        if actual_imm > 0xfff {
            return Err(AsmEmitError::InvalidImmediate(actual_imm as i64));
        }
        
        // ORR Xn, XZR, #imm12
        let insn = 0x92c00000u32 | (actual_imm << 10) | (rt << 5) | 31; // XZR = 31
        
        Ok(insn.to_le_bytes().to_vec())
    }

    /// Emit ADD instruction
    pub fn emit_add_rd_nr_ns(rd: u8, nr: u8, ns: u8) -> EmitResult<Vec<u8>> {
        let rd_enc = RegEncoder::encode_reg(rd)?;
        let nr_enc = RegEncoder::encode_reg(nr)?;
        let ns_enc = RegEncoder::encode_reg(ns)?;
        
        if rd > 31 || nr > 31 || ns > 31 {
            return Err(AsmEmitError::InvalidRegister(rd));
        }
        
        // ADD Rd, Nr, Ns
        let insn = 0x8b000000u32 | (rd_enc << 0) | (nr_enc << 5) | (ns_enc << 10);
        
        Ok(insn.to_le_bytes().to_vec())
    }

    /// Emit ADD with immediate
    pub fn emit_add_imm_rd_nr_imm(rd: u8, nr: u8, imm: u32) -> EmitResult<Vec<u8>> {
        let rd_enc = RegEncoder::encode_reg(rd)?;
        let nr_enc = RegEncoder::encode_reg(nr)?;
        
        if rd > 31 || nr > 31 {
            return Err(AsmEmitError::InvalidRegister(rd));
        }
        
        // Validate immediate
        if imm > 0xfff || imm % 8 != 0 {
            return Err(AsmEmitError::InvalidImmediate(imm as i64));
        }
        
        // ADD Rd, Nr, #imm
        let encoded_imm = imm >> 3;
        let insn = 0x91000000u32 | (encoded_imm << 10) | (nr_enc << 5) | rd_enc;
        
        Ok(insn.to_le_bytes().to_vec())
    }

    /// Emit SUB instruction
    pub fn emit_sub_rd_nr_imm(rd: u8, nr: u8, imm: u32) -> EmitResult<Vec<u8>> {
        let rd_enc = RegEncoder::encode_reg(rd)?;
        let nr_enc = RegEncoder::encode_reg(nr)?;
        
        if rd > 31 || nr > 31 {
            return Err(AsmEmitError::InvalidRegister(rd));
        }
        
        if imm > 0xfff || imm % 8 != 0 {
            return Err(AsmEmitError::InvalidImmediate(imm as i64));
        }
        
        // SUB Rd, Nr, #imm
        let encoded_imm = imm >> 3;
        let insn = 0x9b000000u32 | (encoded_imm << 10) | (nr_enc << 5) | rd_enc;
        
        Ok(insn.to_le_bytes().to_vec())
    }
}

/// Branch instruction emitters
pub struct BranchEmitter;

impl BranchEmitter {
    /// Emit branch link (bl function_name)
    pub fn emit_bl(target_offset: i32) -> EmitResult<Vec<u8>> {
        // BL target - calculate relative offset
        // B target (relative) - cond=111, I1=0, I2=0, S=bit24, immhi=bits[25:26], immlo=bits[0:23]
        let offset_words = target_offset / 4;
        
        if offset_words < -(i32::MAX as i64 / 4) || offset_words > i32::MAX as i64 / 4 {
            return Err(AsmEmitError::InvalidImmediate(target_offset));
        }
        
        let imm26 = (offset_words & 0x03ffffff) as u32;
        let insn = 0x94000000u32 | imm26; // BL instruction
        
        Ok(insn.to_le_bytes().to_vec())
    }

    /// Emit branch with link placeholder (for future resolution)
    pub fn emit_bl_placeholder() -> Vec<u8> {
        // Placeholder that will be resolved later
        vec![0x00, 0x00, 0x00, 0x00]
    }

    /// Emit unconditional branch (b label)
    pub fn emit_b(target_offset: i32) -> EmitResult<Vec<u8>> {
        let offset_words = target_offset / 4;
        
        if offset_words < -(i32::MAX as i64 / 4) || offset_words > i32::MAX as i64 / 4 {
            return Err(AsmEmitError::InvalidImmediate(target_offset));
        }
        
        let imm26 = (offset_words & 0x03ffffff) as u32;
        let insn = 0x14000000u32 | imm26; // B instruction
        
        Ok(insn.to_le_bytes().to_vec())
    }

    /// Emit compare branch zero (cbz x0, label)
    pub fn emit_cbz(rt: u8, target_offset: i32) -> EmitResult<Vec<u8>> {
        let rt_enc = RegEncoder::encode_reg(rt)?;
        
        if rt > 31 {
            return Err(AsmEmitError::InvalidRegister(rt));
        }
        
        let offset_words = target_offset / 4;
        let imm19 = (offset_words & 0x7ffff) as u32;
        
        // CBZ Xt, label
        let insn = 0x34000000u32 | (imm19 << 5) | rt_enc;
        
        Ok(insn.to_le_bytes().to_vec())
    }
}

/// Control flow emitters
pub struct ControlFlowEmitter;

impl ControlFlowEmitter {
    /// Emit subroutine call return (ret)
    pub fn emit_ret() -> Vec<u8> {
        // RET - BR XN where N=LR (16)
        0xd63f03c0u32.to_le_bytes().to_vec()
    }

    /// Emit exception/supervisor call (svc #n)
    pub fn emit_svc_imm(imm: u32) -> EmitResult<Vec<u8>> {
        if imm > 0xffff {
            return Err(AsmEmitError::InvalidImmediate(imm as i64));
        }
        
        let insn = 0xd4000000u32 | (imm << 0);
        Ok(insn.to_le_bytes().to_vec())
    }

    /// Emit halt instruction (hlt #n)
    pub fn emit_hlt_imm(imm: u32) -> EmitResult<Vec<u8>> {
        if imm > 0xffff {
            return Err(AsmEmitError::InvalidImmediate(imm as i64));
        }
        
        let insn = 0xd65f03c0u32 | (imm << 0);
        Ok(insn.to_le_bytes().to_vec())
    }

    /// Emit break point (brk #n)
    pub fn emit_brk_imm(imm: u32) -> EmitResult<Vec<u8>> {
        if imm > 0xffff {
            return Err(AsmEmitError::InvalidImmediate(imm as i64));
        }
        
        let insn = 0xd4200000u32 | (imm << 0);
        Ok(insn.to_le_bytes().to_vec())
    }
}

/// Combine common load/store operations
#[allow(dead_code)]
pub struct EmitHelper;

impl EmitHelper {
    /// Emit LDR with immediate offset (post-indexed preferred)
    pub fn emit_ldr_post_index(rt: u8, rn: u8, imm9: i32) -> EmitResult<Vec<u8>> {
        LdStEmitter::emit_ldr_rt_rn(rt, rn, imm9)
    }

    /// Emit STR with immediate offset (post-indexed preferred)  
    pub fn emit_str_post_index(rt: u8, rn: u8, imm9: i32) -> EmitResult<Vec<u8>> {
        LdStEmitter::emit_str_rt_rn(rt, rn, imm9)
    }

    /// Emit LDR with pre-indexed addressing
    pub fn emit_ldr_pre_index(rt: u8, rn: u8, imm9: i32) -> EmitResult<Vec<u8>> {
        let mut code = LdStEmitter::emit_ldr_rt_rn(rt, rn, 0)?;
        // Modify to use pre-indexed (bit 29 set)
        let mut words = u32::from_le_bytes([code[0], code[1], code[2], code[3]]);
        words |= 1 << 29;
        code.clear();
        code.extend_from_slice(&words.to_le_bytes());
        Ok(code)
    }

    /// Emit STR with pre-indexed addressing
    pub fn emit_str_pre_index(rt: u8, rn: u8, imm9: i32) -> EmitResult<Vec<u8>> {
        let mut code = LdStEmitter::emit_str_rt_rn(rt, rn, 0)?;
        // Modify to use pre-indexed (bit 29 set)
        let mut words = u32::from_le_bytes([code[0], code[1], code[2], code[3]]);
        words |= 1 << 29;
        code.clear();
        code.extend_from_slice(&words.to_le_bytes());
        Ok(code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_emit_mov_x_imm() {
        let bytes = ArithEmitter::emit_mov_x_imm(0, 42).unwrap();
        assert_eq!(bytes.len(), 4);
    }

    #[test]
    fn test_emit_add_imm() {
        let bytes = ArithEmitter::emit_add_imm_rd_nr_imm(0, 1, 8).unwrap();
        assert_eq!(bytes.len(), 4);
    }

    #[test]
    fn test_emit_ldr_str() {
        let ldr_bytes = LdStEmitter::emit_ldr_rt_rn(0, 1, 8).unwrap();
        let str_bytes = LdStEmitter::emit_str_rt_rn(0, 1, 8).unwrap();
        
        assert_eq!(ldr_bytes.len(), 4);
        assert_eq!(str_bytes.len(), 4);
        // Verify they differ (different instruction encodings)
        assert_ne!(ldr_bytes, str_bytes);
    }

    #[test]
    fn test_emit_bl_placeholder() {
        let placeholder = BranchEmitter::emit_bl_placeholder();
        assert_eq!(placeholder.len(), 4);
        assert_eq!(placeholder, vec![0x00, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn test_emit_invalid_register() {
        let result = LdStEmitter::emit_ldr_rt_rn(32, 1, 8);
        assert!(result.is_err());
    }

    #[test]
    fn test_emit_context() {
        let mut ctx = EmitContext::new();
        ctx.emit(&[0x01, 0x02, 0x03, 0x04]);
        assert_eq!(ctx.current_pos(), 4);
    }
}
