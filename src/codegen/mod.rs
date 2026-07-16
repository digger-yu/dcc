// src/codegen/mod.rs - 代码生成调度
// 第一性原理: codegen 入口只做"按 target 选后端"。

pub mod disasm;
pub mod elf;
pub mod x86_64;
pub mod aarch64;

use crate::debug;
use crate::ir::Module;
use crate::lto;
use crate::sanitizer::{SanitizerFlags, sanitizer_metadata};
use crate::target::Target;

pub fn emit(
    module: &Module,
    target: &Target,
    relocatable: bool,
    emit_asm: bool,
    source_name: &str,
    debug_level: u8,
    sanitizers: SanitizerFlags,
    lto_enabled: bool,
    function_sections: bool,
    parallel_codegen: bool,
) -> Vec<u8> {
    match target.arch {
        crate::cli::Arch::X86_64 => {
            let mut cg = x86_64::Codegen::new(target.clone());
            cg.relocatable = relocatable || emit_asm;
            cg.function_sections = function_sections || lto_enabled;
            cg.emit_asm_text = emit_asm;
            cg.parallel_codegen = parallel_codegen && !emit_asm;
            cg.source_name = source_name.to_string();
            cg.debug_level = debug_level;
            cg.emit_cfi = debug_level > 0 || target.frame_pointer;
            cg.compile(module.source());
            if emit_asm {
                return cg.build_asm_output(module.source()).into_bytes();
            }
            if debug_level > 0 && relocatable {
                let funcs: Vec<(&str, u64)> = module
                    .functions
                    .iter()
                    .filter_map(|f| cg.labels.get(&f.name).map(|&pc| (f.name.as_str(), pc)))
                    .collect();
                let dbg = debug::build_dwarf(source_name, &funcs);
                cg.extra_sections.extend(debug::dwarf_elf_sections(&dbg));
                cg.extra_sections.push(elf::ElfSection {
                    name: ".BTF".to_string(),
                    data: debug::build_btf_probe(),
                    ty: elf::SHT_PROGBITS,
                    flags: 0,
                    align: 4,
                });
            }
            if cg.emit_cfi && relocatable {
                let frames: Vec<debug::FuncFrame> = module
                    .functions
                    .iter()
                    .filter_map(|f| {
                        cg.labels.get(&f.name).and_then(|&pc| {
                            cg.func_sizes.get(&f.name).map(|&sz| debug::FuncFrame {
                                pc,
                                size: sz as u32,
                            })
                        })
                    })
                    .collect();
                cg.extra_sections.push(elf::ElfSection {
                    name: ".eh_frame".to_string(),
                    data: debug::build_eh_frame(&frames),
                    ty: elf::SHT_PROGBITS,
                    flags: elf::SHF_ALLOC,
                    align: 8,
                });
                cg.extra_sections.push(elf::ElfSection {
                    name: ".orc_unwind_ip".to_string(),
                    data: debug::build_orc_unwind_ip(&frames),
                    ty: elf::SHT_PROGBITS,
                    flags: elf::SHF_ALLOC,
                    align: 8,
                });
            }
            if sanitizers.any() {
                cg.extra_sections.push(elf::ElfSection {
                    name: ".dcc.san_info".to_string(),
                    data: sanitizer_metadata(sanitizers),
                    ty: elf::SHT_PROGBITS,
                    flags: 0,
                    align: 1,
                });
            }
            if lto_enabled {
                let summaries: Vec<(&str, u64, bool)> = cg
                    .func_sizes
                    .iter()
                    .map(|(name, size)| (name.as_str(), *size as u64, true))
                    .collect();
                let unit = lto::build_unit(source_name, &summaries);
                cg.extra_sections.push(elf::ElfSection {
                    name: ".llvm.lto".to_string(),
                    data: lto::serialize_summary(&unit),
                    ty: elf::SHT_PROGBITS,
                    flags: 0,
                    align: 1,
                });
            }
            if relocatable {
                elf::build_elf_rel_x86_64(
                    &cg.code,
                    &cg.rodata,
                    &cg.data_section,
                    cg.bss_size,
                    cg.text_align,
                    cg.data_align,
                    cg.bss_align,
                    &cg.symbols,
                    &cg.relocations,
                    &cg.extra_sections,
                    &cg.data_relocations,
                    &cg.extra_rela_sections,
                )
            } else {
                elf::build_elf_x86_64(&cg.code, cg.entry)
            }
        }
        crate::cli::Arch::AArch64 => {
            let mut cg = aarch64::Aarch64Codegen::new(target.clone(), relocatable || emit_asm);
            cg.compile(module.source());
            cg.build_output()
        }
        a => panic!("arch {:?} not yet implemented (M0: x86_64 only)", a),
    }
}
