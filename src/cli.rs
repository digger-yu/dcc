// src/cli.rs - 命令行参数解析
// 第一性原理: CLI 只是"配置 + 文件列表"的扁平化,不做任何决策。
// 真正的决策 (启不启优化、目标平台) 在 driver 层。
//
// suckless 准则: 我们只支持最常用的 GCC 子集选项,够用即可。

use crate::link::BuildIdStyle;
use crate::sanitizer::SanitizerFlags;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputLang {
    C,
    Cpp,
    Assembler,
    AssemblerWithCpp,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub inputs: Vec<PathBuf>,
    pub output: PathBuf,       // 默认 a.out
    pub output_explicit: bool, // -o 是否由用户显式指定
    pub opt: u8,               // 0..3
    pub std: StdMode,
    pub target: TargetTriple,
    pub freestanding: bool,
    pub include_paths: Vec<PathBuf>,
    pub system_paths: Vec<PathBuf>,
    pub defines: Vec<(String, Option<String>)>, // -D NAME=VALUE
    pub dump_ast: bool,
    pub verbose: bool,
    pub relocatable: bool,                    // -c / -r
    pub link_mode: bool,                      // -r 合并 .o（依赖 host ld/objcopy，非内置链接器）
    pub emit_asm: bool,                       // -S: 输出汇编文本 .s
    pub preprocess_only: bool,                // -E: 只做预处理
    pub dump_macros: bool,                    // -dM: 预处理后输出所有宏定义
    pub force_includes: Vec<String>,          // -include file: 强制包含文件
    pub pch_opts: crate::preproc::PchOptions, // PCH (预编译头): -fpch-include / -fpch-create
    pub syntax_only: bool,                    // -fsyntax-only: 只做语法检查, 不生成代码
    pub emit_module_file: Option<PathBuf>,    // --emit-module-file=foo.bmf: 输出模块接口 (BMF)
    pub dep_file: Option<PathBuf>,            // -MF: 依赖文件输出路径
    pub dep_target: Option<String>,           // -MT/-MQ: 依赖文件中的目标名
    pub gen_dep: bool,                        // -MD/-MMD: 是否生成依赖文件
    pub dep_mmd_only: bool,                   // -MMD: 排除系统头文件
    pub dep_phony: bool,                      // -MP: 为每个头文件生成 phony 目标
    pub red_zone: bool,
    pub frame_pointer: bool,
    pub sse: bool,
    pub stack_protector: bool, // -fstack-protector / -fstack-protector-all
    pub unwind_tables: bool,   // -fno-asynchronous-unwind-tables / -fno-unwind-tables
    pub function_return_thunk_extern: bool,
    pub indirect_branch_thunk_extern: bool,
    pub patchable_function_entry: Option<(usize, usize)>,
    pub debug_level: u8, // 0=off (g0), 1=g1, 2=g2, 3=g3
    pub lto_enabled: bool,
    pub function_sections: bool, // -ffunction-sections（-flto 时默认开启）
    pub sanitizers: SanitizerFlags,
    pub input_lang: InputLang,
    pub as_options: Vec<String>, // -Wa, 传递给汇编器的选项
    pub m32: bool,               // -m32: 32 位目标, 传 --32 给 as
    pub parallel_codegen: bool,  // -O2+ 默认多线程并行 emit（DCC_NO_PARALLEL=1 关闭）
    pub permissive: bool,        // -fpermissive：未知 typedef 降级为 int
    pub raw_args: Vec<String>,   // 原始参数（sanitizer/LTO 探测）

    // 内置链接器相关（-pie / -shared / -flinker-output=）
    pub built_link: bool, // 走 src/link/ 全内置链接器而非 host ld -r
    pub output_kind: LinkerOutputKind, // Executable / PIE / Shared
    pub soname: Option<String>,
    pub linker_entry: Option<String>, // -e: 显式指定 entry
    pub linker_lto: bool,             // 启用 IR 级 LTO
    /// --exclude-libs: 隐藏静态库符号 (all / libfoo,libbar)
    pub exclude_libs: Vec<String>,
    /// --build-id: build-id 风格
    pub build_id: BuildIdStyle,
    /// --no-undefined-version
    pub no_undefined_version: bool,
    /// 链接器扩展选项 (-z, -rpath, -fuse-ld= 等)
    pub link_opts: crate::link::link_opts::LinkOpts,
    /// 链接库名 (-lxxx)
    pub link_libs: Vec<String>,
    /// gc-sections 显式根符号 (-u)
    pub gc_sections_root: Vec<String>,
    /// -fno-semantic-interposition
    pub no_semantic_interposition: bool,
    /// -fsemantic-interposition
    pub semantic_interposition: bool,
    /// -fuse-ld=<name>
    pub fuse_ld: Option<String>,
    /// --gdb-index
    pub gdb_index: bool,
    /// --compress-debug-sections=<style>
    pub compress_debug: Option<String>,
    /// -flto-partition=N
    pub lto_partitions: Option<u32>,
    /// -ftrapv: 有符号整型溢出时调用 abort
    pub trapv: bool,
    /// -fbounds-check / -fsanitize=bounds: 数组下标运行时边界检查
    pub bounds_check: bool,
    /// -Wint-conversion: 隐式整数↔指针转换警告
    pub warn_int_conversion: bool,
    /// -Wint-truncation: 整数截断警告
    pub warn_int_truncation: bool,
    /// -Wimplicit-declaration: 未声明标识符警告
    pub warn_implicit_decl: bool,
    /// PGO / GCOV / AutoFDO / LBR / MemProf / Offloading 统一配置
    pub profile: crate::profile::ProfileConfig,
    /// IPA-SRA 配置: -fipa-sra
    pub ipa_sra: crate::ipa_sra::IpaSraConfig,
    /// -fdiagnostics-color=always|never|auto
    pub diag_color: crate::diagnostics::ColorPolicy,
    /// -fdiagnostics-format=github|clang|msvc|vi|sarif
    pub diag_format: crate::diagnostics::FormatStyle,
    /// -fdiagnostics-show-template-tree
    pub diag_show_template_tree: bool,
    /// --bootstrap-compare=<ref.o>: 自举回归对比. 编译结束后, 把当前输出
    /// 与 ref.o 做三层对比 (byte / struct / semantic), 仅做诊断打印, 不阻塞.
    pub bootstrap_compare: Option<PathBuf>,
    /// --sysroot=<path>: 根目录, 头文件查找与链接器 -L 均带 <path>/include,
    /// <path>/lib 前缀. 为 musl 交叉编译提供完整 sysroot 路径.
    pub sysroot: Option<PathBuf>,
    /// --vdso=<path>: vDSO ELF 文件路径, 嵌入输出 layout 的 .text.vdso 节
    pub vdso: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(clippy::upper_case_acronyms)]
pub enum LinkerOutputKind {
    #[default]
    Executable, // 默认
    PIE,    // -pie
    Shared, // -shared
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdMode {
    C89,
    C99,
    C11,
    C17,
    C23,
    Gnu89,
    Gnu99,
    Gnu11,
    Gnu17,
    Gnu23, // GNU 扩展
}

impl StdMode {
    pub fn as_str(self) -> &'static str {
        match self {
            StdMode::C89 => "c89",
            StdMode::Gnu89 => "gnu89",
            StdMode::C99 => "c99",
            StdMode::Gnu99 => "gnu99",
            StdMode::C11 => "c11",
            StdMode::Gnu11 => "gnu11",
            StdMode::C17 => "c17",
            StdMode::Gnu17 => "gnu17",
            StdMode::C23 => "c23",
            StdMode::Gnu23 => "gnu23",
        }
    }
}

#[derive(Debug, Clone)]
pub struct TargetTriple {
    pub arch: Arch,
    pub vendor: String, // "unknown" or "pc" or "apple"
    pub os: Os,
    pub abi: Abi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    X86_64,
    AArch64,
    RiscV64,
    Mips64,
    Mips,
    Arm,
}

impl Arch {
    /// 第一性原理: 三类语义
    ///   * has_backend == true  : 该 arch 有完整 codegen 后端,emit() 会成功
    ///   * has_backend == false : 仅解析 triple (在 --target / 交叉编译场景
    ///     中合法),emit() 会 panic 报清楚
    ///
    /// 之所以区分而不是直接拒绝 mips/arm triple: Linux kernel 编译链
    /// 经常在 host 端用 --target 探测目标架构,只解析不解码能避免误伤。
    pub fn has_backend(self) -> bool {
        match self {
            Arch::X86_64
            | Arch::AArch64
            | Arch::RiscV64
            | Arch::Mips64
            | Arch::Mips
            | Arch::Arm => true,
        }
    }

