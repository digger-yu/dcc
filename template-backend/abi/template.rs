// {isa_name}/abi/call.rs - Calling convention implementation template
// Parameter passing rules per ISA-specific ABI specification

use crate::ast::{Expr, Type};
use crate::ir::ValueType;

/// Call description for function invocations
pub struct CallDescription {
    pub func_name: String,
    pub args: Vec<CallArg>,
    pub ret_ty: Option<ValueType>,
}

/// Single function argument in calling convention format
pub struct CallArg {
    pub expr: Box<Expr>,
    pub reg_or_stack: ArgLocation,
}

/// Where argument is passed (register or stack)
pub enum ArgLocation {
    GpRegister(u8),      // General purpose register number
    FpRegister(u8),      // Floating point/SIMD register number
    StackOffset(i32),    // Stack offset (for large aggregates)
}

/// Get parameter registers for this ISA
/// Returns list of register numbers used for arguments
pub fn get_arg_registers(is_float: bool) -> &'static [u8] {
    if is_float {
        // Return FP/SIMD argument registers
        &[]  // Implement based on ABI spec
    } else {
        // Return GP argument registers  
        &[]  // Implement based on ABI spec
    }
}

/// Calculate return value location per ABI
pub fn classify_return_type(ret_ty: &ValueType) -> ReturnClass {
    match ret_ty {
        ValueType::Integer { bits, .. } => {
            if *bits <= 64 {
                ReturnClass::GpRegister  // Fits in X0/X1
            } else {
                ReturnClass::MultipleRegs  // Use multiple registers
            }
        }
        ValueType::Float { bits } => {
            if *bits <= 64 {
                ReturnClass::FpRegister  // Fits in V0/V1
            } else {
                ReturnClass::Memory  // Large floats go to memory
            }
        }
        _ => ReturnClass::Sret  // Structs use hidden pointer
    }
}

pub enum ReturnClass {
    GpRegister,       // Return in GP register (X0-X1)
    FpRegister,       // Return in FP register (V0-V1)
    MultipleRegs,     // Return in multiple registers
    Sret,             // Hidden sret pointer parameter
    Memory,           // Return value in memory slot
}
