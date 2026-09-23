// src/codegen/arm_backend/abi/mod.rs - ARM32 ABI Implementation
// Per ARM Architecture Procedure Call Standard (AAPCS32)
//
// This module implements the ARM32 calling convention including:
// - Argument passing rules (integer in r0-r3, float in s0-s7)
// - Return value semantics
// - Stack alignment and frame layout
// - Variadic function handling

#![allow(dead_code)]
#![allow(unused_variables)]

use super::registers::{ArmFpr, ArmGpr, MAX_FPINREG_ARG_COUNT, MAX_INREG_ARG_COUNT};

/// Stack alignment requirement (per AAPCS32): 8-byte alignment
pub const STACK_ALIGNMENT: usize = 8;

/// Maximum stack arguments before spill
pub const MAX_STACK_ARG_THRESHOLD: usize = MAX_INREG_ARG_COUNT;

/// Stack pointer adjustment per call frame
pub const FRAME_MIN_SIZE: i32 = 16; // Minimum stack frame

/// Red zone size (ARM doesn't have red zone like x86-64)
pub const RED_ZONE_SIZE: i32 = 0;

/// Size of each argument slot on stack
pub const ARG_SLOT_SIZE: i32 = 4;

/// Floating-point argument slot size
pub const FP_ARG_SLOT_SIZE: i32 = 4; // S-registers are 32-bit

/// Structure description for aggregate return values
#[derive(Debug, Clone)]
pub struct AggTypeInfo {
    pub size: usize,
    pub alignment: usize,
    pub is_return_via_memory: bool,
}

impl AggTypeInfo {
    pub fn new(size: usize, alignment: usize) -> Self {
        Self {
            size,
            alignment,
            is_return_via_memory: size > 8,
        }
    }
}

/// Argument information for a single parameter
#[derive(Debug, Clone)]
pub struct ArgInfo {
    /// Register to pass argument in (if fits)
    pub reg: Option<ArmReg>,
    /// Stack offset (if spilled)
    pub stack_offset: Option<i32>,
    /// Type category
    pub category: ArgCategory,
    /// Size in bytes
    pub size: usize,
}

impl ArgInfo {
    pub fn new() -> Self {
        Self {
            reg: None,
            stack_offset: None,
            category: ArgCategory::Integer,
            size: 4,
        }
    }
}

/// Category of argument type
#[derive(Debug, Clone, Copy)]
pub enum ArgCategory {
    Integer,   // Simple integer or pointer
    Float,     // Single-precision float
    Double,    // Double-precision float (in two S regs)
    Aggregate, // Struct or complex type
}

/// Unified ARM register representation
#[derive(Debug, Clone, Copy)]
pub enum ArmReg {
    Gpr(ArmGpr),
    Fpr(ArmFpr),
}

impl ArmReg {
    pub fn as_register(&self) -> u8 {
        match self {
            Self::Gpr(gpr) => gpr.index(),
            Self::Fpr(fpr) => fpr.index(),
        }
    }
}

/// AAPCS32 ABI implementation
pub struct ArmAbi;

impl ArmAbi {
    /// Determine which register(s) an argument uses per AAPCS32
    pub fn arg_info(ty_kind: &str, n: usize) -> Option<ArgInfo> {
        if n >= MAX_INREG_ARG_COUNT {
            return None; // Spilled to stack
        }

        let category = Self::classify_arg_type(ty_kind);
        let size = Self::type_size(ty_kind);

        let mut info = ArgInfo {
            reg: None,
            stack_offset: None,
            category,
            size,
        };

        match category {
            ArgCategory::Integer | ArgCategory::Aggregate => {
                // Integer args in R0-R3
                let reg_idx = n as u8;
                if reg_idx < MAX_INREG_ARG_COUNT as u8 {
                    if let Some(gpr) = ArmGpr::from_index(reg_idx) {
                        info.reg = Some(ArmReg::Gpr(gpr));
                    }
                }
            }
            ArgCategory::Float | ArgCategory::Double => {
                // Float args in S0-S7 (even indices)
                let fpr_idx = (n * 2) as u8;
                if fpr_idx < 16 {
                    if let Some(fpr) = ArmFpr::from_index(fpr_idx) {
                        info.reg = Some(ArmReg::Fpr(fpr));
                    }
                }
            }
        }

        Some(info)
    }

    /// Classify argument type category
    fn classify_arg_type(ty_kind: &str) -> ArgCategory {
        match ty_kind {
            "int" | "uint" | "byte" | "short" | "ushort" | "long" | "ulong" | "char" | "uchar" | "bool" | "ptr" | "enum" => {
                ArgCategory::Integer
            }
            "float" => ArgCategory::Float,
            "double" | "complex_f64" => ArgCategory::Double,
            _ => ArgCategory::Aggregate,
        }
    }

