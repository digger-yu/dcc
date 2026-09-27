// src/codegen/aarch64/codegen/mod.rs - core state + orchestration.
// Mechanically split from the monolithic aarch64 backend.
// 布局: struct_def.rs = 状态结构体定义, pipeline.rs = 构造/compile/finalize 主干。

mod common;
mod functions;
mod output;
mod pipeline;
mod struct_def;

pub(crate) use common::*;
pub(crate) use pipeline::BranchKind;
pub(crate) use struct_def::Aarch64Codegen;
