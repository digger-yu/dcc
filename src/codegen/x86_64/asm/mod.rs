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
// P10: render_one 已移出本目录 (x86_64/render_one.rs), 它现在只服务原生
// 发射路径的 -S 文本反推, 不再是内联汇编专属.
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
pub(crate) mod to_typed; // AsmOperand→X86_64Opcode 转换层 (方案 3)

impl crate::codegen::x86_64::Codegen {
    /// 登记一条 RIP-relative 数据符号引用，并声明 disp32 之后还有几个字节。
    ///
    /// `tail` 只度量**同一条指令内** disp32 之后的字节（`addl $imm, sym(%rip)` =
    /// imm 长度；`vpextr … imm8` = 1），跨指令的后续字节不算。PC32 的加数是
    /// `off - 4 - tail`：A 要凑到指令尾，而旧实现一律 `off - 4`，凡是立即数在
    /// disp32 后面的形状都写偏 tail 个字节（实测 `addl $9, g(%rip)` 写到 `g+1`，
    /// 读回来是 `0x900` 而不是 9；内核 `_static_cpu_has` 的 `testb` 也踩过同一坑，
    /// 那里已就地修成 `-4 - imm_size`，见报告 §5.28）。
    pub(crate) fn push_rip_data_rel(&mut self, disp32_pos: u64, sym: String, tail: u8) {
        self.pending_data_rels.push((disp32_pos, sym));
        if tail != 0 {
            self.pending_data_rel_tails.insert(disp32_pos, tail);
        }
    }
}
