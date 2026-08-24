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
pub mod linker_script;
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
    /// vDSO ELF 文件路径 (--vdso=)，用于嵌入 vDSO 文本和符号
    pub vdso: Option<PathBuf>,
    /// DSO 解析缓存 (跨 link invocation 复用, 按 path/mtime/size 去重)
    pub dso_cache: dso_cache::DsoCache,
    /// 链接器脚本路径 (-T script.lds)
    pub linker_script: Option<PathBuf>,
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
            vdso: None,
            dso_cache: dso_cache::DsoCache::new(),
            linker_script: None,
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
    builder.arch = cfg.arch;
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
    builder.needed = cfg.needed.clone();
    builder.linker_script = cfg.linker_script.clone();
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
    // 2.9b attribute((ifunc("resolver"))) 从 .dcc.ifunc 节收集 IFUNC 映射
    // 并生成 R_X86_64_IRELATIVE 重定位
    {
        let (ifunc_table, ifunc_relas) = ifunc::collect_from_bundle(bundle, &layout);
        for (name, &vaddr) in &ifunc_table.resolvers {
            if cfg.verbose {
                eprintln!("dcc-link: ifunc '{}' -> resolver @0x{:x}", name, vaddr);
            }
        }
        for r in ifunc_relas {
            layout.output_relas.push(r);
        }
    }
    // 2.10 为静态链接可执行文件合成 _start 桩 (如果没有 _start)
    ensure_start_stub(&mut layout);
    // 2.10.1 vDSO 集成: 嵌入 .text.vdso + .note.GNU-vdso + 符号
    if let Some(ref vdso_path) = cfg.vdso {
        embed_vdso(&mut layout, vdso_path, cfg.verbose)?;
    }
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
        // DSO 缓存统计
        let (hits, misses) = cfg.dso_cache.stats();
        if hits + misses > 0 {
            eprintln!(
                "dcc-link: DSO cache: {} hits, {} misses ({:.1}% hit rate)",
                hits,
                misses,
                if hits + misses > 0 {
                    (hits as f64 / (hits + misses) as f64) * 100.0
                } else {
                    0.0
                }
            );
        }
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
    crate::link::layout::assign_section_addrs(
        &mut layout.sections,
        layout.base_addr,
        layout.file_base,
    );
    for sym in &mut layout.symbols {
        let si = sym.section as usize;
        if sym.section != 0 && si < layout.sections.len() {
            sym.vaddr = layout.sections[si].vaddr + sym.value;
        }
    }
}