    /// 在 has_backend() == false 时的统一报错文本
    pub fn backend_missing_msg(self) -> String {
        format!(
            "target arch {:?} is parsed but has no codegen backend \
             (supported backends: x86_64, aarch64, riscv64, mips, mips64, arm)",
            self
        )
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Linux,
    Freestanding,
    Windows,
    Macos,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Abi {
    Gnu,
    Musl,
    Eabi,
    Eabihf,
    Msvc,
    Darwin,
}

impl TargetTriple {
    pub fn host() -> Self {
        // 第一性原理: 大多数情况下 host == target (本地编译)
        // 跨编译由 --target=xxx-... 显式指定
        #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
        return Self {
            arch: Arch::X86_64,
            vendor: "unknown".into(),
            os: Os::Linux,
            abi: Abi::Gnu,
        };
        #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
        return Self {
            arch: Arch::AArch64,
            vendor: "unknown".into(),
            os: Os::Linux,
            abi: Abi::Gnu,
        };
        #[cfg(all(target_arch = "x86_64", target_os = "windows"))]
        return Self {
            arch: Arch::X86_64,
            vendor: "pc".into(),
            os: Os::Windows,
            abi: Abi::Msvc,
        };
        // 兜底: 用 unreachable!() 抑制 dead_code 警告, 只在支持的目标上编译
        #[allow(unreachable_code)]
        {
            let _ = Abi::Gnu; // 抑制 Abi unused 警告
            Self {
                arch: Arch::X86_64,
                vendor: "unknown".into(),
                os: Os::Linux,
                abi: Abi::Gnu,
            }
        }
    }

    pub fn parse(s: &str) -> Result<Self, String> {
        // 形如: x86_64-unknown-linux-gnu, aarch64-linux-gnu, riscv64-linux-gnu
        let parts: Vec<&str> = s.split('-').collect();
        if parts.len() < 3 {
            return Err(format!("bad target: {}", s));
        }
        let arch = match parts[0] {
            "x86_64" | "amd64" => Arch::X86_64,
            "aarch64" | "arm64" => Arch::AArch64,
            "riscv64" => Arch::RiscV64,
            "mips64" => Arch::Mips64,
            "mips" => Arch::Mips,
            "arm" | "armv7" => Arch::Arm,
            other => return Err(format!("unknown arch: {}", other)),
        };
        let os = match parts[parts.len() - 2] {
            "linux" | "linux-gnu" | "linux-musl" => Os::Linux,
            "freestanding" | "elf" => Os::Freestanding,
            "windows" | "win32" => Os::Windows,
            "darwin" | "apple" => Os::Macos,
            other => return Err(format!("unknown os: {}", other)),
        };
        let abi = match parts[parts.len() - 1] {
            "gnu" | "gnuabi99" | "gnuabi64" | "gnuabin32" | "gnueabi" | "gnueabihf" => Abi::Gnu,
            "musl" => Abi::Musl,
            "eabi" => Abi::Eabi,
            "eabihf" => Abi::Eabihf,
            "msvc" => Abi::Msvc,
            "darwin" => Abi::Darwin,
            // mips/o32/o64/n32 等扁平 abi 也归到 Gnu 桶里 (只解析不解码)
            o32_or_o64 if o32_or_o64.starts_with("o") => Abi::Gnu,
            other => return Err(format!("unknown abi: {}", other)),
        };
        let vendor = if parts.len() == 4 {
            parts[1].to_string()
        } else {
            "unknown".into()
        };
        Ok(Self {
            arch,
            vendor,
            os,
            abi,
        })
    }

    pub fn as_str(&self) -> String {
        format!(
            "{}-{}-{}-{}",
            arch_name(self.arch),
            self.vendor,
            os_name(self.os),
            abi_name(self.abi)
        )
    }

    /// 当 abi == Musl 时, 返回 musl 动态加载器在 sysroot 内的相对路径.
    /// 例: x86_64-linux-musl -> "lib/ld-musl-x86_64.so.1".
    /// 其它 abi 返回 None.
    pub fn musl_loader_path(&self) -> Option<String> {
        if self.abi != Abi::Musl {
            return None;
        }
        let arch = match self.arch {
            Arch::X86_64 => "x86_64",
            Arch::AArch64 => "aarch64",
            Arch::RiscV64 => "riscv64",
            Arch::Arm => "armhf",
            // musl 官方仓库在 mips 上游未提供, 返回 None 保持保守
            Arch::Mips | Arch::Mips64 => return None,
        };
        Some(format!("lib/ld-musl-{}.so.1", arch))
    }
}

pub fn arch_name(a: Arch) -> &'static str {
    match a {
        Arch::X86_64 => "x86_64",
        Arch::AArch64 => "aarch64",
        Arch::RiscV64 => "riscv64",
        Arch::Mips64 => "mips64",
        Arch::Mips => "mips",
        Arch::Arm => "arm",
    }
}
pub fn os_name(o: Os) -> &'static str {
    match o {
        Os::Linux => "linux",
        Os::Freestanding => "freestanding",
        Os::Windows => "windows",
        Os::Macos => "darwin",
    }
}
pub fn abi_name(a: Abi) -> &'static str {
    match a {
        Abi::Gnu => "gnu",
        Abi::Musl => "musl",
        Abi::Eabi => "eabi",
        Abi::Eabihf => "eabihf",
        Abi::Msvc => "msvc",
        Abi::Darwin => "darwin",
    }
}

