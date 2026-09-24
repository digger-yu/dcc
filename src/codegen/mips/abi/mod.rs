// src/codegen/mips_backend/abi/mod.rs - MIPS32 ABI Implementation
// Per MIPS O32, N32, and ABI64 calling conventions
//
// This module implements multiple MIPS ABIs with support for:
// - O32 (traditional 32-bit)
// - N32 (new ABI with 64-bit pointers)
// - ABI64 (full 64-bit)
// - Load/store architecture handling
// - Delay slot filling strategies

#![allow(dead_code)]
#![allow(unused_variables)]

use super::registers::{MipsFpr, MipsGpr};

/// O32 ABI: Stack alignment requirement (8-byte)
pub const STACK_ALIGNMENT_O32: usize = 8;

/// N32 ABI: Stack alignment requirement (16-byte)
pub const STACK_ALIGNMENT_N32: usize = 16;

/// Maximum integer arguments passed in registers (O32: $a0-$a3)
pub const MAX_INREG_ARGS: usize = 4;

/// Maximum float arguments passed in registers (O32 hard-float)
pub const MAX_FLOAT_REG_ARGS: usize = 2; // $f12, $f14

/// Default stack frame minimum size (O32)
pub const FRAME_MIN_SIZE: i32 = 32;

/// O32 uses word-aligned return values
pub const RETURN_VALUE_WORDS: usize = 1; // R0 is the main return register

/// MIPS supports three different ABIs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MipsAbiType {
    O32,   // Traditional 32-bit
    N32,   // New ABI (64-bit ints, 32-bit pointers)
    ABI64, // Full 64-bit ABI
}

impl MipsAbiType {
    pub fn default_abi(target_pointer_bits: u8) -> Self {
        match target_pointer_bits {
            64 => Self::ABI64,
            32 => Self::O32,
            _ => Self::O32,
        }
    }

    pub fn stack_alignment(self) -> usize {
        match self {
            Self::O32 => STACK_ALIGNMENT_O32,
            Self::N32 | Self::ABI64 => STACK_ALIGNMENT_N32,
        }
    }
}

/// Argument information per MIPS ABI
#[derive(Debug, Clone)]
pub struct ArgInfo {
    /// Integer register or floating-point register
    pub reg: Option<MipsArgReg>,
    /// Stack offset if spilled
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

/// Unified MIPS argument register representation
#[derive(Debug, Clone, Copy)]
pub enum MipsArgReg {
    Gpr(MipsGpr),
    Fpr(MipsFpr),
}

impl MipsArgReg {
    pub fn as_register(&self) -> u8 {
        match self {
            Self::Gpr(gpr) => gpr.index(),
            Self::Fpr(fpr) => fpr.index(),
        }
    }
}

/// Category of argument type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgCategory {
    Integer,   // General-purpose register
    Float,     // Floating-point register
    Double,    // Double-precision (pair of F regs)
    Aggregate, // Struct/union passed by memory
}

/// MIPS ABI implementation
pub struct MipsAbi {
    abi_type: MipsAbiType,
}

impl MipsAbi {
    pub fn new(abi_type: MipsAbiType) -> Self {
        Self { abi_type }
    }

    /// Determine argument passing strategy per MIPS ABI
    pub fn arg_info(ty_kind: &str, n: usize) -> Option<ArgInfo> {
        if n >= MAX_INREG_ARGS {
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
            ArgCategory::Integer => {
                // Integer args in $a0-$a3 ($4-$7)
                let reg_idx = 4 + n as u8;
                if let Some(gpr) = MipsGpr::from_index(reg_idx) {
                    info.reg = Some(MipsArgReg::Gpr(gpr));
                }
            }
            ArgCategory::Float | ArgCategory::Double => {
                // Float args in $f12, $f14 for O32 hard-float
                // O32 uses even-numbered regs: f12, f14, f16, ...
                let fpr_idx = 12 + (n * 2) as u8;
                if fpr_idx <= 30 {
                    if let Some(fpr) = MipsFpr::from_index(fpr_idx) {
                        info.reg = Some(MipsArgReg::Fpr(fpr));
                    }
                }
            }
            _ => {
                // Aggregates passed via memory pointer
                // Pointer goes in $a0
                info.stack_offset = Some(-4); // Hidden parameter on stack
                info.reg = Some(MipsArgReg::Gpr(MipsGpr::A0));
            }
        }

        Some(info)
    }

