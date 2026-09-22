// src/codegen/aarch64/backend.rs - Backend trait implementation for AArch64 Codegen
// First Principle: AArch64 backend must implement the unified Backend trait
//
// 职责划分 (与旧 codegen::emit() 的 AArch64 分支一一对应):
//   compile()      : 选项注入 + IR → 机器码 + 附加节 (DWARF/.eh_frame/san/offload)
//   render_asm()   : -S 路径 (汇编文本)
//   build_object() : ELF 打包

use crate::cli::Arch;
use crate::codegen::aarch64::Aarch64Codegen;
use crate::codegen::backend::{
    Backend, CommonImage, CompileInput, ExtraSection, ObjectRelocs, ObjectSymbols, TlsImage,
};
use crate::codegen::features::FeatureSet;
use crate::codegen::{build_dwarf_cu, elf};
use crate::debug;
use crate::diagnostics::Diagnostic;

impl Backend for Aarch64Codegen {
    fn arch(&self) -> Arch {
        Arch::AArch64
    }

    fn features(&self) -> FeatureSet {
        // 能力清单逐项依据见 `FeatureSet::tier1_cross()` 文档注释
        // (Lto / ParallelCodegen / BoundsCheck / Trapv / Tls 在本后端实测不存在)。
        FeatureSet::tier1_cross()
    }

    fn compile(&mut self, input: &CompileInput<'_>) -> Result<CommonImage, Vec<Diagnostic>> {
        self.set_source_name(&input.opts.source_name);
        self.set_asm_text_mode(input.opts.emit_asm);
        // HWASAN (-fsanitize=hwaddress) 在 AArch64 上启用 MTE 硬件 tag:
        // prologue tag 栈帧 + 指针 tag 流程由后端直接发射 IRG/ADDG/STG。
        self.set_mte_enabled(input.opts.sanitizers.hwasan);
        // Perform the existing code generation (inherent Aarch64Codegen::compile)
        self.compile(input.module);
        build_aux_sections(self, input);

        // Map to CommonImage with explicit field mapping
        Ok(CommonImage {
            code: self.code.clone(),
            rodata: self.rodata.clone(),
            data: vec![], // TODO: extract data section from extra_sections
            bss: 0,      // TODO: compute BSS size
            tls: TlsImage {
                offset: 0,
                size: self.tls_globals.len() as u64, // Estimate
                decl: None,
            },
            symbols: ObjectSymbols {
                globals: self.symbols.iter().map(|s| s.name.clone()).collect(),
                locals: vec![],
                imports: vec![],
            },
            relocs: ObjectRelocs {
                rellocs: self.relocations.iter().map(|r| format!("{:?}", r)).collect(),
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
            func_sizes: self.func_sizes.iter().map(|(k, v)| (k.clone(), *v)).collect(),
        })
    }

    fn render_asm(&mut self, input: &CompileInput<'_>) -> Result<Vec<u8>, Vec<Diagnostic>> {
        self.set_source_name(&input.opts.source_name);
        self.set_asm_text_mode(true);
        self.set_mte_enabled(input.opts.sanitizers.hwasan);
        self.compile(input.module);
        Ok(self.build_asm_output(input.module).into_bytes())
    }

    fn build_object(&mut self) -> Result<Vec<u8>, Vec<Diagnostic>> {
        Ok(self.build_output())
    }
}

/// 代码生成完成后组装附加节 (debug / unwind / sanitizer / offload)
fn build_aux_sections(cg: &mut Aarch64Codegen, input: &CompileInput<'_>) {
    let opts = input.opts;
    let module = input.module;

    if opts.debug_level > 0 && opts.relocatable {
        let func_pcs: Vec<(&str, u64, u64)> = module
            .functions
            .iter()
            .filter_map(|f| {
                cg.labels.get(&f.name).map(|&pc| (f.name.as_str(), pc, pc + 64))
            })
            .collect();
        let dbg_opts = debug::DebugOptions {
            level: opts.debug_level,
            dwarf_version: opts.dwarf_version,
            source_name: opts.source_name.clone(),
            arch: debug::ArchKind::AArch64,
            ..debug::DebugOptions::default()
        };
        // 构建 DWARF 行号表条目
        let line_entries: Vec<debug::LineEntry> = cg
            .line_entries
            .iter()
            .map(|&(pc, line)| debug::LineEntry {
                pc,
                line,
                column: 1,
                file_idx: 0,
                discriminator: 0,
                is_stmt: true,
                basic_block: false,
            })
            .collect();
        let cu = build_dwarf_cu(module, &opts.source_name, &func_pcs, line_entries);
        let dbg = debug::build_dwarf_full(&cu, &dbg_opts);
        // `.debug_info` 由下面的 `dwarf_elf_sections()` 统一发（它内部就是
        // `DebugSections::to_elf_sections`，已包含 `dbg.info`）。这里以前**又**手工
        // push 了一遍同名节，于是 `-g -c` 的 .o 里出现两个内容逐字节相同的
        // `.debug_info` PROGBITS 节（x86_64 只走共享生产者，所以只有交叉后端中招）。
        // 选择删手工 push、而不给 `push_extra_section` 加同名去重：后者会把未来
        // 有意的重复（如 COMDAT 组）一并吞掉，是掩盖不是修。
        for sec in debug::dwarf_elf_sections(&dbg) {
            cg.push_extra_section(sec);
        }
        // BTF 已在 build_dwarf_full 中生成
        if !dbg.btf.is_empty() {
            cg.push_extra_section(elf::ElfSection {
                name: ".BTF".to_string(),
                data: dbg.btf,
                ty: elf::SHT_PROGBITS,
                flags: 0,
                align: 4,
                group_name: None,
            });
        }
        // .BTF.ext: func_info + line_info (BPF verifier)
        if !dbg.btf_ext.is_empty() {
            cg.push_extra_section(elf::ElfSection {
                name: ".BTF.ext".to_string(),
                data: dbg.btf_ext,
                ty: elf::SHT_PROGBITS,
                flags: 0,
                align: 4,
                group_name: None,
            });
        }
    }

    if opts.relocatable && input.target.unwind_tables {
        let frames = cg.func_frames(module);
        if !frames.is_empty() {
            cg.push_extra_section(elf::ElfSection {
                name: ".eh_frame".to_string(),
                data: debug::build_eh_frame_with_arch(&frames, debug::ArchKind::AArch64),
                ty: elf::SHT_PROGBITS,
                flags: elf::SHF_ALLOC,
                align: 8,
                group_name: None,
            });
        }
    }

    if opts.sanitizers.any() {
        cg.push_extra_section(elf::ElfSection {
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
            cg.push_extra_section(elf::ElfSection {
                name: ".dcc.offload".to_string(),
                data: o.to_binary(),
                ty: elf::SHT_PROGBITS,
                flags: 0,
                align: 8,
                group_name: None,
            });
        }
    }
}