/// 为可执行文件合成 _start 桩 (仅当 _start 不存在且 entry=_start 时).
/// _start 调用 main(argc=0, argv=NULL, envp=NULL) 然后 exit(return).
/// 支持 x86_64, aarch64, riscv64, arm 架构 (MIPS 暂未实现).
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

    let code: Vec<u8> = match layout.arch {
        ArchKind::X86_64 => {
            // xor rdi,rdi; xor rsi,rsi; xor rdx,rdx; call rel32;
            // mov rdi,rax; mov eax,60; syscall
            let mut code = vec![
                0x48, 0x31, 0xff, // xor rdi,rdi
                0x48, 0x31, 0xf6, // xor rsi,rsi
                0x48, 0x31, 0xd2, // xor rdx,rdx
                0xe8, 0, 0, 0, 0, // call rel32 (placeholder)
                0x48, 0x89, 0xc7, // mov rdi,rax
                0xb8, 0x3c, 0, 0, 0, // mov eax,60
                0x0f, 0x05, // syscall
            ];
            // call rel32 at offset 9, 5 bytes, rel32 at [10..14]
            let call_next = start_vaddr + 9 + 5;
            let rel32 = (main_vaddr.wrapping_sub(call_next)) as i32 as u32;
            code[10..14].copy_from_slice(&rel32.to_le_bytes());
            code
        }
        ArchKind::AArch64 => {
            // mov x0,xzr; mov x1,xzr; mov x2,xzr; bl main;
            // mov x8,#93; svc #0
            let mut code = vec![
                0xe0, 0x03, 0x00, 0xaa, // mov x0, xzr
                0xe1, 0x03, 0x00, 0xaa, // mov x1, xzr
                0xe2, 0x03, 0x00, 0xaa, // mov x2, xzr
                0, 0, 0, 0, // bl main (placeholder)
                0xa8, 0x0b, 0x80, 0xd2, // mov x8, #93
                0x01, 0x00, 0x00, 0xd4, // svc #0
            ];
            // bl at offset 12, 4 bytes
            let call_next = start_vaddr + 12 + 4;
            let offset = ((main_vaddr.wrapping_sub(call_next)) as i64 / 4) as i32;
            let bl_insn = 0x94000000u32 | ((offset as u32) & 0x03FFFFFF);
            code[12..16].copy_from_slice(&bl_insn.to_le_bytes());
            code
        }
        ArchKind::RiscV64 => {
            // addi a0,x0,0; addi a1,x0,0; addi a2,x0,0;
            // jal ra,main; addi a7,x0,93; ecall
            let mut code = vec![
                0x13, 0x05, 0x00, 0x00, // addi a0, x0, 0
                0x93, 0x05, 0x00, 0x00, // addi a1, x0, 0
                0x13, 0x06, 0x00, 0x00, // addi a2, x0, 0
                0, 0, 0, 0, // jal ra, main (placeholder)
                0x93, 0x08, 0xd0, 0x05, // addi a7, x0, 93
                0x73, 0x00, 0x00, 0x00, // ecall
            ];
            // jal at offset 12, 4 bytes
            let call_next = start_vaddr + 12 + 4;
            let offset = ((main_vaddr.wrapping_sub(call_next)) as i64 / 4) as i32;
            let imm = offset as u32;
            // RISC-V JAL rd=ra(x1) encoding:
            // imm[20|10:1|11|19:12] rd=00001 1101111
            let mut jal_insn = 0x6fu32;
            jal_insn |= 1 << 7; // rd = ra = x1
            let bit20 = (imm >> 20) & 1;
            let bits10_5 = (imm >> 5) & 0x3f;
            let bit11 = (imm >> 11) & 1;
            let bits19_12 = (imm >> 12) & 0xff;
            jal_insn |= bit20 << 31;
            jal_insn |= bits10_5 << 25;
            jal_insn |= bit11 << 20;
            jal_insn |= bits19_12 << 12;
            code[12..16].copy_from_slice(&jal_insn.to_le_bytes());
            code
        }
        ArchKind::Mips | ArchKind::Mips64 => {
            // MIPS _start 暂未实现 (需要 ABI 确认延迟槽/GP 寄存器)
            return;
        }
        ArchKind::Arm => {
            // mov r0,#0; mov r1,#0; mov r2,#0; bl main;
            // mov r7,#1; svc #0
            let mut code = vec![
                0x00, 0x00, 0xa0, 0xe3, // mov r0, #0
                0x01, 0x00, 0xa0, 0xe3, // mov r1, #0
                0x02, 0x00, 0xa0, 0xe3, // mov r2, #0
                0, 0, 0, 0, // bl main (placeholder)
                0x01, 0x70, 0xa0, 0xe3, // mov r7, #1
                0x00, 0x00, 0x00, 0xef, // svc #0
            ];
            // ARM BL at offset 12, 4 bytes, pc = instruction_addr + 8
            let call_next = start_vaddr + 12 + 8;
            let offset = ((main_vaddr.wrapping_sub(call_next)) as i64 / 4) as i32;
            let bl_insn = 0x0b000000u32 | ((offset as u32) & 0x00FFFFFF);
            code[12..16].copy_from_slice(&bl_insn.to_le_bytes());
            code
        }
    };

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

