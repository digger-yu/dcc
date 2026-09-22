// src/codegen/x86_64/parallel/mod.rs
// Parallel codegen for x86_64 backend
//
// batch A 起 `orchestration` 为真实生产路径（mod.rs 逐字搬迁，非影子）。
// 原 `collector.rs` 影子（RelocationCollector / collect_relocations）在 mod.rs 中
// 无任何对应生产函数——relocation 全部由 Codegen 的 pending_* 字段在
// emit_functions_parallel / 输出阶段内联处理——故整体作废删除。

pub mod orchestration;
