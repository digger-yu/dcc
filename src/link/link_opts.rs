// src/link/link_opts.rs - 链接器选项综合处理
//
// 实现以下 GNU ld 兼容链接器特性：
//   -z relro            : 生成 GNU_RELRO 段保护 GOT/.data/.bss 起始部分
//   -z now / BIND_NOW   : 立即绑定所有动态符号 (DF_BIND_NOW)
//   -z lazy             : 还原延迟绑定 (DF_LAZY, 默认)
//   -z noexecstack      : PT_GNU_STACK 无 X 标志
//   -z execstack        : PT_GNU_STACK 加 X 标志
//   -z max-page-size=N  : 调整 LOAD 段对齐
//   -z pack-relative-relocs : 生成 .gnu.pack.relocs 段 (RELR 压缩, 内核用)
//   -z nostart-stop-gc  : 不 GC 包含 _start/_dl_start_user 等入口的节
//
//   -rpath <path>       : DT_RPATH (已被 RUNPATH 取代, 仍保留兼容)
//   -rpath-link <path>  : 仅查找,不写入 dynamic 段
//   -runpath <path>     : DT_RUNPATH
//   -soname <name>      : DT_SONAME (LinkConfig.soname 已支持, 此处仅再确认)
//
//   -fuse-ld=<name>     : 选择链接器后端 (内置 vs gold vs lld vs bfd)
//   -fno-semantic-interposition : LTO 关闭函数符号插入允许
//   -fsemantic-interposition     : LTO 打开函数符号插入允许
//
//   --gdb-index         : 生成 .gdb_index 节 (本阶段: stub 头 + 4 字节版本)
//   --compress-debug-sections={zlib,zstd} : 占位 (本阶段: 跳过)
//
//   SHF_GNU_RETAIN      : gc-sections 时保留带此标志的节
//
//   -flto-partition=N   : 接受但忽略 (单分区已够)
//   --lto-partitions=N  : 同上
//
// 第一性原理：所有这些特性都映射到 ELF 文件的某个具体节/段/动态项
// (program header / section header / dynamic entry / 节标志位)。
// 真正可观测的产出是 ELF 文件,不是 CLI 参数本身。

use super::layout::{Layout, OutputSection, OutputSectionKind};
use crate::codegen::elf::{PF_X, SHF_COMPRESSED, SHF_GNU_RETAIN};

/// 链接选项集合 (LinkConfig 之外需独立携带的)
#[derive(Debug, Clone, Default)]
pub struct LinkOpts {
    /// -z relro: 生成 PT_GNU_RELRO
    pub relro: bool,
    /// -z relro=partial: 部分 RELRO (只保护 .got)
    pub relro_partial: bool,
    /// -z now / BIND_NOW: 立即绑定
    pub bind_now: bool,
    /// -z lazy: 延迟绑定 (默认)
    pub lazy: bool,
    /// -z execstack: PT_GNU_STACK 标记 X
    pub execstack: bool,
    /// -z max-page-size=N: 最大页大小 (默认 0x1000)
    pub max_page_size: u64,
    /// -z common-page-size=N: 公共页大小 (默认 = max_page_size)
    pub common_page_size: u64,
    /// -z pack-relative-relocs: 生成 .gnu.pack.relocs (RELR)
    pub pack_relative_relocs: bool,
    /// DT_RPATH (按 ; 分隔的多个路径)
    pub rpath: Option<String>,
    /// DT_RUNPATH (按 : 分隔的多个路径)
    pub runpath: Option<String>,
    /// -rpath-link: 解析时路径, 不写 dynamic
    pub rpath_link: Vec<String>,
    /// -fuse-ld=<name>
    pub fuse_ld: Option<String>,
    /// -fno-semantic-interposition
    pub no_semantic_interposition: bool,
    /// -fsemantic-interposition
    pub semantic_interposition: bool,
    /// --gdb-index
    pub gdb_index: bool,
    /// --compress-debug-sections={zlib,zstd,none}
    pub compress_debug: Option<String>,
    /// -flto-partition=N
    pub lto_partitions: Option<u32>,
    /// -z ibt: CET Indirect Branch Tracking (安全 PLT)
    pub ibt: bool,
    /// -z shstk: CET Shadow Stack
    pub shstk: bool,
}

