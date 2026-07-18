// src/link/mod.rs - 多 TU 链接器入口
//
// 包含：
//   - ELF ET_REL 目标文件解析（object.rs）
//   - 跨 TU 符号解析（resolve.rs）
//   - 节合并 + 虚拟地址布局（layout.rs）
//   - 重定位引擎（relocate.rs）
//   - PLT / GOT 生成（plt_got.rs）
//   - ET_EXEC 输出（elf_exec.rs）
//   - ET_DYN 输出（PIE / .so）（elf_dyn.rs）
//   - 自有 LTO 位码（lto_bitcode.rs）
//
// 用法：
//   let r = link::link_multi_tu(&[a.o, b.o], &output, &LinkConfig::default());
//   let r = link::link_pie(&[a.o, b.o], &output, &LinkConfig::default());
//   let r = link::link_shared(&[a.o, b.o], &output, &LinkConfig::default());
//   let r = link::link_with_lto(&[a.o, b.o], &output, &LinkConfig::default(), &[("a.c", module), ...]);

pub mod object;
pub mod resolve;
pub mod layout;
pub mod relocate;
pub mod plt_got;
pub mod elf_exec;
pub mod elf_dyn;
pub mod lto_bitcode;
pub mod legacy;

use std::path::{Path, PathBuf};

use crate::diagnostics::{Diagnostic, Span};

pub use object::InputBundle;
pub use resolve::resolve;
pub use layout::{LayoutBuilder, OutputSectionKind};
pub use relocate::apply_relocations;
pub use lto_bitcode::{serialize_module, deserialize_module, run_cross_tu_lto, BitcodeModule};
pub use elf_exec::write_elf_exec_x86_64;
pub use elf_dyn::{write_elf_pie_x86_64, write_elf_shared_x86_64};
pub use legacy::link_relocatable;

/// 链接配置
#[derive(Debug, Clone)]
pub struct LinkConfig {
    /// 输出类型
    pub output_kind: OutputKind,
    /// 是否启用 IR 级 LTO
    pub lto: bool,
    /// 入口符号
    pub entry: String,
    /// 基础地址（ET_EXEC 默认 0x400000，PIE/.so 默认 0）
    pub base_addr: u64,
    /// 共享库 SONAME
    pub soname: Option<String>,
    /// 需要的共享库列表（.so）
    pub needed: Vec<String>,
    /// 是否丢弃 debug 节
    pub strip_debug: bool,
    /// gc-sections
    pub gc_sections: bool,
    /// 显式 unresolved 允许列表（运行时入口/弱 libc）
    pub allowed_undef: Vec<String>,
    /// 详细日志
    pub verbose: bool,
}

impl Default for LinkConfig {
    fn default() -> Self {
        Self {
            output_kind: OutputKind::Executable,
            lto: false,
            entry: "_start".into(),
            base_addr: 0x400000,
            soname: None,
            needed: vec![],
            strip_debug: false,
            gc_sections: true,
            allowed_undef: vec![
                "memcpy".into(), "memmove".into(), "memset".into(), "memcmp".into(),
                "printf".into(), "puts".into(), "exit".into(), "abort".into(),
                "malloc".into(), "free".into(),
                "__stack_chk_fail".into(), "__cxa_atexit".into(), "__cxa_finalize".into(),
                "_start".into(), "main".into(),
            ],
            verbose: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputKind {
    Executable,  // ET_EXEC
    PIE,         // ET_DYN, entry != 0
    Shared,      // ET_DYN, entry = 0
}

/// 链接诊断
#[derive(Debug)]
pub struct LinkDiagnostic {
    pub msg: String,
    pub code: String,
}

impl std::fmt::Display for LinkDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.msg)
    }
}

fn diag(code: &'static str, msg: impl Into<String>) -> Vec<Diagnostic> {
    vec![Diagnostic::err(code, msg.into(), Span::point("", 0, 0))]
}

/// 主入口：链接多个 .o 文件
pub fn link_multi_tu(inputs: &[PathBuf], output: &Path, cfg: &LinkConfig) -> Result<Vec<u8>, Vec<Diagnostic>> {
    let bundle = InputBundle::from_files(inputs).map_err(|e| diag("E0301", e.to_string()))?;
    link_from_bundle(&bundle, output, cfg)
}

