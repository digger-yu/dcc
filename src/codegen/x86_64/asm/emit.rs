// src/codegen/x86_64/asm/emit.rs
// Assembly emission functions for x86_64 backend
// Contains main entry points for inline assembly code generation
// Extracted from src/codegen/x86_64/mod.rs (PR-A2 Phase)

use crate::ast::Expr;
use crate::codegen::x86_64::{AsmDirectiveExpander, AsmLocalLabels, Codegen};

/// Context for ASM operand binding
#[derive(Debug, Clone)]
pub struct Binding {
    pub reg: u8,
    pub xmm_width: u16,
    pub is_output: bool,
    pub is_mem: bool,
}

impl Codegen {
    /// 发射内联汇编模板
    /// 
    /// This function extracts the complete emit_inline_asm implementation
    /// originally at line 32455-32543 in mod.rs (~90 lines of code)
    pub fn emit_inline_asm(
        &mut self,
        asm_str: &str,
        outputs: &[Expr],
        output_names: &[Option<String>],
        output_constraints: &[String],
        inputs: &[Expr],
        input_names: &[Option<String>],
        input_constraints: &[String],
        clobbers: &[String],
        volatile: bool,
    ) {
        let mut bindings = Vec::new();
        let op_map = self.build_asm_operand_map(
            outputs,
            output_names,
            output_constraints,
            inputs,
            input_names,
            input_constraints,
            clobbers,
            &mut bindings,
        );
        let s = if asm_str.starts_with('"') && asm_str.ends_with('"') && asm_str.len() >= 2 {
            &asm_str[1..asm_str.len() - 1]
        } else {
            asm_str
        };
        // 先展开高级指令 (.rept/.macro/.if 等)
        let raw_lines: Vec<String> = s
            .split(['\n', ';'])
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        // 合并独立成行的前缀 (lock/rep/repz/repnz/repe) 与下一条指令
        let raw_lines = Self::merge_standalone_prefixes(raw_lines);
        let mut expander = AsmDirectiveExpander::new();
        let expanded_lines = expander.expand(&raw_lines);
        // 再解析操作数占位符
        let resolved_lines = AsmLocalLabels::expand_label_insn_lines(
            expanded_lines
                .iter()
                .map(|line| {
                    self.resolve_asm_template(
                        line.trim(),
                        outputs,
                        output_names,
                        inputs,
                        input_names,
                        &op_map,
                    )
                })
                .filter(|l| !l.trim().is_empty())
                .collect::<Vec<_>>(),
        );
        // volatile asm: 完整屏障，防止优化器重排
        // 若代码尾部已是 mfence (前一 volatile asm 的后屏障), 跳过冗余前屏障
        if volatile {
            let code_len = self.code.len();
            let already_fenced = code_len >= 3
                && self.code[code_len - 3] == 0x0F
                && self.code[code_len - 2] == 0xAE
                && self.code[code_len - 1] == 0xF0;
            if !already_fenced {
                self.emit(&[0x0F, 0xAE, 0xF0]); // mfence
            }
        }
        // 记录 asm 块起始 PC (用于 DWARF 行号表)
        let asm_pc_start = self.code.len() as u64;
        let asm_src_line = self.current_asm_src_line;
        self.emit_resolved_asm_lines(&resolved_lines, false);
        // 记录 asm 块 DWARF 行号条目
        if self.debug_level > 0 && asm_src_line > 0 {
            let asm_pc_end = self.code.len() as u64;
            if asm_pc_end > asm_pc_start {
                self.asm_line_entries
                    .push((asm_pc_start, asm_pc_end, asm_src_line));
            }
        }
        for (i, binding) in bindings.iter().enumerate() {
            if binding.is_output && !binding.is_mem && i < outputs.len() {
                self.writeback_asm_operand(&outputs[i], binding.reg, binding.xmm_width);
            }
        }
        if clobbers.iter().any(|c| c == "memory") || volatile {
            // memory clobber 或 volatile: 编译器屏障，防止指令重排越过 asm
            self.emit(&[0x0F, 0xAE, 0xF0]); // mfence
        }
    }

    /// asm goto 专用：与 emit_inline_asm 相同，但输出写回仅 fall-through 路径生效。
    /// GCC 语义：asm goto 输出约束写回仅 fall-through 路径生效;
    /// 跳转分支保留原值。实现方式：asm 执行后直接写回输出;
    /// 若 asm 内部跳转到 goto label，控制流跳过写回代码，变量保持初值。
    pub fn emit_inline_asm_goto(
        &mut self,
        asm_str: &str,
        outputs: &[Expr],
        output_names: &[Option<String>],
        output_constraints: &[String],
        inputs: &[Expr],
        input_names: &[Option<String>],
        input_constraints: &[String],
        clobbers: &[String],
    ) {
        let mut bindings = Vec::new();
        let op_map = self.build_asm_operand_map(
            outputs,
            output_names,
            output_constraints,
            inputs,
            input_names,
            input_constraints,
            clobbers,
            &mut bindings,
        );
        let s = if asm_str.starts_with('"') && asm_str.ends_with('"') && asm_str.len() >= 2 {
            &asm_str[1..asm_str.len() - 1]
        } else {
            asm_str
        };
        // 先展开高级指令 (.rept/.macro/.if 等)
        let raw_lines: Vec<String> = s
            .split(['\n', ';'])
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        // 合并独立成行的前缀 (lock/rep/repz/repnz/repe) 与下一条指令
        let raw_lines = Self::merge_standalone_prefixes(raw_lines);
        let mut expander = AsmDirectiveExpander::new();
        let expanded_lines = expander.expand(&raw_lines);
        // 再解析操作数占位符
        let resolved_lines = AsmLocalLabels::expand_label_insn_lines(
            expanded_lines
                .iter()
                .map(|line| {
                    self.resolve_asm_template(
                        line.trim(),
                        outputs,
                        output_names,
                        inputs,
                        input_names,
                        &op_map,
                    )
                })
                .filter(|l| !l.trim().is_empty())
                .collect::<Vec<_>>(),
        );
        // 记录 asm goto 块起始 PC (用于 DWARF 行号表)
        let asm_pc_start = self.code.len() as u64;
        let asm_src_line = self.current_asm_src_line;
        self.emit_resolved_asm_lines(&resolved_lines, false);
        // 记录 asm goto 块 DWARF 行号条目
        if self.debug_level > 0 && asm_src_line > 0 {
            let asm_pc_end = self.code.len() as u64;
            if asm_pc_end > asm_pc_start {
                self.asm_line_entries
                    .push((asm_pc_start, asm_pc_end, asm_src_line));
            }
        }
        // asm goto: fall-through 路径执行输出写回; 若 asm 内部跳转到 goto label,
        // 控制流跳过此处的写回代码，变量保持初值 (GCC 语义)。
        for (i, binding) in bindings.iter().enumerate() {
            if binding.is_output && !binding.is_mem && i < outputs.len() {
                self.writeback_asm_operand(&outputs[i], binding.reg, binding.xmm_width);
            }
        }
        if clobbers.iter().any(|c| c == "memory") {
            self.emit(&[0x0F, 0xAE, 0xF0]); // mfence
        }
    }
}