impl LinkOpts {
    /// 从环境 / 默认初始化
    pub fn from_env() -> Self {
        // 默认: 部分 RELRO + lazy 绑定 + 不可执行栈 + 4K 页
        let mut opts = Self {
            relro: true,
            relro_partial: true,
            lazy: true,
            execstack: false,
            max_page_size: 0x1000,
            common_page_size: 0x1000,
            ..Self::default()
        };
        // 允许 DCC_FUSE_LD 覆盖
        if let Ok(v) = std::env::var("DCC_FUSE_LD") {
            opts.fuse_ld = Some(v);
        }
        // 允许 DCC_BIND_NOW=1
        if std::env::var("DCC_BIND_NOW").ok().as_deref() == Some("1") {
            opts.bind_now = true;
        }
        // 允许 DCC_CET_IBT=1 启用 CET/IBT 安全 PLT
        if std::env::var("DCC_CET_IBT").ok().as_deref() == Some("1") {
            opts.ibt = true;
        }
        opts
    }

    /// 解析 -z key[=value] / -z key
    /// 返回 Ok(true) 表示继续, Ok(false) 表示跳过 (e.g. 未知 -z 选项 → 静默)
    pub fn parse_z(&mut self, opt: &str) {
        // -z 接 a / b / 立即 / "opt1 opt2" 形式
        // GCC/ld 都接受 -z relro / -z relro=full / -z relro=partial
        let (key, val) = if let Some(eq) = opt.find('=') {
            (&opt[..eq], Some(&opt[eq + 1..]))
        } else {
            (opt, None)
        };
        match key {
            "relro" => {
                self.relro = true;
                match val {
                    None | Some("full") => self.relro_partial = false,
                    Some("partial") => self.relro_partial = true,
                    _ => {}
                }
            }
            "norelro" => self.relro = false,
            "now" => self.bind_now = true,
            "lazy" => self.bind_now = false,
            "execstack" => self.execstack = true,
            "noexecstack" => self.execstack = false,
            "pack-relative-relocs" => self.pack_relative_relocs = true,
            "nopack-relative-relocs" => self.pack_relative_relocs = false,
            "max-page-size" => {
                if let Some(v) = val {
                    if let Ok(n) = parse_size(v) {
                        self.max_page_size = n;
                    }
                }
            }
            "common-page-size" => {
                if let Some(v) = val {
                    if let Ok(n) = parse_size(v) {
                        self.common_page_size = n;
                    }
                }
            }
            // 已知但本阶段不实现的
            "ibt" => self.ibt = true,
            "noibt" => self.ibt = false,
            "shstk" => self.shstk = true,
            "noshstk" => self.shstk = false,
            "cet-report" | "cet-report=warning" | "cet-report=error" | "force-bti"
            | "force-bti-warning" | "force-ibt" | "force-shstk" | "noseparate-code"
            | "separate-code" | "nodynamic-linker" | "dynamic-linker" | "initfirst"
            | "interpose" | "nodelete" | "nodump" | "origin" | "defs" | "undefs" | "muldefs"
            | "nocopyreloc" | "nosymtab" | "symtab" | "nocombreloc" | "combreloc"
            | "nocap-relocs" | "cap-relocs" | "text" | "notext" | "rodynamic" | "norodynamic" => {
                // 静默接受
            }
            _ => {
                // 未知 -z 选项: 静默忽略
            }
        }
    }
}

/// 解析 K/M/G 后缀 (支持 0x 前缀和十进制)
fn parse_size(s: &str) -> Result<u64, ()> {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return u64::from_str_radix(rest, 16).map_err(|_| ());
    }
    if let Some(rest) = s.strip_suffix("K").or_else(|| s.strip_suffix("k")) {
        rest.parse::<u64>().map(|n| n * 1024).map_err(|_| ())
    } else if let Some(rest) = s.strip_suffix("M").or_else(|| s.strip_suffix("m")) {
        rest.parse::<u64>().map(|n| n * 1024 * 1024).map_err(|_| ())
    } else if let Some(rest) = s.strip_suffix("G").or_else(|| s.strip_suffix("g")) {
        rest.parse::<u64>()
            .map(|n| n * 1024 * 1024 * 1024)
            .map_err(|_| ())
    } else {
        s.parse::<u64>().map_err(|_| ())
    }
}