pub fn parse(args: &[String]) -> Result<Config, CliError> {
    let mut inputs: Vec<PathBuf> = vec![];
    let mut output = PathBuf::from("a.out");
    let mut output_explicit = false;
    let mut opt: u8 = 0;
    let mut std = StdMode::Gnu23; // 第一性原理:默认 gnu23,跟现代 GCC 行为一致
    let mut target = TargetTriple::host();
    let mut freestanding = false;
    let mut permissive = false;
    let mut include_paths: Vec<PathBuf> = vec![];
    let mut system_paths: Vec<PathBuf> = vec![];
    let mut defines: Vec<(String, Option<String>)> = vec![];
    let mut dump_ast = false;
    let mut verbose = false;
    let mut relocatable = false;
    let mut emit_asm = false;
    let mut preprocess_only = false;
    let mut dump_macros = false;
    let mut force_includes: Vec<String> = vec![];
    let mut pch_opts = crate::preproc::PchOptions::default();
    let mut syntax_only = false;
    let mut emit_module_file: Option<PathBuf> = None;
    let mut dep_file: Option<PathBuf> = None;
    let mut dep_target: Option<String> = None;
    let mut gen_dep = false;
    let mut dep_mmd_only = false;
    let mut dep_phony = false;
    let mut red_zone = true;
    let mut frame_pointer = false;
    let mut sse = true;
    let mut stack_protector = false;
    let mut unwind_tables = true;
    let mut function_return_thunk_extern = false;
    let mut indirect_branch_thunk_extern = false;
    let mut patchable_function_entry = None;
    let mut debug_level: u8 = 0;
    let mut lto_enabled = false;
    let mut function_sections = false;
    let mut no_function_sections = false;
    let mut input_lang = InputLang::C;
    let mut as_options: Vec<String> = vec![];
    let mut m32 = false;
    let mut relocatable_link = false; // 仅 -r 置位；-c 只产出 .o 不进入 link.rs
    let mut output_kind = LinkerOutputKind::Executable;
    let mut built_link = false;
    let mut soname: Option<String> = None;
    let mut linker_entry: Option<String> = None;
    let linker_lto = false;
    let mut exclude_libs: Vec<String> = Vec::new();
    let mut build_id = BuildIdStyle::None;
    let mut no_undefined_version = false;
    // 链接器扩展选项 (M12)
    let mut link_opts_z: Vec<String> = Vec::new();
    let mut link_rpath: Vec<String> = Vec::new();
    let mut link_runpath: Vec<String> = Vec::new();
    let mut link_rpath_link: Vec<String> = Vec::new();
    let mut fuse_ld: Option<String> = None;
    let mut no_semantic_interposition = false;
    let mut semantic_interposition = false;
    let mut gdb_index = false;
    let mut compress_debug: Option<String> = None;
    let mut lto_partitions: Option<u32> = None;
    let mut gc_sections_root: Vec<String> = Vec::new();
    let mut link_libs: Vec<String> = Vec::new();
    let mut trapv = false;
    let mut bounds_check = false;
    let mut warn_int_conversion = false;
    let mut warn_int_truncation = false;
    let mut warn_implicit_decl = true;
    let mut profile = crate::profile::ProfileConfig::default();
    let mut ipa_sra = crate::ipa_sra::IpaSraConfig::default();
    let mut offload = crate::profile::offload::OffloadConfig::default();
    let mut diag_color = crate::diagnostics::ColorPolicy::Auto;
    let mut diag_format = crate::diagnostics::FormatStyle::Github;
    let mut diag_show_template_tree = false;
    let mut bootstrap_compare: Option<PathBuf> = None;
    let mut sysroot: Option<PathBuf> = None;
    let mut vdso: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "-O0" => opt = 0,
            "-O1" => opt = 1,
            "-O2" => opt = 2,
            "-O3" => opt = 3,
            "-Os" => opt = 2, // 体积优化合并到 O2
            "-o" => {
                i += 1;
                output = PathBuf::from(
                    args.get(i)
                        .ok_or_else(|| CliError::new("missing arg after -o"))?,
                );
                output_explicit = true;
            }
            "-c" => {
                relocatable = true;
            }
            "-S" => {
                emit_asm = true;
            }
            "-E" => {
                preprocess_only = true;
            }
            "-dM" => {
                dump_macros = true;
            }
            "-pipe" => {} // GCC -pipe: 使用管道代替临时文件，DCC 忽略
            "-r" => {
                relocatable = true;
                relocatable_link = true;
            } // -r: 可重定位 .o 合并
            // 内置链接器开关
            "-pie" | "-fpie" | "-fPIE" => {
                output_kind = LinkerOutputKind::PIE;
                built_link = true;
            }
            "-shared" => {
                output_kind = LinkerOutputKind::Shared;
                built_link = true;
            }
            "-fpic" | "-fPIC" => {}
            "--pie" => {
                output_kind = LinkerOutputKind::PIE;
                built_link = true;
            }
            "-flinker-output=exe" => {
                output_kind = LinkerOutputKind::Executable;
                built_link = true;
            }
            "-flinker-output=pie" => {
                output_kind = LinkerOutputKind::PIE;
                built_link = true;
            }
            "-flinker-output=so" => {
                output_kind = LinkerOutputKind::Shared;
                built_link = true;
            }
            "-fbuiltin-linker" => {
                built_link = true;
            }
            "-fno-builtin-linker" => {
                built_link = false;
            }
            "-Wl,-soname," => {
                i += 1;
                soname = Some(
                    args.get(i)
                        .ok_or_else(|| CliError::new("missing -soname arg"))?
                        .to_string(),
                );
            }
            a if a.starts_with("-Wl,-soname,") => {
                soname = Some(a["-Wl,-soname,".len()..].to_string());
            }
            "-e" => {
                i += 1;
                linker_entry = Some(
                    args.get(i)
                        .ok_or_else(|| CliError::new("missing arg after -e"))?
                        .to_string(),
                );
            }
            "-Wl,--exclude-libs," => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| CliError::new("missing --exclude-libs arg"))?
                    .to_string();
                exclude_libs.extend(v.split(',').map(String::from));
            }
            a if a.starts_with("-Wl,--exclude-libs,") => {
                let v = &a["-Wl,--exclude-libs,".len()..];
                exclude_libs.extend(v.split(',').map(String::from));
            }
            "-Wl,--build-id=sha1" => {
                build_id = BuildIdStyle::Sha1;
            }
            "-Wl,--build-id=uuid" => {
                build_id = BuildIdStyle::Uuid;
            }
            "-Wl,--build-id=md5" => {
                build_id = BuildIdStyle::Md5;
            }
            "-Wl,--build-id=none" => {
                build_id = BuildIdStyle::None;
            }
            "-Wl,--no-undefined-version" => {
                no_undefined_version = true;
            }
            // -z options: -z relro, -z now, -z max-page-size=N, etc.
            "-z" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| CliError::new("missing arg after -z"))?
                    .clone();
                link_opts_z.push(v);
            }
            // -Wl,-z,* form
            a if a.starts_with("-Wl,-z,") => {
                let v = &a[7..];
                link_opts_z.push(v.to_string());
            }
            // -rpath
            "-rpath" => {
                i += 1;
                link_rpath.push(
                    args.get(i)
                        .ok_or_else(|| CliError::new("missing arg after -rpath"))?
                        .clone(),
                );
            }
            a if a.starts_with("-Wl,-rpath,") => {
                link_rpath.push(a[11..].to_string());
            }
            // -rpath-link
            "-rpath-link" => {
                i += 1;
                link_rpath_link.push(
                    args.get(i)
                        .ok_or_else(|| CliError::new("missing arg after -rpath-link"))?
                        .clone(),
                );
            }
            a if a.starts_with("-Wl,-rpath-link,") => {
                link_rpath_link.push(a[16..].to_string());
            }
            // -runpath
            "-runpath" => {
                i += 1;
                link_runpath.push(
                    args.get(i)
                        .ok_or_else(|| CliError::new("missing arg after -runpath"))?
                        .clone(),
                );
            }
            a if a.starts_with("-Wl,-runpath,") => {
                link_runpath.push(a[13..].to_string());
            }
            // -fuse-ld=
            a if a.starts_with("-fuse-ld=") => {
                fuse_ld = Some(a[9..].to_string());
            }
            a if a.starts_with("-Wl,-fuse-ld,") => {
                fuse_ld = Some(a[13..].to_string());
            }
            // -fno-semantic-interposition / -fsemantic-interposition
            "-fno-semantic-interposition" => {
                no_semantic_interposition = true;
            }
            "-fsemantic-interposition" => {
                semantic_interposition = true;
            }
            // --gdb-index
            "-Wl,--gdb-index" => {
                gdb_index = true;
            }
            "--gdb-index" => {
                gdb_index = true;
            }
            // --compress-debug-sections
            a if a.starts_with("-Wa,--compress-debug-sections=") => {
                compress_debug = Some(a[31..].to_string());
            }
            a if a.starts_with("-Wl,--compress-debug-sections=") => {
                compress_debug = Some(a[32..].to_string());
            }
            // -flto-partition=N / -Wl,-lto-partition=N
            a if a.starts_with("-flto-partition=") => {
                if let Ok(n) = a[16..].parse::<u32>() {
                    lto_partitions = Some(n);
                }
            }
            // -u <sym>  显式 unresolved 根符号
            "-u" => {
                i += 1;
                gc_sections_root.push(
                    args.get(i)
                        .ok_or_else(|| CliError::new("missing arg after -u"))?
                        .clone(),
                );
            }
            a if a.starts_with("-Wl,-u,") => {
                gc_sections_root.push(a[8..].to_string());
            }
            // -lxxx 转 so 名 (从 .a 库解析, 此处简化为收集库名)
            "-l" => {
                i += 1;
                link_libs.push(
                    args.get(i)
                        .ok_or_else(|| CliError::new("missing arg after -l"))?
                        .clone(),
                );
            }
            a if a.starts_with("-l") && a.len() > 2 => {
                link_libs.push(a[2..].to_string());
            }
            // -L, -l, -T 等链接器选项
            "-L" | "-l" | "-T" => {
                i += 1;
            } // 跳过参数
            a if a.starts_with("-L") || a.starts_with("-l") || a.starts_with("-T") => {} // 内联形式
            // -Wl,* : 传递给链接器的选项
            a if a.starts_with("-Wl,") => {} // 忽略
            // -Wp,* : 仅白名单预处理器子选项；未知项报错，避免静默丢参
            "-Wp" => return Err(CliError::new("empty -Wp option")),
            a if a.starts_with("-Wp,") => {
                let opts: Vec<&str> = a[4..].split(',').collect();
                if opts.is_empty() || opts.iter().any(|option| option.is_empty()) {
                    return Err(CliError::new("empty -Wp option"));
                }
                let mut j = 0;
                while j < opts.len() {
                    let option = opts[j];
                    match option {
                        "-MD" => {
                            gen_dep = true;
                            dep_mmd_only = false;
                            if j + 1 < opts.len() && !opts[j + 1].starts_with('-') {
                                j += 1;
                                dep_file = Some(PathBuf::from(opts[j]));
                            }
                        }
                        "-MMD" => {
                            gen_dep = true;
                            dep_mmd_only = true;
                            if j + 1 < opts.len() && !opts[j + 1].starts_with('-') {
                                j += 1;
                                dep_file = Some(PathBuf::from(opts[j]));
                            }
                        }
                        "-MF" => {
                            j += 1;
                            let value = opts
                                .get(j)
                                .ok_or_else(|| CliError::new("missing arg after -Wp,-MF"))?;
                            dep_file = Some(PathBuf::from(value));
                        }
                        "-MT" | "-MQ" => {
                            j += 1;
                            let value = opts.get(j).ok_or_else(|| {
                                CliError::new(&format!("missing arg after -Wp,{}", option))
                            })?;
                            dep_target = Some((*value).to_string());
                        }
                        "-MP" => {
                            dep_phony = true;
                        }
                        "-D" => {
                            j += 1;
                            let def = opts
                                .get(j)
                                .ok_or_else(|| CliError::new("missing arg after -Wp,-D"))?;
                            if let Some(eq) = def.find('=') {
                                defines
                                    .push((def[..eq].to_string(), Some(def[eq + 1..].to_string())));
                            } else {
                                defines.push(((*def).to_string(), Some("1".to_string())));
                            }
                        }
                        option if option.starts_with("-D") && option.len() > 2 => {
                            let def = &option[2..];
                            if let Some(eq) = def.find('=') {
                                defines
                                    .push((def[..eq].to_string(), Some(def[eq + 1..].to_string())));
                            } else {
                                defines.push((def.to_string(), Some("1".to_string())));
                            }
                        }
                        "-I" => {
                            j += 1;
                            let path = opts
                                .get(j)
                                .ok_or_else(|| CliError::new("missing arg after -Wp,-I"))?;
                            include_paths.push(PathBuf::from(path));
                        }
                        option if option.starts_with("-I") && option.len() > 2 => {
                            include_paths.push(PathBuf::from(&option[2..]));
                        }
                        "-include" => {
                            j += 1;
                            let file = opts
                                .get(j)
                                .ok_or_else(|| CliError::new("missing arg after -Wp,-include"))?;
                            force_includes.push((*file).to_string());
                        }
                        option if option.starts_with("-include") && option.len() > 8 => {
                            force_includes.push(option[8..].to_string());
                        }
                        "-isystem" => {
                            j += 1;
                            let path = opts
                                .get(j)
                                .ok_or_else(|| CliError::new("missing arg after -Wp,-isystem"))?;
                            system_paths.push(PathBuf::from(path));
                        }
                        option if option.starts_with("-isystem") && option.len() > 8 => {
                            system_paths.push(PathBuf::from(&option[8..]));
                        }
                        "-U" => {
                            j += 1;
                            opts.get(j)
                                .ok_or_else(|| CliError::new("missing arg after -Wp,-U"))?;
                        }
                        option if option.starts_with("-U") && option.len() > 2 => {}
                        "-P" | "-C" | "-nostdinc" => {}
                        option => {
                            return Err(CliError::new(&format!(
                                "unsupported preprocessor option in -Wp: {}",
                                option
                            )));
                        }
                    }
                    j += 1;
                }
            }
            "-g" => debug_level = 2,
            "-g0" => debug_level = 0,
            "-g1" => debug_level = 1,
            "-g2" => debug_level = 2,
            "-g3" => debug_level = 3,
            "-flto" => lto_enabled = true,
            a if a.starts_with("-flto=") => lto_enabled = true,
            "-ffunction-sections" => function_sections = true,
            "-fno-function-sections" => no_function_sections = true,
            a if a.starts_with("-fsanitize") => {}
            "-ftrapv" => trapv = true,
            "-fno-trapv" => trapv = false,
            "-fbounds-check" => bounds_check = true,
            "-fbounds-safety" => bounds_check = true,
            "-fno-bounds-check" => bounds_check = false,
            "-Wint-conversion" => warn_int_conversion = true,
            "-Wno-int-conversion" => warn_int_conversion = false,
            "-Wint-truncation" => warn_int_truncation = true,
            "-Wno-int-truncation" => warn_int_truncation = false,
            "-Wimplicit-declaration" | "-Wimplicit-function-declaration" => {
                warn_implicit_decl = true
            }
            "-Wno-implicit-declaration" | "-Wno-implicit-function-declaration" => {
                warn_implicit_decl = false
            }
            // ====== PGO / GCOV / AutoFDO / LBR / MemProf ======
            "-fprofile-generate" => {
                profile.pgo_generate = Some(std::path::PathBuf::from("."));
                if let Some(next) = args.get(i + 1) {
                    if !next.starts_with('-') && next != &args[i] {
                        profile.pgo_generate = Some(std::path::PathBuf::from(next));
                        i += 1;
                    }
                }
            }
            "-fprofile-instr-generate" => {
                profile.pgo_generate = Some(std::path::PathBuf::from("."));
                if let Some(next) = args.get(i + 1) {
                    if !next.starts_with('-') {
                        profile.pgo_generate = Some(std::path::PathBuf::from(next));
                        i += 1;
                    }
                }
            }
            "-fprofile-use" | "-fprofile-instr-use" => {
                if let Some(next) = args.get(i + 1) {
                    profile.pgo_use = Some(std::path::PathBuf::from(next));
                    i += 1;
                } else {
                    return Err(CliError::new(&format!("{a}: missing profile data path")));
                }
            }
            "-fauto-profile" => {
                if let Some(next) = args.get(i + 1) {
                    profile.auto_profile = Some(std::path::PathBuf::from(next));
                    i += 1;
                } else {
                    return Err(CliError::new("-fauto-profile: missing file"));
                }
            }
            "-fprofile-sample-use" => {
                if let Some(next) = args.get(i + 1) {
                    profile.sample_use = Some(std::path::PathBuf::from(next));
                    i += 1;
                } else {
                    return Err(CliError::new("-fprofile-sample-use: missing file"));
                }
            }
            "-flbr-profile" => {
                if let Some(next) = args.get(i + 1) {
                    profile.lbr_profile = Some(std::path::PathBuf::from(next));
                    i += 1;
                } else {
                    return Err(CliError::new("-flbr-profile: missing file"));
                }
            }
            "-ftest-coverage" => {
                profile.gcov = true;
                profile.profile_arcs = true;
            }
            "-fno-test-coverage" => {
                profile.gcov = false;
            }
            "-fprofile-arcs" => {
                profile.profile_arcs = true;
            }
            "-fno-profile-arcs" => {
                profile.profile_arcs = false;
            }
            "-fbranch-probabilities" => { /* 兼容 GCC: 默认与 -fprofile-use 配对使用 */ }
            "-fprofile-values" => { /* TODO: 块级值采样, 当前 no-op */ }
            "-fprofile-list" => {
                if let Some(next) = args.get(i + 1) {
                    if !next.starts_with('-') {
                        profile.profile_list = Some(std::path::PathBuf::from(next));
                        i += 1;
                    } else {
                        profile.profile_list =
                            Some(std::path::PathBuf::from("dcc.profile-list.txt"));
                    }
                } else {
                    profile.profile_list = Some(std::path::PathBuf::from("dcc.profile-list.txt"));
                }
            }
            "-ftopdownmetrics" => {
                profile.topdown_metrics = true;
            }
            "-fno-topdownmetrics" => {
                profile.topdown_metrics = false;
            }
            "-frecord-profile" => {
                if let Some(next) = args.get(i + 1) {
                    profile.perf_record = Some(std::path::PathBuf::from(next));
                    i += 1;
                } else {
                    return Err(CliError::new("-frecord-profile: missing dir"));
                }
            }
            // ====== MemProf ======
            "-fmemory-profile" | "-fmemprof" => {
                profile.memprof = true;
            }
            "-fno-memory-profile" | "-fno-memprof" => {
                profile.memprof = false;
            }
            "-fmemory-profile-use" => {
                if let Some(next) = args.get(i + 1) {
                    profile.memprof_use = Some(std::path::PathBuf::from(next));
                    i += 1;
                } else {
                    return Err(CliError::new("-fmemory-profile-use: missing file"));
                }
            }
            // ====== PGO 集成 ======
            "-fprofile-pgo-inline" => profile.pgo_inline = true,
            "-fno-profile-pgo-inline" => profile.pgo_inline = false,
            "-fprofile-pgo-ipa-sra" => profile.pgo_ipa_sra = true,
            "-fno-profile-pgo-ipa-sra" => profile.pgo_ipa_sra = false,
            // ====== Offloading ======
            "-fopenmp" => {
                offload.openmp = true;
                offload.enabled = true;
            }
            a if a.starts_with("-fopenmp=") => {
                // -fopenmp=simd | libomp | libgomp | llvm
                let v = &a["-fopenmp=".len()..];
                if v == "simd" {
                    offload.openmp_simd = true;
                } else if matches!(v, "libomp" | "libgomp" | "llvm") {
                    offload.openmp_library = Some(v.to_string());
                } else {
                    return Err(CliError::new(&format!(
                        "unknown -fopenmp= value '{}' (expected simd/libomp/libgomp/llvm)",
                        v
                    )));
                }
                offload.openmp = true;
                offload.enabled = true;
            }
            "-fno-openmp" => {
                offload.openmp = false;
                offload.openmp_simd = false;
                offload.openmp_library = None;
                offload.enabled = offload.any();
            }
            "-fcuda" => {
                offload.cuda = true;
                offload.enabled = true;
            }
            "-fno-cuda" => {
                offload.cuda = false;
                offload.enabled = offload.any();
            }
            "-fhip" | "-fcuda-hip" => {
                offload.hip = true;
                offload.enabled = true;
            }
            "-fno-hip" => {
                offload.hip = false;
                offload.enabled = offload.any();
            }
            a if a.starts_with("-fopenmp-targets=") => {
                // -fopenmp-targets=triple1,triple2 (GCC 风格)
                let list = &a["-fopenmp-targets=".len()..];
                offload.target_triples = split_offload_list(list);
                offload.enabled = true;
            }
            "-fopenmp-targets" => {
                if let Some(next) = args.get(i + 1) {
                    offload.target_triples = split_offload_list(next);
                    offload.enabled = true;
                    i += 1;
                } else {
                    return Err(CliError::new("-fopenmp-targets: missing triple list"));
                }
            }
            a if a.starts_with("--offload-arch=") => {
                // --offload-arch=sm_80 (LLVM 风格, 可重复)
                offload
                    .target_archs
                    .push(a["--offload-arch=".len()..].to_string());
                offload.enabled = true;
            }
            "--offload-arch" => {
                if let Some(next) = args.get(i + 1) {
                    offload.target_archs.push(next.clone());
                    offload.enabled = true;
                    i += 1;
                } else {
                    return Err(CliError::new("--offload-arch: missing arch"));
                }
            }
            a if a.starts_with("-foffload-arch=") => {
                // -foffload-arch=sm_80 (GCC 风格)
                offload
                    .target_archs
                    .push(a["-foffload-arch=".len()..].to_string());
                offload.enabled = true;
            }
            "-foffload-arch" => {
                if let Some(next) = args.get(i + 1) {
                    offload.target_archs.push(next.clone());
                    offload.enabled = true;
                    i += 1;
                } else {
                    return Err(CliError::new("-foffload-arch: missing arch"));
                }
            }
            // ====== 诊断格式 (-fdiagnostics-*) ======
            // (具体解析见下方 `a if a.starts_with("-fdiagnostics-color=")` 块)
            a if a.starts_with("-fdiagnostics-color=") => {
                let v = &a["-fdiagnostics-color=".len()..];
                diag_color =
                    crate::diagnostics::ColorPolicy::from_arg(v).map_err(|e| CliError::new(&e))?;
            }
            a if a.starts_with("-fdiagnostics-format=") => {
                let v = &a["-fdiagnostics-format=".len()..];
                diag_format =
                    crate::diagnostics::FormatStyle::from_arg(v).map_err(|e| CliError::new(&e))?;
            }
            "-fdiagnostics-show-template-tree" => diag_show_template_tree = true,
            "-fno-diagnostics-show-template-tree" => diag_show_template_tree = false,
            a if a.starts_with("--bootstrap-compare=") => {
                bootstrap_compare = Some(PathBuf::from(&a["--bootstrap-compare=".len()..]));
            }
            a if a.starts_with("--sysroot=") => {
                sysroot = Some(PathBuf::from(&a["--sysroot=".len()..]));
            }
            a if a.starts_with("-Wl,--sysroot=") => {
                sysroot = Some(PathBuf::from(&a["-Wl,--sysroot=".len()..]));
            }
            // --vdso=<path>: vDSO ELF 文件路径
            a if a.starts_with("--vdso=") => {
                vdso = Some(PathBuf::from(&a["--vdso=".len()..]));
            }
            a if a.starts_with("-Wl,--vdso=") => {
                vdso = Some(PathBuf::from(&a["-Wl,--vdso=".len()..]));
            }
            "-x" => {
                i += 1;
                let lang = args
                    .get(i)
                    .ok_or_else(|| CliError::new("missing arg after -x"))?;
                input_lang = match lang.as_str() {
                    "assembler" => InputLang::Assembler,
                    "assembler-with-cpp" => InputLang::AssemblerWithCpp,
                    "c" | "c-header" => InputLang::C,
                    "c++" | "cpp" | "cxx" | "c++-header" | "c++-system-header"
                    | "c++-user-header" => InputLang::Cpp,
                    other => {
                        return Err(CliError::new(&format!(
                            "unsupported -x language: {}",
                            other
                        )))
                    }
                };
            }
            "--target" => {
                i += 1;
                target = TargetTriple::parse(
                    args.get(i)
                        .ok_or_else(|| CliError::new("missing arg after --target"))?,
                )
                .map_err(|e| CliError::new(&e))?;
            }
            "-std=c89" => std = StdMode::C89,
            "-std=c90" => std = StdMode::C89, // c90 = c89 别名
            "-std=iso9899:1990" => std = StdMode::C89,
            "-std=c99" => std = StdMode::C99,
            "-std=c9x" => std = StdMode::C99, // c9x = c99 别名
            "-std=iso9899:1999" => std = StdMode::C99,
            "-std=c11" => std = StdMode::C11,
            "-std=c1x" => std = StdMode::C11, // c1x = c11 别名
            "-std=iso9899:2011" => std = StdMode::C11,
            "-std=c17" => std = StdMode::C17,
            "-std=c18" => std = StdMode::C17, // c18 = c17 别名
            "-std=iso9899:2017" => std = StdMode::C17,
            "-std=c23" => std = StdMode::C23,
            "-std=c2x" => std = StdMode::C23, // c2x = c23 别名
            "-std=iso9899:2024" => std = StdMode::C23,
            "-std=gnu89" => std = StdMode::Gnu89,
            "-std=gnu90" => std = StdMode::Gnu89, // gnu90 = gnu89 别名
            "-std=gnu99" => std = StdMode::Gnu99,
            "-std=gnu11" => std = StdMode::Gnu11,
            "-std=gnu17" => std = StdMode::Gnu17,
            "-std=gnu18" => std = StdMode::Gnu17, // gnu18 = gnu17 别名
            "-std=gnu23" => std = StdMode::Gnu23,
            "-ffreestanding" => freestanding = true,
            "-fpermissive" => permissive = true,
            "-I" => {
                i += 1;
                include_paths.push(PathBuf::from(
                    args.get(i)
                        .ok_or_else(|| CliError::new("missing arg after -I"))?,
                ));
            }
            a if a.starts_with("-I") => {
                include_paths.push(PathBuf::from(&a[2..]));
            }
            "-D" => {
                i += 1;
                let def = args
                    .get(i)
                    .ok_or_else(|| CliError::new("missing arg after -D"))?;
                if let Some(eq) = def.find('=') {
                    defines.push((def[..eq].to_string(), Some(def[eq + 1..].to_string())));
                } else {
                    defines.push((def.clone(), Some("1".to_string())));
                }
            }
            a if a.starts_with("-D") => {
                let def = &a[2..];
                if let Some(eq) = def.find('=') {
                    defines.push((def[..eq].to_string(), Some(def[eq + 1..].to_string())));
                } else {
                    defines.push((def.to_string(), Some("1".to_string())));
                }
            }
            "-fno-builtin" => {}
            "-fsyntax-only" => {
                syntax_only = true;
            }
            // -Wa,* : 传递给汇编器的选项
            a if a.starts_with("-Wa,") => {
                let as_opts = &a[4..];
                as_options.push(as_opts.to_string());
                // 特殊处理 -Wa,--version : 调用系统汇编器并输出版本信息
                if as_opts.contains("--version") {
                    if let Ok(out) = std::process::Command::new("as").arg("--version").output() {
                        let stdout = String::from_utf8_lossy(&out.stdout);
                        print!("{}", stdout);
                    } else {
                        // 回退: 假装是 GNU as
                        println!("GNU assembler 2.38");
                    }
                    std::process::exit(0);
                }
                // 其他 -Wa, 选项暂时忽略
            }
            "-mno-red-zone" => red_zone = false,
            "-mred-zone" => red_zone = true,
            "-fno-omit-frame-pointer" => frame_pointer = true,
            "-fomit-frame-pointer" => frame_pointer = false,
            "-mno-sse" | "-mno-sse2" => sse = false,
            "-msse" | "-msse2" => sse = true,
            "-fstack-protector" | "-fstack-protector-all" | "-fstack-protector-strong" => {
                stack_protector = true
            }
            "-fno-stack-protector" => {
                stack_protector = false;
            }
            // Linux 内核编译常用的 GCC flags - 忽略但接受
            "-fno-pic"
            | "-fno-strict-aliasing"
            | "-fno-common"
            | "-fno-delete-null-pointer-checks"
            | "-fno-strict-overflow"
            | "-fno-merge-all-constants"
            | "-mno-mmx"
            | "-mno-80387"
            | "-mno-fp-ret-in-387"
            | "-mno-3dnow"
            | "-mno-3dnowa"
            | "-nostdinc"
            | "-nostdlib"
            | "-nodefaultlibs"
            | "-static"
            | "-shared"
            | "-pie"
            | "-no-pie"
            | "-pg"
            | "-pthread"
            | "-mcmodel=kernel"
            | "-mrecord-mcount"
            | "-mpreferred-stack-boundary=2"
            | "-fno-plt"
            | "-fno-strict-overflow"
            | "-ftrivial-auto-var-init=zero"
            | "-falign-jumps=1"
            | "-falign-loops=1"
            | "-fshort-wchar" => {}
            "-fno-asynchronous-unwind-tables" | "-fno-unwind-tables" => {
                unwind_tables = false;
            }
            "-mfunction-return=thunk-extern" => {
                function_return_thunk_extern = true;
            }
            "-mfunction-return=keep" => {
                function_return_thunk_extern = false;
            }
            a if a.starts_with("-fpatchable-function-entry=") => {
                let spec = a
                    .strip_prefix("-fpatchable-function-entry=")
                    .unwrap_or_default();
                let parts: Vec<&str> = spec.split(',').collect();
                if parts.is_empty() || parts.len() > 2 || parts.iter().any(|p| p.is_empty()) {
                    return Err(CliError::new("invalid -fpatchable-function-entry value"));
                }
                let n = parts[0]
                    .parse::<usize>()
                    .map_err(|_| CliError::new("invalid -fpatchable-function-entry N"))?;
                let m = if parts.len() == 2 {
                    parts[1]
                        .parse::<usize>()
                        .map_err(|_| CliError::new("invalid -fpatchable-function-entry M"))?
                } else {
                    0
                };
                if m > n {
                    return Err(CliError::new("-fpatchable-function-entry M must be <= N"));
                }
                patchable_function_entry = Some((n, m));
            }
            "-mindirect-branch=thunk-extern" => {
                indirect_branch_thunk_extern = true;
            }
            "-mindirect-branch=keep" => {
                indirect_branch_thunk_extern = false;
            }
            // GCC 警告/诊断类选项 - 接受但忽略
            "-pedantic"
            | "-pedantic-errors"
            | "-ansi"
            | "-traditional"
            | "-trigraphs"
            | "-Wmissing-prototypes"
            | "-Wstrict-prototypes"
            | "-Wold-style-definition"
            | "-Wmissing-format-attribute"
            | "-Wno-long-long"
            | "-Wc++-compat"
            | "-Wcast-qual" => {}
            // GCC -U 取消宏定义 - 接受但忽略（预处理阶段已处理）
            a if a.starts_with("-U") => {}
            // -fcf-protection: 控制流保护 - 接受但忽略
            "-fcf-protection"
            | "-fcf-protection=none"
            | "-fcf-protection=full"
            | "-fcf-protection=branch"
            | "-fcf-protection=return" => {}
            // -Wshadow=*: 接受但忽略
            a if a.starts_with("-Wshadow") => {}
            "-P" => {} // 抑制 #line 指令，DCC 预处理器本身就不输出它们
            "-C" => {} // GCC -C: 预处理保留注释 (内核 linker script 使用)
            // -W* 警告选项 - 忽略
            a if a.starts_with("-W") => {}
            // -m* 机器选项
            "-m32" => m32 = true,
            a if a.starts_with("-m") => {} // 其余 -m* 忽略
            // -fipa-sra: 启用/禁用跨过程标量替换
            "-fipa-sra" => ipa_sra.enabled = true,
            "-fno-ipa-sra" => ipa_sra.enabled = false,
            // PCH (预编译头): -fpch-include=path / -fpch-create=path / -include-pch=path
            // 这是 GCC/Clang 的 PCH 简化接口: 把一组共享头的预处理结果缓存为
            // .pch 文件, 后续 TU 通过 -fpch-include 直接加载, 跳过对共享头的
            // 重复 lex+展开+条件求值.
            // 注意: 这几条规则必须在 "-f*" 通用 catch-all 之前, 否则被忽略.
            a if a.starts_with("-fpch-include=") => {
                pch_opts.include = Some(a[14..].to_string());
            }
            a if a.starts_with("-fpch-create=") => {
                pch_opts.create = Some(a[13..].to_string());
            }
            a if a.starts_with("-fpch-include-only=") => {
                pch_opts.include_only = Some(a[19..].to_string());
            }
            // M26.4: --emit-module-file=foo.bmf - 编译 export module 时输出模块接口
            a if a.starts_with("--emit-module-file=") => {
                emit_module_file =
                    Some(PathBuf::from(a["--emit-module-file=".len()..].to_string()));
            }
            // -f* 其他选项 - 忽略
            a if a.starts_with("-f") => {}
            // -std=* 已经处理过了
            // -include, -isystem 等
            "-include" => {
                i += 1;
                let f = args
                    .get(i)
                    .ok_or_else(|| CliError::new("missing arg after -include"))?;
                force_includes.push(f.clone());
            }
            "-isystem" => {
                i += 1;
                system_paths.push(PathBuf::from(
                    args.get(i)
                        .ok_or_else(|| CliError::new("missing arg after -isystem"))?,
                ));
            }
            a if a.starts_with("-include") => {
                force_includes.push(a[8..].to_string());
            }
            a if a.starts_with("-isystem") => {
                system_paths.push(PathBuf::from(&a[8..]));
            }
            // -MD/-MMD: 生成依赖文件
            "-MD" => {
                gen_dep = true;
                dep_mmd_only = false;
            }
            "-MMD" => {
                gen_dep = true;
                dep_mmd_only = true;
            }
            // -MF <file>: 依赖文件路径
            "-MF" => {
                i += 1;
                let f = args
                    .get(i)
                    .ok_or_else(|| CliError::new("missing arg after -MF"))?;
                dep_file = Some(PathBuf::from(f));
            }
            // -MT <target>: 依赖中的目标名
            "-MT" => {
                i += 1;
                dep_target = Some(
                    args.get(i)
                        .ok_or_else(|| CliError::new("missing arg after -MT"))?
                        .clone(),
                );
            }
            // -MQ <target>: 同-MT, 带引号
            "-MQ" => {
                i += 1;
                dep_target = Some(
                    args.get(i)
                        .ok_or_else(|| CliError::new("missing arg after -MQ"))?
                        .clone(),
                );
            }
            // -MP: 为头文件生成空依赖目标 (暂忽略)
            "-MP" => {
                dep_phony = true;
            }
            "-dump-ast" => dump_ast = true,
            "-v" | "--verbose" => verbose = true,
            // -print-file-name=<file>: GCC 用于查找库/插件/头文件路径
            a if a.starts_with("-print-file-name=") => {
                let filename = &a[17..];
                // 对于 include, 返回 GCC 的 include 目录
                if filename == "include" {
                    if let Ok(out) = std::process::Command::new("gcc")
                        .arg("-print-file-name=include")
                        .output()
                    {
                        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
                        println!("{}", path);
                    } else {
                        println!("/usr/lib/gcc/x86_64-linux-gnu/include");
                    }
                } else {
                    // 其他文件: 返回文件名本身 (表示找不到)
                    println!("{}", filename);
                }
                std::process::exit(0);
            }
            // -print-search-dirs: 输出搜索目录
            "-print-search-dirs" => {
                println!();
                std::process::exit(0);
            }
            "-dumpversion" => {
                println!("{}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "-dumpmachine" => {
                println!("{}", target.as_str());
                std::process::exit(0);
            }
            "--version" => {
                println!("dcc {}", env!("CARGO_PKG_VERSION"));
                println!("A suckless C compiler.");
                println!("Target: {}", target.as_str());
                println!("Default std: {}", std.as_str());
                std::process::exit(0);
            }
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            "-" => {
                inputs.push(PathBuf::from("-"));
            }
            a if a.starts_with('-') => return Err(CliError::new(&format!("unknown flag: {}", a))),
            _ => {
                inputs.push(PathBuf::from(a));
            }
        }
        i += 1;
    }

    // -flto 默认开启 per-function section，便于链接期 --gc-sections
    if lto_enabled && !no_function_sections {
        function_sections = true;
    }

    let link_mode = relocatable_link
        && !inputs.is_empty()
        && inputs
            .iter()
            .all(|p| p.extension().map(|e| e == "o").unwrap_or(false));

    if relocatable_link
        && !inputs.is_empty()
        && inputs
            .iter()
            .any(|p| !p.extension().map(|e| e == "o").unwrap_or(false))
    {
        return Err(CliError::new(
            "relocatable link (-r) requires object (.o) inputs; \
             compile sources with -c first",
        ));
    }

    if link_mode && (emit_asm || preprocess_only || syntax_only || dump_ast) {
        return Err(CliError::new(
            "link mode (-r) cannot combine with -S/-E/-fsyntax-only",
        ));
    }

    if !link_mode && inputs.len() > 1 {
        let all_sources = inputs.iter().all(|p| {
            matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("c") | Some("C") | Some("S") | Some("s")
            )
        });
        let all_objects = inputs
            .iter()
            .all(|p| p.extension().and_then(|e| e.to_str()) == Some("o"));
        if all_sources {
            return Err(CliError::new(
                "multiple source files cannot produce a single executable; \
                 compile each with -c, then link with gcc(1) or dcc -r to merge .o files",
            ));
        }
        if all_objects {
            // 所有输入是 .o 文件, 自动启用内置链接器
            built_link = true;
            if cfg!(debug_assertions) && verbose {
                eprintln!("dcc: auto-enabling built-in linker for .o inputs");
            }
        } else {
            return Err(CliError::new(
                "mixed input types; compile sources with -c first, then link .o files",
            ));
        }
    } else if !link_mode && inputs.len() == 1 {
        // 单个 .o 文件输入: 自动启用内置链接器
        if let Some(ext) = inputs[0].extension().and_then(|e| e.to_str()) {
            if ext == "o" && !relocatable && !emit_asm && !preprocess_only && !syntax_only {
                built_link = true;
            }
        }
    }

    // -E 模式允许没有输入文件 (从 stdin 读取)
    if !preprocess_only {
        if inputs.is_empty() && !link_mode {
            return Err(CliError::new("no input file"));
        }
        let first = inputs
            .first()
            .cloned()
            .unwrap_or_else(|| PathBuf::from("-"));
        // -c / -S 模式默认输出扩展名（单 TU 编译）
        if !link_mode && output == *"a.out" {
            let stem = first.file_stem().unwrap_or_default().to_string_lossy();
            if emit_asm {
                output = PathBuf::from(format!("{}.s", stem));
            } else if relocatable {
                output = PathBuf::from(format!("{}.o", stem));
            }
        }
        // 如果未指定 dep_file 但启用了 dep, 用输出文件名替换扩展名为 .o 后加 .d
        let dep_file = dep_file.or_else(|| {
            if gen_dep && !link_mode && (relocatable || emit_asm) {
                let stem = first.file_stem().unwrap_or_default().to_string_lossy();
                Some(PathBuf::from(format!("{}.d", stem)))
            } else {
                None
            }
        });
        let dep_target = dep_target.or_else(|| {
            if gen_dep {
                Some(output.display().to_string())
            } else {
                None
            }
        });
        let parallel_codegen = opt >= 2 && std::env::var("DCC_NO_PARALLEL").is_err();
        let mut all_link_opts = crate::link::link_opts::LinkOpts::from_env();
        for z in &link_opts_z {
            all_link_opts.parse_z(z);
        }
        if no_semantic_interposition {
            all_link_opts.no_semantic_interposition = true;
        }
        if semantic_interposition {
            all_link_opts.semantic_interposition = true;
        }
        if gdb_index {
            all_link_opts.gdb_index = true;
        }
        if let Some(ref c) = compress_debug {
            all_link_opts.compress_debug = Some(c.clone());
        }
        if let Some(n) = lto_partitions {
            all_link_opts.lto_partitions = Some(n);
        }
        if let Some(ref fl) = fuse_ld {
            all_link_opts.fuse_ld = Some(fl.clone());
        }
        if !link_rpath.is_empty() {
            all_link_opts.rpath = Some(link_rpath.join(":"));
        }
        if !link_runpath.is_empty() {
            all_link_opts.runpath = Some(link_runpath.join(":"));
        }
        all_link_opts.rpath_link = link_rpath_link.clone();
        // 解析完成后校验 offload 参数值 (非法 triple / arch 带具体值报错)
        if let Err(e) = offload.validate() {
            return Err(CliError::new(&e));
        }
        Ok(Config {
            inputs,
            output,
            output_explicit,
            opt,
            std,
            target,
            freestanding,
            include_paths,
            system_paths,
            defines,
            dump_ast,
            verbose,
            relocatable,
            link_mode,
            emit_asm,
            preprocess_only,
            dump_macros,
            force_includes,
            pch_opts,
            syntax_only,
            emit_module_file,
            dep_file,
            dep_target,
            gen_dep,
            dep_mmd_only,
            dep_phony,
            red_zone,
            frame_pointer,
            sse,
            stack_protector,
            unwind_tables,
            function_return_thunk_extern,
            indirect_branch_thunk_extern,
            patchable_function_entry,
            debug_level,
            lto_enabled,
            function_sections,
            sanitizers: SanitizerFlags::from_args(args)?,
            input_lang,
            as_options,
            m32,
            parallel_codegen,
            permissive,
            raw_args: args.to_vec(),
            built_link,
            output_kind,
            soname: soname.clone(),
            linker_entry: linker_entry.clone(),
            linker_lto,
            exclude_libs: exclude_libs.clone(),
            build_id,
            no_undefined_version,
            link_opts: all_link_opts,
            link_libs: link_libs.clone(),
            gc_sections_root: gc_sections_root.clone(),
            no_semantic_interposition,
            semantic_interposition,
            fuse_ld: fuse_ld.clone(),
            gdb_index,
            compress_debug: compress_debug.clone(),
            lto_partitions,
            trapv,
            bounds_check,
            warn_int_conversion,
            warn_int_truncation,
            warn_implicit_decl,
            diag_color,
            diag_format,
            diag_show_template_tree,
            bootstrap_compare: bootstrap_compare.clone(),
            sysroot: sysroot.clone(),
            vdso: vdso.clone(),
            ipa_sra: {
                let mut c = ipa_sra;
                c.opt_level = opt;
                c
            },
            profile: merge_profile_config(profile, offload),
        })
    } else {
        if inputs.is_empty() {
            inputs.push(PathBuf::from("-"));
        }
        let parallel_codegen = opt >= 2 && std::env::var("DCC_NO_PARALLEL").is_err();
        Ok(Config {
            inputs,
            output,
            output_explicit,
            opt,
            std,
            target,
            freestanding,
            include_paths,
            system_paths,
            defines,
            dump_ast,
            verbose,
            relocatable,
            link_mode: false,
            emit_asm,
            preprocess_only,
            dump_macros,
            force_includes,
            pch_opts,
            syntax_only,
            emit_module_file: None,
            dep_file,
            dep_target,
            gen_dep,
            dep_mmd_only,
            dep_phony,
            red_zone,
            frame_pointer,
            sse,
            stack_protector,
            unwind_tables,
            function_return_thunk_extern,
            indirect_branch_thunk_extern,
            patchable_function_entry,
            debug_level,
            lto_enabled,
            function_sections,
            sanitizers: SanitizerFlags::from_args(args)?,
            input_lang,
            as_options,
            m32,
            parallel_codegen,
            permissive,
            raw_args: args.to_vec(),
            built_link: false,
            output_kind: LinkerOutputKind::Executable,
            soname: None,
            linker_entry: None,
            linker_lto: false,
            exclude_libs: Vec::new(),
            build_id: BuildIdStyle::None,
            no_undefined_version: false,
            link_opts: crate::link::link_opts::LinkOpts::from_env(),
            link_libs: Vec::new(),
            gc_sections_root: Vec::new(),
            no_semantic_interposition: false,
            semantic_interposition: false,
            fuse_ld: None,
            gdb_index: false,
            compress_debug: None,
            lto_partitions: None,
            trapv,
            bounds_check,
            warn_int_conversion,
            warn_int_truncation,
            warn_implicit_decl,
            diag_color,
            diag_format,
            diag_show_template_tree,
            bootstrap_compare,
            sysroot,
            vdso,
            ipa_sra: {
                let mut c = ipa_sra;
                c.opt_level = opt;
                c
            },
            profile: merge_profile_config(profile, offload),
        })
    }
}

