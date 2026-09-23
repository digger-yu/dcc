// src/codegen/aarch64/emit/expr/mod.rs
// Expression Evaluation Module
//
// Handles evaluation of expressions and their conversion to AArch64 instructions.
// This submodule contains specialized handlers for different expression types:
// - Binary operations (+, -, *, /, logical ops)
// - Unary operations (!, ~, increment/decrement)
// - Literal values (integers, floats, strings)
// - Cast operations
// - LValue address computation (struct members, array indexing)
// - Function calls
// - Compound literals
// - Variadic argument handling

mod binary;
mod unary;
mod literal;
mod cast;
mod lvalue;
mod call;
mod compound;
mod va_arg;
mod init;

pub use binary::*;
pub use unary::*;
pub use literal::*;
pub use cast::*;
pub use lvalue::*;
pub use call::*;
pub use compound::*;
pub use va_arg::*;
pub use init::*;