    /// Get type size in bytes
    fn type_size(ty_kind: &str) -> usize {
        match ty_kind {
            "byte" | "char" | "bool" | "u8" | "i8" => 1,
            "short" | "ushort" | "u16" | "i16" => 2,
            "int" | "uint" | "long" | "ulong" | "ptr" | "u32" | "i32" => 4,
            "float" => 4,
            "double" | "u64" | "i64" => 8,
            _ => 4, // Default
        }
    }

    /// Return value passing rules per AAPCS32
    pub fn return_info(ty_kind: &str) -> ReturnInfo {
        let size = Self::type_size(ty_kind);

        // Simple types returned in R0
        if size <= 4 {
            ReturnInfo {
                via_reg: true,
                reg: ArmReg::Gpr(ArmGpr::R0),
                size: size as i32,
                kind: ReturnKind::SimpleInt,
            }
        }
        // 64-bit values in R0+R1
        else if size == 8 && ty_kind == "double" {
            ReturnInfo {
                via_reg: true,
                reg: ArmReg::Fpr(ArmFpr::S0), // S0+S1 (or D0)
                size: 8,
                kind: ReturnKind::Double,
            }
        }
        // Large structs returned via hidden pointer in R0
        else {
            ReturnInfo {
                via_reg: false,
                reg: None,
                size: size as i32,
                kind: ReturnKind::ViaMemory,
            }
        }
    }

    /// Check if type needs stack allocation
    pub fn needs_stack_slot(ty_kind: &str) -> bool {
        let size = Self::type_size(ty_kind);
        size > 8
    }

    /// Calculate stack frame layout
    pub fn calculate_frame(
        locals_count: usize,
        needs_canary: bool,
        variadic: bool,
    ) -> FrameLayout {
        let mut frame = FrameLayout::new();

        // Local variables
        let local_bytes = locals_count as i32 * ARG_SLOT_SIZE;
        frame.local_bytes = local_bytes;

        // Save area for variadic functions (va_save region)
        if variadic {
            frame.va_save_bytes = 16; // Space for va_list state
        }

        // Stack protector canary
        if needs_canary {
            frame.canary_bytes = 4;
        }

        // Total and alignment
        let mut total = frame.local_bytes + frame.va_save_bytes + frame.canary_bytes;
        
        // Align to 8 bytes
        total = ((total + (STACK_ALIGNMENT as i32 - 1)) / STACK_ALIGNMENT as i32) 
                * STACK_ALIGNMENT as i32;

        frame.total_size = total;
        frame.alignment = STACK_ALIGNMENT as i32;

        frame
    }

    /// Load the saved frame pointer at start of function prologue
    pub fn load_fp_instruction(offset: i32) -> String {
        format!("ldr lr, [sp, #{offset}]")
    }

    /// Store the new frame pointer
    pub fn store_fp_instruction(offset: i32) -> String {
        format!("str lr, [sp, #{offset}]")
    }

    /// Generate prologue for function entry
    pub fn generate_prologue(
        frame_size: i32,
        save_regs: &[ArmGpr],
        needs_fp: bool,
    ) -> Vec<String> {
        let mut instructions = Vec::new();

        // Save LR
        instructions.push("push {lr}");

        // Save callee-saved registers specified
        if !save_regs.is_empty() {
            let reg_names: Vec<String> = save_regs.iter().map(|r| r.name()).collect();
            instructions.push(format!("push {{{}}}", reg_names.join(",")));
        }

        // Set up frame pointer (if needed)
        if needs_fp && frame_size > 0 {
            instructions.push("mov fp, sp");
            instructions.push(&format!("sub sp, sp, #{frame_size}"));
        }

        instructions
    }

    /// Generate epilogue for function exit
    pub fn generate_epilogue(
        frame_size: i32,
        save_regs: &[ArmGpr],
        needs_fp: bool,
    ) -> Vec<String> {
        let mut instructions = Vec::new();

        // Restore stack frame
        if needs_fp && frame_size > 0 {
            instructions.push(&format!("add sp, sp, #{frame_size}"));
            instructions.push("mov sp, fp");
        }

        // Restore callee-saved registers
        if !save_regs.is_empty() {
            let reg_names: Vec<String> = save_regs.iter().map(|r| r.name()).collect();
            instructions.push(format!("pop {{{}}}", reg_names.join(",")));
        }

        // Restore LR and return
        instructions.push("pop {lr}");
        instructions.push("bx lr".to_string());

        instructions
    }

    /// Determine if structure should be passed by value or reference
    pub fn should_pass_by_ref(size: usize) -> bool {
        // Pass large structures by reference
        size > 8
    }
}

