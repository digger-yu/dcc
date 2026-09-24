// src/codegen/riscv64/codegen/mod.rs - core state + orchestration.
// Mechanically split from the monolithic riscv64 backend.

mod codegen;
mod common;
mod functions;
mod output;

pub(crate) use codegen::{BranchKind, Riscv64Codegen};
pub(crate) use common::*;
