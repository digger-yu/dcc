// src/codegen/x86_64/parallel/mod.rs
// Parallel codegen and ELF relocation collection for x86_64 backend
// Multi-threaded compilation and relocation entry gathering infrastructure

pub mod collector;
pub mod orchestration;

pub use collector::{RelocationCollector, collect_relocations};
pub use orchestration::compile_parallel;
