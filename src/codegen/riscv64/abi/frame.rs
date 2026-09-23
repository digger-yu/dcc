// src/codegen/riscv64/abi/frame.rs
// RISC-V 64 Frame Management - SysV ABI Implementation
//
// Phase B Day 2 - ABI Module Extraction (2026/9/22)
// Reference: RISC-V ELF psABI specification, System V AMD64 ABI

use std::collections::HashMap;

use crate::codegen::riscv64::registers::*;
use crate::target::Target;


/// Stack alignment for RISC-V 64 (must be 16 bytes per ABI spec)
pub const STACK_ALIGN: usize = 16;

/// Minimum stack frame size
pub const MIN_FRAME_SIZE: u64 = 112; // Enough space for call out + saved registers

/// Size of register save area in stack frame
pub fn reg_save_area_size() -> u64 {
    // We need to save: ra, s0-s11
    // 13 registers × 8 bytes = 104 bytes
    104
}

/// Initial stack frame layout for function prologue
#[derive(Debug, Clone)]
pub struct StackFrame {
    /// Total frame size (including any local variables)
    pub total_size: u64,
    
    /// Local variable offset from stack pointer
    pub local_var_offset: i64,
    
    /// Saved register offsets (relative to SP after adjustment)
    pub ra_offset: i64,
    pub s0_offset: i64,
    pub s1_offset: i64,
    
    /// Whether we need to emit prologue/epilogue
    pub needs_prologue: bool,
    pub needs_epilogue: bool,
}

impl StackFrame {
    /// Create new stack frame with default values
    pub fn new(relocatable: bool) -> Self {
        let mut frame = Self {
            total_size: 0,
            local_var_offset: 0,
            ra_offset: 0,
            s0_offset: 0,
            s1_offset: 0,
            needs_prologue: false,
            needs_epilogue: false,
        };
        
        if !relocatable {
            // Only calculate full frame for executable objects
            frame.calculate_frame();
        }
        
        frame
    }
    
    /// Calculate required stack space based on function needs
    pub fn calculate_frame(&mut self) {
        // Reserve space for 16-byte aligned stack + callee-saved registers
        let base_size = STACK_ALIGN as u64;
        let saved_regs_size = reg_save_area_size();
        
        self.total_size = base_size.max(saved_regs_size);
        
        // Set up register save areas at known offsets
        self.ra_offset = -self.total_size as i64;
        self.s0_offset = self.ra_offset + 8;
        self.s1_offset = self.ra_offset + 16;
        
        // Mark that we need prologue/epilogue
        self.needs_prologue = true;
        self.needs_epilogue = true;
    }
    
    /// Adjust frame for additional local variables
    pub fn add_locals(&mut self, size: u64) {
        let aligned_size = align_to(size, STACK_ALIGN as u64);
        self.local_var_offset -= aligned_size as i64;
        self.total_size += aligned_size;
    }
    
    /// Emit function prologue assembly code
    pub fn emit_prologue<R: Into<u64>>(&self, _regs: &RegistersType, _target: &Target, _out: &mut Vec<R>) {
        // Prologue: allocate stack and save caller-saved registers
        
        // Save ra
        // addi sp, sp, -offset
        // sd ra, ra_offset(sp)
        
        if self.needs_prologue && self.total_size > 0 {
            // This would generate actual assembly in the emission phase
            // For now, just return the layout information
        }
    }
    
    /// Get offset for saving a specific register
    pub fn reg_save_offset(&self, reg: u8) -> i64 {
        match reg {
            1 => self.ra_offset,
            8 => self.s0_offset,
            _ => 0,
        }
    }
}

/// Align value up to given boundary
#[inline]
fn align_to(value: u64, align: u64) -> u64 {
    (value + align - 1) & !(align - 1)
}

/// Function argument layout according to RISC-V SysV ABI
#[derive(Debug)]
pub struct ArgList {
    /// Arguments in order of declaration
    pub args: Vec<ArgInfo>,
    
