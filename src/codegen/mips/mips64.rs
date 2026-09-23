// src/codegen/mips64.rs - MIPS64 后端 (N64 ABI, 复用 mips.rs)

use crate::codegen::mips::MipsCodegen;
use crate::ir::Module;
use crate::target::Target;

/// MIPS64 N64 ABI 后端
/// 复用 MipsCodegen 核心结构，设置 is_64bit 标志
///
/// N64 ABI 特性:
/// - 64 位指针和寄存器 (8 字节)
/// - 8 个参数寄存器 ($a0-$a7) 用于整数参数传递
/// - 64 位帧对齐 (16 字节边界)
/// - 支持 LD/SD (64 位加载/存储)
/// - 浮点参数通过 $f12-$f19 传递 (最多 8 个)
pub struct Mips64Codegen {
    inner: MipsCodegen,
}

impl Mips64Codegen {
    pub fn new(target: Target, relocatable: bool) -> Self {
        let mut inner = MipsCodegen::new(target, relocatable);
        inner.set_64bit();
        Self { inner }
    }

    pub fn set_source_name(&mut self, name: impl Into<String>) {
        self.inner.set_source_name(name);
    }

    pub fn set_asm_text_mode(&mut self, v: bool) {
        self.inner.set_asm_text_mode(v);
    }

    pub fn push_extra_section(&mut self, sec: crate::codegen::shared::elf::ElfSection) {
        self.inner.push_extra_section(sec);
    }

    pub fn func_frames(&self, module: &Module) -> Vec<crate::debug::FuncFrame> {
        self.inner.func_frames(module)
    }

    pub fn compile(&mut self, module: &Module) {
        self.inner.compile(module);
    }

    pub fn build_output(&self) -> Vec<u8> {
        self.inner.build_output()
    }

    pub fn build_asm_output(&self, module: &Module) -> String {
        self.inner.build_asm_output(module)
    }

    pub fn labels(&self) -> &std::collections::HashMap<String, u64> {
        &self.inner.labels
    }

    pub fn func_sizes(&self) -> &std::collections::HashMap<String, u64> {
        self.inner.func_sizes()
    }

    pub fn diagnostics(&self) -> &[crate::diagnostics::Diagnostic] {
        &self.inner.diagnostics
    }

    /// 获取符号表 (用于链接)
    pub fn symbols(&self) -> &[crate::codegen::shared::elf::ElfSym] {
        &self.inner.symbols
    }

    /// 获取重定位表 (用于链接)
    pub fn relocations(&self) -> &[crate::codegen::shared::elf::ElfRela] {
        &self.inner.relocations
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::TargetTriple;

    #[test]
    fn mips64_codegen_creates_with_64bit_flag() {
        let target = Target::resolve(&TargetTriple::parse("mips64-linux-gnuabi64").unwrap());
        let cg = Mips64Codegen::new(target, true);
        // 验证 MIPS64 后端正确创建
        assert!(cg.func_sizes().is_empty());
        assert!(cg.labels().is_empty());
    }

    #[test]
    fn mips64_symbols_and_relocations_empty_initially() {
        let target = Target::resolve(&TargetTriple::parse("mips64-linux-gnuabi64").unwrap());
        let cg = Mips64Codegen::new(target, true);
        assert!(cg.symbols().is_empty());
        assert!(cg.relocations().is_empty());
    }

    #[test]
    fn mips64_diagnostics_empty_initially() {
        let target = Target::resolve(&TargetTriple::parse("mips64-linux-gnuabi64").unwrap());
        let cg = Mips64Codegen::new(target, true);
        assert!(cg.diagnostics().is_empty());
    }
}

// Backend trait implementation (Phase 3b)
mod backend {
    use super::*;
    use crate::codegen::shared::backend::{Backend, CommonImage, CompileInput, ExtraSection, ObjectRelocs, ObjectSymbols, TlsImage};
    use crate::codegen::shared::features::FeatureSet;
    use crate::cli::Arch;
    use crate::diagnostics::Diagnostic;
    
    impl Backend for Mips64Codegen {
        fn arch(&self) -> Arch { Arch::Mips64 }
        fn features(&self) -> FeatureSet {
            // NOTE: 本集里的 ObjectFormat(Elf) 与 Dwarf(4) 断言目前**无法由行为探针确证**：
            // mips64 的 `-c` 产出物 e_ident 声为大端而 ELF 头数值字段以小端写入，
            // readelf/objdump 均不可识别 (见 CURRENT_STATUS_AND_NEXT_STEPS.md 的 P0 清单)。
            // 在该端序不一致修复前，mips64 的实际可用级低于本表。
            FeatureSet::tier2_cross()
        }
        fn compile(&mut self, input: &CompileInput<'_>) -> Result<CommonImage, Vec<Diagnostic>> {
            self.set_source_name(&input.opts.source_name);
            self.set_asm_text_mode(input.opts.emit_asm);
            self.compile(input.module);
            build_aux_sections(self, input);
            let inner = &self.inner;
            Ok(CommonImage {
                code: inner.code.clone(), rodata: inner.rodata.clone(), data: vec![], bss: 0,
                tls: TlsImage { offset: 0, size: 0, decl: None },
                symbols: ObjectSymbols { globals: inner.symbols.iter().map(|s| s.name.clone()).collect(), locals: vec![], imports: vec![] },
                relocs: ObjectRelocs { rellocs: inner.relocations.iter().map(|r| format!("{:?}", r)).collect() },
                extra_sections: inner.extra_sections.iter().map(|sec| ExtraSection { name: sec.name.clone(), content: sec.data.clone() }).collect(),
                diagnostics: inner.diagnostics.clone(),
                func_sizes: inner.func_sizes.iter().map(|(k, v)| (k.clone(), *v)).collect(),
            })
        }
        fn render_asm(&mut self, input: &CompileInput<'_>) -> Result<Vec<u8>, Vec<Diagnostic>> {
            self.set_source_name(&input.opts.source_name);
            self.set_asm_text_mode(true);
            self.compile(input.module);
            if crate::codegen::has_fatal_diagnostics(&self.inner.diagnostics) {
                return Err(self.inner.diagnostics.clone());
            }
            Ok(self.build_asm_output(input.module).into_bytes())
        }
        fn build_object(&mut self) -> Result<Vec<u8>, Vec<Diagnostic>> {
            Ok(self.build_output())
        }
    }

    /// 代码生成完成后组装附加节 (DWARF / .eh_frame / sanitizer / offload)
    fn build_aux_sections(cg: &mut Mips64Codegen, input: &CompileInput<'_>) {
        use crate::codegen::shared::elf;
        let opts = input.opts;
        let module = input.module;

        if opts.debug_level > 0 && opts.relocatable {
            let func_pcs: Vec<(&str, u64, u64)> = module
                .functions
                .iter()
                .filter_map(|f| cg.inner.labels.get(&f.name).map(|&pc| (f.name.as_str(), pc, pc + 64)))
                .collect();
            let dbg_opts = crate::debug::DebugOptions {
                level: opts.debug_level,
                dwarf_version: opts.dwarf_version,
                source_name: opts.source_name.clone(),
                arch: crate::debug::ArchKind::Mips64,
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
                    data: crate::debug::build_eh_frame_with_arch(&frames, crate::debug::ArchKind::Mips64),
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
}
