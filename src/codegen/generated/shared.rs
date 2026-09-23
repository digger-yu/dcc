// Shared modules index
// AUTO-GENERATED - DO NOT EDIT!

/// This module re-exports all architecture-specific instruction definitions

pub mod x86_64_instructions;
pub mod aarch64_instructions;
pub mod riscv64_instructions;

/// Re-export all instructions for easy access
pub use self::x86_64_instructions::*;
pub use self::aarch64_instructions::*;
pub use self::riscv64_instructions::*;
