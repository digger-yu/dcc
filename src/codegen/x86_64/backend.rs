// src/codegen/x86_64/backend.rs - Backend trait implementation for x86_64 Codegen
// First Principle: x86_64 backend must implement the unified Backend trait
//
// 职责划分 (与旧 codegen::emit() 的 x86_64 分支一一对应):
//   compile()      : EmitOptions → Codegen 状态, IR → 机器码, 附加节 (DWARF /
//                    .eh_frame / .san_info / .offload / .ifunc / LTO) 组装
//   render_asm()   : -S 路径, 只做代码生成 + 汇编文本输出
//   build_object() : 把 Codegen 内部节镜像序列化为 ELF ET_REL / COFF / ET_EXEC

use crate::cli::Arch;
use crate::codegen::shared::backend::{
    Backend, CommonImage, CompileInput, ExtraSection, ObjectRelocs, ObjectSymbols, TlsImage,
};
use crate::codegen::shared::features::FeatureSet;
use crate::codegen::x86_64::Codegen;
use crate::codegen::{build_dwarf_cu, debug_abs_reloc_type};
use crate::codegen::{coff, shared::elf};
use crate::debug;
use crate::diagnostics::Diagnostic;
use crate::ir::Module;

impl Backend for Codegen {
    fn arch(&self) -> Arch {
        Arch::X86_64
    }

    fn features(&self) -> FeatureSet {
        // x86_64 supports all features fully
        FeatureSet::full()
    }

    fn compile(&mut self, input: &CompileInput<'_>) -> Result<CommonImage, Vec<Diagnostic>> {
        apply_emit_options(self, input);
        // Perform the existing code generation (inherent Codegen::compile)
        self.compile_module(input.module);
        build_aux_sections(self, input);

        // Map to CommonImage with explicit field mapping
        Ok(CommonImage {
            code: self.code.clone(),
            rodata: self.rodata.clone(),
            data: self.data_section.clone(),
            bss: self.bss_size,
            tls: TlsImage {
                offset: 0, // TODO: compute TLS offset from tdata/tbss
                size: self.tdata_section.len() as u64 + self.tbss_size,
                decl: None,
            },
            symbols: ObjectSymbols {
                globals: self
                    .symbols
                    .iter()
                    .filter(|s| s.bind == elf::STB_GLOBAL)
                    .map(|s| s.name.clone())
                    .collect(),
                locals: vec![],  // TODO: track local symbols
                imports: vec![], // TODO: extract import symbols
            },
            relocs: ObjectRelocs {
                rellocs: self
                    .relocations
                    .iter()
                    .map(|r| format!("{:?}", r))
                    .collect(),
            },
            extra_sections: self
                .extra_sections
                .iter()
                .map(|sec| ExtraSection {
                    name: sec.name.clone(),
                    content: sec.data.clone(),
                })
                .collect(),
            diagnostics: self.diagnostics.clone(),
            func_sizes: self
                .func_sizes
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            asm_passthrough: self.asm_passthrough.clone(),
            asm_passthrough_regions: self.asm_passthrough_regions.clone(),
        })
    }

    fn render_asm(&mut self, input: &CompileInput<'_>) -> Result<Vec<u8>, Vec<Diagnostic>> {
        // -S: 与旧 emit() 一致 —— 先设置选项 (含 emit_asm_text), 生成代码,
        // 再输出汇编文本; 不构建 DWARF / ELF。
        apply_emit_options(self, input);
        self.compile_module(input.module);
        self.render_asm_from_state(input.module)
    }

    fn render_asm_from_state(&mut self, module: &Module) -> Result<Vec<u8>, Vec<Diagnostic>> {
        // x86_64 的 -S 本就是“对自己刚编码的字节做反汇编渲染”, 天然与 -c 同源;
        // 本方法只是把这个既有事实暴露给 TU 级降级路径, 避免重跑 codegen。
        Ok(self.build_asm_output(module.source()).into_bytes())
    }

