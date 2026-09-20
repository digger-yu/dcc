// src/codegen/x86_64/abi/mod.rs
// SysV AMD64 ABI implementation for x86_64 backend
// Handles return value classification, register allocation, and calling conventions

pub mod classes;
pub mod call;
pub mod frame;

// Note: Internal helper functions remain private in their modules
// They are accessed via qualified paths like abi::classes::classify_field
