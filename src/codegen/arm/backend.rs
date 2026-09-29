// ARM backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::cli::Arch;
use crate::codegen::shared::backend::{
    Backend, CommonImage, CompileInput, ExtraSection, ObjectRelocs, ObjectSymbols, TlsImage,
};
use crate::codegen::shared::features::FeatureSet;
use crate::diagnostics::Diagnostic;
use crate::ir::Module;

use super::codegen::ArmCodegen;

impl Backend for ArmCodegen {
    fn arch(&self) -> Arch {
        Arch::Arm
    }

    fn features(&self) -> FeatureSet {
        FeatureSet::tier2_cross()
    }

    fn compile(&mut self, input: &CompileInput<'_>) -> Result<CommonImage, Vec<Diagnostic>> {
        self.set_source_name(&input.opts.source_name);
        self.compile(input.module);
        build_aux_sections(self, input);
        Ok(CommonImage {
            code: self.code.clone(),
            rodata: self.rodata.clone(),
            data: vec![],
            bss: 0,
            tls: TlsImage {
                offset: 0,
                size: 0,
                decl: None,
            },
            symbols: ObjectSymbols {
                globals: self.symbols.iter().map(|s| s.name.clone()).collect(),
                locals: vec![],
                imports: vec![],
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
        // -S 与 -c 共用同一次 codegen 运行, 仅渲染器不同。
        self.set_source_name(&input.opts.source_name);
        self.compile(input.module);
        self.render_asm_from_state(input.module)
    }

    fn render_asm_from_state(&mut self, module: &Module) -> Result<Vec<u8>, Vec<Diagnostic>> {
        if crate::codegen::has_fatal_diagnostics(&self.diagnostics) {
            return Err(self.diagnostics.clone());
        }
        Ok(self.build_asm_output(module).into_bytes())
    }

    fn build_object(&mut self) -> Result<Vec<u8>, Vec<Diagnostic>> {
        Ok(self.build_output())
    }
}

/// 代码生成完成后组装附加节 (DWARF / .eh_frame / sanitizer / offload)
fn build_aux_sections(cg: &mut ArmCodegen, input: &CompileInput<'_>) {
    use crate::codegen::shared::elf;
    let opts = input.opts;
    let module = input.module;

    if opts.debug_level > 0 && opts.relocatable {
        let func_pcs: Vec<(&str, u64, u64)> = module
            .functions
            .iter()
            .filter_map(|f| {
                cg.labels
                    .get(&f.name)
                    .map(|&pc| (f.name.as_str(), pc, pc + 64))
            })
            .collect();
        let dbg_opts = crate::debug::DebugOptions {
            level: opts.debug_level,
            dwarf_version: opts.dwarf_version,
            source_name: opts.source_name.clone(),
            arch: crate::debug::ArchKind::Arm,
            ..crate::debug::DebugOptions::default()
        };
        let cu = crate::codegen::build_dwarf_cu(module, &opts.source_name, &func_pcs, vec![]);
        let dbg = crate::debug::build_dwarf_full(&cu, &dbg_opts);
        let extra_secs = crate::debug::dwarf_elf_sections(&dbg);
        // 不再在这里手工 push `.debug_info`：它与 `extra_secs`（来自
        // `DebugSections::to_elf_sections`）里的那份逐字节相同，双发只会得到
        // 两个同名 PROGBITS 节。完整理由见 `aarch64/backend.rs` 里同一处注释。
        for sec in extra_secs {
            cg.push_extra_section(sec);
        }
    }

    if opts.relocatable {
        let frames = cg.func_frames(module);
        if !frames.is_empty() {
            cg.push_extra_section(elf::ElfSection {
                name: ".eh_frame".to_string(),
                data: crate::debug::build_eh_frame_with_arch(&frames, crate::debug::ArchKind::Arm),
                ty: elf::SHT_PROGBITS,
                flags: elf::SHF_ALLOC,
                align: 4,
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