    fn build_object(&mut self) -> Result<Vec<u8>, Vec<Diagnostic>> {
        let bytes = if self.relocatable {
            if self.target.triple.os == crate::cli::Os::Windows {
                coff::build_coff_rel_x86_64(
                    &self.code,
                    &self.rodata,
                    &self.data_section,
                    self.bss_size,
                    &self.tdata_section,
                    self.tbss_size,
                    self.text_align,
                    self.data_align,
                    self.bss_align,
                    self.tdata_align,
                    self.tbss_align,
                    &self.symbols,
                    &self.relocations,
                    &self.extra_sections,
                    &self.data_relocations,
                    &self.extra_rela_sections,
                )
            } else {
                elf::build_elf_rel_x86_64(
                    &self.code,
                    &self.rodata,
                    &self.data_section,
                    self.bss_size,
                    &self.tdata_section,
                    self.tbss_size,
                    self.text_align,
                    self.data_align,
                    self.bss_align,
                    self.tdata_align,
                    self.tbss_align,
                    &self.symbols,
                    &self.relocations,
                    &self.extra_sections,
                    &self.data_relocations,
                    &self.extra_rela_sections,
                    &self.comdat_groups,
                )
            }
        } else {
            elf::build_elf_exec_x86_64(
                &self.code,
                self.entry,
                self.exec_text_size,
                self.rodata.len() as u64,
                self.tdata_section.len() as u64,
                self.tbss_size,
                self.data_section.len() as u64,
                self.bss_size,
                &self.symbols,
            )
        };
        Ok(bytes)
    }
}

/// EmitOptions → Codegen 字段 (旧 codegen::emit() x86_64 分支的开头若干行)
fn apply_emit_options(cg: &mut Codegen, input: &CompileInput<'_>) {
    let opts = input.opts;
    cg.relocatable = opts.relocatable || opts.emit_asm;
    cg.function_sections = opts.function_sections || opts.lto_enabled;
    cg.emit_asm_text = opts.emit_asm;
    cg.parallel_codegen = opts.parallel_codegen && !opts.emit_asm;
    cg.source_name = opts.source_name.clone();
    cg.line_map = opts.line_map.to_vec();
    cg.debug_level = opts.debug_level;
    // 优化档同样下行：区内家与 A2 的护栏读的是 `Codegen::opt_level`，而 -O0 与 -O2
    // 对同一个形状给出不同的保守决定就是「四档结果必须一致」这条口径的违反。
    cg.opt_level = opts.opt_level;
    cg.emit_cfi = opts.debug_level > 0 || input.target.frame_pointer;
    cg.bounds_check = opts.bounds_check;
    cg.trapv = opts.trapv;
    cg.kasan = opts.sanitizers.kasan;
}

