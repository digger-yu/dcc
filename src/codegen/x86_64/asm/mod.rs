// src/codegen/x86_64/asm/mod.rs
// Inline assembly module for x86_64 backend
// Provides modular structure for assembly emission and parsing
// Part of PR-A2: ASM Module Extraction (Big-Bang refactoring)

pub mod directive;
pub mod disasm; // Disassembler helper module
pub mod emit;
pub mod expansion; // Assembly directive expansion
pub mod label;
pub mod labels; // NEW: Numeric local label handling (Turn 34)
pub mod operand;
pub mod planner;
pub mod template; // NEW: Register allocator planning

// Re-export main utilities
pub use directive::{AsmDirectiveExpander, AsmDirectiveExpander as LegacyExpander};
pub use labels::AsmLocalLabels; // Turn 34: Added numeric label support
                                // pub use emit::{emit_inline_asm, emit_resolved_asm_lines};  // Temporarily disabled during refactor
pub use operand::{
    parse_asm_operand_str, strip_asm_comment, AsmOperand, ASM_BASE_RIP, ASM_REG_CR, ASM_REG_DR,
    ASM_REG_K, ASM_REG_MMX, ASM_REG_ST, ASM_REG_TMM, ASM_REG_XMM, ASM_REG_YMM, ASM_REG_ZMM,
};
// pub use bindings::AsmOpBinding;  // Disabled - file deleted
pub use planner::{AsmRegUse, AsmRegisterPlanner};
