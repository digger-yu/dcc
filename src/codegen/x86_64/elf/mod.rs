// src/codegen/x86_64/elf/mod.rs
// ELF image construction, symbol collection and relocation resolution.

pub mod attr_diag;
pub mod helpers;
pub mod init_list;
pub mod output;
pub mod prescan;
pub mod rel_symbols;
pub mod resolve;
pub mod static_locals;
pub mod symbols;
