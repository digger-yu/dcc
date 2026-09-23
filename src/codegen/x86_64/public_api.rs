// Public API exports for x86_64 codegen
//! This module provides the public interface for the x86_64 code generation backend.

pub mod abi;
pub mod asm;
pub mod emit;
pub mod mem;
pub mod parallel;
pub mod special;

// Re-export main types
pub use crate::codegen::CodegenContext;

