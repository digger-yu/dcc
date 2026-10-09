//! Shared code generation infrastructure used by all backends
//!
//! This module contains utilities and traits that are common across
//! all architecture backends, following the design-first principle.

pub mod abi; // Architecture-independent ABI handling
pub mod asm_constraints; // Cross-ISA inline asm constraint parsing
pub mod asm_engine; // 共享 .s 渲染骨架 (段/符号/注释符/字节序)
pub mod asm_operand; // 架构中性的指令记录模型 (InsnRec / Operand / LayoutMap)
pub mod asm_source; // 内联汇编源行的单一真源 (解析/分类/编码/透传判据)
pub mod backend; // Backend trait definition
pub mod control_flow; // Control flow: jumps, labels, basic blocks
pub mod elf; // ELF object file generation
pub mod expression_executor; // Expression evaluation engine
pub mod expressions; // Expression lowering utilities
pub mod features; // FeatureSet capability declarations
/// 「这个后端的家能力面」与各 target 的寄存器名表（Phase 6.4）。
pub mod homes;
pub mod mini_asm; // Non-x86 inline assembly parser
pub mod pipeline; // Instruction pipeline management
pub mod registry; // Backend registration/discovery
pub mod types; // Type definitions for codegen

// ===== KEY BACKEND TYPE EXPORTS =====
// Re-export key AST types for cross-backend access
pub use crate::ast::{BinOp, Block, Decl, Expr, FuncDecl, Program, Stmt, Type, UnOp};
pub use crate::ir::{Function, Global, Module, TypedExpr, TypedStmt, ValueType};

// Re-export key traits and results
pub use control_flow::{BranchCond, ControlFlowBackend, ControlFlowState, JumpTarget};
pub use expression_executor::{ExprResult, ExpressionExecutor, RegValue};
pub use types::CaseInfo;
