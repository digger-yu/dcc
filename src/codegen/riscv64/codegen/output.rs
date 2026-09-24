// RISC-V 64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::codegen::riscv64::codegen::{Riscv64Codegen, find_func};
use crate::codegen::shared::asm_constraints::{
    self, resolve_asm_operands, resolve_asm_template_enhanced, AsmArch,
};
use crate::codegen::shared::elf::{self};
use crate::ir::{Module};

impl Riscv64Codegen {
    pub(crate) fn build_output(&self) -> Vec<u8> {
        if self.relocatable {
            elf::build_elf_rel_riscv64(
                &self.code,
                &self.rodata,
                &self.symbols,
                &self.relocations,
                &self.rodata_relocations,
                &self.extra_sections,
                &self.comdat_groups,
            )
        } else {
            elf::build_elf_riscv64(&self.code, &self.rodata, self.entry)
        }
    }

    pub(crate) fn build_asm_output(&self, module: &Module) -> String {
        let prog = module.source();
        let mut out = String::new();
        let name = if self.source_name.is_empty() {
            "input.c".to_string()
        } else {
            self.source_name.clone()
        };
        out.push_str(&format!("\t.file \"{name}\"\n"));
        if !self.rodata.is_empty() {
            out.push_str("\t.section .rodata\n");
            for g in &module.globals {
                if self.global_rodata.contains_key(&g.name) {
                    out.push_str(&format!("\t.globl {}\n", g.name));
                    out.push_str(&format!("{}:\n", g.name));
                }
            }
        }
        out.push_str("\t.text\n");
        // 发射文件级 asm 原始文本
        for line in &self.raw_global_asm_lines {
            out.push_str(&format!("\t{line}\n"));
        }
        for func in &module.functions {
            let is_weak = find_func(prog, &func.name)
                .map(|f| f.is_weak)
                .unwrap_or(false);
            if is_weak {
                out.push_str(&format!("\t.weak {}\n", func.name));
            } else {
                out.push_str(&format!("\t.globl {}\n", func.name));
            }
            out.push_str(&format!("{}:\n", func.name));
            // 发射函数级内联 asm 原始文本
            for (fname, lines) in &self.raw_func_asm_lines {
                if fname == &func.name || fname.starts_with("__inline_") {
                    for line in lines {
                        out.push_str(&format!("\t{line}\n"));
                    }
                }
            }
            if let Some(size) = self.func_sizes.get(&func.name) {
                out.push_str(&format!("\t// IR lowering, {} bytes\n", size));
            }
            out.push_str("\tret\n");
        }
        out
    }

    /// 增强版 asm 模板占位符替换: 使用共享约束解析器分配寄存器并替换为实际寄存器名
    pub(crate) fn resolve_asm_template_enhanced(
        asm_str: &str,
        output_constraints: &[String],
        input_constraints: &[String],
        output_names: &[Option<String>],
        input_names: &[Option<String>],
        goto_labels: &[(String, String)],
    ) -> String {
        let pool = asm_constraints::riscv64_reg_pool();
        let operands = resolve_asm_operands(
            output_constraints,
            input_constraints,
            output_names,
            input_names,
            &pool,
        );
        resolve_asm_template_enhanced(
            asm_str,
            &operands,
            output_names,
            input_names,
            goto_labels,
            AsmArch::RiscV64,
            true,
        )
    }

}
