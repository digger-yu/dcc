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
//   - GNU hash TIE 压缩（gnu_hash.rs）
//   - .eh_frame_hdr 搜索表（eh_frame.rs）
//   - .note.gnu.build-id 段生成（build_id.rs）
//   - IFUNC 间接函数解析（ifunc.rs）
//
// 用法：
//   let r = link::link_multi_tu(&[a.o, b.o], &output, &LinkConfig::default());
//   let r = link::link_pie(&[a.o, b.o], &output, &LinkConfig::default());
//   let r = link::link_shared(&[a.o, b.o], &output, &LinkConfig::default());
//   let r = link::link_with_lto(&[a.o, b.o], &output, &LinkConfig::default(), &[("a.c", module), ...]);

pub mod build_id;
pub mod dso_cache;
pub mod eh_frame;
pub mod elf_dyn;
pub mod elf_exec;
pub mod fini_dispatch;
pub mod gnu_hash;
pub mod ifunc;
pub mod layout;
pub mod legacy;
pub mod link_opts;
pub mod lto_bitcode;
pub mod object;
pub mod plt_got;
pub mod relocate;
pub mod resolve;
pub mod symver;
pub mod target_clones;
pub mod vdso;

use std::path::{Path, PathBuf};

use crate::debug::ArchKind;
use crate::diagnostics::{Diagnostic, Span};

pub use elf_dyn::{
    write_elf_pie_aarch64, write_elf_pie_arm, write_elf_pie_mips64, write_elf_pie_riscv64,
    write_elf_pie_x86_64, write_elf_shared_aarch64, write_elf_shared_arm, write_elf_shared_mips64,
    write_elf_shared_riscv64, write_elf_shared_x86_64,
};
pub use elf_exec::{
    write_elf_exec_aarch64, write_elf_exec_arm, write_elf_exec_mips64, write_elf_exec_riscv64,
    write_elf_exec_x86_64, write_elf_relocatable_aarch64, write_elf_relocatable_arm,
    write_elf_relocatable_mips64, write_elf_relocatable_riscv64, write_elf_relocatable_x86_64,
};
pub use layout::LayoutBuilder;
pub use legacy::link_relocatable;
pub use lto_bitcode::{deserialize_module, run_cross_tu_lto, serialize_module, BitcodeModule};
pub use object::InputBundle;
pub use relocate::{apply_relocations, collect_pie_dynamic_relas};

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
    /// --exclude-libs：需要隐藏的静态库符号集合（all / libname1,libname2）
    pub exclude_libs: Vec<String>,
    /// --build-id：build-id 风格 (None / Sha1 / Uuid / Md5 / Hex)
    pub build_id: BuildIdStyle,
    /// --no-undefined-version：拒绝无版本 undef 符号
    pub no_undefined_version: bool,
    /// 详细日志
    pub verbose: bool,
    /// 扩展链接选项 (-z *, -rpath, -fuse-ld= 等)
    pub opts: link_opts::LinkOpts,
    /// DT_NEEDED: 需要的共享库列表 (-lxxx 转化的 so 名)
    pub libs: Vec<String>,
    /// 显式 unresolved 根符号 (--entry 之外的 gc-sections 根)
    pub gc_sections_root: Vec<String>,
    /// -fno-semantic-interposition / -fsemantic-interposition
    pub no_semantic_interposition: bool,
    /// -fuse-ld=
    pub fuse_ld: Option<String>,
    /// VersionTable (--no-undefined-version 等)
    pub version_table: symver::VersionTable,
    /// target_clones 收集表
    pub target_clones: target_clones::TargetClonesTable,
    /// FiniArray builder (__cxa_atexit / destructor 调度)
    pub fini_builder: fini_dispatch::FiniArrayBuilder,
    /// 目标架构 (决定 ELF e_machine 及链接行为)
    pub arch: ArchKind,
    /// 自定义动态链接器路径 (-Wl,--dynamic-linker=)
    pub dynamic_linker: Option<String>,
}

