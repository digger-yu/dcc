//! asm/global.rs —— 文件级 `asm(...)` (Decl::GlobalAsm) 发射。
//! 自 monolith (28a9063) 逐字恢复：迁移期间调用点被注释并留下
//! "TODO: Implement emit_global_asm" 影子, 导致 .type/@function 标签、
//! 全局汇编符号整体丢失。仅移动/可见性调整, 不改语义。
#![allow(clippy::all)]
#![allow(unused_imports)]
use crate::ast::*;
use crate::codegen::shared::elf::*;
use crate::codegen::x86_64::asm::*;
use crate::codegen::x86_64::Codegen;
use crate::codegen::x86_64::*;

impl Codegen {
    pub(crate) fn emit_global_asm(
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
        self.emit_resolved_asm_lines(&resolved_lines, true);
        for (i, binding) in bindings.iter().enumerate() {
            if binding.is_output && !binding.is_mem && i < outputs.len() {
                self.writeback_asm_operand(
                    &outputs[i],
                    binding.reg,
                    binding.xmm_width,
                    binding.saved_inout_addr_reg,
                );
            }
        }
        let _ = clobbers;
    }
}