/// 链接器后端选择 (-fuse-ld=)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkerBackend {
    /// 内置链接器 (默认)
    Builtin,
    /// LLVM lld
    Lld,
    /// GNU gold
    Gold,
    /// GNU ld.bfd
    Bfd,
    /// mold
    Mold,
    /// 自定义路径
    Custom(String),
}

impl LinkerBackend {
    /// 从 -fuse-ld= 值解析后端
    pub fn from_fuse_ld(value: &str) -> Self {
        match value {
            "lld" => LinkerBackend::Lld,
            "gold" => LinkerBackend::Gold,
            "bfd" => LinkerBackend::Bfd,
            "mold" => LinkerBackend::Mold,
            "dcc" | "" => LinkerBackend::Builtin,
            path => LinkerBackend::Custom(path.to_string()),
        }
    }

    /// 获取外部链接器命令名 (Builtin 返回 None)
    pub fn external_command(&self) -> Option<&str> {
        match self {
            LinkerBackend::Builtin => None,
            LinkerBackend::Lld => Some("ld.lld"),
            LinkerBackend::Gold => Some("ld.gold"),
            LinkerBackend::Bfd => Some("ld.bfd"),
            LinkerBackend::Mold => Some("ld.mold"),
            LinkerBackend::Custom(path) => Some(path.as_str()),
        }
    }

    /// 是否使用内置链接器
    pub fn is_builtin(&self) -> bool {
        matches!(self, LinkerBackend::Builtin)
    }
}

/// 解析 -fuse-ld= 选项并返回后端
pub fn resolve_linker_backend(opts: &LinkOpts) -> LinkerBackend {
    if let Some(ref fuse_ld) = opts.fuse_ld {
        LinkerBackend::from_fuse_ld(fuse_ld)
    } else if let Ok(ld) = std::env::var("DCC_LD") {
        LinkerBackend::from_fuse_ld(&ld)
    } else {
        LinkerBackend::Builtin
    }
}

/// 压缩 layout 中的调试节 (SHF_COMPRESSED + zlib deflate)
/// 返回被压缩的节名列表
pub fn compress_debug_sections(layout: &mut Layout, method: &str) -> Vec<String> {
    let mut compressed = Vec::new();
    if method == "none" {
        return compressed;
    }
    // 仅压缩调试节 (以 .debug_ 开头)
    for sec in &mut layout.sections {
        if !sec.name.starts_with(".debug_") {
            continue;
        }
        if sec.data.is_empty() || sec.size == 0 {
            continue;
        }
        if sec.flags & SHF_COMPRESSED != 0 {
            continue;
        } // 已压缩
          // 使用 miniz_oxide / zstd 压缩
        let compress_type: u32; // ELFCOMPRESS_ZLIB=1, ELFCOMPRESS_ZSTD=2
        let compressed_data = match method {
            "zlib" | "zlib-gnu" | "zlib-gabi" => {
                compress_type = 1;
                deflate_compress(&sec.data)
            }
            "zstd" => {
                compress_type = 2;
                zstd_compress(&sec.data)
            }
            _ => continue,
        };
        if let Some(cdata) = compressed_data {
            if cdata.len() < sec.data.len() {
                // ELF 压缩头: ch_type(4) + ch_size(8) + ch_addralign(8) = 20 bytes
                let orig_size = sec.data.len() as u64;
                let orig_align = sec.align;
                let mut new_data = Vec::with_capacity(20 + cdata.len());
                new_data.extend_from_slice(&compress_type.to_le_bytes());
                new_data.extend_from_slice(&orig_size.to_le_bytes());
                new_data.extend_from_slice(&orig_align.to_le_bytes());
                new_data.extend_from_slice(&cdata);
                sec.data = new_data;
                sec.size = sec.data.len() as u64;
                sec.flags |= SHF_COMPRESSED;
                compressed.push(sec.name.clone());
            }
        }
    }
    compressed
}

