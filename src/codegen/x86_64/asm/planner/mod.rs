// src/codegen/x86_64/asm/planner/mod.rs
// Register allocation and planning for inline assembly
// Extracted from src/codegen/x86_64/mod.rs (PR-A2 Phase - Part 3)

// Re-export from orchestration module
pub use orchestration::{AsmRegisterPlanner, AsmRegUse};

mod orchestration;
