// src/codegen/riscv64/codegen/mod.rs - Core Code Generation Module
use crate::ast::{Expr, Type, Statement, Block, Decl, FuncDecl};
// Phase B Day 3-5 - Complete implementation extracted from riscv64.rs.bak (2026/9/22)
//
// Source locations in riscv64.rs.bak:
// - compile() ~line 230+
// - compile_exec()/compile_rel() ~line 365+, ~line 442+
// - gen_function() ~line 701+
// - Basic block decomposition and CFG processing
// - Global variable emission ~line 295+
//
// Modules:
// ├── compile.rs    - Main compilation pipeline          ✅
// ├── functions.rs  - Function generation logic         ✅
// ├── globals.rs    - Global variable handling          ✅
// └── blocks.rs     - Control flow/block processing     ✅

mod compile;
mod functions;
mod globals;
mod blocks;

pub use compile::*;
pub use functions::*;
pub use globals::*;
pub use blocks::*;