/// Return value information
#[derive(Debug, Clone, Copy)]
pub struct ReturnInfo {
    /// Whether returned via register
    pub via_reg: bool,
    /// Which register (if applicable)
    pub reg: Option<ArmReg>,
    /// Size of return value
    pub size: i32,
    /// Return mechanism
    pub kind: ReturnKind,
}

/// Return mechanism type
#[derive(Debug, Clone, Copy)]
pub enum ReturnKind {
    SimpleInt,       // R0 (1-4 bytes)
    Double,          // S0/S1 or R0/R1 (8 bytes)
    ViaMemory,       // Hidden pointer in R0
}

/// Stack frame layout information
#[derive(Debug, Clone, Copy)]
pub struct FrameLayout {
    /// Total size of stack frame
    pub total_size: i32,
    /// Local variable area size
    pub local_bytes: i32,
    /// VA save area size
    pub va_save_bytes: i32,
    /// Canary area size
    pub canary_bytes: i32,
    /// Alignment requirement
    pub alignment: i32,
}

impl FrameLayout {
    pub fn new() -> Self {
        Self {
            total_size: FRAME_MIN_SIZE,
            local_bytes: 0,
            va_save_bytes: 0,
            canary_bytes: 0,
            alignment: STACK_ALIGNMENT as i32,
        }
    }

    pub fn has_variadic_area(&self) -> bool {
        self.va_save_bytes > 0
    }
}

impl Default for FrameLayout {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod arm_abi_tests {
    use super::*;

    #[test]
    fn test_argument_register_allocation() {
        // First 4 integers in R0-R3
        let arg0 = ArmAbi::arg_info("int", 0).unwrap();
        assert!(arg0.reg.is_some());
        if let Some(ArmReg::Gpr(r)) = arg0.reg {
            assert_eq!(r.index(), 0);
        }

        let arg3 = ArmAbi::arg_info("int", 3).unwrap();
        assert!(arg3.reg.is_some());
        if let Some(ArmReg::Gpr(r)) = arg3.reg {
            assert_eq!(r.index(), 3);
        }

        // 5th argument spilled to stack
        let arg4 = ArmAbi::arg_info("int", 4);
        assert!(arg4.is_none());
    }

    #[test]
    fn test_float_argument_allocation() {
        // Floats in even-numbered S registers
        let arg0 = ArmAbi::arg_info("float", 0).unwrap();
        assert!(arg0.reg.is_some());
        if let Some(ArmReg::Fpr(s)) = arg0.reg {
            assert_eq!(s.index(), 0);
        }

        let arg1 = ArmAbi::arg_info("float", 1).unwrap();
        assert!(arg1.reg.is_some());
        if let Some(ArmReg::Fpr(s)) = arg1.reg {
            assert_eq!(s.index(), 2);
        }
    }

    #[test]
    fn test_return_value_rules() {
        // Simple integers in R0
        let ret = ArmAbi::return_info("int");
        assert!(ret.via_reg);
        assert_eq!(ret.kind, ReturnKind::SimpleInt);
        if let Some(ArmReg::Gpr(r)) = ret.reg {
            assert_eq!(r.index(), 0);
        }

        // Double in S0 (8 bytes)
        let ret = ArmAbi::return_info("double");
        assert!(ret.via_reg);
        assert_eq!(ret.kind, ReturnKind::Double);

        // Large structs via memory
        let ret = ArmAbi::return_info("struct_large");
        assert!(!ret.via_reg);
        assert_eq!(ret.kind, ReturnKind::ViaMemory);
    }

    #[test]
    fn test_stack_alignment() {
        assert_eq!(STACK_ALIGNMENT, 8);
        
        let layout = ArmAbi::calculate_frame(4, false, false);
        assert_eq!(layout.total_size % 8, 0);
        assert_eq!(layout.alignment, 8);
    }

    #[test]
    fn test_frame_layout_calculation() {
        let layout = ArmAbi::calculate_frame(8, false, false);
        assert!(layout.local_bytes > 0);
        assert_eq!(layout.va_save_bytes, 0);
        
        let layout_var = ArmAbi::calculate_frame(8, false, true);
        assert!(layout_var.has_variadic_area());
        
        let layout_canary = ArmAbi::calculate_frame(8, true, false);
        assert_eq!(layout_canary.canary_bytes, 4);
    }

    #[test]
    fn test_prologue_generation() {
        let prologue = ArmAbi::generate_prologue(
            16,
            &[ArmGpr::R4, ArmGpr::R5],
            true,
        );
        
        assert!(prologue.contains("push"));
        assert!(prologue.len() >= 3);
    }

    #[test]
    fn test_epilogue_generation() {
        let epilogue = ArmAbi::generate_epilogue(
            16,
            &[ArmGpr::R4, ArmGpr::R5],
            true,
        );
        
        assert!(epilogue.contains("pop"));
        assert!(epilogue.last().unwrap() == &"bx lr".to_string());
    }
}
