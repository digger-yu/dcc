// src/codegen/aarch64/abi/call.rs
// Parameter Passing Implementation for AAPCS64 Calling Convention
// 
// Implements argument passing rules, return value handling, and call conventions.

use super::*;
use crate::ir::ValueType;

/// Generate function call sequence following AAPCS64
/// 
/// AAPCS64 calling convention (§5):
/// - First 8 arguments in X0-X7 (GP) / V0-V7 (FP/SIMD)
/// - Variadic or overflow arguments on stack
/// - Return values in X0/X1 (GP) / V0/V1 (FP)
/// - Struct >16B requires hidden sret pointer as first argument
/// 
/// # Arguments
/// * `callee_return_ty` - Return type of callee function
/// * `arg_types` - Types of all arguments
/// * `aggregate_fields` - Available field information
/// * `is_variadic` - Whether callee is variadic function
/// * `requires_sret` - Whether caller needs to provide hidden pointer
/// * `sret_ptr` - Optional sret pointer (if required)
/// 
/// # Returns
/// Complete call description including register assignments
pub fn generate_call(
    callee_return_ty: &ValueType,
    arg_types: &[ValueType],
    aggregate_fields: &std::collections::HashMap<String, (Vec<FieldInfo>, bool)>,
    is_variadic: bool,
    requires_sret: bool,
    sret_ptr: Option<usize>,
) -> CallDescription {
    let mut result = CallDescription::new();
    
    // Classify return type (determines if sret needed)
    let ret_class = classify_return_type(callee_return_ty, aggregate_fields);
    
    if ret_class.requires_sret() || requires_sret {
        // Add hidden sret pointer argument
        if let Some(ptr) = sret_ptr {
            result.arg_descriptors.push(ArgDescriptor {
                position: 0,
                class: ArgClass::Gp,
                reg: Some(Registers::X0),
                ptr_value: Some(ptr),
            });
        }
        
        // Remaining args start from position 1
        for (i, arg_ty) in arg_types.iter().enumerate() {
            add_arg_descriptor(&mut result, arg_ty, i + 1, aggregate_fields);
        }
    } else {
        // No sret pointer needed
        for (i, arg_ty) in arg_types.iter().enumerate() {
            add_arg_descriptor(&mut result, arg_ty, i, aggregate_fields);
        }
    }
    
    result.ret_class = Some(ret_class);
    result.is_variadic = is_variadic;
    
    result
}

/// Helper to add parameter descriptor
fn add_arg_descriptor(result: &mut CallDescription, arg_ty: &ValueType, pos: usize, aggregate_fields: &std::collections::HashMap<String, (Vec<FieldInfo>, bool)>) {
    let arg_class = classify_type(arg_ty);
    
    let reg_pos = pos - 1;
    let (gp_reg, fp_reg) = get_arg_registers(reg_pos);
    
    let desc = ArgDescriptor {
        position: pos,
        class: arg_class,
        gp_reg,
        fp_reg,
        is_variadic_overflow: check_variadic_overflow(pos, aggregate_fields),
    };
    
    result.arg_descriptors.push(desc);
}

/// Get appropriate registers for argument at position n
/// 
/// AAPCS64 parameter registers:
/// X0-X7 for GP types
/// V0-V7 for FP/SIMD types
/// 
/// # Arguments
/// * `n` - Argument index (0-based)
/// 
/// # Returns
/// (GP_register, FP_register) tuple
fn get_arg_registers(n: usize) -> (Option<u8>, Option<u8>) {
    if n >= MAX_REG_ARG_SLOTS {
        return (None, None);
    }
    
    // First 8 slots for GP, next 8 could be used for FP (but AArch64 uses separate sets)
    if n < 8 {
        (Some(Registers::X0 + n as u8), Some(Registers::V0 + n as u8))
    } else {
        // Slots 8-15 still use same register numbering
        (Some(Registers::X0 + n as u8), Some(Registers::V0 + n as u8))
    }
}

/// Check if argument overflows into variadic area
fn check_variadic_overflow(_pos: usize, _aggregate_fields: &std::collections::HashMap<String, (Vec<FieldInfo>, bool)>) -> bool {
    // Simplified: check against max register slots
    // Real implementation would count active GP/FP parameters
    false
}

/// Handle struct return via memory copy (memcpy_sp_to_x0 pattern)
pub fn gen_memcpy_sp_to_x0(_stack_offset: i32, _size: usize) -> Vec<u32> {
    // Simplified - returns empty for now
    Vec::new()
}

/// Load HFA from stack to vector registers
pub fn load_hfa_to_vregs(_stack_offset: i32, _hfa_info: HfaInfo, _base_reg: u8) -> Vec<u32> {
    Vec::new()
}

/// Store HFA from vector registers to stack
pub fn store_hfa_from_vregs(_stack_offset: i32, _hfa_info: HfaInfo, _base_reg: u8) -> Vec<u32> {
    Vec::new()
}

/// Call description structure
#[derive(Debug, Clone, Default)]
pub struct CallDescription {
    pub arg_descriptors: Vec<ArgDescriptor>,
    pub ret_class: Option<ReturnClass>,
    pub is_variadic: bool,
}

impl CallDescription {
    pub fn new() -> Self {
        Self {
            arg_descriptors: Vec::new(),
            ret_class: None,
            is_variadic: false,
        }
    }
    
    /// Count total argument slots needed
    pub fn arg_count(&self) -> usize {
        self.arg_descriptors.len()
    }
    
    /// Check if any argument uses memory (pointer passing)
    pub fn has_memory_args(&self) -> bool {
        self.arg_descriptors.iter().any(|a| a.class.is_memory())
    }
    
    /// Get register assignment for position n
    pub fn reg_for_arg(&self, pos: usize) -> Option<&ArgDescriptor> {
        self.arg_descriptors.get(pos.saturating_sub(1))
    }
}

/// Descriptor for single argument's passing method
#[derive(Debug, Clone)]
pub struct ArgDescriptor {
    pub position: usize,
    pub class: ArgClass,
    pub gp_reg: Option<u8>,
    pub fp_reg: Option<u8>,
    pub ptr_value: Option<usize>,  // For memory args
    pub is_variadic_overflow: bool,
}




// Register constants
mod Registers {
    pub const X0: u8 = 0;
    pub const X1: u8 = 1;
    pub const X2: u8 = 2;
    pub const X3: u8 = 3;
    pub const X4: u8 = 4;
    pub const X5: u8 = 5;
    pub const X6: u8 = 6;
    pub const X7: u8 = 7;
    pub const X8: u8 = 8;
    pub const X9: u8 = 9;
    pub const X10: u8 = 10;
    pub const X11: u8 = 11;
    pub const X12: u8 = 12;
    pub const X13: u8 = 13;
    pub const X14: u8 = 14;
    pub const X15: u8 = 15;
    pub const X19: u8 = 19;
    pub const X20: u8 = 20;
    pub const X29: u8 = 29;
    pub const X30: u8 = 30;
    pub const W0: u8 = 0;  // Alias for lower 32-bit view
    pub const V0: u8 = 0;
    pub const V1: u8 = 1;
    pub const V2: u8 = 2;
    pub const V3: u8 = 3;
    pub const V4: u8 = 4;
    pub const V5: u8 = 5;
    pub const V6: u8 = 6;
    pub const V7: u8 = 7;
    pub const SP: u8 = 31;  // Stack pointer
    
    pub const LR: u8 = 30;  // Link register
}
