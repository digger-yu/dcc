// src/codegen/aarch64/mod.rs - AArch64 backend (modular).
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
//   emit/expr/*           - expression-level emission (call, arithmetic,
//                           atomics, float, branches, bitfield)
//   expression_impl*.rs   - gen_expr family (scalar/float/vector/complex/
//                           from-ast)
//   special/*             - builtins, atomics, TLS, inline asm, cache/DSB,
//                           MTE

mod asm_dialect;
mod backend;
mod codegen;
mod emit;
#[cfg(test)]
mod encode_truth_test;
mod expression_impl;
mod expression_impl2;
mod expression_impl3;
mod insn;
mod special;

#[cfg(test)]
mod tests_split;

pub(crate) use codegen::Aarch64Codegen;
