// src/codegen/mips/mod.rs - MIPS Code Generation Backend
//
// TODO: [LATER] Complex architecture implementation with many errors
// Impact: Full MIPS/MIPS64 code generation unavailable
// Root cause: Missing trait implementations, type mismatches, incomplete refactoring from backup files
// Suggested solution: Fix ExprKind::Index syntax (unit vs tuple variant), add missing imports,
//                      implement missing methods for ControlFlowBackend trait
// Effort: ~8 hours of systematic debugging

// Phase C Extension - Complete MIPS/MIPS64 backend modularization

mod abi;
mod baseline_mips;
mod registers;

pub mod mips64; // MIPS64 N64 ABI adapter (use 'pub mod' for file-level module)

// Re-export main codegen types
pub use self::mips64::Mips64Codegen;
pub use baseline_mips::MipsCodegen;
