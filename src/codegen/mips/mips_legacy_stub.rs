//! MIPS Legacy monolith - original implementation for backward compatibility
//! This provides the actual MIPS backend functionality using stub functions

use anyhow::Result;

/// Stub function for MIPS code generation - minimal implementation for compilation
pub fn emit_function(_func: &crate::ir::Function) -> Result<Vec<u8>> {
    Err(anyhow::anyhow!("MIPS backend not fully implemented yet"))
}

/// MIPS register abstraction
#[derive(Debug, Clone, Copy)]
pub struct MipsRegister(pub u8);

impl MipsRegister {
    pub fn new(index: u8) -> Self {
        MipsRegister(index)
    }
    
    pub fn index(&self) -> u8 {
        self.0
    }
}

/// MIPS backend structure
pub struct MipsBackend;

impl MipsBackend {
    pub fn new() -> Self {
        MipsBackend
    }
}

/// MIPS code generator wrapper (stub)
pub struct MipsCodeGenerator;

impl MipsCodeGenerator {
    pub fn new() -> Self {
        MipsCodeGenerator
    }
}
