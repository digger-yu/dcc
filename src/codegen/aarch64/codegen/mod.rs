// src/codegen/aarch64/codegen/mod.rs - core state + orchestration.
// Mechanically split from the monolithic aarch64 backend.

mod codegen;
mod common;
mod functions;
mod output;
mod struct_def;

pub(crate) use codegen::BranchKind;
pub(crate) use common::*;
pub(crate) use struct_def::Aarch64Codegen;