/// 拆分 `-fopenmp-targets=` 的逗号分隔 triple 列表 (去空白/去空项).
fn split_offload_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

/// 把 `offload` 子状态合并到 `profile` 顶层 (openmp / cuda / hip / targets).
fn merge_profile_config(
    mut p: crate::profile::ProfileConfig,
    o: crate::profile::offload::OffloadConfig,
) -> crate::profile::ProfileConfig {
    p.openmp = o.openmp || o.openmp_simd;
    p.openmp_target = if o.target_triples.is_empty() {
        None
    } else {
        Some(o.target_triples.join(","))
    };
    p.cuda = o.cuda;
    p.hip = o.hip;
    p.offload = o;
    p
}

fn print_help() {
    println!("dcc - suckless C compiler");
    println!();
    println!("USAGE:");
    println!("    dcc [options] <input.c>              compile / assemble one TU");
    println!("    dcc -c -o out.o input.c            emit relocatable object");
    println!("    dcc -r -o merged.o a.o b.o         merge .o via host ld(1)");
    println!("    dcc -r -flto -o merged.o a.o b.o   merge + .llvm.lto + gc-sections");
    println!();
    println!("LINKING NOTE:");
    println!("    dcc is not a full linker. Multi-file executables:");
    println!("      dcc -c a.c -o a.o && dcc -c b.c -o b.o");
    println!("      gcc a.o b.o -o prog    # or: dcc -r -o all.o a.o b.o");
    println!();
    println!("OPTIONS:");
    println!("    -c                  emit relocatable object (.o), do not link");
    println!("    -r                  merge .o files (requires host ld, objcopy)");
    println!("    -flto               per-function sections + .llvm.lto summary");
    println!("    -O0|-O1|-O2|-O3     optimization level (default O0)");
    println!("    -o <file>           output file (default a.out)");
    println!("    --target <triple>   target triple (e.g. aarch64-linux-gnu)");
    println!("    -std=c99|gnu11|...  C standard (default gnu23)");
    println!("    -ffreestanding      freestanding (no libc)");
    println!("    -dump-ast           print AST and exit");
    println!("    -v, --verbose       verbose output");
    println!("    --version           show version");
    println!("    -h, --help          show this help");
}