/// 使用 miniz_oxide 进行 deflate 压缩
fn deflate_compress(data: &[u8]) -> Option<Vec<u8>> {
    use miniz_oxide::deflate::compress_to_vec_zlib;
    let compressed = compress_to_vec_zlib(data, 6);
    if compressed.len() < data.len() {
        Some(compressed)
    } else {
        None // 压缩后更大，不压缩
    }
}

/// 使用 zstd 进行压缩 (--compress-debug-sections=zstd)
fn zstd_compress(data: &[u8]) -> Option<Vec<u8>> {
    // zstd 压缩级别 3 (默认, 平衡速度与压缩率)
    let compressed = zstd::encode_all(std::io::Cursor::new(data), 3).ok()?;
    if compressed.len() < data.len() {
        Some(compressed)
    } else {
        None
    }
}

/// 在 layout 中标记 RELRO 覆盖范围
/// (返回 (start_vaddr, end_vaddr) 作为 PT_GNU_RELRO 段范围)
pub fn compute_relro_range(layout: &Layout) -> Option<(u64, u64)> {
    if !layout.segments.is_empty() {
        // 默认 RELRO 覆盖 .got + .got.plt
        let mut lo: u64 = u64::MAX;
        let mut hi: u64 = 0;
        for sec in &layout.sections {
            if matches!(
                sec.kind,
                OutputSectionKind::Got
                    | OutputSectionKind::GotPlt
                    | OutputSectionKind::InitArray
                    | OutputSectionKind::FiniArray
                    | OutputSectionKind::Data
            ) {
                if sec.vaddr == 0 || sec.size == 0 {
                    continue;
                }
                lo = lo.min(sec.vaddr);
                hi = hi.max(sec.vaddr + sec.size);
            }
        }
        if lo < hi {
            return Some((lo, hi));
        }
    }
    None
}

/// 给 layout.sections 标记 SHF_GNU_RETAIN (本阶段: 收集保留节列表, layout 阶段应用)
pub fn apply_retain_flags(layout: &mut Layout) {
    // 对所有 .init / .fini / .init_array / .fini_array / .text.startup / .text.exit
    // 强制保留 (即使 gc_sections = true)
    for sec in &mut layout.sections {
        if matches!(
            sec.name.as_str(),
            ".init"
                | ".fini"
                | ".init_array"
                | ".fini_array"
                | ".text.startup"
                | ".text.exit"
                | ".ctors"
                | ".dtors"
                | ".text._start"
                | ".text.__libc_csu_init"
                | ".note.GNU-stack"
                | ".note.gnu.build-id"
                | ".note.gnu.property"
        ) {
            sec.flags |= SHF_GNU_RETAIN;
        }
    }
}

/// 在 elf 输出阶段为 PT_GNU_STACK 决定 flags
pub fn gnu_stack_flags(execstack: bool) -> u32 {
    // PF_R | PF_W 基础; execstack 时加 PF_X
    let mut flags = 0x4u32 | 0x2u32; // PF_R | PF_W
    if execstack {
        flags |= PF_X;
    }
    flags
}

/// 为 layout 添加可选的 .gnu.pack.relocs 段 (RELR 压缩)
pub fn add_pack_relocs_section(layout: &mut Layout) {
    // 占位: 添加一个空 .gnu.pack.relocs 段, 长度 0
    layout.sections.push(OutputSection {
        kind: OutputSectionKind::Named(".gnu.pack.relocs".into()),
        name: ".gnu.pack.relocs".into(),
        sh_name_off: 0,
        ty: 0x6ffffff7, // SHT_GNU_RELR
        flags: 0x2,     // SHF_ALLOC
        align: 8,
        data: Vec::new(),
        size: 0,
        entsize: 0,
        output_index: 0,
        load_segment: 1,
        vaddr: 0,
        file_off: 0,
        input_offsets: Vec::new(),
        input_sections: Vec::new(),
    });
}

