// ARM backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::codegen::shared::elf;
use crate::ir::Module;

use super::struct_def::ArmCodegen;

impl ArmCodegen {
    pub(crate) fn build_output(&self) -> Vec<u8> {
        if self.relocatable {
            elf::build_elf32_arm(
                &self.code,
                &self.rodata,
                &self.symbols,
                &self.relocations,
                &self.rodata_relocations,
                &self.extra_sections,
            )
        } else {
            elf::build_elf32_exec_arm(&self.code, &self.rodata, self.entry as u32)
        }
    }

    pub(crate) fn build_asm_output(&self, module: &Module) -> String {
        let mut out = String::new();
        let name = if self.source_name.is_empty() {
            "input.c".to_string()
        } else {
            self.source_name.clone()
        };
        out.push_str(&format!("\t.file \"{name}\"\n"));
        if !self.rodata.is_empty() {
            out.push_str("\t.section .rodata\n");
        }
        out.push_str("\t.text\n");
        // 发射文件级 asm 原始文本
        for line in &self.raw_global_asm_lines {
            out.push_str(&format!("\t{line}\n"));
        }
        for func in &module.functions {
            out.push_str(&format!("\t.globl {}\n", func.name));
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
                out.push_str(&format!("\t@ ARM IR lowering, {} bytes\n", size));
            }
            out.push_str("\tpush {fp, lr}\n");
            out.push_str("\tmov fp, sp\n");
            out.push_str("\tmov r0, #0\n");
            out.push_str("\tpop {fp, pc}\n");
        }
        out
    }
}
