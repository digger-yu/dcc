// src/codegen/x86_64/elf/mod.rs
// ELF image construction, symbol collection and relocation resolution.

pub mod attr_diag;
/// C++ 零成本异常: `.gcc_except_table` / `.eh_frame` LSDA 字段与重定位装配
pub mod eh_sections;
pub mod helpers;
pub mod init_list;
pub mod output;
pub mod prescan;
pub mod rel_symbols;
pub mod resolve;
pub mod static_locals;
pub mod symbols;
