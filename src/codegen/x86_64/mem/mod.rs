// src/codegen/x86_64/mem/mod.rs
// Memory access and address calculation for x86_64 backend
// Load/store operations, RIP-relative addressing, global variable access

pub mod access;
pub mod addr;
pub mod global;
pub mod load_store;
pub mod object_resolver;

// 不再在这里 `pub use` 重新导出: 已验证的调用方均直接走 `mem::addr::…` /
// `self.…` 路径，重新导出层无人消费 (属搬桩时留下的空壳外表)。
