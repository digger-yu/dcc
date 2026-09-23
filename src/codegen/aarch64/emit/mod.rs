// src/codegen/aarch64/emit/mod.rs
// Binary Section Generation Module
// 
// Handles the emission of binary code from intermediate representation (IR).
// This module contains statement-level and expression-level code generation
// logic that converts high-level IR constructs into AArch64 assembly instructions.
//
// Modules:
// - stmt.rs: Statement emission (control flow, declarations)
// - expr/: Expression evaluation (binary, unary, literals, etc.)

mod stmt;
pub mod expr;

pub use stmt::*;
pub use expr::*;