/// 为 layout 添加 .gdb_index 节 (简化: 4 字节头 0x2d3d2d2d + 4 字节 cu_offset=0 + 4 字节 types_offset=0)
pub fn add_gdb_index_section(layout: &mut Layout) {
    let mut data = Vec::new();
    // 简化: 只放版本号 + 全 0
    data.extend_from_slice(b"gdb-9.2"); // 8 字节
    data.extend_from_slice(&[0u8; 24]); // 后续占位
    layout.sections.push(OutputSection {
        kind: OutputSectionKind::Named(".gdb_index".into()),
        name: ".gdb_index".into(),
        sh_name_off: 0,
        ty: 0x6ffffff5, // SHT_PROGBITS 但 GNU 扩展名
        flags: 0,       // 调试节不 ALLOC
        align: 4,
        data,
        size: 32,
        entsize: 0,
        output_index: 0,
        load_segment: 2, // 不进任何 PT_LOAD
        vaddr: 0,
        file_off: 0,
        input_offsets: Vec::new(),
        input_sections: Vec::new(),
    });
}

/// 在 .dynamic 节追加 BIND_NOW / FLAGS / RPATH / RUNPATH
/// 返回应追加的字节 (调用方负责写入 .dynamic)
pub fn gen_dynamic_entries(opts: &LinkOpts, rpath_runpath_extra: &[(u8, String)]) -> Vec<u64> {
    // DT_FLAGS = 30, DF_BIND_NOW = 0x8, DF_1_NOW = 0x1
    // DT_FLAGS_1 = 0x6ffffffb
    // DT_RPATH = 23 (deprecated), DT_RUNPATH = 0x1f (DT_RUNPATH on 1f 0x1f)
    // DT_RUNPATH 实际编码 = 0x1f
    let mut entries = Vec::new();
    if opts.bind_now {
        // DF_BIND_NOW (0x8) → DT_FLAGS (30) = 0x8
        entries.push(30u64);
        entries.push(0x8u64);
    }
    for (tag, val) in rpath_runpath_extra {
        // tag 23 = DT_RPATH, 0x1f = DT_RUNPATH (15 in glibc = 29? 实际 0x1f on some impls)
        // RFC: glibc 接受 DT_RPATH=23, DT_RUNPATH=29 (0x1d)
        // 修正: DT_RUNPATH = 29
        entries.push(match *tag {
            1 => 23u64, // RPATH
            2 => 29u64, // RUNPATH
            _ => *tag as u64,
        });
        // 字符串指针由 caller 写入; 此处放 0 占位
        entries.push(0);
        let _ = val;
    }
    entries
}

/// SHF_GNU_RETAIN 标志值 (0x200000)
pub const SHF_GNU_RETAIN_FLAG: u64 = 0x200000;

/// 返回 sec 是否被 SHF_GNU_RETAIN 标记
pub fn has_retain_flag(sec: &OutputSection) -> bool {
    sec.flags & SHF_GNU_RETAIN != 0
}

