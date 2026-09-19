// src/codegen/x86_64/asm/mod.rs
// Inline assembly module for x86_64 backend
// Provides modular structure for assembly emission and parsing

pub mod directive;
pub mod emit;
pub mod label;
pub mod operand;
pub mod template;

pub use directive::AsmDirectiveExpander;
pub use emit::{emit_inline_asm, emit_resolved_asm_lines};
pub use label::AsmLocalLabels;
pub use operand::{parse_asm_operand_str, strip_asm_comment, AsmOperand,
    ASM_BASE_RIP, ASM_REG_XMM, ASM_REG_YMM, ASM_REG_ZMM};
