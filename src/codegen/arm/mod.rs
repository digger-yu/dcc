// src/codegen/arm/mod.rs - ARM backend (AAPCS, modular).
//
// Mechanically split from the original monolithic implementation. The
// split is a pure move: every method body is unchanged; only visibility
// and import paths were adjusted.
//
// Layout:
//   backend.rs            - Backend trait implementation / entry wiring
//   codegen/              - state struct, constructor, compile pipeline,
//                           function generation, output builders
//   emit/block.rs         - statement / block-item emission
//   emit/expression.rs    - gen_expr_from_ast
//   special/inline_asm.rs - inline asm emission
//   special/float.rs      - VFPv3 float instruction emitters

mod backend;
mod codegen;
mod emit;
mod special;

#[cfg(test)]
mod tests_split;

pub(crate) use codegen::ArmCodegen;