/// vDSO 集成: 解析 vDSO ELF 并嵌入输出布局.
///
/// 生成:
///   - `.text.vdso` 节: 包含 vDSO .text 载荷 (可执行, 只读)
///   - `.note.GNU-vdso` 节: 包含 vDSO 元信息 (namesz/descsz/type + 符号表摘要)
///   - vDSO 符号: 每个 FUNC 符号加入输出符号表 (STB_GLOBAL, STT_FUNC)
///
/// 布局位置:
///   - `.text.vdso` 放在 .text 之后, 同属段 0 (R+X)
///   - `.note.GNU-vdso` 作为 note 节, 不加载 (flags=0)
fn embed_vdso(
    layout: &mut crate::link::layout::Layout,
    vdso_path: &Path,
    verbose: bool,
) -> Result<(), Vec<Diagnostic>> {
    use crate::codegen::elf::{
        SHF_ALLOC, SHF_EXECINSTR, SHT_NOTE, SHT_PROGBITS, STB_GLOBAL, STT_FUNC,
    };
    use crate::link::layout::{OutputSection, OutputSectionKind, OutputSymbol};

    // 1. 解析 vDSO ELF
    let info = vdso::parse_vdso(vdso_path).map_err(|e| {
        diag(
            "E0305",
            format!("failed to parse vDSO '{}': {}", vdso_path.display(), e),
        )
    })?;

    if verbose {
        eprintln!(
            "dcc-link: embedding vDSO '{}' ({} bytes text, {} symbols)",
            info.source_path,
            info.text_size,
            info.symbols.len()
        );
    }

    // 2. 找 .text 节的位置, 在其后插入 .text.vdso
    let text_idx = layout
        .sections
        .iter()
        .position(|s| s.kind == OutputSectionKind::Text);

    // 3. 创建 .text.vdso 节
    let vdso_text_name = ".text.vdso".to_string();
    let vdso_text_size = info.text.len() as u64;
    let new_idx = layout.sections.len() as u32;

    let vdso_text_section = OutputSection {
        kind: OutputSectionKind::Named(vdso_text_name.clone()),
        name: vdso_text_name,
        sh_name_off: 0,
        ty: SHT_PROGBITS,
        flags: SHF_ALLOC | SHF_EXECINSTR,
        align: 16,
        data: info.text.clone(),
        size: vdso_text_size,
        entsize: 0,
        output_index: new_idx,
        load_segment: 0, // 段 0: R+X
        vaddr: 0,        // 稍后由 recompute_section_vaddrs 分配
        file_off: 0,
        input_offsets: Vec::new(),
        input_sections: Vec::new(),
    };

    // 插入到 .text 之后 (如果有), 否则追加到末尾
    if let Some(idx) = text_idx {
        layout.sections.insert(idx + 1, vdso_text_section);
        // 更新后续节的 output_index
        for i in (idx + 2)..layout.sections.len() {
            layout.sections[i].output_index = i as u32;
        }
    } else {
        layout.sections.push(vdso_text_section);
    }

    let vdso_text_section_idx = text_idx.map(|i| i + 1).unwrap_or(new_idx as usize);
    let vdso_text_vaddr_base = layout.sections[vdso_text_section_idx].vaddr;

    // 4. 添加 vDSO 符号到输出符号表
    let mut vdso_syms: Vec<OutputSymbol> = Vec::new();
    for (name, sym) in &info.symbols {
        let sym_vaddr = vdso_text_vaddr_base + sym.text_offset;
        vdso_syms.push(OutputSymbol {
            name: name.clone(),
            value: sym.text_offset,
            vaddr: sym_vaddr,
            size: sym.size,
            bind: STB_GLOBAL,
            ty: STT_FUNC,
            kind: STT_FUNC,
            visibility: 0,
            section: vdso_text_section_idx as u16,
            shndx_if_section: 0,
        });
    }
    layout.symbols.extend(vdso_syms);

    // 5. 创建 .note.GNU-vdso 节 (元信息)
    // 格式: namesz(4) + descsz(4) + type(4) + name + desc
    // name = "GNU\0", type = 0x100 (NT_GNU_VDSO, 自定义)
    // desc = vDSO 符号数量(8) + text_size(8)
    let note_name = b"GNU\0";
    let namesz = note_name.len() as u32;
    let desc_data: Vec<u8> = {
        let mut d = Vec::new();
        d.extend_from_slice(&(info.symbols.len() as u64).to_le_bytes());
        d.extend_from_slice(&info.text_size.to_le_bytes());
        d
    };
    let descsz = desc_data.len() as u32;
    let name_pad = (4 - (namesz % 4)) % 4;
    let desc_pad = (4 - (descsz % 4)) % 4;

    let mut note_data = Vec::new();
    note_data.extend_from_slice(&namesz.to_le_bytes());
    note_data.extend_from_slice(&descsz.to_le_bytes());
    note_data.extend_from_slice(&0x100u32.to_le_bytes()); // NT_GNU_VDSO (自定义)
    note_data.extend_from_slice(note_name);
    note_data.resize(note_data.len() + name_pad as usize, 0);
    note_data.extend_from_slice(&desc_data);
    note_data.resize(note_data.len() + desc_pad as usize, 0);

    let note_name_str = ".note.GNU-vdso".to_string();
    let note_idx = layout.sections.len() as u32;
    let note_size = note_data.len() as u64;
    let note_section = OutputSection {
        kind: OutputSectionKind::Note,
        name: note_name_str,
        sh_name_off: 0,
        ty: SHT_NOTE,
        flags: 0, // 不加载, 仅调试
        align: 4,
        data: note_data,
        size: note_size,
        entsize: 0,
        output_index: note_idx,
        load_segment: 0,
        vaddr: 0,
        file_off: 0,
        input_offsets: Vec::new(),
        input_sections: Vec::new(),
    };
    layout.sections.push(note_section);

    // 6. 重算 vaddr (vDSO 节已加入)
    recompute_section_vaddrs(layout);

    // 7. 更新符号 vaddr (因为 .text.vdso 的 vaddr 可能变了)
    let vdso_text_vaddr = layout.sections[vdso_text_section_idx].vaddr;
    for sym in &mut layout.symbols {
        if sym.section == vdso_text_section_idx as u16 {
            sym.vaddr = vdso_text_vaddr + sym.value;
        }
    }

    Ok(())
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

    #[test]
    fn embed_vdso_adds_text_and_note_sections() {
        // 构造一个最小 vDSO ELF
        let shstrtab = b"\0.text\0.symtab\0.strtab\0.shstrtab\0";
        let text = b"\x90\x90\x90\x90\xc3"; // nop*4 ; ret
        let mut strtab = vec![0u8];
        strtab.extend_from_slice(b"__vdso_clock_gettime");
        strtab.push(0);
        let strtab_clock_off = 1usize;

        let mut out = vec![0u8; 64];
        let text_off = out.len();
        out.extend_from_slice(text);
        let symtab_off = out.len();
        out.resize(out.len() + 24, 0); // null sym
        out.extend_from_slice(&(strtab_clock_off as u32).to_le_bytes());
        out.push((1 << 4) | 2); // GLOBAL FUNC
        out.push(0);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes());
        out.extend_from_slice(&5u64.to_le_bytes());

        let strtab_off = out.len();
        out.extend_from_slice(&strtab);
        let shstrtab_off = out.len();
        out.extend_from_slice(shstrtab);
        while !out.len().is_multiple_of(8) {
            out.push(0);
        }
        let sh_off = out.len();
        out.resize(out.len() + 5 * 64, 0);

        out[0..4].copy_from_slice(&elf::ELFMAG);
        out[4] = elf::ELFCLASS64;
        out[5] = elf::ELFDATA2LSB;
        out[6] = elf::EV_CURRENT;
        out[16..18].copy_from_slice(&3u16.to_le_bytes()); // ET_DYN
        out[18..20].copy_from_slice(&elf::EM_X86_64.to_le_bytes());
        out[20..24].copy_from_slice(&1u32.to_le_bytes());
        out[40..48].copy_from_slice(&(sh_off as u64).to_le_bytes());
        out[58..60].copy_from_slice(&64u16.to_le_bytes());
        out[60..62].copy_from_slice(&5u16.to_le_bytes());
        out[62..64].copy_from_slice(&4u16.to_le_bytes());

        let write_sh = |buf: &mut Vec<u8>,
                        idx: usize,
                        name_off: u32,
                        sh_type: u32,
                        flags: u64,
                        addr: u64,
                        offset: u64,
                        size: u64,
                        link: u32,
                        info: u32,
                        addralign: u64,
                        entsize: u64| {
            let base = sh_off + idx * 64;
            buf[base..base + 4].copy_from_slice(&name_off.to_le_bytes());
            buf[base + 4..base + 8].copy_from_slice(&sh_type.to_le_bytes());
            buf[base + 8..base + 16].copy_from_slice(&flags.to_le_bytes());
            buf[base + 16..base + 24].copy_from_slice(&addr.to_le_bytes());
            buf[base + 24..base + 32].copy_from_slice(&offset.to_le_bytes());
            buf[base + 32..base + 40].copy_from_slice(&size.to_le_bytes());
            buf[base + 40..base + 44].copy_from_slice(&link.to_le_bytes());
            buf[base + 44..base + 48].copy_from_slice(&info.to_le_bytes());
            buf[base + 48..base + 56].copy_from_slice(&addralign.to_le_bytes());
            buf[base + 56..base + 64].copy_from_slice(&entsize.to_le_bytes());
        };
        write_sh(&mut out, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0);
        write_sh(
            &mut out,
            1,
            1,
            elf::SHT_PROGBITS,
            0x6,
            0,
            text_off as u64,
            text.len() as u64,
            0,
            0,
            1,
            0,
        );
        write_sh(
            &mut out,
            2,
            7,
            elf::SHT_SYMTAB,
            0,
            0,
            symtab_off as u64,
            48,
            3,
            1,
            8,
            24,
        );
        write_sh(
            &mut out,
            3,
            15,
            elf::SHT_STRTAB,
            0,
            0,
            strtab_off as u64,
            strtab.len() as u64,
            0,
            0,
            1,
            0,
        );
        write_sh(
            &mut out,
            4,
            23,
            elf::SHT_STRTAB,
            0,
            0,
            shstrtab_off as u64,
            shstrtab.len() as u64,
            0,
            0,
            1,
            0,
        );

        // 写 vDSO 到临时文件
        let dir = std::env::temp_dir();
        let vdso_path = dir.join("dcc-test-vdso.so");
        std::fs::write(&vdso_path, &out).unwrap();

        // 构造一个最小 layout
        let mut layout = crate::link::layout::Layout {
            is_pie: false,
            is_shared: false,
            arch: crate::debug::ArchKind::X86_64,
            base_addr: 0x400000,
            file_base: 0x1000,
            sections: vec![crate::link::layout::OutputSection {
                kind: crate::link::layout::OutputSectionKind::Text,
                name: ".text".into(),
                sh_name_off: 0,
                ty: elf::SHT_PROGBITS,
                flags: elf::SHF_ALLOC | elf::SHF_EXECINSTR,
                align: 16,
                data: vec![0xc3], // ret
                size: 1,
                entsize: 0,
                output_index: 0,
                load_segment: 0,
                vaddr: 0x401000,
                file_off: 0x1000,
                input_offsets: Vec::new(),
                input_sections: Vec::new(),
            }],
            segments: Vec::new(),
            symbols: Vec::new(),
            entry: 0x401000,
            has_symtab: true,
            has_dynamic: false,
            n_locals: 0,
            output_relas: Vec::new(),
            needed: Vec::new(),
            dynamic_linker: None,
        };

        // 调用 embed_vdso
        embed_vdso(&mut layout, &vdso_path, false).expect("embed_vdso ok");

        // 验证: .text.vdso 节存在
        let vdso_text = layout.sections.iter().find(|s| s.name == ".text.vdso");
        assert!(vdso_text.is_some(), ".text.vdso section should exist");
        let vdso_text = vdso_text.unwrap();
        assert_eq!(vdso_text.data, text);
        assert_eq!(vdso_text.size, 5);

        // 验证: .note.GNU-vdso 节存在
        let vdso_note = layout.sections.iter().find(|s| s.name == ".note.GNU-vdso");
        assert!(vdso_note.is_some(), ".note.GNU-vdso section should exist");
        let vdso_note = vdso_note.unwrap();
        assert_eq!(vdso_note.ty, elf::SHT_NOTE);
        assert!(vdso_note.size > 0);

        // 验证: vDSO 符号存在
        let vdso_sym = layout
            .symbols
            .iter()
            .find(|s| s.name == "__vdso_clock_gettime");
        assert!(
            vdso_sym.is_some(),
            "__vdso_clock_gettime symbol should exist"
        );
        let vdso_sym = vdso_sym.unwrap();
        assert_eq!(vdso_sym.bind, elf::STB_GLOBAL);
        assert_eq!(vdso_sym.ty, elf::STT_FUNC);

        let _ = std::fs::remove_file(vdso_path);
    }
}
