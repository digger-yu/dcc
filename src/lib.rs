// src/lib.rs - DCC library
//
// Re-exports all internal modules so integration tests can access them.
// The binary (src/main.rs) is a thin wrapper around this library.

#![allow(dead_code, unreachable_patterns, dropping_references)]

pub mod ast;
pub mod ast_asm;
pub mod ast_cmp_fold;
pub mod ast_dispatch;
pub mod ast_gvn;
#[cfg(test)]
mod ast_tests;
pub mod bootstrap;
pub mod cli;
pub mod codegen;
pub mod const_eval;
pub mod cpp;
pub mod cpuid;
pub mod debug;
pub mod diagnostics;
pub mod driver;
pub mod fixdep;
pub mod float128;
pub mod float80;
pub mod format_check;
pub mod incremental;
pub mod ipa_modref;
pub mod ipa_sra;
pub mod ir;
pub mod link;
pub mod linker_script;
pub mod location;
pub mod loop_opt;
pub mod lto;
pub mod memory_report;
pub mod mmap_io;
pub mod node_pool;
pub mod opt;
pub mod output_format;
pub mod parse_profile;
pub mod parser;
pub mod pratt;
pub mod preproc;
pub mod profile;
pub mod riscv32;
pub mod sanitizer;
pub mod semantics;
pub mod ssa;
pub mod static_locals;
pub mod symbol;
pub mod target;
pub mod thumb;
pub mod timing;
pub mod tokens;
