// src/codegen/x86_64/elf/mod.rs
// ELF image construction, symbol collection and relocation resolution.

pub mod resolve;
pub mod helpers;
pub mod symbols;
pub mod attr_diag;
pub mod prescan;
pub mod static_locals;