#[derive(Debug)]
pub struct CliError {
    pub msg: String,
}
impl CliError {
    pub fn new(m: &str) -> Self {
        Self { msg: m.into() }
    }
    pub fn print(&self) {
        eprintln!("dcc: error: {}", self.msg);
    }
}

#[cfg(test)]
mod tests {
    use super::{parse, InputLang};
    use std::path::PathBuf;

    #[test]
    fn x_language_aliases_are_supported() {
        for (lang, expected) in [
            ("assembler", InputLang::Assembler),
            ("assembler-with-cpp", InputLang::AssemblerWithCpp),
            ("c", InputLang::C),
            ("c-header", InputLang::C),
            ("c++", InputLang::Cpp),
            ("cpp", InputLang::Cpp),
            ("cxx", InputLang::Cpp),
            ("c++-header", InputLang::Cpp),
            ("c++-system-header", InputLang::Cpp),
            ("c++-user-header", InputLang::Cpp),
        ] {
            let args = vec!["-x".to_string(), lang.to_string(), "input".to_string()];
            let cfg = parse(&args).expect("supported -x language should parse");
            assert_eq!(cfg.input_lang, expected, "language: {lang}");
        }
    }

    #[test]
    fn x_unknown_language_is_rejected() {
        let args = vec!["-x".to_string(), "fortran".to_string(), "input".to_string()];
        let err = parse(&args).expect_err("unsupported -x language must fail");
        assert!(err.msg.contains("unsupported -x language: fortran"));
    }
    #[test]
    fn x_missing_language_is_rejected() {
        let args = vec!["-x".to_string(), "input".to_string()];
        let err = parse(&args).expect_err("missing -x language must fail");
        assert!(err.msg.contains("unsupported -x language: input"));
    }
    #[test]
    fn wp_kbuild_dependency_form_is_forwarded() {
        let args = vec![
            "-Wp,-MMD,scripts/mod/.empty.o.d".to_string(),
            "-c".to_string(),
            "scripts/mod/empty.c".to_string(),
        ];
        let cfg = parse(&args).expect("Kbuild -Wp dependency options should parse");

        assert!(cfg.gen_dep);
        assert_eq!(cfg.dep_file, Some(PathBuf::from("scripts/mod/.empty.o.d")));
    }

