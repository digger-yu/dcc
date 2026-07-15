// src/codegen/mod.rs - 代码生成调度
// 第一性原理: codegen 入口只做"按 target 选后端"。
// suckless 准则: 一行分发。

pub mod elf;
pub mod x86_64;

use crate::ast::Program;
use crate::target::Target;

pub fn emit(prog: &Program, target: &Target, relocatable: bool) -> Vec<u8> {
    match target.arch {
        crate::cli::Arch::X86_64 => {
            let mut cg = x86_64::Codegen::new(target.clone());
            cg.relocatable = relocatable;
            cg.compile(prog);
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
