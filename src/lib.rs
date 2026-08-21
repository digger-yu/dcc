// src/lib.rs - DCC library
//
// Re-exports all internal modules so integration tests can access them.
// The binary (src/main.rs) is a thin wrapper around this library.

#![allow(dead_code, unreachable_patterns, dropping_references)]

pub mod ast;
#[cfg(test)]
mod ast_tests;
pub mod bootstrap;
pub mod cli;
pub mod codegen;
pub mod const_eval;
pub mod cpp;
pub mod debug;
pub mod diagnostics;
pub mod driver;
pub mod format_check;
pub mod fixdep;
pub mod ipa_sra;
pub mod ir;
pub mod link;
pub mod loop_opt;
pub mod lto;
pub mod mmap_io;
pub mod opt;
pub mod parser;
pub mod preproc;
pub mod profile;
pub mod sanitizer;
pub mod ssa;
pub mod ssa_gvn;
pub mod ssa_passes;
pub mod target;
pub mod tokens;