    /// Classify argument type
    fn classify_arg_type(ty_kind: &str) -> ArgCategory {
        match ty_kind {
            "int" | "uint" | "byte" | "short" | "ushort" | "long" | "ulong" 
            | "char" | "uchar" | "bool" | "ptr" | "enum" | "void_ptr"
            | "u64" | "i64" => {
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
            _ => 4,
        }
    }

    /// Return value passing rules
    pub fn return_info(ty_kind: &str) -> ReturnInfo {
        // 未知类型名视为聚合 (struct/union): 无法在寄存器中返回,
        // 通过 $a0 隐藏指针. 必须在查 size 之前判定, 否则 type_size
        // 对未知类型返回默认值 4 会被误判成整数.
        if Self::classify_arg_type(ty_kind) == ArgCategory::Aggregate {
            return ReturnInfo {
                via_reg: false,
                reg: None,
                size: Self::type_size(ty_kind) as i32,
                kind: ReturnKind::ViaMemory,
            };
        }

        let size = Self::type_size(ty_kind);

        // Simple types returned in $v0
        if size <= 4 {
            ReturnInfo {
                via_reg: true,
                reg: Some(MipsArgReg::Gpr(MipsGpr::V0)),
                size: size as i32,
                kind: ReturnKind::SimpleInt,
            }
        }
        // 64-bit integers use $v0+$v1
        else if size == 8 && ty_kind != "double" {
            ReturnInfo {
                via_reg: true,
                reg: Some(MipsArgReg::Gpr(MipsGpr::V0)),
                size: 8,
                kind: ReturnKind::Int64TwoRegs,
            }
        }
        // Doubles in $f0 or $f0+$f1 (hardfloat)
        else if ty_kind == "double" {
            ReturnInfo {
                via_reg: true,
                reg: Some(MipsArgReg::Fpr(MipsFpr::F0)),
                size: 8,
                kind: ReturnKind::Double,
            }
        }
        // Large structs via hidden pointer
        else {
            ReturnInfo {
                via_reg: false,
                reg: None,
                size: size as i32,
                kind: ReturnKind::ViaMemory,
            }
        }
    }

    /// Check if structure should be passed by reference
    pub fn should_pass_by_ref(size: usize) -> bool {
        size > 4 // O32 passes small aggregates by value, large by ref
    }

    /// Calculate stack frame layout for O32 ABI
    pub fn calculate_frame(
        locals_count: usize,
        needs_canary: bool,
        variadic: bool,
    ) -> FrameLayout {
        let abi = Self::new(MipsAbiType::O32);
        let mut frame = FrameLayout::new();

        // Local variables (each 4 bytes)
        let local_bytes = locals_count as i32 * 4;
        frame.local_bytes = local_bytes;

        // Alignment padding
        let total = local_bytes + 4; // Space for return address, etc.
        let aligned_total = ((total + STACK_ALIGNMENT_O32 as i32 - 1) / STACK_ALIGNMENT_O32 as i32)
                            * STACK_ALIGNMENT_O32 as i32;
        frame.total_size = aligned_total.max(FRAME_MIN_SIZE);

        // Variadic function save area
        if variadic {
            frame.variadic_area = 16;
        }

        // Stack protector canary
        if needs_canary {
            frame.canary_bytes = 4;
        }

        frame
    }

    /// Generate MIPS-style function prologue
    /// Note: MIPS uses lw/sw instructions, not push/pop
    pub fn generate_prologue(
        frame_size: i32,
        save_regs: &[MipsGpr],
        needs_fp: bool,
    ) -> Vec<String> {
        let mut instructions = Vec::new();

        // Save RA (return address) in $ra
        instructions.push("sw $ra, 0($sp)".to_string());

        // Save saved registers ($s0-$s5)
        if !save_regs.is_empty() {
            for (i, reg) in save_regs.iter().enumerate() {
                let offset = (4 * (i + 1)) as i32;
                instructions.push(format!("sw ${}, {}($sp)", reg.name(), offset));
            }
        }

        // Set up stack pointer
        if frame_size > 0 {
            if frame_size <= 65535 {
                instructions.push(format!("addi $sp, $sp, -{}", frame_size));
            }
        }

        // Set up frame pointer (optional, but common)
        if needs_fp {
            instructions.push("move $fp, $sp".to_string());
        }

        instructions
    }

    /// Generate MIPS-style function epilogue
    pub fn generate_epilogue(
        frame_size: i32,
        save_regs: &[MipsGpr],
        needs_fp: bool,
    ) -> Vec<String> {
        let mut instructions = Vec::new();

        // Restore stack pointer
        if frame_size > 0 {
            instructions.push(format!("addi $sp, $sp, {}", frame_size));
        }

        // Restore saved registers
        if !save_regs.is_empty() {
            for (i, reg) in save_regs.iter().enumerate().rev() {
                let offset = (4 * (i + 1)) as i32;
                instructions.push(format!("lw ${}, {}($sp)", reg.name(), offset));
            }
        }

        // Restore return address and return
        instructions.push("lw $ra, 0($sp)".to_string());
        
        if needs_fp {
            instructions.push("move $sp, $fp".to_string());
        }
        
        instructions.push("jr $ra".to_string());
        instructions.push("nop".to_string()); // Delay slot fill

        instructions
    }

    /// Generate delay slot filler instruction
    /// Strategy: find useful instruction that has no dependency on branch
    pub fn fill_delay_slot(branch_instr: &str, next_stmt: &Option<String>) -> String {
        // Priority 1: Use independent next statement
        if let Some(stmt) = next_stmt {
            if !Self::has_branch_dependency(branch_instr, stmt) {
                return stmt.clone();
            }
        }

        // Priority 2: Use nop if dependent
        "nop".to_string()
    }

    /// Check if instruction has data dependency on branch
    fn has_branch_dependency(branch: &str, instr: &str) -> bool {
        // Simplified dependency check
        // In real implementation, analyze register usage
        let dep_indicators = ["$v0", "$v1", "$at", "$zero"];
        
        for indicator in &dep_indicators {
            if branch.contains(indicator) && instr.contains(indicator) {
                return true;
            }
        }
        
        false
    }
}

/// Return value information
#[derive(Debug, Clone, Copy)]
pub struct ReturnInfo {
    /// Whether returned via register
    pub via_reg: bool,
    /// Which register(s) (if applicable)
    pub reg: Option<MipsArgReg>,
    /// Size of return value
    pub size: i32,
    /// Return mechanism
    pub kind: ReturnKind,
}

/// Return mechanism type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReturnKind {
    SimpleInt,           // $v0 (1-4 bytes)
    Int64TwoRegs,        // $v0+$v1 (8 bytes)
    Double,              // $f0 or $f0+$f1 (8 bytes)
    ViaMemory,           // Hidden pointer in $a0
}

