// AArch64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::codegen::aarch64::codegen::{find_func, value_type_size, Aarch64Codegen};
use crate::codegen::shared::asm_constraints::{
    self, resolve_asm_operands, resolve_asm_template_enhanced, AsmArch,
};
use crate::codegen::shared::elf::{self};
use crate::ir::Module;

impl Aarch64Codegen {
    pub(crate) fn build_output(&self) -> Vec<u8> {
        if self.relocatable {
            elf::build_elf_rel_aarch64(
                &self.code,
                &self.rodata,
                &self.symbols,
                &self.relocations,
                &self.rodata_relocations,
                &self.extra_sections,
                &self.comdat_groups,
            )
        } else {
            elf::build_elf_aarch64(&self.code, &self.rodata, self.entry)
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
            out.push_str("\t.section .rodata,\"a\",@progbits\n");
            for g in &module.globals {
                if let Some(&off) = self.global_rodata.get(&g.name) {
                    out.push_str(&format!("\t.globl {}\n", g.name));
                    out.push_str(&format!("\t.type {}, %object\n", g.name));
                    out.push_str(&format!("\t.size {}, {}\n", g.name, value_type_size(&g.ty)));
                    out.push_str(&format!("{}:\n", g.name));
                    out.push_str(&format!("\t// rodata offset {off}\n"));
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
            out.push_str(&format!("\t.type {}, %function\n", func.name));
            // -fpatchable-function-entry: emit NOPs in asm output
            if let Some((n, m)) = self._target.patchable_function_entry {
                let prefix_len = m.min(n);
                let suffix_len = n.saturating_sub(prefix_len);
                if n > 0 {
                    out.push_str("\t.section .patchable_function_entry,\"aw\",@progbits\n");
                }
                for _ in 0..prefix_len {
                    out.push_str("\tnop\n");
                }
                out.push_str(&format!("{}:\n", func.name));
                for _ in 0..suffix_len {
                    out.push_str("\tnop\n");
                }
                if n > 0 {
                    out.push_str("\t.text\n");
                }
            } else {
                out.push_str(&format!("{}:\n", func.name));
            }
            // 发射函数级内联 asm 原始文本
            for (fname, lines) in &self.raw_func_asm_lines {
                if fname == &func.name || fname.starts_with("__inline_") {
                    for line in lines {
                        out.push_str(&format!("\t{line}\n"));
                    }
                }
            }
            if let Some(size) = self.func_sizes.get(&func.name) {
                out.push_str(&format!("\t// IR lowering, {} bytes machine code\n", size));
            }
            out.push_str("\tret\n");
            out.push_str(&format!("\t.size {}, .-{}\n", func.name, func.name));
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
        let pool = asm_constraints::aarch64_reg_pool();
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
            AsmArch::AArch64,
            false,
        )
    }
}