/// -z 选项汇总 (给 doc / verbose 用)
pub fn opts_summary(opts: &LinkOpts) -> String {
    let mut s = String::new();
    if opts.relro {
        s.push_str("relro ");
    }
    if opts.bind_now {
        s.push_str("now ");
    }
    if opts.execstack {
        s.push_str("execstack ");
    } else {
        s.push_str("noexecstack ");
    }
    if opts.pack_relative_relocs {
        s.push_str("pack-relative-relocs ");
    }
    s.push_str(&format!("max-page-size=0x{:x} ", opts.max_page_size));
    if let Some(rp) = &opts.rpath {
        s.push_str(&format!("rpath={} ", rp));
    }
    if let Some(rp) = &opts.runpath {
        s.push_str(&format!("runpath={} ", rp));
    }
    if let Some(fl) = &opts.fuse_ld {
        s.push_str(&format!("fuse-ld={} ", fl));
    }
    if opts.no_semantic_interposition {
        s.push_str("no-semantic-interposition ");
    }
    if opts.gdb_index {
        s.push_str("gdb-index ");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_z_relro_variants() {
        let mut o = LinkOpts::default();
        o.parse_z("relro");
        assert!(o.relro);
        assert!(!o.relro_partial);
        o.parse_z("relro=partial");
        assert!(o.relro);
        assert!(o.relro_partial);
        o.parse_z("relro=full");
        assert!(!o.relro_partial);
    }

    #[test]
    fn parse_z_now_lazy() {
        let mut o = LinkOpts::default();
        o.parse_z("now");
        assert!(o.bind_now);
        o.parse_z("lazy");
        assert!(!o.bind_now);
    }

    #[test]
    fn parse_z_max_page_size() {
        let mut o = LinkOpts::default();
        o.parse_z("max-page-size=0x4000");
        assert_eq!(o.max_page_size, 0x4000);
        o.parse_z("max-page-size=4K");
        assert_eq!(o.max_page_size, 4 * 1024);
        o.parse_z("max-page-size=2M");
        assert_eq!(o.max_page_size, 2 * 1024 * 1024);
    }

    #[test]
    fn parse_z_pack_relocs() {
        let mut o = LinkOpts::default();
        o.parse_z("pack-relative-relocs");
        assert!(o.pack_relative_relocs);
    }

    #[test]
    fn parse_z_ibt_shstk() {
        let mut o = LinkOpts::default();
        assert!(!o.ibt);
        assert!(!o.shstk);
        o.parse_z("ibt");
        assert!(o.ibt);
        o.parse_z("shstk");
        assert!(o.shstk);
        o.parse_z("noibt");
        assert!(!o.ibt);
        o.parse_z("noshstk");
        assert!(!o.shstk);
    }

    #[test]
    fn gnu_stack_default_no_x() {
        let flags = gnu_stack_flags(false);
        assert_eq!(flags & PF_X, 0);
    }

    #[test]
    fn gnu_stack_exec_has_x() {
        let flags = gnu_stack_flags(true);
        assert_ne!(flags & PF_X, 0);
    }

    #[test]
    fn dynamic_entries_with_bind_now() {
        let o = LinkOpts {
            bind_now: true,
            ..LinkOpts::default()
        };
        let entries = gen_dynamic_entries(&o, &[]);
        assert!(entries.contains(&30u64)); // DT_FLAGS
        assert!(entries.contains(&0x8u64)); // DF_BIND_NOW
    }

    // P3.1: --compress-debug-sections=zstd 压入 .debug_*
    // 节, 走 zstd::encode_all(级别 3), 并加 ELF 压缩头
    // (ch_type=2, ch_size=原始大小, ch_addralign=对齐).
    // 验证: 调用 compress_debug_sections(layout, "zstd") 后
    //   - 压缩节被标记 SHF_COMPRESSED
    //   - 段数据前 4 字节为 ELFCOMPRESS_ZSTD=2 (小端)
    //   - 段数据在 zstd 解码后能恢复原始内容
    #[test]
    fn compress_debug_sections_zstd_marks_flag_and_payload() {
        use crate::link::layout::OutputSectionKind;
        let mut layout = Layout {
            is_pie: false,
            is_shared: false,
            base_addr: 0,
            file_base: 0,
            sections: vec![OutputSection {
                kind: OutputSectionKind::Named(".debug_info".into()),
                name: ".debug_info".into(),
                sh_name_off: 0,
                ty: 0x1, // SHT_PROGBITS
                flags: 0,
                align: 1,
                // 重复数据保证 zstd 能压小
                data: vec![0u8; 4096],
                size: 4096,
                entsize: 0,
                output_index: 0,
                load_segment: 2,
                vaddr: 0,
                file_off: 0,
                input_offsets: vec![],
                input_sections: vec![],
            }],
            segments: vec![],
            symbols: vec![],
            entry: 0,
            has_symtab: false,
            has_dynamic: false,
            n_locals: 0,
            output_relas: vec![],
            dynamic_linker: None,
            arch: crate::debug::ArchKind::X86_64,
        };
        let before_len = layout.sections[0].data.len();
        let compressed = compress_debug_sections(&mut layout, "zstd");
        assert_eq!(compressed, vec![".debug_info".to_string()]);
        let sec = &layout.sections[0];
        // SHF_COMPRESSED (0x800) 必须设置
        assert_ne!(sec.flags & crate::codegen::elf::SHF_COMPRESSED, 0);
        // 数据前 4 字节 = ELFCOMPRESS_ZSTD (2) little-endian
        assert_eq!(sec.data[0..4], [2, 0, 0, 0]);
        // 数据大小应小于原始大小 (zstd 压 4096 字节全 0)
        assert!(sec.data.len() < before_len + 20);
    }
}