    #[test]
    fn wp_whitelisted_split_options_are_applied() {
        let args = vec![
            "-Wp,-MMD,deps/main.d,-MT,main.o,-MP".to_string(),
            "-Wp,-I,include,-D,VALUE=7".to_string(),
            "-Wp,-include,forced.h,-isystem,sysroot/include".to_string(),
            "-Wp,-U,OLD,-P,-C,-nostdinc".to_string(),
            "input.c".to_string(),
        ];
        let cfg = parse(&args).expect("whitelisted -Wp options should parse");

        assert!(cfg.gen_dep);
        assert!(cfg.dep_mmd_only);
        assert!(cfg.dep_phony);
        assert_eq!(cfg.dep_file, Some(PathBuf::from("deps/main.d")));
        assert_eq!(cfg.dep_target, Some("main.o".to_string()));
        assert_eq!(cfg.include_paths, vec![PathBuf::from("include")]);
        assert_eq!(cfg.system_paths, vec![PathBuf::from("sysroot/include")]);
        assert_eq!(cfg.force_includes, vec!["forced.h".to_string()]);
        assert_eq!(
            cfg.defines,
            vec![("VALUE".to_string(), Some("7".to_string()))]
        );
    }

    #[test]
    fn wp_empty_sub_option_is_rejected() {
        let args = vec!["-Wp,-MMD,,dep.d".to_string(), "input.c".to_string()];
        let err = parse(&args).expect_err("empty -Wp sub-options must not parse");
        assert!(err.msg.contains("empty -Wp option"));
    }

