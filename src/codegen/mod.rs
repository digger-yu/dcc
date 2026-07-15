// src/codegen/mod.rs - 代码生成调度
// 第一性原理: codegen 入口只做"按 target 选后端"。
// suckless 准则: 一行分发。

pub mod elf;
pub mod x86_64;

use crate::ir::Module;
use crate::target::Target;

pub fn emit(module: &Module, target: &Target, relocatable: bool, emit_asm: bool, source_name: &str) -> Vec<u8> {
    match target.arch {
        crate::cli::Arch::X86_64 => {
            let mut cg = x86_64::Codegen::new(target.clone());
            cg.relocatable = relocatable || emit_asm;
            cg.emit_asm_text = emit_asm;
            cg.source_name = source_name.to_string();
            cg.compile(module.source());
            if emit_asm {
                return cg.build_asm_output().into_bytes();
            }
            if relocatable {
                elf::build_elf_rel_x86_64(
                    &cg.code, &cg.rodata, &cg.data_section,
                    cg.bss_size, cg.text_align, cg.data_align, cg.bss_align,
                    &cg.symbols, &cg.relocations,
                    &cg.extra_sections, &cg.data_relocations,
                    &cg.extra_rela_sections,
                )
            } else {
                elf::build_elf_x86_64(&cg.code, cg.entry)
            }
        }
        a => panic!("arch {:?} not yet implemented (M0: x86_64 only)", a),
    }
}