/// Stack frame layout information
#[derive(Debug, Clone, Copy)]
pub struct FrameLayout {
    /// Total size of stack frame
    pub total_size: i32,
    /// Local variable area size
    pub local_bytes: i32,
    /// Variadic function save area
    pub variadic_area: i32,
    /// Canary area size
    pub canary_bytes: i32,
    /// Additional scratch space
    pub scratch_area: i32,
}

impl FrameLayout {
    pub fn new() -> Self {
        Self {
            total_size: FRAME_MIN_SIZE,
            local_bytes: 0,
            variadic_area: 0,
            canary_bytes: 0,
            scratch_area: 0,
        }
    }
}

impl Default for FrameLayout {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod mips_abi_tests {
    use super::*;

    #[test]
    fn test_argument_allocation_o32() {
        let abi = MipsAbi::new(MipsAbiType::O32);
        
        // First 4 integers in $a0-$a3
        for i in 0..4 {
            let arg = MipsAbi::arg_info("int", i).unwrap();
            assert!(arg.reg.is_some());
        }

        // 5th argument spilled
        let arg = MipsAbi::arg_info("int", 4);
        assert!(arg.is_none());
    }

    #[test]
    fn test_float_arguments_hardfloat() {
        let abi = MipsAbi::new(MipsAbiType::O32);
        
        // Float args in $f12, $f14
        let arg0 = MipsAbi::arg_info("float", 0).unwrap();
        assert!(arg0.reg.is_some());
        if let Some(MipsArgReg::Fpr(f)) = arg0.reg {
            assert_eq!(f.index(), 12);
        }
        
        let arg1 = MipsAbi::arg_info("float", 1).unwrap();
        assert!(arg1.reg.is_some());
        if let Some(MipsArgReg::Fpr(f)) = arg1.reg {
            assert_eq!(f.index(), 14);
        }
    }

