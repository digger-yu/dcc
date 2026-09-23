// src/codegen/aarch64/helpers/mod.rs
// Helper Functions Module
//
// Provides utility functions for AArch64 code generation:
// - Instruction encoding utilities
// - Register number helpers
// - Immediate value encoding/decoding
// - Bit manipulation utilities
//
// This module extracts common functionality from original_monolith.rs into
// a reusable library of helper functions.

mod encoding;

pub use encoding::*;

/// ARM64 register count (general purpose)
pub const NUM_GPRS: u32 = 31;

/// ARM64 vector register count  
pub const NUM_VREGS: u32 = 32;

/// Stack alignment requirement (16 bytes for AArch64)
pub const STACK_ALIGNMENT: usize = 16;

/// Check if register is general purpose
pub fn is_gpr(reg: u8) -> bool {
    reg as u32 <= NUM_GPRS
}

/// Check if register is vector
pub fn is_vreg(reg: u8) -> bool {
    reg as u32 <= NUM_VREGS
}

/// Get register class from register number
pub fn get_reg_class(reg: u8) -> &'static str {
    if reg == 30 { "lr" } else { "gp" }
}
