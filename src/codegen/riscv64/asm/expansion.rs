// src/codegen/riscv64/asm/expansion.rs - Addressing Mode Expansion Engine
// Phase B Day 3 - Extracted from riscv64.rs.bak (2026/9/22)
//
// Source location in riscv64.rs.bak:
// - Large immediate handling (~line 1000+) for loading constants > 12 bits
// - Complex address calculation helpers for memory operands

use super::emitter::{RiscvEmitter, RegEncoder};

// RISC-V specific types
type u12 = u16;  // 12-bit unsigned immediate
type i12 = i16;  // 12-bit signed immediate

/// Addressing mode expansion helper
pub struct AddressExpansion;

impl AddressExpansion {
    /// Expand large constant load (values that don't fit in 12-bit immediate)
    pub fn expand_load_constant(
        rd: u8, 
        value: i64, 
        temp_reg: Option<u8>,
        emitter: &mut RiscvEmitter,
    ) -> Vec<String> {
        let mut instructions = Vec::new();
        
        if let Some(tr) = temp_reg {
            // Load upper immediate then addi for full 64-bit value
            let upper = (value >> 16) as u32;
            
            if upper != 0 {
                emitter.encode_lui(rd, upper);
                let lower_val = (value & 0xffff) as i32;
                
                // Sign-extend if negative
                if lower_val < 0 {
                    emitter.encode_i_type(0x13, rd, rd, (lower_val & 0xfff) as u12, 0);
                } else {
                    emitter.encode_i_type(0x13, rd, rd, (lower_val & 0xfff) as u12, 0);
                }
            }
            
            instructions.push(format!("lui x{}, {}", rd, upper));
        } else {
            // Use rd directly
            emitter.encode_lui(rd, ((value >> 16) & 0xffff_ffff) as u32);
            instructions.push(format!("lui x{}, {}", rd, (value >> 16)));
        }

        instructions
    }

    /// Generate lui+addiw sequence for positive 32-bit constants
    pub fn expand_load_positive(
        rd: u8, 
        value: i32,
        temp_reg: Option<u8>,
    ) -> Vec<String> {
        let mut instructions = Vec::new();
        
        if value >= 0 && value <= 0x7fff {
            // Small positive: just addi
            instructions.push(format!("addi x{}, x0, {}", rd, value));
        } else if value < 0 && value >= -0x800 {
            // Small negative: addi with sign-extend
            instructions.push(format!("addi x{}, x0, {}", rd, value));
        } else {
            // Need lui sequence
            let upper = (value >> 16) as u32;
            
            if upper == 0 {
                // Only needs addiw if value is small enough
                instructions.push(format!("addiw x{}, x0, {}", rd, value));
            } else {
                instructions.push(format!("lui x{}, {}", rd, upper));
                instructions.push(format!("addiw x{}, x{}, {}", rd, rd, (value & 0xffff)));
            }
        }

        instructions
    }

    /// Generate addressing modes for memory operands
    pub fn expand_memory_address(
        &self,
        base_reg: u8, 
        offset: i32,
        dest_reg: Option<u8>,
        use_add: bool,
    ) -> Vec<String> {
        let mut instructions = Vec::new();

        if let Some(dr) = dest_reg {
            if offset == 0 {
                // Direct use of base register
                instructions.push(format!(
                    "{} x{}, {}",
                    if use_add { "add" } else { "move" },
                    dr,
                    base_reg
                ));
            } else if offset >= -2048 && offset < 2048 {
                // Fits in 12-bit immediate
                instructions.push(format!(
                    "{} {}, x{}, {}",
                    if use_add { "addi" } else { "li" },
                    dr,
                    base_reg,
                    offset
                ));
            } else {
                // Must construct address through temp
                let _temp = self.generate_temp_address(base_reg, offset);
                instructions.extend(_temp.1);
                instructions.push(format!("add x{}, x{}, x{}", dr, base_reg, _temp.0));
            }
        } else {
            // In-place modification
            if offset >= -2048 && offset < 2048 {
                instructions.push(format!("addi x{}, x{}, {}", base_reg, base_reg, offset));
            } else {
                let temp = self.generate_temp_address(base_reg, offset);
                instructions.extend(temp);
            }
        }

        instructions
    }

    /// Helper to generate temporary address calculation
    fn generate_temp_address(&self, base_reg: u8, offset: i32) -> (u8, String) {
        let upper = (offset >> 16) as u32;
        let lower = (offset & 0xffff) as i32;
        
        let temp_reg = 28; // t3 - a safe temporary
        
        let instr = if upper != 0 {
            format!("lui x{}, 0x{:x}", temp_reg, upper)
        } else if lower < 0 {
            format!("li x{}, {}", temp_reg, lower)
        } else {
            format!("li x{}, {}", temp_reg, lower)
        };
        
        (temp_reg, instr)
    }

