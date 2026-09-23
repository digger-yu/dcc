// src/codegen/mips/mod.rs - MIPS Backend Modular Implementation
// Phase C Extension - Complete MIPS/MIPS64 backend modularization

mod baseline_mips;
mod registers;
mod abi;

pub mod mips64;  // MIPS64 N64 ABI adapter (use 'pub mod' for file-level module)

// Re-export main codegen types
pub use baseline_mips::MipsCodegen;
pub use self::mips64::Mips64Codegen;
