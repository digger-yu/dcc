// src/codegen/aarch64/asm/planner/mod.rs
// AArch64 Assembly Instruction Planner
// 
// This module provides infrastructure for future instruction scheduling optimization:
// - Basic block planning (stub structure)
// - Branch optimization hints
// - Dependency analysis framework (future)
//
// Phase A Day 4-5 - ASM Module Extraction (9/22/2026)

pub mod orchestration;

pub use self::orchestration::{AsmPlanner, SchedulerHints};

/// Result type for planning operations
pub type PlanResult<T> = Result<T, PlanError>;

/// Error type for planning operations
#[derive(Debug, Clone)]
pub enum PlanError {
    /// Cycle budget exceeded
    BudgetExceeded { actual: usize, limit: usize },
    /// Deadlock detected in dependency graph
    CircularDependency(String),
    /// Invalid schedule order
    InvalidOrder(String),
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlanError::BudgetExceeded { actual, limit } => {
                write!(f, "cycle budget exceeded: {} > {}", actual, limit)
            }
            PlanError::CircularDependency(name) => {
                write!(f, "circular dependency detected: {}", name)
            }
            PlanError::InvalidOrder(order) => {
                write!(f, "invalid schedule order: {}", order)
            }
        }
    }
}

impl std::error::Error for PlanError {}
