// src/codegen/aarch64/emit/expr/va_arg.rs
// Variadic Argument Evaluator
//
// Handles __builtin_va_arg for extracting arguments from variadic functions.
// Supports integer, float, and aggregate type extraction.
//
// Extracted from: src/codegen/aarch64/original_monolith.rs

pub fn gen_va_arg_int(_ap: &str) {
    todo!("Extract va_arg int handling from original_monolith.rs")
}

pub fn gen_va_arg_double(_ap: &str) {
    todo!("Extract va_arg double handling from original_monolith.rs")
}

pub fn gen_va_arg_float(_ap: &str) {
    todo!("Extract va_arg float handling from original_monolith.rs")
}

pub fn gen_va_arg_struct(_ap: &str, _arg_ty: &crate::ir::ValueType) {
    todo!("Extract va_arg struct handling from original_monolith.rs")
}