pub fn link_from_bundle(bundle: &InputBundle, output: &Path, cfg: &LinkConfig) -> Result<Vec<u8>, Vec<Diagnostic>> {
    if cfg.verbose {
        eprintln!("dcc-link: {} object(s), output = {}", bundle.objects.len(), output.display());
    }
    // 1. 符号解析
    let undef: Vec<&str> = cfg.allowed_undef.iter().map(|s| s.as_str()).collect();
    let (resolved, resolve_errors) = resolve(bundle, &undef);
    if !resolve_errors.is_empty() {
        let errs: Vec<Diagnostic> = resolve_errors.iter().map(|e| {
            Diagnostic::err("E0302", e.to_string(), Span::point("", 0, 0))
        }).collect();
        return Err(errs);
    }
    // 2. 布局
    let mut builder = LayoutBuilder::new();
    builder.is_pie = cfg.output_kind == OutputKind::PIE;
    builder.is_shared = cfg.output_kind == OutputKind::Shared;
    builder.base_addr = if cfg.output_kind == OutputKind::Executable { cfg.base_addr } else { 0 };
    builder.entry_name = cfg.entry.clone();
    builder.gc_sections = cfg.gc_sections;
    builder.emit_dynamic = matches!(cfg.output_kind, OutputKind::PIE | OutputKind::Shared);
    builder.keep_debug = !cfg.strip_debug;
    let mut layout = builder.build(bundle, &resolved).map_err(|e| diag("E0303", e))?;
    // 3. 生成 PLT/GOT（仅动态）
    if builder.emit_dynamic {
        plt_got::fill_plt_got(&mut layout);
    }
    // 4. 应用重定位
    let _dyn = apply_relocations(&mut layout).map_err(|e| diag("E0304", e.to_string()))?;
    // 5. 输出 ELF
    let bytes = match cfg.output_kind {
        OutputKind::Executable => write_elf_exec_x86_64(&layout),
        OutputKind::PIE => write_elf_pie_x86_64(&layout),
        OutputKind::Shared => write_elf_shared_x86_64(&layout),
    };
    if cfg.verbose {
        eprintln!("dcc-link: wrote {} ({} bytes)", output.display(), bytes.len());
    }
    Ok(bytes)
}

/// 链接并启用 LTO：先读 .dcc.lto 节，做跨 TU 优化后写回新 .o
pub fn link_with_lto(
    inputs: &[PathBuf],
    output: &Path,
    cfg: &LinkConfig,
    bitcodes: &[(String, crate::ir::Module)],
) -> Result<Vec<u8>, Vec<Diagnostic>> {
    // 1. 序列化每个 TU 的位码并合并
    let mut modules: Vec<BitcodeModule> = bitcodes.iter()
        .map(|(path, m)| {
            let payload = serialize_module(m, path);
            let mut bc = deserialize_module(&payload).expect("self roundtrip");
            bc.source_path = path.clone();
            bc
        })
        .collect();
    // 2. 跨 TU 优化
    run_cross_tu_lto(&mut modules);
    // 3. 链接（简化：LTO 暂只做 DCE，后续可加 inlining 后 codegen）
    let bytes = link_multi_tu(inputs, output, cfg)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::elf::{self, ElfSection, ElfSym, ElfRela, SHT_PROGBITS, STB_GLOBAL, STT_FUNC, STV_DEFAULT};

    fn build_minimal_obj(name: &str, syms: &[ElfSym], text: &[u8]) -> Vec<u8> {
        elf::build_elf_rel_x86_64(
            text, &[], &[], 0, &[], 0, 16, 16, 16, 16, 16,
            syms, &[], &[], &[], &[],
        )
    }

    #[test]
    fn link_two_object_executable() {
        let dir = std::env::temp_dir();
        let p1 = dir.join("dcc-link-1.o");
        let p2 = dir.join("dcc-link-2.o");
        // a.c: 导出 helper
        let a_sym = ElfSym {
            name: "helper".into(), section: 1, value: 0, size: 4,
            bind: STB_GLOBAL, ty: STT_FUNC, visibility: STV_DEFAULT,
        };
        let a = build_minimal_obj("a", &[a_sym], &[0xb8, 0x2a, 0, 0, 0, 0xc3]);
        // b.c: 导出 main，引用 helper
        let b_sym = ElfSym {
            name: "main".into(), section: 1, value: 0, size: 10,
            bind: STB_GLOBAL, ty: STT_FUNC, visibility: STV_DEFAULT,
        };
        // call helper; mov $0,%eax; ret
        // e8 <rel32> ; b8 00 00 00 00 ; c3
        let b_text = vec![0xe8, 0, 0, 0, 0, 0xb8, 0, 0, 0, 0, 0xc3];
        let mut b_syms = vec![b_sym];
        // 添加对 helper 的 undefined 引用
        b_syms.push(ElfSym {
            name: "helper".into(), section: 0, value: 0, size: 0,
            bind: STB_GLOBAL, ty: STT_FUNC, visibility: STV_DEFAULT,
        });
        let b_relocs = vec![ElfRela { offset: 1, sym_index: 1, rel_type: 4, addend: -4 }];
        let b = elf::build_elf_rel_x86_64(
            &b_text, &[], &[], 0, &[], 0, 16, 16, 16, 16, 16,
            &b_syms, &b_relocs, &[], &[], &[],
        );
        std::fs::write(&p1, &a).unwrap();
        std::fs::write(&p2, &b).unwrap();
        let mut cfg = LinkConfig::default();
        cfg.entry = "main".into();  // 用 main 当 entry
        let bytes = link_multi_tu(&[p1.clone(), p2.clone()], Path::new("test.out"), &cfg)
            .expect("link ok");
        // 头 4 字节是 ELF magic
        assert_eq!(&bytes[0..4], b"\x7fELF");
        let _ = std::fs::remove_file(p1);
        let _ = std::fs::remove_file(p2);
    }
}
