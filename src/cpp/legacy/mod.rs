//! Legacy C++ lowering 子模块
//!
//! Stage 1 拆分: 把 lower.rs 按职责拆分为多个子模块，行为保持不变。
//!
//! 子模块:
//! - runtime: typedef / runtime prologue / extern 声明
//! - layout:  class layout / virtual base 模拟
//! - expr:    C 表达式输出
//!
//! 注意: 这些是 legacy 路径（C 文本降级），新代码应走 sema → hir →
//! abi_layout → lower_ir 的 native 路径。

pub mod expr;
pub mod layout;
pub mod runtime;
