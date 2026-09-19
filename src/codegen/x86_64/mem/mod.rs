// src/codegen/x86_64/mem/mod.rs
// Memory access and address calculation for x86_64 backend
// Load/store operations, RIP-relative addressing, global variable access

pub mod addr;
pub mod load_store;
pub mod global;

pub use addr::{gen_lvalue_addr, gen_expr_decay};
pub use load_store::{store_rsp_qword, load_rsp_qword};
pub use global::emit_lea_r13;
