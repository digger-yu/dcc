//! ARM Legacy monolith - original implementation for backward compatibility
//! This provides the actual ARM backend functionality using stub functions

use anyhow::Result;

/// Stub function for ARM code generation - minimal implementation for compilation
pub fn emit_function(_func: &crate::ir::Function) -> Result<Vec<u8>> {
    Err(anyhow::anyhow!("ARM backend not fully implemented yet"))
}

/// ARM register abstraction
#[derive(Debug, Clone, Copy)]
pub struct ArmRegister(pub u8);

impl ArmRegister {
    pub fn new(index: u8) -> Self {
        ArmRegister(index)
    }
    
    pub fn index(&self) -> u8 {
        self.0
    }
}

/// ARM backend structure
pub struct ArmBackend;

impl ArmBackend {
    pub fn new() -> Self {
        ArmBackend
    }
}

/// ARM code generator wrapper (stub)
pub struct ArmCodeGenerator;

impl ArmCodeGenerator {
    pub fn new() -> Self {
        ArmCodeGenerator
    }
}
