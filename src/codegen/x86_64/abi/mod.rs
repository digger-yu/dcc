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
// layout.rs 是帧布局的**解算阶段**（P12 字面形态 Phase 2）：发射之前一次算完
// 各带边界，`emit_sub_rsp` 与所有 `%r13+disp` 消费者读同一份 `LayoutMap`。
pub mod layout;
// regbank.rs 是「哪个物理寄存器被占着」的唯一真源（Phase 1，见
// docs/p12-physreg-spill-design.md §3 H4 / §7 Phase 1）。
pub mod regbank;
// regnames.rs 是「寄存器名字 → 号/位宽」的唯一名表（Phase 6.1 的第一半）。以前有三份：
// `emit/func_setup.rs::parse_reg_name`（register asm / clobber 用）与 `emit/expr/utils.rs`、
// `asm/expansion.rs` 两份逐字相同的 `parse_reg_name_bits`。三份表各自演化就会漂移，
// 而漂移的具体形状已经实测到过：`%r13` 一类的写法在一处认得、另一处不认得
// （`register asm("r13")` 在 -O0 照做、在 -O2 静默降级）。接受哪种写法（`%` 前缀、
// 大写、APX）现在是表数据的一个维度，不是散在调用点的 if。
pub(crate) mod regnames;

// Note: Internal helper functions remain private in their modules
// They are accessed via qualified paths like abi::tyclass::abi_classes
