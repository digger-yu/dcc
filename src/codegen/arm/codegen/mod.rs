// src/codegen/arm/codegen/mod.rs - core state + orchestration.
// Mechanically split from the monolithic arm backend. Pure move:
// semantics preserved; only visibility and import paths were adjusted.

mod codegen;
mod common;
mod functions;
mod output;
mod struct_def;

pub(crate) use codegen::BranchKind;
pub(crate) use struct_def::ArmCodegen;
