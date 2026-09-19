// src/codegen/x86_64/abi/mod.rs
// SysV AMD64 ABI implementation for x86_64 backend
// Handles return value classification, register allocation, and calling conventions

pub mod classes;
pub mod call;
pub mod frame;

pub use classes::{AbiClass, abi_classes, classify_field, merge_classes, leaf_abi_class};
pub use call::emit_sysv_call;
pub use frame::{emit_sub_rsp, emit_add_rsp, emit_zero_init_frame};
