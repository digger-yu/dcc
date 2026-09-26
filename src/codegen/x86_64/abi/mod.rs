// src/codegen/x86_64/abi/mod.rs
// SysV AMD64 ABI implementation for x86_64 backend
// Handles return value classification, register allocation, and calling conventions

// ABI 分类的**唯一真源**是 tyclass.rs（`abi_classes` / `classes_force_mem` /
// `needs_memory_return`）。曾存在的 `classes.rs` 是第二份 `AbiClass` 定义 +
// 第二套分类引擎，全仓零引用（仅靠本文件的 `pub mod classes;` 参与编译），
// 属于典型死影子，已删。
pub mod tyclass;
// call.rs 已由 batch 3 从 mod.rs 逐字搬迁 `emit_sysv_call` 生产实现, 影子存根作废。
pub mod call;
pub mod frame;

// Note: Internal helper functions remain private in their modules
// They are accessed via qualified paths like abi::tyclass::abi_classes
