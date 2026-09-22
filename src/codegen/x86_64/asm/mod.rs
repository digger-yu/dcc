// src/codegen/x86_64/asm/mod.rs
// Inline assembly module for x86_64 backend
// Provides modular structure for assembly emission and parsing
// Part of PR-A2: ASM Module Extraction (Big-Bang refactoring)

pub mod directive;
pub mod emit;
pub mod label;
pub mod operand;
pub mod template;
pub mod expansion;  // Assembly directive expansion
pub mod disasm;     // Disassembler helper module
pub mod planner;    // NEW: Register allocator planning

// Re-export main utilities
pub use directive::AsmDirectiveExpander as LegacyExpander;
// pub use emit::{emit_inline_asm, emit_resolved_asm_lines};  // Temporarily disabled during refactor
// pub use label::AsmLocalLabels;
pub use operand::{parse_asm_operand_str, strip_asm_comment, AsmOperand,
    ASM_BASE_RIP, ASM_REG_XMM, ASM_REG_YMM, ASM_REG_ZMM};
// pub use bindings::AsmOpBinding;  // Disabled - file deleted
pub use planner::{AsmRegisterPlanner, AsmRegUse};