    /// Expand function call sequence
    pub fn expand_call_sequence(
        target: &str,
        callee_saved: &[u8],
        return_regs: &[u8],
    ) -> Vec<String> {
        let mut instructions = Vec::new();

        // Save callee-saved registers
        for reg in callee_saved {
            instructions.push(format!("push x{}", reg));
        }

        // Call instruction
        instructions.push(format!("call {}", target));

        // Restore registers
        for &reg in callee_saved.iter().rev() {
            instructions.push(format!("pop x{}", reg));
        }

        // Return value setup (a0/a1)
        for (i, &reg) in return_regs.iter().enumerate().take(2) {
            if i == 0 {
                instructions.push(format!("mv x{}, a0", reg));
            } else if i == 1 {
                instructions.push(format!("mv x{}, a1", reg));
            }
        }

        instructions
    }

    /// Handle immediate overflow during arithmetic
    pub fn handle_overflow_arithmetic(
        op: &str,
        ops: &[u8],
        imm: i32,
        result_reg: u8,
    ) -> Vec<String> {
        if imm >= -2048 && imm < 2048 {
            vec![format!("{} x{}, {}, {}", op, result_reg, ops[0], imm)]
        } else {
            // Construct immediate using lui/addi
            let upper = (imm >> 16) as u32;
            let lower = (imm & 0xffff) as i16;
            
            let mut instrs = Vec::new();
            instrs.push(format!("lui at, 0x{:x}", upper));
            instrs.push(format!("addiw at, at, {}", lower));
            instrs.push(format!("{} x{}, {}, at", op, result_reg, ops[0]));
            instrs
        }
    }

    /// Calculate optimal encoding strategy for an immediate
    pub fn analyze_immediate(imm: i32) -> ImmediateStrategy {
        if imm >= -2048 && imm < 2048 {
            ImmediateStrategy::Direct { immediate: imm }
        } else if imm >= 0 && imm <= 0x7fff {
            ImmediateStrategy::AddiSeq
        } else {
            ImmediateStrategy::LuiAddi { high: (imm >> 16), low: (imm & 0xffff) }
        }
    }
}

/// Strategy for encoding immediates
#[derive(Debug, Clone)]
pub enum ImmediateStrategy {
    Direct { immediate: i32 },          // Single addi
    AddiSeq,                            // Multiple addi instructions  
    LuiAddi { high: u32, low: i16 },   // lui + addi combination
}

// ========================================================================
// Helper functions for common patterns
// ========================================================================

/// Get maximum signed 12-bit value
pub const MAX_SIGNED_12_BIT: i32 = 2047;

/// Get minimum signed 12-bit value  
pub const MIN_SIGNED_12_BIT: i32 = -2048;

/// Check if a value fits in signed 12-bit range
pub fn fits_in_12bit(imm: i32) -> bool {
    imm >= MIN_SIGNED_12_BIT && imm < MAX_SIGNED_12_BIT
}

/// Check if unsigned value fits in 12 bits
pub fn fits_in_12bit_unsigned(imm: u32) -> bool {
    imm <= 0xfff
}

/// Encode an immediate safely, returning the strategy used
pub fn encode_safe_immediate(imm: i32) -> ImmediateStrategy {
    AddressExpansion::analyze_immediate(imm)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fits_in_12bit() {
        assert!(fits_in_12bit(0));
        assert!(fits_in_12bit(1000));
        assert!(fits_in_12bit(-1000));
        assert!(fits_in_12bit(2047));
        assert!(fits_in_12bit(-2048));
        
        assert!(!fits_in_12bit(2048));
        assert!(!fits_in_12bit(-2049));
    }

    #[test]
    fn test_constant_expansion() {
        let mut emitter = RiscvEmitter::new();
        let instructions = AddressExpansion::expand_load_constant(10, 0x12345678, None, &mut emitter);
        assert_eq!(instructions.len(), 1);
        assert!(instructions[0].contains("lui"));
    }

    #[test]
    fn test_positive_constant() {
        let instructions = AddressExpansion::expand_load_positive(10, 0x12345, None);
        assert!(!instructions.is_empty());
    }

    #[test]
    fn test_memory_address_expansion() {
        let expansion = AddressExpansion;
        let instructions = expansion.expand_memory_address(10, 8, Some(11), true);
        assert!(!instructions.is_empty());
        assert!(instructions[0].contains("addi") || instructions[0].contains("li"));
    }

    #[test]
    fn test_immediate_strategy() {
        let s = AddressExpansion::analyze_immediate(100);
        matches!(s, ImmediateStrategy::Direct { .. });

        let s = AddressExpansion::analyze_immediate(0x10000);
        matches!(s, ImmediateStrategy::LuiAddi { .. });
    }
}
