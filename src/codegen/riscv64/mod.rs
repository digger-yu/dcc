// src/codegen/riscv64/mod.rs - RISC-V 64 backend (modular).
//
// Mechanically split from the original monolithic implementation. The
// split is a pure move: every method body is unchanged; only visibility
// and import paths were adjusted.
//
// Layout:
//   backend.rs            - Backend trait implementation / entry wiring
//   codegen/              - state struct, constructor, compile pipeline,
//                           function generation, output builders and the
//                           shared free helper functions
//   emit/block.rs         - statement / block-item emission
//   emit/expr/*           - expression-level emission (lvalue, call,
//                           unary, binary, literal, bitfield)
//   expression_impl*.rs   - gen_expr family (scalar/float/vector/complex/
//                           from-ast)
//   special/*             - atomics, TLS, inline asm, float conversions,
//                           compressed insns, RVV, CSR

mod backend;
mod codegen;
mod emit;
mod expression_impl;
mod expression_impl2;
mod expression_impl3;
mod special;

#[cfg(test)]
mod tests_split;

pub(crate) use codegen::Riscv64Codegen;