/// 代码生成完成后组装所有附加节 (debug / unwind / sanitizer / offload / ifunc / LTO)
fn build_aux_sections(cg: &mut Codegen, input: &CompileInput<'_>) {
    let opts = input.opts;
    let module = input.module;
    let target = input.target;

    if opts.debug_level > 0 && opts.relocatable {
        build_debug_sections(cg, module, target, opts);
    }

    if opts.relocatable && target.unwind_tables {
        // dcc 输出 .eh_frame FDE 需带 .rela.eh_frame relocations 才能在 ld 链接时
        // 正确解析 PC begin / PC range。-fno-asynchronous-unwind-tables 时不生成。
        //
        // 这里一直输出到链接期可解析的形式: FDE 的 initial location 是
        // `DW_EH_PE_pcrel|sdata4`, 汇编期无法知道函数最终地址。没有这些重定位时,
        // 链接后的 `.eh_frame` 指向错误 PC -> unwinder 找不到帧 -> C++ 异常直接
        // terminate (这也是旧实现只在 -g 下输出且内容不对的根因)。
        let frames: Vec<debug::FuncFrame> = cg.func_frames(module);
        let image = debug::build_eh_frame_linked(&frames);
        let sec_idx = cg.extra_sections.len();
        cg.extra_sections.push(elf::ElfSection {
            name: ".eh_frame".to_string(),
            data: image.bytes,
            ty: elf::SHT_PROGBITS,
            flags: elf::SHF_ALLOC,
            align: 8,
            group_name: None,
        });
        let mut relocs = Vec::new();
        for rela in &image.relocs {
            let sym = match rela.target {
                debug::EhRelTarget::TextSection => ".text",
                debug::EhRelTarget::ExceptTableSection => ".gcc_except_table",
                debug::EhRelTarget::PersonalitySlot => debug::GXX_PERSONALITY_SLOT,
            };
            let Some(&sym_index) = cg.sym_index.get(sym) else {
                cg.diagnostics.push(crate::diagnostics::Diagnostic {
                    sev: crate::diagnostics::Severity::Error,
                    code: "E1123",
                    msg: format!("unwind table needs symbol '{}' but it is missing", sym),
                    span: crate::diagnostics::Span::point("<eh_frame>", 0, 0),
                    hint: Some("personality 槽由 C++ 前端作为 weak 全局变量发射".into()),
                    source_line: None,
                });
                continue;
            };
            relocs.push(elf::ElfRela {
                offset: rela.offset,
                sym_index,
                rel_type: elf::R_X86_64_PC32,
                addend: rela.addend,
            });
        }
        if !relocs.is_empty() {
            cg.extra_rela_sections.push(elf::ExtraRelaSection {
                name: ".rela.eh_frame".to_string(),
                relocations: relocs,
                target_sec_index: 7 + sec_idx as u32,
            });
        }
    }

    if opts.sanitizers.any() {
        cg.extra_sections.push(elf::ElfSection {
            name: ".dcc.san_info".to_string(),
            data: crate::sanitizer::sanitizer_metadata(opts.sanitizers),
            ty: elf::SHT_PROGBITS,
            flags: 0,
            align: 1,
            group_name: None,
        });
    }

    if let Some(o) = opts.offload {
        if !o.is_empty() {
            cg.extra_sections.push(elf::ElfSection {
                name: ".dcc.offload".to_string(),
                data: o.to_binary(),
                ty: elf::SHT_PROGBITS,
                flags: 0,
                align: 8,
                group_name: None,
            });
        }
    }

    // ifunc 属性: 收集所有 ifunc 函数, 生成 .dcc.ifunc 节
    // 格式: 每条记录 = u16 name_len + name bytes + u16 resolver_len + resolver bytes
    if opts.relocatable {
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
                group_name: None,
            });
        }
    }

    if opts.lto_enabled {
        // 嵌入 dcc 自有 IR 位码（供 src/link 多 TU LTO 使用）
        let bc = crate::link::lto_bitcode::serialize_module(module, &opts.source_name);
        if opts.verbose {
            eprintln!(
                "dcc: lto    = .dcc.lto {} bytes ({} funcs, {} globals)",
                bc.len(),
                module.functions.len(),
                module.globals.len()
            );
        }
        cg.extra_sections.push(elf::ElfSection {
            name: ".dcc.lto".to_string(),
            data: bc,
            ty: elf::SHT_PROGBITS,
            flags: 0,
            align: 1,
            group_name: None,
        });
        // 同时输出 .llvm.lto 文本摘要，兼容 GC-sections 探测与 ld -r 合并路径
        let funcs: Vec<(&str, u64, bool)> = module
            .functions
            .iter()
            .map(|f| {
                let sz = cg.func_sizes.get(&f.name).copied().unwrap_or(0);
                (f.name.as_str(), sz, true)
            })
            .collect();
        let summary_unit = crate::lto::build_unit(&opts.source_name, &funcs);
        let summary_data = crate::lto::serialize_summary(&summary_unit);
        if opts.verbose {
            eprintln!(
                "dcc: lto    = .llvm.lto {} bytes ({} funcs)",
                summary_data.len(),
                funcs.len()
            );
        }
        cg.extra_sections.push(elf::ElfSection {
            name: ".llvm.lto".to_string(),
            data: summary_data,
            ty: elf::SHT_PROGBITS,
            flags: 0,
            align: 1,
            group_name: None,
        });
    }
}

/// 把发射器采集的「局部 → 位置」表填进 DWARF 的函数信息（Phase 6.2）。
///
/// 三条换算规则（其余一律**不发**，宁缺不错）：
/// * `Stack(off)`（`%r13 + off`：变量区槽，或在采集时就已翻译回来的溢出槽）→
///   `DW_OP_fbreg(off - cfa)`，`cfa` 就是 CFI 用来定 `CFA = rsp + cfa` 的那一个数
///   （单源，见 `cfa_frame_offset`）；
/// * `Reg(r)` → `DW_OP_regN`，N 是 **DWARF** 号而不是硬件号（psABI 两者不同序）；
/// * 拿不出对应 DWARF 号的寄存器，或尚未接线的家形状（如 `Imm`）→ 不发这个变量的位置。
///   少一个 DIE 只是调试信息缺，错一个 DIE 是把调试器指向另一个寄存器。
fn fill_dwarf_local_vars(
    cu: &mut crate::debug::CompileUnit,
    facts: &[(String, Vec<crate::codegen::x86_64::DebugVarLoc>)],
    target: &crate::target::Target,
) {
    use crate::codegen::x86_64::elf::output::cfa_frame_offset;
    use crate::location::Location;
    if facts.is_empty() {
        return;
    }
    let cfa = cfa_frame_offset(target.frame_pointer) as i64;
    let arch = crate::debug::ArchKind::X86_64;
    for f in cu.functions.iter_mut() {
        let Some((_, list)) = facts.iter().find(|(n, _)| *n == f.name) else {
            continue;
        };
        for v in list {
            let loc = match v.loc {
                Location::Stack(off) => crate::debug::DwarfParamLocation::fbreg(off - cfa),
                Location::Reg(r) => match arch.dwarf_reg_of_gp_hw(r.n) {
                    Some(dr) => crate::debug::DwarfParamLocation::reg(dr),
                    None => continue,
                },
                _ => continue,
            };
            f.local_vars.push(crate::debug::DwarfLocalVar {
                name: v.name.clone(),
                ty: crate::codegen::ast_type_to_dwarf(&v.ty),
                location: loc,
                // 分段位置（loclist）：同一个号只在一段 PC 里成立时，剩下的区间要回
                // 到栈槽上。没分段位置时这一张表为空 ⇒ DWARF 侧照旧发单 exprloc。
                ranges: v
                    .ranges
                    .iter()
                    .filter_map(|r| {
                        let l = match r.loc {
                            crate::location::Location::Stack(off) => {
                                crate::debug::DwarfParamLocation::fbreg(off - cfa)
                            },
                            crate::location::Location::Reg(rr) => match arch.dwarf_reg_of_gp_hw(rr.n) {
                                Some(dr) => crate::debug::DwarfParamLocation::reg(dr),
                                None => return None,
                            },
                            _ => return None,
                        };
                        Some(crate::debug::DwarfLocRange {
                            begin_pc: r.begin_pc,
                            end_pc: r.end_pc,
                            loc: l,
                        })
                    })
                    .collect(),
                decl_line: v.decl_line,
            });
        }
    }
}

