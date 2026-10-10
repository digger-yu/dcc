// src/codegen/x86_64/asm/mod.rs
// Inline assembly module for x86_64 backend
// Provides modular structure for assembly emission and parsing
// Part of PR-A2: ASM Module Extraction (Big-Bang refactoring)

pub mod binding; // AsmOpBinding / AsmDataExprValue (asm 操作数绑定数据)
                 // encoder 已按职责簇拆分为 constraint/opmap/template_resolve/directive_emit/
                 // naked/emit_lines/enc_mov/enc_alu/enc_ctrl/enc_simd/enc_misc（见本文件底部声明）
pub mod directive;
pub mod emit;
pub mod expansion; // Assembly directive expansion
pub mod label;
pub mod labels; // NEW: Numeric local label handling (Turn 34)
pub mod operand;
pub mod planner;
pub mod render_one; // P9 方案 C: 单指令 x86-64 解码 → GAS 文本
pub mod template; // NEW: Register allocator planning

// Re-export main utilities
pub use binding::{AsmDataExprValue, AsmOpBinding};
pub use directive::{AsmDirectiveExpander, AsmDirectiveExpander as LegacyExpander};
pub use expansion::{ExplicitRegInfo, ExplicitRegKind};
pub use labels::AsmLocalLabels; // Turn 34: Added numeric label support
                                // pub use emit::{emit_inline_asm, emit_resolved_asm_lines};  // Temporarily disabled during refactor
pub use operand::{
    parse_asm_operand_str, strip_asm_comment, AsmOperand, ASM_BASE_RIP, ASM_REG_CR, ASM_REG_DR,
    ASM_REG_K, ASM_REG_MMX, ASM_REG_ST, ASM_REG_TMM, ASM_REG_XMM, ASM_REG_YMM, ASM_REG_ZMM,
};
// pub use bindings::AsmOpBinding;  // Disabled - file deleted
pub use planner::{AsmRegUse, AsmRegisterPlanner};
pub(crate) mod constraint;
pub(crate) mod directive_emit;
pub(crate) mod emit_lines;
pub(crate) mod enc_alu;
pub(crate) mod enc_ctrl;
pub(crate) mod enc_misc;
pub(crate) mod enc_mov;
pub(crate) mod enc_simd;
pub(crate) mod global;
pub(crate) mod naked;
pub(crate) mod opmap;
pub(crate) mod template_resolve;