/// build-id 风格
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BuildIdStyle {
    #[default]
    None,
    Sha1,
    FastSha1, // 优化版：复用已计算 hash，避免重算
    Uuid,
    Md5,
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
                "memcpy".into(),
                "memmove".into(),
                "memset".into(),
                "memcmp".into(),
                "printf".into(),
                "puts".into(),
                "exit".into(),
                "abort".into(),
                "malloc".into(),
                "free".into(),
                "__stack_chk_fail".into(),
                "__cxa_atexit".into(),
                "__cxa_finalize".into(),
                "_start".into(),
                "main".into(),
            ],
            exclude_libs: vec![],
            build_id: BuildIdStyle::None,
            no_undefined_version: false,
            verbose: false,
            opts: link_opts::LinkOpts::from_env(),
            libs: vec![],
            gc_sections_root: vec![],
            no_semantic_interposition: false,
            fuse_ld: None,
            version_table: symver::VersionTable::new(),
            target_clones: target_clones::TargetClonesTable::new(),
            fini_builder: fini_dispatch::FiniArrayBuilder::new(),
            arch: ArchKind::X86_64,
            dynamic_linker: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::upper_case_acronyms)]
pub enum OutputKind {
    Executable,  // ET_EXEC
    PIE,         // ET_DYN, entry != 0
    Shared,      // ET_DYN, entry = 0
    Relocatable, // ET_REL, -r 自研合并
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
pub fn link_multi_tu(
    inputs: &[PathBuf],
    output: &Path,
    cfg: &LinkConfig,
) -> Result<Vec<u8>, Vec<Diagnostic>> {
    let bundle = InputBundle::from_files(inputs).map_err(|e| diag("E0301", e.to_string()))?;
    link_from_bundle(&bundle, output, cfg)
}

pub fn link_from_bundle(
    bundle: &InputBundle,
    output: &Path,
    cfg: &LinkConfig,
) -> Result<Vec<u8>, Vec<Diagnostic>> {
    if cfg.verbose {
        eprintln!(
            "dcc-link: {} object(s), output = {}",
            bundle.objects.len(),
            output.display()
        );
        eprintln!("dcc-link: opts: {}", link_opts::opts_summary(&cfg.opts));
    }
    // 0. 收集 input bundle 中的 .symver / target_clones / destructor 信息
    let mut cfg_owned = cfg.clone();
    collect_link_directives(bundle, &mut cfg_owned);
    let cfg = &cfg_owned;
    // 1. 符号解析
    let undef: Vec<&str> = cfg.allowed_undef.iter().map(|s| s.as_str()).collect();
    let (resolved, resolve_errors) = resolve::resolve_with_config(bundle, &undef, cfg);
    if !resolve_errors.is_empty() {
        let errs: Vec<Diagnostic> = resolve_errors
            .iter()
            .map(|e| Diagnostic::err("E0302", e.to_string(), Span::point("", 0, 0)))
            .collect();
        return Err(errs);
    }
    // 2. 布局
    let mut builder = LayoutBuilder::new();
    builder.is_pie = cfg.output_kind == OutputKind::PIE;
    builder.is_shared = cfg.output_kind == OutputKind::Shared;
    builder.is_relocatable = cfg.output_kind == OutputKind::Relocatable;
    builder.base_addr = if cfg.output_kind == OutputKind::Executable {
        cfg.base_addr
    } else {
        0
    };
    builder.entry_name = cfg.entry.clone();
    builder.gc_sections = cfg.gc_sections;
    builder.emit_dynamic = matches!(cfg.output_kind, OutputKind::PIE | OutputKind::Shared);
    builder.keep_debug = !cfg.strip_debug;
    builder.gc_sections_root = cfg.gc_sections_root.clone();
    builder.dynamic_linker = cfg.dynamic_linker.clone();
    let mut layout = builder
        .build(bundle, &resolved)
        .map_err(|e| diag("E0303", e))?;
    // 2.5 SHF_GNU_RETAIN 标记 (M12+ 链接器补洞)
    link_opts::apply_retain_flags(&mut layout);
    // 2.5.1 压缩调试节 (--compress-debug-sections)
    if let Some(ref method) = cfg.opts.compress_debug {
        link_opts::compress_debug_sections(&mut layout, method);
    }
    // 2.5.2 .interp 节由 LayoutBuilder::build 内部创建 (需参与 vaddr 分配,
    // 且必须位于首个 PT_LOAD 段内), 见 layout.rs 的 build 流程。
    // 2.6 fini_array / __dso_handle 调度
    fini_dispatch::ensure_fini_runtime_symbols(&mut layout);
    // 2.7 -z pack-relative-relocs -> .gnu.pack.relocs
    if cfg.opts.pack_relative_relocs {
        link_opts::add_pack_relocs_section(&mut layout);
    }
    // 2.8 --gdb-index
    if cfg.opts.gdb_index {
        link_opts::add_gdb_index_section(&mut layout);
    }
    // 2.9 target_clones IFUNC resolver + IRELATIVE 重定位
    if !cfg.target_clones.funcs.is_empty() {
        let (new_syms, irel_relas) = cfg.target_clones.gen_irelative_relas(&layout);
        for s in new_syms {
            layout.symbols.push(s);
        }
        // IRELATIVE 重定位加入 output_relas
        for r in irel_relas {
            layout.output_relas.push(r);
        }
    }
    // 2.10 为静态链接可执行文件合成 _start 桩 (如果没有 _start)
    ensure_start_stub(&mut layout);
    // 2.11 PIE/Shared: 数据段绝对重定位 (R_X86_64_64) 转 R_X86_64_RELATIVE
    // 动态重定位 (必须在 fill_plt_got 之前收集, 使 .rela.dyn 的 size 分配
    // 计入这些条目; ld.so 加载时写入 load_bias + addend)。
    let pie_relas = collect_pie_dynamic_relas(&mut layout);
    // 3. 生成 PLT/GOT（仅动态）
    if builder.emit_dynamic {
        // 第一遍: 填充动态节 size (vaddr 引用此时是 size=0 时的旧值)
        plt_got::fill_plt_got_with_opts(&mut layout, &cfg.opts, &pie_relas);
        // 动态节 size 已确定: 重算 vaddr, 否则文件偏移与 vaddr 错位
        // (LOAD 段 filesz > memsz, 运行段错误)。
        recompute_section_vaddrs(&mut layout);
        // 第二遍: vaddr 已更新, 重新生成内容 (PLT 位移 / .dynamic 的
        // DT_STRTAB/DT_SYMTAB/DT_PLTGOT 等 vaddr 引用)。
        plt_got::fill_plt_got_with_opts(&mut layout, &cfg.opts, &pie_relas);
    }
    // 4. 应用重定位
    let _dyn = apply_relocations(&mut layout).map_err(|e| diag("E0304", e.to_string()))?;
    // 5. 输出 ELF (按架构分发)
    let bytes = match (cfg.output_kind, cfg.arch) {
        (OutputKind::Executable, ArchKind::X86_64) => write_elf_exec_x86_64(&layout),
        (OutputKind::Executable, ArchKind::AArch64) => write_elf_exec_aarch64(&layout),
        (OutputKind::Executable, ArchKind::RiscV64) => write_elf_exec_riscv64(&layout),
        (OutputKind::Executable, ArchKind::Mips64) => write_elf_exec_mips64(&layout),
        (OutputKind::Executable, ArchKind::Arm) => write_elf_exec_arm(&layout),
        (OutputKind::PIE, ArchKind::X86_64) => write_elf_pie_x86_64(&layout),
        (OutputKind::PIE, ArchKind::AArch64) => write_elf_pie_aarch64(&layout),
        (OutputKind::PIE, ArchKind::RiscV64) => write_elf_pie_riscv64(&layout),
        (OutputKind::PIE, ArchKind::Mips64) => write_elf_pie_mips64(&layout),
        (OutputKind::PIE, ArchKind::Arm) => write_elf_pie_arm(&layout),
        (OutputKind::Shared, ArchKind::X86_64) => write_elf_shared_x86_64(&layout),
        (OutputKind::Shared, ArchKind::AArch64) => write_elf_shared_aarch64(&layout),
        (OutputKind::Shared, ArchKind::RiscV64) => write_elf_shared_riscv64(&layout),
        (OutputKind::Shared, ArchKind::Mips64) => write_elf_shared_mips64(&layout),
        (OutputKind::Shared, ArchKind::Arm) => write_elf_shared_arm(&layout),
        (OutputKind::Relocatable, ArchKind::X86_64) => write_elf_relocatable_x86_64(&layout),
        (OutputKind::Relocatable, ArchKind::AArch64) => write_elf_relocatable_aarch64(&layout),
        (OutputKind::Relocatable, ArchKind::RiscV64) => write_elf_relocatable_riscv64(&layout),
        (OutputKind::Relocatable, ArchKind::Mips64) => write_elf_relocatable_mips64(&layout),
        (OutputKind::Relocatable, ArchKind::Arm) => write_elf_relocatable_arm(&layout),
        (OutputKind::Executable, _) => write_elf_exec_x86_64(&layout),
        (OutputKind::PIE, _) => write_elf_pie_x86_64(&layout),
        (OutputKind::Shared, _) => write_elf_shared_x86_64(&layout),
        (OutputKind::Relocatable, _) => write_elf_relocatable_x86_64(&layout),
    };
    if cfg.verbose {
        eprintln!(
            "dcc-link: wrote {} ({} bytes)",
            output.display(),
            bytes.len()
        );
    }
    Ok(bytes)
}

/// 收集 input bundle 中的 .symver / target_clones / destructor 信息
fn collect_link_directives(bundle: &InputBundle, cfg: &mut LinkConfig) {
    for obj in &bundle.objects {
        // 扫描 .symver 指令
        for sec in &obj.sections {
            if sec.name == ".symver" || sec.name == ".gnu.symver" {
                // 每行 8 字节指针
                for chunk in sec.data.chunks(8) {
                    if chunk.len() == 8 {
                        let ptr = u64::from_le_bytes(chunk.try_into().unwrap()) as usize;
                        if ptr < bundle.symbols.len() {
                            let sym = &bundle.symbols[ptr];
                            if !sym.name.is_empty() {
                                // 这是 .symver NAME,NAME@VER 的第二个符号 (target)
                                // 真实 name 在 .rodata (简化: 跳过解析)
                            }
                        }
                    }
                }
            }
        }
        // 扫描 .note.GNU.target_clones (target_clones 通常以 .note 段记录)
        for sym in &obj.symbols {
            if sym.name.starts_with("__target_clones__") {
                // 简化: 跳过; 让 driver 层显式注册
            }
        }
    }
    // 收集 .fini_array 已注册的 destructor (M11+)
    // dcc 在 codegen 阶段把 destructor 写入 .fini_array, 此处只统计大小
    for obj in &bundle.objects {
        for sec in &obj.sections {
            if sec.name == ".fini_array" {
                let n_dtor = (sec.size / 8) as usize;
                for i in 0..n_dtor {
                    let off = i * 8;
                    if off + 8 <= sec.data.len() {
                        let vaddr = u64::from_le_bytes(sec.data[off..off + 8].try_into().unwrap());
                        // 找对应符号
                        for s in &obj.symbols {
                            if s.section == 1
                                && s.value as usize + (obj.sections[1].data.as_ptr() as usize)
                                    == vaddr as usize
                            {
                                cfg.fini_builder.add(&s.name, 0);
                            }
                        }
                    }
                }
            }
        }
    }
}

/// 链接并启用 LTO：先读 .dcc.lto 节，做跨 TU 优化后写回新 .o
pub fn link_with_lto(
    inputs: &[PathBuf],
    output: &Path,
    cfg: &LinkConfig,
    bitcodes: &[(String, crate::ir::Module)],
) -> Result<Vec<u8>, Vec<Diagnostic>> {
    // 1. 序列化每个 TU 的位码并合并
    let mut modules: Vec<BitcodeModule> = bitcodes
        .iter()
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

/// 节 size/data 变化 (如动态节填充、入口桩追加) 后重算 vaddr,
/// 并同步重算符号 vaddr (vaddr = section.vaddr + value)。
fn recompute_section_vaddrs(layout: &mut crate::link::layout::Layout) {
    crate::link::layout::assign_section_addrs(&mut layout.sections, layout.base_addr, layout.file_base);
    for sym in &mut layout.symbols {
        let si = sym.section as usize;
        if sym.section != 0 && si < layout.sections.len() {
            sym.vaddr = layout.sections[si].vaddr + sym.value;
        }
    }
}

/// 为静态链接可执行文件合成 _start 桩 (仅当 _start 不存在且 entry=_start 时).
/// _start 调用 main(argc=0, argv=NULL, envp=NULL) 然后 exit(return).
fn ensure_start_stub(layout: &mut crate::link::layout::Layout) {
    use crate::codegen::elf::{STB_GLOBAL, STT_FUNC};
    use crate::link::layout::{OutputSectionKind, OutputSymbol};
    // 如果已存在 _start 或不是可执行文件, 跳过
    // PIE 同样需要入口桩: _start 桩全部由 PC-relative 指令构成, 位置无关。
    if layout.symbols.iter().any(|s| s.name == "_start") {
        return;
    }
    if layout.is_shared {
        return;
    }
    // 找 main 符号
    let main_vaddr = match layout.symbols.iter().find(|s| s.name == "main") {
        Some(s) => s.vaddr,
        None => return,
    };
    // 找 .text 输出节
    let text_idx = match layout
        .sections
        .iter()
        .position(|s| s.kind == OutputSectionKind::Text)
    {
        Some(i) => i,
        None => return,
    };
    let text_vaddr = layout.sections[text_idx].vaddr;
    let text_old_size = layout.sections[text_idx].size;
    let start_vaddr = text_vaddr + text_old_size;
    // 构建 _start 桩 (22 字节):
    //   xor  rdi, rdi        ; 48 31 ff
    //   xor  rsi, rsi        ; 48 31 f6
    //   xor  rdx, rdx        ; 48 31 d2
    //   call rel32           ; e8 xx xx xx xx
    //   mov  rdi, rax        ; 48 89 c7
    //   mov  eax, 60         ; b8 3c 00 00 00
    //   syscall               ; 0f 05
    let mut code = vec![
        0x48, 0x31, 0xff, // xor rdi,rdi
        0x48, 0x31, 0xf6, // xor rsi,rsi
        0x48, 0x31, 0xd2, // xor rdx,rdx
        0xe8, 0, 0, 0, 0, // call rel32 (placeholder)
        0x48, 0x89, 0xc7, // mov rdi,rax
        0xb8, 0x3c, 0, 0, 0, // mov eax,60
        0x0f, 0x05, // syscall
    ];
    // 计算 call rel32: 目标 = main_vaddr, 下一条指令地址 = start_vaddr + 9 + 5
    // call 指令在 stub 偏移 9 处 (3+3+3 字节前缀), 长度 5 字节
    let call_next = start_vaddr + 9 + 5;
    let rel32 = (main_vaddr.wrapping_sub(call_next)) as i32 as u32;
    code[10..14].copy_from_slice(&rel32.to_le_bytes()); // rel32 在 code[10..14]
                                                        // 追加到 .text 节
    let text = &mut layout.sections[text_idx];
    text.data.extend_from_slice(&code);
    text.size += code.len() as u64;
    // stub 追加改变了 .text 大小: 重算 vaddr, 否则与文件偏移错位
    recompute_section_vaddrs(layout);
    // 添加 _start 输出符号
    let start_value = text_old_size;
    let sym_idx = layout.symbols.len() as u32;
    layout.symbols.push(OutputSymbol {
        name: "_start".into(),
        value: start_value,
        vaddr: start_vaddr,
        size: code.len() as u64,
        bind: STB_GLOBAL,
        ty: STT_FUNC,
        kind: STT_FUNC,
        visibility: 0,
        section: text_idx as u16,
        shndx_if_section: 0,
    });
    layout.entry = start_vaddr;
    let _ = sym_idx;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::elf::{self, ElfRela, ElfSym, STB_GLOBAL, STT_FUNC, STV_DEFAULT};

    fn build_minimal_obj(_name: &str, syms: &[ElfSym], text: &[u8]) -> Vec<u8> {
        elf::build_elf_rel_x86_64(
            text,
            &[],
            &[],
            0,
            &[],
            0,
            16,
            16,
            16,
            16,
            16,
            syms,
            &[],
            &[],
            &[],
            &[],
        )
    }

    #[test]
    fn link_two_object_executable() {
        let dir = std::env::temp_dir();
        let p1 = dir.join("dcc-link-1.o");
        let p2 = dir.join("dcc-link-2.o");
        // a.c: 导出 helper
        let a_sym = ElfSym {
            name: "helper".into(),
            section: 1,
            value: 0,
            size: 4,
            bind: STB_GLOBAL,
            ty: STT_FUNC,
            visibility: STV_DEFAULT,
        };
        let a = build_minimal_obj("a", &[a_sym], &[0xb8, 0x2a, 0, 0, 0, 0xc3]);
        // b.c: 导出 main，引用 helper
        let b_sym = ElfSym {
            name: "main".into(),
            section: 1,
            value: 0,
            size: 10,
            bind: STB_GLOBAL,
            ty: STT_FUNC,
            visibility: STV_DEFAULT,
        };
        // call helper; mov $0,%eax; ret
        // e8 <rel32> ; b8 00 00 00 00 ; c3
        let b_text = vec![0xe8, 0, 0, 0, 0, 0xb8, 0, 0, 0, 0, 0xc3];
        let mut b_syms = vec![b_sym];
        // 添加对 helper 的 undefined 引用
        b_syms.push(ElfSym {
            name: "helper".into(),
            section: 0,
            value: 0,
            size: 0,
            bind: STB_GLOBAL,
            ty: STT_FUNC,
            visibility: STV_DEFAULT,
        });
        let b_relocs = vec![ElfRela {
            offset: 1,
            sym_index: 1,
            rel_type: 4,
            addend: -4,
        }];
        let b = elf::build_elf_rel_x86_64(
            &b_text,
            &[],
            &[],
            0,
            &[],
            0,
            16,
            16,
            16,
            16,
            16,
            &b_syms,
            &b_relocs,
            &[],
            &[],
            &[],
        );
        std::fs::write(&p1, &a).unwrap();
        std::fs::write(&p2, &b).unwrap();
        let cfg = LinkConfig {
            entry: "main".into(),
            ..LinkConfig::default()
        };
        let bytes =
            link_multi_tu(&[p1.clone(), p2.clone()], Path::new("test.out"), &cfg).expect("link ok");
        // 头 4 字节是 ELF magic
        assert_eq!(&bytes[0..4], b"\x7fELF");
        let _ = std::fs::remove_file(p1);
        let _ = std::fs::remove_file(p2);
    }
}