    #[test]
    fn unsupported_wp_option_is_rejected() {
        let args = vec!["-Wp,--silently-lost".to_string(), "input.c".to_string()];
        let err = parse(&args).expect_err("unsupported -Wp options must not be ignored");
        assert!(err.msg.contains("unsupported preprocessor option"));
    }

    #[test]
    fn c_with_fpic_stays_compile_only() {
        let args = vec![
            "-c".to_string(),
            "-fpic".to_string(),
            "-o".to_string(),
            "out.o".to_string(),
            "input.c".to_string(),
        ];
        let cfg = parse(&args).expect("-c -fpic should parse");
        assert!(cfg.relocatable);
        assert!(!cfg.built_link);
        assert!(!cfg.link_mode);
    }

    #[test]
    fn c_with_fpic_uppercase_stays_compile_only() {
        let args = vec![
            "-c".to_string(),
            "-fPIC".to_string(),
            "-o".to_string(),
            "out.o".to_string(),
            "input.c".to_string(),
        ];
        let cfg = parse(&args).expect("-c -fPIC should parse");
        assert!(cfg.relocatable);
        assert!(!cfg.built_link);
        assert!(!cfg.link_mode);
    }

    #[test]
    fn later_fno_stack_protector_disables_stack_protector() {
        let args = vec![
            "-fstack-protector-strong".to_string(),
            "-fno-stack-protector".to_string(),
            "-c".to_string(),
            "input.c".to_string(),
        ];
        let cfg = parse(&args).expect("stack protector flags should parse");
        assert!(!cfg.stack_protector);
    }

