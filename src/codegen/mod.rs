// src/codegen/mod.rs - 代码生成调度
// 第一性原理: codegen 入口只做"按 target 选后端"。

pub mod aarch64;
pub mod arm;
pub mod disasm;
pub mod elf;
pub mod mips;
pub mod mips64;
pub mod riscv64;
pub mod x86_64;

use crate::debug;
use crate::ir::Module;
use crate::preproc::LineSpan;
use crate::sanitizer::{sanitizer_metadata, SanitizerFlags};
use crate::target::Target;

#[allow(clippy::too_many_arguments)]
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
    bounds_check: bool,
    trapv: bool,
    offload: Option<&crate::profile::offload::OffloadSection>,
    line_map: &[LineSpan],
) -> Vec<u8> {
    match target.arch {
        crate::cli::Arch::X86_64 => {
            let mut cg = x86_64::Codegen::new(target.clone());
            cg.relocatable = relocatable || emit_asm;
            cg.function_sections = function_sections || lto_enabled;
            cg.emit_asm_text = emit_asm;
            cg.parallel_codegen = parallel_codegen && !emit_asm;
            cg.source_name = source_name.to_string();
            cg.line_map = line_map.to_vec();
            cg.debug_level = debug_level;
            cg.emit_cfi = debug_level > 0 || target.frame_pointer;
            cg.bounds_check = bounds_check;
            cg.trapv = trapv;
            cg.kasan = sanitizers.kasan;
            cg.compile(module);
            if emit_asm {
                return cg.build_asm_output(module.source()).into_bytes();
            }
            if debug_level > 0 && relocatable {
                let funcs: Vec<(&str, u64)> = module
                    .functions
                    .iter()
                    .filter_map(|f| cg.labels.get(&f.name).map(|&pc| (f.name.as_str(), pc)))
                    .collect();
                // 将 asm 块行号记录转换为 DWARF LineEntry
                let asm_line_entries: Vec<debug::LineEntry> = cg
                    .asm_line_entries
                    .iter()
                    .map(|&(pc_start, _pc_end, src_line)| debug::LineEntry {
                        pc: pc_start,
                        line: src_line,
                        column: 1,
                        file_idx: 0,
                        discriminator: 0,
                        is_stmt: true,
                        basic_block: false,
                    })
                    .collect();
                let mut dbg = debug::build_dwarf(source_name, &funcs);
                // 如果有 asm 行号条目, 重新生成带行号的 DWARF
                if !asm_line_entries.is_empty() {
                    let cu = debug::CompileUnit {
                        source_name: source_name.to_string(),
                        comp_dir: None,
                        producer: format!("dcc {}", env!("CARGO_PKG_VERSION")),
                        language: 0x000C, // DW_LANG_C99
                        functions: funcs
                            .iter()
                            .map(|(name, pc)| debug::DwarfFuncInfo {
                                name: name.to_string(),
                                linkage_name: None,
                                low_pc: *pc,
                                high_pc: *pc + cg.func_sizes.get(*name).copied().unwrap_or(16),
                                decl_line: 0,
                                decl_file_idx: 0,
                                return_type: debug::DwarfType {
                                    name: "void".to_string(),
                                    byte_size: 0,
                                    kind: debug::DwarfTypeKind::Base(debug::TypeEncoding::Unsigned),
                                },
                                params: vec![],
                                is_external: true,
                                is_inline: false,
                                is_declaration: false,
                            })
                            .collect(),
                        globals: vec![],
                        types: vec![],
                        line_entries: asm_line_entries,
                        line_files: vec![source_name.to_string()],
                        line_dirs: vec![".".to_string()],
                        low_pc: cg.asm_line_entries.first().map(|e| e.0).unwrap_or(0),
                        high_pc: cg.asm_line_entries.last().map(|e| e.1).unwrap_or(0),
                        func_ranges: funcs.iter().map(|(name, pc)| {
                            (*pc, *pc + cg.func_sizes.get(*name).copied().unwrap_or(16))
                        }).collect(),
                        cfi_rows: vec![],
                    };
                    let opts = debug::DebugOptions {
                        level: debug_level,
                        source_name: source_name.to_string(),
                        ..Default::default()
                    };
                    dbg = debug::build_dwarf_full(&cu, &opts);
                }
                cg.extra_sections.extend(debug::dwarf_elf_sections(&dbg));
                cg.extra_sections.push(elf::ElfSection {
                    name: ".BTF".to_string(),
                    data: debug::build_btf_probe(),
                    ty: elf::SHT_PROGBITS,
                    flags: 0,
                    align: 4,
                });
            }
            if cg.emit_cfi && relocatable && target.unwind_tables {
                // dcc 输出 .eh_frame FDE 需带 .rela.eh_frame relocations 才能在 ld 链接时
                // 正确解析 PC begin / PC range. 之前 (M15 阶段) 临时跳过是因 R_X86_64_PC32
                // 未生成, 报 "PHDR segment not covered by LOAD segment". 现 relocation 已
                // 正常生成 (.rela.eh_frame, 见 mod.rs 下方), 这里默认恢复 .eh_frame 输出,
                // setjmp / longjmp / C++ 异常等栈展开路径可正确恢复 callee-saved 寄存器.
                // 注意: -fno-asynchronous-unwind-tables 时不生成 .eh_frame (内核等场景).
                let frames: Vec<debug::FuncFrame> = cg.func_frames(module);
                cg.extra_sections.push(elf::ElfSection {
                    name: ".eh_frame".to_string(),
                    data: debug::build_eh_frame(&frames),
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
            if let Some(o) = offload {
                if !o.is_empty() {
                    cg.extra_sections.push(elf::ElfSection {
                        name: ".dcc.offload".to_string(),
                        data: o.to_binary(),
                        ty: elf::SHT_PROGBITS,
                        flags: 0,
                        align: 8,
                    });
                }
            }
            // ifunc 属性: 收集所有 ifunc 函数, 生成 .dcc.ifunc 节
            // 格式: 每条记录 = u16 name_len + name bytes + u16 resolver_len + resolver bytes
            if relocatable {
                let mut ifunc_data = Vec::new();
                let prog = module.source();
                for decl in &prog.decls {
                    if let crate::ast::Decl::Func(f) = decl {
                        if let Some(resolver) = &f.ifunc_resolver {
                            let name = f.name.name.as_bytes();
                            let res = resolver.as_bytes();
                            ifunc_data.extend_from_slice(&(name.len() as u16).to_le_bytes());
                            ifunc_data.extend_from_slice(name);
                            ifunc_data.extend_from_slice(&(res.len() as u16).to_le_bytes());
                            ifunc_data.extend_from_slice(res);
                        }
                    }
                }
                if !ifunc_data.is_empty() {
                    cg.extra_sections.push(elf::ElfSection {
                        name: ".dcc.ifunc".to_string(),
                        data: ifunc_data,
                        ty: elf::SHT_PROGBITS,
                        flags: 0,
                        align: 1,
                    });
                }
            }
            if lto_enabled {
                // 嵌入 dcc 自有 IR 位码（供 src/link 多 TU LTO 使用）
                let bc = crate::link::lto_bitcode::serialize_module(module, source_name);
                cg.extra_sections.push(elf::ElfSection {
                    name: ".dcc.lto".to_string(),
                    data: bc,
                    ty: elf::SHT_PROGBITS,
                    flags: 0,
                    align: 1,
                });
                // 同时输出 .llvm.lto 文本摘要，兼容：
                //   1) GC-sections 探测（外部工具检查 .llvm.lto 判断是否有 LTO 信息）
                //   2) ld -r 合并路径（lto.rs 读写 .llvm.lto 进行跨 TU 合并）
                // 与 .dcc.lto DBC 位码并存，两者职责不同。
                let funcs: Vec<(&str, u64, bool)> = module
                    .functions
                    .iter()
                    .map(|f| {
                        let sz = cg.func_sizes.get(&f.name).copied().unwrap_or(0);
                        (f.name.as_str(), sz, true)
                    })
                    .collect();
                let summary_unit = crate::lto::build_unit(source_name, &funcs);
                cg.extra_sections.push(elf::ElfSection {
                    name: ".llvm.lto".to_string(),
                    data: crate::lto::serialize_summary(&summary_unit),
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
                    &cg.tdata_section,
                    cg.tbss_size,
                    cg.text_align,
                    cg.data_align,
                    cg.bss_align,
                    cg.tdata_align,
                    cg.tbss_align,
                    &cg.symbols,
                    &cg.relocations,
                    &cg.extra_sections,
                    &cg.data_relocations,
                    &cg.extra_rela_sections,
                )
            } else {
                elf::build_elf_exec_x86_64(
                    &cg.code,
                    cg.entry,
                    cg.exec_text_size,
                    cg.rodata.len() as u64,
                    cg.tdata_section.len() as u64,
                    cg.tbss_size,
                    &cg.symbols,
                )
            }
        }
        crate::cli::Arch::AArch64 => {
            let mut cg = aarch64::Aarch64Codegen::new(target.clone(), relocatable || emit_asm);
            cg.set_source_name(source_name);
            if emit_asm {
                cg.compile(module);
                return cg.build_asm_output(module).into_bytes();
            }
            cg.compile(module);
            if debug_level > 0 && relocatable {
                let funcs: Vec<(&str, u64)> = module
                    .functions
                    .iter()
                    .filter_map(|f| cg.labels.get(&f.name).map(|&pc| (f.name.as_str(), pc)))
                    .collect();
                let opts = debug::DebugOptions {
                    level: debug_level,
                    source_name: source_name.to_string(),
                    arch: debug::ArchKind::AArch64,
                    ..debug::DebugOptions::default()
                };
                let cu = debug::CompileUnit {
                    source_name: source_name.to_string(),
                    comp_dir: None,
                    producer: "dcc".to_string(),
                    language: 12, // DW_LANG_C99
                    functions: funcs
                        .iter()
                        .map(|(n, pc)| debug::DwarfFuncInfo {
                            name: n.to_string(),
                            linkage_name: None,
                            low_pc: *pc,
                            high_pc: *pc + 64,
                            decl_line: 0,
                            decl_file_idx: 0,
                            return_type: debug::DwarfType {
                                name: "void".to_string(),
                                byte_size: 0,
                                kind: debug::DwarfTypeKind::Void,
                            },
                            params: Vec::new(),
                            is_external: *n == "main",
                            is_inline: false,
                            is_declaration: false,
                        })
                        .collect(),
                    globals: Vec::new(),
                    types: Vec::new(),
                    line_entries: Vec::new(),
                    line_files: vec![source_name.to_string()],
                    line_dirs: Vec::new(),
                    low_pc: funcs.first().map(|(_, pc)| *pc).unwrap_or(0),
                    high_pc: funcs.last().map(|(_, pc)| *pc + 64).unwrap_or(0),
                    func_ranges: funcs.iter().map(|(_, pc)| (*pc, 64)).collect(),
                    cfi_rows: Vec::new(),
                };
                let dbg = debug::build_dwarf_full(&cu, &opts);
                let extra_secs = debug::dwarf_elf_sections(&dbg);
                cg.push_extra_section(elf::ElfSection {
                    name: ".debug_info".to_string(),
                    data: dbg.info,
                    ty: elf::SHT_PROGBITS,
                    flags: 0,
                    align: 1,
                });
                for sec in extra_secs {
                    cg.push_extra_section(sec);
                }
                cg.push_extra_section(elf::ElfSection {
                    name: ".BTF".to_string(),
                    data: debug::build_btf_probe(),
                    ty: elf::SHT_PROGBITS,
                    flags: 0,
                    align: 4,
                });
            }
            if relocatable && target.unwind_tables {
                let frames = cg.func_frames(module);
                if !frames.is_empty() {
                    cg.push_extra_section(elf::ElfSection {
                        name: ".eh_frame".to_string(),
                        data: debug::build_eh_frame_with_arch(&frames, debug::ArchKind::AArch64),
                        ty: elf::SHT_PROGBITS,
                        flags: elf::SHF_ALLOC,
                        align: 8,
                    });
                }
            }
            if sanitizers.any() {
                cg.push_extra_section(elf::ElfSection {
                    name: ".dcc.san_info".to_string(),
                    data: sanitizer_metadata(sanitizers),
                    ty: elf::SHT_PROGBITS,
                    flags: 0,
                    align: 1,
                });
            }
            if let Some(o) = offload {
                if !o.is_empty() {
                    cg.push_extra_section(elf::ElfSection {
                        name: ".dcc.offload".to_string(),
                        data: o.to_binary(),
                        ty: elf::SHT_PROGBITS,
                        flags: 0,
                        align: 8,
                    });
                }
            }
            cg.build_output()
        }
        crate::cli::Arch::RiscV64 => {
            let mut cg = riscv64::Riscv64Codegen::new(target.clone(), relocatable || emit_asm);
            cg.set_source_name(source_name);
            if emit_asm {
                cg.compile(module);
                return cg.build_asm_output(module).into_bytes();
            }
            cg.compile(module);
            if debug_level > 0 && relocatable {
                let funcs: Vec<(&str, u64)> = module
                    .functions
                    .iter()
                    .filter_map(|f| cg.labels.get(&f.name).map(|&pc| (f.name.as_str(), pc)))
                    .collect();
                let opts = debug::DebugOptions {
                    level: debug_level,
                    source_name: source_name.to_string(),
                    arch: debug::ArchKind::RiscV64,
                    ..debug::DebugOptions::default()
                };
                let cu = debug::CompileUnit {
                    source_name: source_name.to_string(),
                    comp_dir: None,
                    producer: "dcc".to_string(),
                    language: 12, // DW_LANG_C99
                    functions: funcs
                        .iter()
                        .map(|(n, pc)| debug::DwarfFuncInfo {
                            name: n.to_string(),
                            linkage_name: None,
                            low_pc: *pc,
                            high_pc: *pc + 64,
                            decl_line: 0,
                            decl_file_idx: 0,
                            return_type: debug::DwarfType {
                                name: "void".to_string(),
                                byte_size: 0,
                                kind: debug::DwarfTypeKind::Void,
                            },
                            params: Vec::new(),
                            is_external: *n == "main",
                            is_inline: false,
                            is_declaration: false,
                        })
                        .collect(),
                    globals: Vec::new(),
                    types: Vec::new(),
                    line_entries: Vec::new(),
                    line_files: vec![source_name.to_string()],
                    line_dirs: Vec::new(),
                    low_pc: funcs.first().map(|(_, pc)| *pc).unwrap_or(0),
                    high_pc: funcs.last().map(|(_, pc)| *pc + 64).unwrap_or(0),
                    func_ranges: funcs.iter().map(|(_, pc)| (*pc, 64)).collect(),
                    cfi_rows: Vec::new(),
                };
                let dbg = debug::build_dwarf_full(&cu, &opts);
                let extra_secs = debug::dwarf_elf_sections(&dbg);
                cg.push_extra_section(elf::ElfSection {
                    name: ".debug_info".to_string(),
                    data: dbg.info,
                    ty: elf::SHT_PROGBITS,
                    flags: 0,
                    align: 1,
                });
                for sec in extra_secs {
                    cg.push_extra_section(sec);
                }
                cg.push_extra_section(elf::ElfSection {
                    name: ".BTF".to_string(),
                    data: debug::build_btf_probe(),
                    ty: elf::SHT_PROGBITS,
                    flags: 0,
                    align: 4,
                });
            }
            if relocatable && target.unwind_tables {
                let frames = cg.func_frames(module);
                if !frames.is_empty() {
                    cg.push_extra_section(elf::ElfSection {
                        name: ".eh_frame".to_string(),
                        data: debug::build_eh_frame_with_arch(&frames, debug::ArchKind::RiscV64),
                        ty: elf::SHT_PROGBITS,
                        flags: elf::SHF_ALLOC,
                        align: 8,
                    });
                }
            }
            if sanitizers.any() {
                cg.push_extra_section(elf::ElfSection {
                    name: ".dcc.san_info".to_string(),
                    data: sanitizer_metadata(sanitizers),
                    ty: elf::SHT_PROGBITS,
                    flags: 0,
                    align: 1,
                });
            }
            if let Some(o) = offload {
                if !o.is_empty() {
                    cg.push_extra_section(elf::ElfSection {
                        name: ".dcc.offload".to_string(),
                        data: o.to_binary(),
                        ty: elf::SHT_PROGBITS,
                        flags: 0,
                        align: 8,
                    });
                }
            }
            cg.build_output()
        }
        crate::cli::Arch::Mips => {
            let mut cg = mips::MipsCodegen::new(target.clone(), relocatable || emit_asm);
            cg.set_source_name(source_name);
            if emit_asm {
                cg.compile(module);
                return cg.build_asm_output(module).into_bytes();
            }
            cg.compile(module);
            if debug_level > 0 && relocatable {
                let funcs: Vec<(&str, u64)> = module
                    .functions
                    .iter()
                    .filter_map(|f| cg.labels.get(&f.name).map(|&pc| (f.name.as_str(), pc)))
                    .collect();
                let opts = debug::DebugOptions {
                    level: debug_level,
                    source_name: source_name.to_string(),
                    arch: debug::ArchKind::Mips,
                    ..debug::DebugOptions::default()
                };
                let cu = debug::CompileUnit {
                    source_name: source_name.to_string(),
                    comp_dir: None,
                    producer: "dcc".to_string(),
                    language: 12,
                    functions: funcs
                        .iter()
                        .map(|(n, pc)| debug::DwarfFuncInfo {
                            name: n.to_string(),
                            linkage_name: None,
                            low_pc: *pc,
                            high_pc: *pc + 64,
                            decl_line: 0,
                            decl_file_idx: 0,
                            return_type: debug::DwarfType {
                                name: "void".to_string(),
                                byte_size: 0,
                                kind: debug::DwarfTypeKind::Void,
                            },
                            params: Vec::new(),
                            is_external: *n == "main",
                            is_inline: false,
                            is_declaration: false,
                        })
                        .collect(),
                    globals: Vec::new(),
                    types: Vec::new(),
                    line_entries: Vec::new(),
                    line_files: vec![source_name.to_string()],
                    line_dirs: Vec::new(),
                    low_pc: funcs.first().map(|(_, pc)| *pc).unwrap_or(0),
                    high_pc: funcs.last().map(|(_, pc)| *pc + 64).unwrap_or(0),
                    func_ranges: funcs.iter().map(|(_, pc)| (*pc, 64)).collect(),
                    cfi_rows: Vec::new(),
                };
                let dbg = debug::build_dwarf_full(&cu, &opts);
                let extra_secs = debug::dwarf_elf_sections(&dbg);
                cg.push_extra_section(elf::ElfSection {
                    name: ".debug_info".to_string(),
                    data: dbg.info,
                    ty: elf::SHT_PROGBITS,
                    flags: 0,
                    align: 1,
                });
                for sec in extra_secs {
                    cg.push_extra_section(sec);
                }
            }
            if relocatable {
                let frames = cg.func_frames(module);
                if !frames.is_empty() {
                    cg.push_extra_section(elf::ElfSection {
                        name: ".eh_frame".to_string(),
                        data: debug::build_eh_frame_with_arch(&frames, debug::ArchKind::Mips),
                        ty: elf::SHT_PROGBITS,
                        flags: elf::SHF_ALLOC,
                        align: 4,
                    });
                }
            }
            if sanitizers.any() {
                cg.push_extra_section(elf::ElfSection {
                    name: ".dcc.san_info".to_string(),
                    data: sanitizer_metadata(sanitizers),
                    ty: elf::SHT_PROGBITS,
                    flags: 0,
                    align: 1,
                });
            }
            if let Some(o) = offload {
                if !o.is_empty() {
                    cg.push_extra_section(elf::ElfSection {
                        name: ".dcc.offload".to_string(),
                        data: o.to_binary(),
                        ty: elf::SHT_PROGBITS,
                        flags: 0,
                        align: 8,
                    });
                }
            }
            cg.build_output()
        }
        crate::cli::Arch::Mips64 => {
            let mut cg = mips64::Mips64Codegen::new(target.clone(), relocatable || emit_asm);
            cg.set_source_name(source_name);
            if emit_asm {
                cg.compile(module);
                return cg.build_asm_output(module).into_bytes();
            }
            cg.compile(module);
            if debug_level > 0 && relocatable {
                let funcs: Vec<(&str, u64)> = module
                    .functions
                    .iter()
                    .filter_map(|f| cg.labels().get(&f.name).map(|&pc| (f.name.as_str(), pc)))
                    .collect();
                let opts = debug::DebugOptions {
                    level: debug_level,
                    source_name: source_name.to_string(),
                    arch: debug::ArchKind::Mips64,
                    ..debug::DebugOptions::default()
                };
                let cu = debug::CompileUnit {
                    source_name: source_name.to_string(),
                    comp_dir: None,
                    producer: "dcc".to_string(),
                    language: 12,
                    functions: funcs
                        .iter()
                        .map(|(n, pc)| debug::DwarfFuncInfo {
                            name: n.to_string(),
                            linkage_name: None,
                            low_pc: *pc,
                            high_pc: *pc + 64,
                            decl_line: 0,
                            decl_file_idx: 0,
                            return_type: debug::DwarfType {
                                name: "void".to_string(),
                                byte_size: 0,
                                kind: debug::DwarfTypeKind::Void,
                            },
                            params: Vec::new(),
                            is_external: *n == "main",
                            is_inline: false,
                            is_declaration: false,
                        })
                        .collect(),
                    globals: Vec::new(),
                    types: Vec::new(),
                    line_entries: Vec::new(),
                    line_files: vec![source_name.to_string()],
                    line_dirs: Vec::new(),
                    low_pc: funcs.first().map(|(_, pc)| *pc).unwrap_or(0),
                    high_pc: funcs.last().map(|(_, pc)| *pc + 64).unwrap_or(0),
                    func_ranges: funcs.iter().map(|(_, pc)| (*pc, 64)).collect(),
                    cfi_rows: Vec::new(),
                };
                let dbg = debug::build_dwarf_full(&cu, &opts);
                let extra_secs = debug::dwarf_elf_sections(&dbg);
                cg.push_extra_section(elf::ElfSection {
                    name: ".debug_info".to_string(),
                    data: dbg.info,
                    ty: elf::SHT_PROGBITS,
                    flags: 0,
                    align: 1,
                });
                for sec in extra_secs {
                    cg.push_extra_section(sec);
                }
            }
            if relocatable {
                let frames = cg.func_frames(module);
                if !frames.is_empty() {
                    cg.push_extra_section(elf::ElfSection {
                        name: ".eh_frame".to_string(),
                        data: debug::build_eh_frame_with_arch(&frames, debug::ArchKind::Mips64),
                        ty: elf::SHT_PROGBITS,
                        flags: elf::SHF_ALLOC,
                        align: 8,
                    });
                }
            }
            if sanitizers.any() {
                cg.push_extra_section(elf::ElfSection {
                    name: ".dcc.san_info".to_string(),
                    data: sanitizer_metadata(sanitizers),
                    ty: elf::SHT_PROGBITS,
                    flags: 0,
                    align: 1,
                });
            }
            if let Some(o) = offload {
                if !o.is_empty() {
                    cg.push_extra_section(elf::ElfSection {
                        name: ".dcc.offload".to_string(),
                        data: o.to_binary(),
                        ty: elf::SHT_PROGBITS,
                        flags: 0,
                        align: 8,
                    });
                }
            }
            cg.build_output()
        }
        crate::cli::Arch::Arm => {
            let mut cg = arm::ArmCodegen::new(target.clone(), relocatable || emit_asm);
            cg.set_source_name(source_name);
            if emit_asm {
                cg.compile(module);
                return cg.build_asm_output(module).into_bytes();
            }
            cg.compile(module);
            if debug_level > 0 && relocatable {
                let funcs: Vec<(&str, u64)> = module
                    .functions
                    .iter()
                    .filter_map(|f| cg.labels.get(&f.name).map(|&pc| (f.name.as_str(), pc)))
                    .collect();
                let opts = debug::DebugOptions {
                    level: debug_level,
                    source_name: source_name.to_string(),
                    arch: debug::ArchKind::Arm,
                    ..debug::DebugOptions::default()
                };
                let cu = debug::CompileUnit {
                    source_name: source_name.to_string(),
                    comp_dir: None,
                    producer: "dcc".to_string(),
                    language: 12,
                    functions: funcs
                        .iter()
                        .map(|(n, pc)| debug::DwarfFuncInfo {
                            name: n.to_string(),
                            linkage_name: None,
                            low_pc: *pc,
                            high_pc: *pc + 64,
                            decl_line: 0,
                            decl_file_idx: 0,
                            return_type: debug::DwarfType {
                                name: "void".to_string(),
                                byte_size: 0,
                                kind: debug::DwarfTypeKind::Void,
                            },
                            params: Vec::new(),
                            is_external: *n == "main",
                            is_inline: false,
                            is_declaration: false,
                        })
                        .collect(),
                    globals: Vec::new(),
                    types: Vec::new(),
                    line_entries: Vec::new(),
                    line_files: vec![source_name.to_string()],
                    line_dirs: Vec::new(),
                    low_pc: funcs.first().map(|(_, pc)| *pc).unwrap_or(0),
                    high_pc: funcs.last().map(|(_, pc)| *pc + 64).unwrap_or(0),
                    func_ranges: funcs.iter().map(|(_, pc)| (*pc, 64)).collect(),
                    cfi_rows: Vec::new(),
                };
                let dbg = debug::build_dwarf_full(&cu, &opts);
                let extra_secs = debug::dwarf_elf_sections(&dbg);
                cg.push_extra_section(elf::ElfSection {
                    name: ".debug_info".to_string(),
                    data: dbg.info,
                    ty: elf::SHT_PROGBITS,
                    flags: 0,
                    align: 1,
                });
                for sec in extra_secs {
                    cg.push_extra_section(sec);
                }
            }
            if relocatable {
                let frames = cg.func_frames(module);
                if !frames.is_empty() {
                    cg.push_extra_section(elf::ElfSection {
                        name: ".eh_frame".to_string(),
                        data: debug::build_eh_frame_with_arch(&frames, debug::ArchKind::Arm),
                        ty: elf::SHT_PROGBITS,
                        flags: elf::SHF_ALLOC,
                        align: 4,
                    });
                }
            }
            if sanitizers.any() {
                cg.push_extra_section(elf::ElfSection {
                    name: ".dcc.san_info".to_string(),
                    data: sanitizer_metadata(sanitizers),
                    ty: elf::SHT_PROGBITS,
                    flags: 0,
                    align: 1,
                });
            }
            if let Some(o) = offload {
                if !o.is_empty() {
                    cg.push_extra_section(elf::ElfSection {
                        name: ".dcc.offload".to_string(),
                        data: o.to_binary(),
                        ty: elf::SHT_PROGBITS,
                        flags: 0,
                        align: 8,
                    });
                }
            }
            cg.build_output()
        }
        a => {
            panic!("{}", a.backend_missing_msg());
        }
    }
}