/// DWARF (.debug_* / .BTF / .rela.debug_*) 节组装
fn build_debug_sections(
    cg: &mut Codegen,
    module: &crate::ir::Module,
    target: &crate::target::Target,
    opts: &crate::codegen::EmitOptions<'_>,
) {
    use crate::codegen::shared::elf::ElfRela;

    // Build func_pcs with actual sizes from codegen
    let func_pcs: Vec<(&str, u64, u64)> = module
        .functions
        .iter()
        .filter_map(|f| {
            cg.labels.get(&f.name).map(|&pc| {
                let sz = cg.func_sizes.get(&f.name).copied().unwrap_or(16);
                (f.name.as_str(), pc, pc + sz)
            })
        })
        .collect();

    // 将 asm 块行号记录转换为 DWARF LineEntry
    // 组合块级条目 (is_stmt=true) 和逐指令条目 (is_stmt=false, discriminator>0)
    // 块级条目: GDB 在 asm 块起始行设置语句边界
    // 逐指令条目: GDB 单步进入 asm 块内部每条指令 (类似 GCC .debug_insn)
    let mut combined_line_entries: Vec<debug::LineEntry> = Vec::new();
    // 普通 C 语句 / 局部声明的条目（由 `Codegen::note_src_line` 在发射时攒下）。
    // 这一份是行断点能绑上的唯一理由：没有它，纯 C 单元的 `line_entries` 恒空，
    // `src/debug.rs` 只能退到「每函数一行」的兜底表，`break file.c:N` 全部绑不上。
    for &(pc, src_line) in cg.stmt_line_entries.iter() {
        combined_line_entries.push(debug::LineEntry {
            pc,
            line: src_line,
            column: 1,
            file_idx: 0,
            discriminator: 0,
            is_stmt: true,
            basic_block: false,
        });
    }
    for &(pc_start, _pc_end, src_line) in cg.asm_line_entries.iter() {
        combined_line_entries.push(debug::LineEntry {
            pc: pc_start,
            line: src_line,
            column: 1,
            file_idx: 0,
            discriminator: 0,
            is_stmt: true,
            basic_block: false,
        });
    }
    let mut last_block_pc = u64::MAX;
    for &(pc, src_line, disc) in cg.asm_insn_debug_entries.iter() {
        let is_first_in_block = pc != last_block_pc;
        last_block_pc = pc;
        combined_line_entries.push(debug::LineEntry {
            pc,
            line: src_line,
            column: 1,
            file_idx: 0,
            discriminator: if is_first_in_block { 0 } else { disc },
            is_stmt: is_first_in_block,
            basic_block: false,
        });
    }
    // 按 PC 排序, 同 PC 时 is_stmt=true 优先
    combined_line_entries.sort_by(|a, b| {
        a.pc.cmp(&b.pc)
            .then_with(|| b.is_stmt.cmp(&a.is_stmt))
            .then_with(|| a.discriminator.cmp(&b.discriminator))
    });

    let mut cu = build_dwarf_cu(module, &opts.source_name, &func_pcs, combined_line_entries);
    // Phase 6.2：把发射器采集的「局部 → 帧内位置」表填进 DWARF。以前这一步根本不存在
    // （`local_vars` 恒空），于是 `-g` 下变量一个 DIE 都没有，而 `src/ssa/regalloc.rs` 也
    // 因此必须禁止寄存器家。位置只有一个出处（`Codegen::debug_var_facts`），本函数只把它
    // 翻译成 DWARF 的说法：`%r13+off` → `DW_OP_fbreg(off - cfa)`，寄存器 → `DW_OP_regN`，
    // 溢出槽 → 它在帧里的实际偏移（与读/写路同一个 `spill_off`，不另算一份）。
    fill_dwarf_local_vars(&mut cu, &cg.debug_var_facts, target);
    let debug_opts = debug::DebugOptions {
        level: opts.debug_level,
        dwarf_version: opts.dwarf_version,
        source_name: opts.source_name.clone(),
        ..Default::default()
    };
    let dbg = debug::build_dwarf_full(&cu, &debug_opts);
    cg.extra_sections.extend(debug::dwarf_elf_sections(&dbg));

    // .rela.debug_* —— 为 debug 节里的函数地址生成绝对重定位
    let relocs = [
        (&dbg.debug_info_relocs, ".debug_info", 8_u32),
        (&dbg.debug_line_relocs, ".debug_line", 10_u32),
        (&dbg.debug_aranges_relocs, ".debug_aranges", 9_u32),
    ];
    // 目标节索引与 ElfRela 的构造走同一个口子：loclist 那一组只是多一个加数，
    // 不允许另写一份解析（否则两处对「.debug_loc 在第几个额外节」的答案会飘）。
    let sec_index_of = |name: &str| -> u32 {
        cg.extra_sections.iter().position(|s| s.name == name).map(|i| 7 + i as u32).unwrap_or(8)
    };
    for (src, sec_name, fallback_idx) in relocs.iter() {
        if src.is_empty() {
            continue;
        }
        let target_sec_index = cg
            .extra_sections
            .iter()
            .position(|s| s.name == *sec_name)
            .map(|i| 7 + i as u32)
            .unwrap_or(*fallback_idx);
        let debug_relocs: Vec<ElfRela> = src
            .iter()
            .filter_map(|(off, sym_name)| {
                let sym_idx = cg.sym_index.get(sym_name).copied()?;
                Some(ElfRela {
                    offset: *off,
                    sym_index: sym_idx,
                    rel_type: debug_abs_reloc_type(target.arch),
                    addend: 0,
                })
            })
            .collect();
        if debug_relocs.is_empty() {
            continue;
        }
        cg.extra_rela_sections.push(elf::ExtraRelaSection {
            name: format!(".rela{}", sec_name),
            relocations: debug_relocs,
            target_sec_index,
        });
    }
    // `.debug_loc`（方案 §4.4 的 loclist）：每一对 begin/end 都是函数**中间**的 pc，
    // 所以必须带加数（S(f) + 函数内偏移）。与上面同一套符号解析与节索引口径。
    if !dbg.loc_relocs.is_empty() {
        let target_sec_index = sec_index_of(".debug_loc");
        let loc_relocs: Vec<ElfRela> = dbg
            .loc_relocs
            .iter()
            .filter_map(|(off, sym_name, addend)| {
                let sym_idx = cg.sym_index.get(sym_name).copied()?;
                Some(ElfRela {
                    offset: *off,
                    sym_index: sym_idx,
                    rel_type: debug_abs_reloc_type(target.arch),
                    addend: *addend,
                })
            })
            .collect();
        if !loc_relocs.is_empty() {
            cg.extra_rela_sections.push(elf::ExtraRelaSection {
                name: ".rela.debug_loc".to_string(),
                relocations: loc_relocs,
                target_sec_index,
            });
        }
    }

    // BTF 已在 build_dwarf_full 中生成，使用 dbg.btf
    if !dbg.btf.is_empty() {
        cg.extra_sections.push(elf::ElfSection {
            name: ".BTF".to_string(),
            data: dbg.btf.clone(),
            ty: elf::SHT_PROGBITS,
            flags: 0,
            align: 4,
            group_name: None,
        });
    }
    // .BTF.ext: func_info + line_info (BPF verifier)
    if !dbg.btf_ext.is_empty() {
        cg.extra_sections.push(elf::ElfSection {
            name: ".BTF.ext".to_string(),
            data: dbg.btf_ext.clone(),
            ty: elf::SHT_PROGBITS,
            flags: 0,
            align: 4,
            group_name: None,
        });
    }
}