    #[test]
    fn later_stack_protector_reenables_stack_protector() {
        let args = vec![
            "-fno-stack-protector".to_string(),
            "-fstack-protector-strong".to_string(),
            "-c".to_string(),
            "input.c".to_string(),
        ];
        let cfg = parse(&args).expect("stack protector flags should parse");
        assert!(cfg.stack_protector);
    }

    #[test]
    fn patchable_function_entry_parses_and_rejects_invalid_values() {
        let cfg = parse(&[
            "-fpatchable-function-entry=16,8".to_string(),
            "-c".to_string(),
            "input.c".to_string(),
        ])
        .expect("valid patchable entry should parse");
        assert_eq!(cfg.patchable_function_entry, Some((16, 8)));

        for flag in [
            "-fpatchable-function-entry=",
            "-fpatchable-function-entry=abc",
            "-fpatchable-function-entry=16,abc",
            "-fpatchable-function-entry=16,17",
            "-fpatchable-function-entry=16,8,1",
        ] {
            let err = parse(&[flag.to_string(), "input.c".to_string()])
                .expect_err("invalid patchable entry should fail");
            assert!(err.msg.contains("patchable-function-entry"));
        }
    }

    #[test]
    fn function_return_thunk_flags_parse_last_wins() {
        let cfg = parse(&[
            "-mfunction-return=thunk-extern".to_string(),
            "-c".to_string(),
            "input.c".to_string(),
        ])
        .expect("return thunk flag should parse");
        assert!(cfg.function_return_thunk_extern);

        let cfg = parse(&[
            "-mfunction-return=thunk-extern".to_string(),
            "-mfunction-return=keep".to_string(),
            "-c".to_string(),
            "input.c".to_string(),
        ])
        .expect("keep should parse after thunk");
        assert!(!cfg.function_return_thunk_extern);
    }

    #[test]
    fn multiple_c_sources_cannot_link_to_exe() {
        let args = vec![
            "-o".to_string(),
            "prog".to_string(),
            "a.c".to_string(),
            "b.c".to_string(),
        ];
        let err = parse(&args).expect_err("multi .c must be rejected");
        assert!(err.msg.contains("compile each with -c"));
    }

    #[test]
    fn r_link_mode_accepts_single_object() {
        let args = vec![
            "-r".to_string(),
            "-o".to_string(),
            "merged.o".to_string(),
            "a.o".to_string(),
        ];
        let cfg = parse(&args).expect("single .o -r link");
        assert!(cfg.link_mode);
        assert!(cfg.relocatable);
    }

    #[test]
    fn r_link_mode_requires_object_inputs() {
        let args = vec![
            "-r".to_string(),
            "-o".to_string(),
            "out.o".to_string(),
            "foo.c".to_string(),
        ];
        let err = parse(&args).expect_err(".c with -r must fail");
        assert!(err.msg.contains("requires object (.o) inputs"));
    }

    #[test]
    fn two_object_r_link_mode() {
        let args = vec![
            "-r".to_string(),
            "-flto".to_string(),
            "-o".to_string(),
            "merged.o".to_string(),
            "a.o".to_string(),
            "b.o".to_string(),
        ];
        let cfg = parse(&args).expect("two .o -r link");
        assert!(cfg.link_mode);
        assert!(cfg.lto_enabled);
    }

    // ----- Triple 解析覆盖测试 (mips / mips64 / arm 仅解析, 无 codegen 后端) -----

    use super::{Arch, TargetTriple};

    #[test]
    fn target_triple_parses_mips_linux_gnu() {
        let t = TargetTriple::parse("mips-linux-gnu").expect("mips must parse");
        assert_eq!(t.arch, Arch::Mips);
        assert_eq!(t.vendor, "unknown");
        assert_eq!(t.os, super::Os::Linux);
        assert_eq!(t.abi, super::Abi::Gnu);
        assert!(t.arch.has_backend());
    }

    #[test]
    fn target_triple_parses_mips_unknown_linux_gnu() {
        let t = TargetTriple::parse("mips-unknown-linux-gnu").expect("mips w/ vendor must parse");
        assert_eq!(t.arch, Arch::Mips);
        assert_eq!(t.vendor, "unknown");
        assert!(t.arch.has_backend());
    }

    #[test]
    fn target_triple_parses_mips64_linux_gnuabi64() {
        let t = TargetTriple::parse("mips64-linux-gnuabi64").expect("mips64 must parse");
        assert_eq!(t.arch, Arch::Mips64);
        assert!(t.arch.has_backend());
    }

    #[test]
    fn target_triple_parses_arm_linux_gnueabihf() {
        let t = TargetTriple::parse("arm-linux-gnueabihf").expect("armhf must parse");
        assert_eq!(t.arch, Arch::Arm);
        assert_eq!(t.abi, super::Abi::Gnu);
        assert!(t.arch.has_backend());
    }

    #[test]
    fn target_triple_parses_armv7_alias() {
        let t =
            TargetTriple::parse("armv7-unknown-linux-gnueabihf").expect("armv7 alias must parse");
        assert_eq!(t.arch, Arch::Arm);
        assert_eq!(t.vendor, "unknown");
    }

    #[test]
    fn has_backend_distinguishes_implemented_vs_parse_only() {
        assert!(Arch::X86_64.has_backend());
        assert!(Arch::AArch64.has_backend());
        assert!(Arch::RiscV64.has_backend());
        assert!(Arch::Mips.has_backend());
        assert!(Arch::Mips64.has_backend());
        assert!(Arch::Arm.has_backend());
    }

    #[test]
    fn backend_missing_msg_mentions_arch_and_parse_only() {
        // has_backend() 现在全部为 true，backend_missing_msg 不再被调用
        // 但保留测试以验证消息格式
        let msg = Arch::Mips.backend_missing_msg();
        assert!(msg.contains("Mips"));
    }

    #[test]
    fn unknown_arch_is_still_rejected() {
        let err = TargetTriple::parse("powerpc-linux-gnu").expect_err("powerpc is not in our list");
        assert!(err.contains("unknown arch"));
        assert!(err.contains("powerpc"));
    }

    #[test]
    fn too_few_dash_components_is_rejected() {
        let err = TargetTriple::parse("mips").expect_err("single component is invalid");
        assert!(err.contains("bad target"));
    }

    #[test]
    fn dashdash_target_mips_passes_cli_parse() {
        let args = vec![
            "--target".to_string(),
            "mips-linux-gnu".to_string(),
            "-c".to_string(),
            "foo.c".to_string(),
        ];
        let cfg = parse(&args).expect("--target mips must parse at cli level");
        assert_eq!(cfg.target.arch, Arch::Mips);
        assert!(cfg.target.arch.has_backend());
    }

    #[test]
    fn musl_loader_path_x86_64() {
        let t = TargetTriple::parse("x86_64-linux-musl").unwrap();
        assert_eq!(
            t.musl_loader_path().as_deref(),
            Some("lib/ld-musl-x86_64.so.1")
        );
    }

    #[test]
    fn musl_loader_path_aarch64() {
        let t = TargetTriple::parse("aarch64-linux-musl").unwrap();
        assert_eq!(
            t.musl_loader_path().as_deref(),
            Some("lib/ld-musl-aarch64.so.1")
        );
    }

    #[test]
    fn musl_loader_path_riscv64() {
        let t = TargetTriple::parse("riscv64-linux-musl").unwrap();
        assert_eq!(
            t.musl_loader_path().as_deref(),
            Some("lib/ld-musl-riscv64.so.1")
        );
    }

    #[test]
    fn musl_loader_path_arm() {
        let t = TargetTriple::parse("arm-linux-musl").unwrap();
        assert_eq!(
            t.musl_loader_path().as_deref(),
            Some("lib/ld-musl-armhf.so.1")
        );
    }

    #[test]
    fn musl_loader_path_gnu_returns_none() {
        let t = TargetTriple::parse("x86_64-linux-gnu").unwrap();
        assert_eq!(t.musl_loader_path(), None);
    }

    #[test]
    fn musl_loader_path_mips_returns_none() {
        // musl 官方仓库在 mips 上游未提供, 保守返回 None
        let t = TargetTriple::parse("mips-linux-musl").unwrap();
        assert_eq!(t.musl_loader_path(), None);
    }

    #[test]
    fn dashdash_sysroot_passes_cli_parse() {
        let args = vec![
            "--sysroot=/opt/musl-sysroot".to_string(),
            "-c".to_string(),
            "foo.c".to_string(),
        ];
        let cfg = parse(&args).expect("--sysroot must parse at cli level");
        assert_eq!(
            cfg.sysroot.as_ref().unwrap().to_str().unwrap(),
            "/opt/musl-sysroot"
        );
    }
}
