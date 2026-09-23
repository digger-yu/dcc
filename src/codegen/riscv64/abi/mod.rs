// src/codegen/riscv64/abi/mod.rs
// RISC-V 64 SysV ABI Implementation
//
// Phase B Day 2-3 - ABI Module (2026/9/22)
// Reference: RISC-V ELF psABI specification, System V AMD64 ABI
//
// Implements calling convention for RISC-V 64 according to SysV ABI:
// - Parameter passing registers: a0-a7 (x10-x17), f0-f7
// - Return value registers: a0 (x10), f0
// - Callee-saved: s0-s11 (x8-x27)
// - Caller-saved: t0-t2, a0-a7, t7-t10, ra, sp, f0-f7, ft0-ft7

mod frame;

use crate::ast;

pub use frame::*;

// ============================================================================
// Calling Convention Constants (SysV ABI for RISC-V)
// ============================================================================

/// Number of integer argument registers (x10-x15 = a0-a5)
pub const NUM_INT_ARG_REGS: usize = 6;

/// Number of floating-point argument registers (f0-f7)
pub const NUM_FP_ARG_REGS: usize = 8;

/// Maximum register save area size for variadic functions
pub const VA_SAVE_AREA_SIZE: usize = 128;

// ============================================================================
// Argument Classification
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ArgClass {
    Gp,      // General purpose register
    Fp,      // Floating point vector register  
    Memory,  // Passed in memory
}

/// Classify a type for argument passing
#[inline]
pub fn classify_type(ty: &crate::ir::ValueType) -> ArgClass {
    match ty {
        crate::ir::ValueType::Integer { .. } => ArgClass::Gp,
        crate::ir::ValueType::Float { .. } => ArgClass::Fp,
        crate::ir::ValueType::Vector(_, _) => ArgClass::Fp,
        _ => ArgClass::Memory,
    }
}

// ============================================================================
// Call Setup and Arguments
// ============================================================================

/// Information about function arguments for call site
pub struct CallArgLayout {
    /// Integer args in registers
    pub int_args: [Option<u8>; 6],
    
    /// Float args in registers
    pub fp_args: [Option<u8>; 8],
    
    /// Stack-allocated overflow arguments
    pub stack_args_start_offset: i64,
    
    /// Whether any args spilled to stack
    pub has_stack_args: bool,
}

impl CallArgLayout {
    pub fn new() -> Self {
        Self {
            int_args: [None; 6],
            fp_args: [None; 8],
            stack_args_start_offset: 0,
            has_stack_args: false,
        }
    }
    
    /// Assign argument registers according to SysV ABI rules
    pub fn assign_registers(&mut self, _args: &[ast::BlockItem]) {
        // Placeholder - will be implemented when extracting from .bak
    }
}

/// Get the appropriate GP argument register for position
#[inline]
fn get_gp_arg_reg(pos: usize) -> u8 {
    assert!(pos < 6);
    match pos {
        0 => super::A0,
        1 => super::A1,
        2 => super::A2,
        3 => super::A3,
        4 => super::A4,
        5 => super::A5,
        _ => unreachable!(),
    }
}

// ============================================================================
// Function Prologue/Epilogue Helpers
// ============================================================================

/// Calculate frame pointer requirement
#[inline]
pub fn needs_frame_pointer(stack_size: u64, callee_saved_count: usize) -> bool {
    // Frame pointer needed if:
    // - Large stack frame (> 112 bytes typical threshold)
    // - Need to save many callee-saved registers
    // - Optimization level allows it
    stack_size > MIN_FRAME_SIZE || callee_saved_count > 4
}

/// Check if type requires alignment beyond natural alignment
#[inline]
pub fn needs_special_alignment(_ty: &crate::ir::ValueType) -> bool {
    false
}

// ============================================================================
// TLS Support (Thread-Local Storage)
// ============================================================================

/// Thread pointer register (tcb offset calculation)
#[inline]
pub fn get_tp_register() -> Option<u8> {
    // RISC-V doesn't have dedicated TP register in base ISA
    // Can be passed in a register or accessed via libc
    None
}

/// Get TCBOFFSET for TLS access
pub fn tls_offset(base_sym: &str) -> String {
    format!("_TLS_OFFSET_{}", base_sym)
}

// ============================================================================
// Testing
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_arg_classify_int() {
        let ty = crate::ir::ValueType::Int(8);
        assert_eq!(classify_type(&ty), ArgClass::Gp);
    }
    
    #[test]
    fn test_arg_classify_float() {
        let ty = crate::ir::ValueType::Float(8);
        assert_eq!(classify_type(&ty), ArgClass::Fp);
    }
    
    #[test]
    fn test_call_arg_layout() {
        let mut layout = CallArgLayout::new();
        assert!(layout.int_args.iter().all(|x| x.is_none()));
        assert!(layout.fp_args.iter().all(|x| x.is_none()));
    }
}
