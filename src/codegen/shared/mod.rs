//! Shared code generation infrastructure used by all backends
//! 
//! This module contains utilities and traits that are common across
//! all architecture backends, following the design-first principle.

pub mod abi;              // Architecture-independent ABI handling
pub mod backend;          // Backend trait definition
pub mod registry;         // Backend registration/discovery  
pub mod features;         // FeatureSet capability declarations
pub mod elf;              // ELF object file generation
pub mod asm_constraints;  // Cross-ISA inline asm constraint parsing
pub mod mini_asm;         // Non-x86 inline assembly parser
pub mod asm;              // Assembly emission utilities
pub mod control_flow;     // Control flow: jumps, labels, basic blocks
pub mod expression_executor; // Expression evaluation engine
pub mod expressions;      // Expression lowering utilities
pub mod instruction_traits; // Instruction templates and traits
pub mod operand_utils;    // Operand manipulation helpers
pub mod pipeline;         // Instruction pipeline management
pub mod types;            // Type definitions for codegen

// ===== KEY BACKEND TYPE EXPORTS =====
// Re-export key AST types for cross-backend access
pub use crate::ast::{Expr, Type, BinOp, UnOp, Stmt, Block, Decl, FuncDecl, Program};
pub use crate::ir::{Module, Function, Global, TypedExpr, TypedStmt, ValueType};

// Re-export key traits and results
pub use control_flow::{ControlFlowBackend, BranchCond, JumpTarget, ControlFlowState};
pub use expression_executor::{ExpressionExecutor, RegValue, ExprResult};
pub use types::CaseInfo;