    /// Number of integer arguments (x10-x15 = a0-a5)
    pub gp_count: usize,
    
    /// Number of floating-point arguments (f0-f7)
    pub fp_count: usize,
}

#[derive(Debug, Clone)]
pub struct ArgInfo {
    /// Argument name
    pub name: String,
    
    /// Type (integer or float/vector)
    pub ty: ArgType,
    
    /// Register assignment if passed in register
    pub reg: Option<u8>,
}

#[derive(Debug, Clone)]
pub enum ArgType {
    Integer,
    Float,
    Vector,
    Aggregate,
}

/// Implement argument passing rules for RISC-V SysV ABI
impl ArgList {
    pub fn new(args: Vec<ArgInfo>) -> Self {
        let gp_count = args.iter().filter(|a| matches!(a.ty, ArgType::Integer | ArgType::Aggregate)).count().min(6);
        let fp_count = args.iter().filter(|a| matches!(a.ty, ArgType::Float | ArgType::Vector)).count().min(8);
        
        Self { args, gp_count, fp_count }
    }
    
    /// Assign registers according to SysV ABI rules
    pub fn assign_registers(&mut self) {
        let mut gp_reg_idx = 0;
        let mut fp_reg_idx = 0;
        
        for arg in &mut self.args {
            match arg.ty {
                ArgType::Integer | ArgType::Aggregate => {
                    if gp_reg_idx < 6 {
                        arg.reg = Some(get_gp_arg_reg(gp_reg_idx));
                        gp_reg_idx += 1;
                    }
                }
                ArgType::Float | ArgType::Vector => {
                    if fp_reg_idx < 8 {
                        arg.reg = Some(get_fp_arg_reg(fp_reg_idx));
                        fp_reg_idx += 1;
                    }
                }
            }
        }
    }
    
    /// Return number of arguments that must be passed on stack
    pub fn stack_args_count(&self) -> usize {
        self.args.len().saturating_sub(self.gp_count).saturating_sub(self.fp_count)
    }
}

/// Get GPR argument register for given position (0-5)
#[inline]
fn get_gp_arg_reg(pos: usize) -> u8 {
    assert!(pos < 6);
    match pos {
        0 => A0,
        1 => A1,
        2 => A2,
        3 => A3,
        4 => A4,
        5 => A5,
        _ => unreachable!(),
    }
}

/// Get FPU argument register for given position (0-7)
#[inline]
fn get_fp_arg_reg(pos: usize) -> u8 {
    assert!(pos < 8);
    F0 + pos as u8
}

/// Check if register is callee-saved and should be preserved
#[inline]
pub fn is_callee_saved_gpr(reg: u8) -> bool {
    // S0_FP(8), S1(9), S2-S11(18-27)
    matches!(reg, 8..=9 | 18..=27)
}

/// Check if register is caller-saved and may be clobbered
#[inline]
pub fn is_caller_saved_gpr(reg: u8) -> bool {
    // T0-T2(5-7), A0-A7(10-17), T7-T10(28-31), RA(1), SP(2)
    matches!(reg, 5..=7 | 10..=17 | 28..=31) || reg == 1 || reg == 2
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_stack_alignment() {
        assert_eq!(STACK_ALIGN, 16);
    }
    
    #[test]
    fn test_frame_calculation() {
        let frame = StackFrame::new(false);
        assert!(frame.needs_prologue);
    }
    
    #[test]
    fn test_arg_register_assignment() {
        let mut args = ArgList::new(vec![
            ArgInfo { name: "a".into(), ty: ArgType::Integer, reg: None },
            ArgInfo { name: "b".into(), ty: ArgType::Float, reg: None },
        ]);
        
        assert_eq!(args.gp_count, 1);
        assert_eq!(args.fp_count, 1);
    }
    
    #[test]
    fn test_is_callee_saved() {
        assert!(is_callee_saved_gpr(S0_FP));
        assert!(!is_callee_saved_gpr(A0));
    }
}
