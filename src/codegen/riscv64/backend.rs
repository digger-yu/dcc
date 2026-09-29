// src/codegen/riscv64/backend.rs - Backend trait wiring for RISC-V 64.
//
// Mirrors the x86_64 wiring: apply EmitOptions, run the inherent compile
// pipeline over the module, then map the internal state to CommonImage.

use crate::cli::Arch;
use crate::codegen::riscv64::Riscv64Codegen;
use crate::codegen::shared::backend::{
    Backend, CommonImage, CompileInput, ExtraSection, ObjectRelocs, ObjectSymbols, TlsImage,
};
use crate::codegen::shared::elf;
use crate::codegen::shared::features::FeatureSet;
use crate::diagnostics::Diagnostic;
use crate::ir::Module;

impl Backend for Riscv64Codegen {
    fn arch(&self) -> Arch {
        Arch::RiscV64
    }

    fn features(&self) -> FeatureSet {
        FeatureSet::tier1_cross()
    }

    fn compile(&mut self, input: &CompileInput<'_>) -> Result<CommonImage, Vec<Diagnostic>> {
        apply_emit_options(self, input);
        self.compile(input.module);
        // 统一失败策略: 只有 Error 才构成失败 (旧写法把 warning 也当错误)。
        if crate::codegen::has_fatal_diagnostics(&self.diagnostics) {
            return Err(self.diagnostics.clone());
        }
        Ok(build_image(self))
    }

    fn render_asm(&mut self, input: &CompileInput<'_>) -> Result<Vec<u8>, Vec<Diagnostic>> {
        // -S 与 -c 共用同一次 codegen 运行, 仅渲染器不同。
        apply_emit_options(self, input);
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
        if !self.diagnostics.is_empty() {
            return Err(self.diagnostics.clone());
        }
        Ok(elf::build_elf_rel_riscv64(
            &self.code,
            &self.rodata,
            &self.symbols,
            &self.relocations,
            &self.rodata_relocations,
            &self.extra_sections,
            &self.comdat_groups,
        ))
    }
}

/// EmitOptions -> codegen state (the option handling that used to live at
/// the top of the monolithic emit path).
fn apply_emit_options(cg: &mut Riscv64Codegen, input: &CompileInput<'_>) {
    let opts = input.opts;
    cg.relocatable = opts.relocatable || opts.emit_asm;
    cg.source_name = opts.source_name.clone();
}

/// Map internal codegen state to the cross-backend CommonImage.
fn build_image(cg: &Riscv64Codegen) -> CommonImage {
    CommonImage {
        code: cg.code.clone(),
        rodata: cg.rodata.clone(),
        data: vec![],
        bss: 0,
        tls: TlsImage::default(),
        symbols: ObjectSymbols {
            globals: cg
                .symbols
                .iter()
                .filter(|s| s.bind == elf::STB_GLOBAL)
                .map(|s| s.name.clone())
                .collect(),
            locals: cg
                .symbols
                .iter()
                .filter(|s| s.bind == elf::STB_LOCAL)
                .map(|s| s.name.clone())
                .collect(),
            imports: vec![],
        },
        relocs: ObjectRelocs {
            rellocs: cg.relocations.iter().map(|r| format!("{:?}", r)).collect(),
        },
        extra_sections: cg
            .extra_sections
            .iter()
            .map(|sec| ExtraSection {
                name: sec.name.clone(),
                content: sec.data.clone(),
            })
            .collect(),
        diagnostics: cg.diagnostics.clone(),
        func_sizes: cg.func_sizes.iter().map(|(k, v)| (k.clone(), *v)).collect(),
        asm_passthrough: cg.asm_passthrough.clone(),
        asm_passthrough_regions: cg.asm_passthrough_regions.clone(),
    }
}