    #[test]
    fn test_return_value_rules() {
        let abi = MipsAbi::new(MipsAbiType::O32);
        
        // Integer in $v0
        let ret = MipsAbi::return_info("int");
        assert!(ret.via_reg);
        assert!(matches!(ret.reg, Some(MipsArgReg::Gpr(MipsGpr::V0))));

        // 64-bit int in $v0+$v1
        let ret = MipsAbi::return_info("i64");
        assert_eq!(ret.kind, ReturnKind::Int64TwoRegs);

        // Double in $f0
        let ret = MipsAbi::return_info("double");
        assert!(matches!(ret.reg, Some(MipsArgReg::Fpr(_))));

        // Large struct via memory
        let ret = MipsAbi::return_info("struct_large");
        assert!(!ret.via_reg);
    }

    #[test]
    fn test_frame_layout() {
        let abi = MipsAbi::new(MipsAbiType::O32);
        let layout = MipsAbi::calculate_frame(8, false, false);
        
        assert!(layout.local_bytes > 0);
        assert_eq!(layout.total_size % 8, 0);
        assert!(layout.total_size >= FRAME_MIN_SIZE);
    }

    #[test]
    fn test_prologue_generation() {
        let abi = MipsAbi::new(MipsAbiType::O32);
        let prologue = MipsAbi::generate_prologue(32, &[MipsGpr::S0, MipsGpr::S1], true);
        
        assert!(prologue.iter().any(|s| s.contains("sw")));
        assert!(prologue.iter().any(|s| s.contains("$ra")));
        assert!(prologue.iter().any(|s| s.contains("$sp")));
    }

    #[test]
    fn test_epilogue_generation() {
        let abi = MipsAbi::new(MipsAbiType::O32);
        let epilogue = MipsAbi::generate_epilogue(32, &[MipsGpr::S0, MipsGpr::S1], true);
        
        assert!(epilogue.iter().any(|s| s.contains("lw")));
        // jr $ra 后必须紧跟延迟槽指令 nop, 因此 jr 不是最后一行
        let jr_pos = epilogue
            .iter()
            .position(|s| s == "jr $ra")
            .expect("epilogue must contain jr $ra");
        assert_eq!(epilogue[jr_pos + 1], "nop"); // Delay slot
    }

    #[test]
    fn test_delay_slot_strategy() {
        let abi = MipsAbi::new(MipsAbiType::O32);
        
        let result = MipsAbi::fill_delay_slot("beq $t0, $t1, label", &Some("nop".to_string()));
        assert_eq!(result, "nop");
    }

    #[test]
    fn test_abi_types() {
        assert_eq!(MipsAbiType::O32.stack_alignment(), 8);
        assert_eq!(MipsAbiType::N32.stack_alignment(), 16);
    }
}
