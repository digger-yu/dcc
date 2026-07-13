// src/cli.rs - 命令行参数解析
// 第一性原理: CLI 只是"配置 + 文件列表"的扁平化,不做任何决策。
// 真正的决策 (启不启优化、目标平台) 在 driver 层。
//
// suckless 准则: 我们只支持最常用的 GCC 子集选项,够用即可。

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub input:    PathBuf,
    pub output:   PathBuf,         // 默认 a.out
    pub opt:      u8,              // 0..3
    pub std:      StdMode,
    pub target:   TargetTriple,
    pub freestanding: bool,
    pub include_paths: Vec<PathBuf>,
    pub system_paths: Vec<PathBuf>,
    pub defines:  Vec<(String, Option<String>)>,  // -D NAME=VALUE
    pub dump_ast: bool,
    pub verbose:  bool,
    pub relocatable: bool,  // -c: 输出可重定位 .o
    pub preprocess_only: bool, // -E: 只做预处理
    pub force_includes: Vec<String>, // -include file: 强制包含文件
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdMode {
    C89, C99, C11, C17, C23,
    Gnu89, Gnu99, Gnu11, Gnu17, Gnu23,   // GNU 扩展
}

impl StdMode {
    pub fn as_str(self) -> &'static str {
        match self {
            StdMode::C89 => "c89", StdMode::Gnu89 => "gnu89",
            StdMode::C99 => "c99", StdMode::Gnu99 => "gnu99",
            StdMode::C11 => "c11", StdMode::Gnu11 => "gnu11",
            StdMode::C17 => "c17", StdMode::Gnu17 => "gnu17",
            StdMode::C23 => "c23", StdMode::Gnu23 => "gnu23",
        }
    }
}

#[derive(Debug, Clone)]
pub struct TargetTriple {
    pub arch:    Arch,
    pub vendor:  String,   // "unknown" or "pc" or "apple"
    pub os:      Os,
    pub abi:     Abi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch  { X86_64, AArch64, RiscV64, Mips64, Mips, Arm }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os    { Linux, Freestanding, Windows, Macos }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Abi   { Gnu, Musl, Eabi, Eabihf, Msvc, Darwin }

impl TargetTriple {
    pub fn host() -> Self {
        // 第一性原理: 大多数情况下 host == target (本地编译)
        // 跨编译由 --target=xxx-... 显式指定
        #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
        return Self { arch: Arch::X86_64, vendor: "unknown".into(),
                      os: Os::Linux, abi: Abi::Gnu };
        #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
        return Self { arch: Arch::AArch64, vendor: "unknown".into(),
                      os: Os::Linux, abi: Abi::Gnu };
        #[cfg(all(target_arch = "x86_64", target_os = "windows"))]
        return Self { arch: Arch::X86_64, vendor: "pc".into(),
                      os: Os::Windows, abi: Abi::Msvc };
        // 兜底: 用 unreachable!() 抑制 dead_code 警告, 只在支持的目标上编译
        #[allow(unreachable_code)]
        {
            let _ = Abi::Gnu; // 抑制 Abi unused 警告
            Self { arch: Arch::X86_64, vendor: "unknown".into(),
                   os: Os::Linux, abi: Abi::Gnu }
        }
    }

    pub fn parse(s: &str) -> Result<Self, String> {
        // 形如: x86_64-unknown-linux-gnu, aarch64-linux-gnu, riscv64-linux-gnu
        let parts: Vec<&str> = s.split('-').collect();
        if parts.len() < 3 { return Err(format!("bad target: {}", s)); }
        let arch = match parts[0] {
            "x86_64" | "amd64"  => Arch::X86_64,
            "aarch64" | "arm64" => Arch::AArch64,
            "riscv64"           => Arch::RiscV64,
            "mips64"            => Arch::Mips64,
            "mips"              => Arch::Mips,
            "arm" | "armv7"     => Arch::Arm,
            other => return Err(format!("unknown arch: {}", other)),
        };
        let os = match parts[parts.len()-2] {
            "linux" | "linux-gnu" | "linux-musl" => Os::Linux,
            "freestanding" | "elf"              => Os::Freestanding,
            "windows" | "win32"                 => Os::Windows,
            "darwin" | "apple"                  => Os::Macos,
            other => return Err(format!("unknown os: {}", other)),
        };
        let abi = match parts[parts.len()-1] {
            "gnu" | "gnuabi99" => Abi::Gnu,
            "musl"            => Abi::Musl,
            "eabi"            => Abi::Eabi,
            "eabihf"          => Abi::Eabihf,
            "msvc"            => Abi::Msvc,
            "darwin"          => Abi::Darwin,
            other => return Err(format!("unknown abi: {}", other)),
        };
        let vendor = if parts.len() == 4 { parts[1].to_string() } else { "unknown".into() };
        Ok(Self { arch, vendor, os, abi })
    }

    pub fn as_str(&self) -> String {
        format!("{}-{}-{}-{}",
            arch_name(self.arch), self.vendor, os_name(self.os), abi_name(self.abi))
    }
}

pub fn arch_name(a: Arch) -> &'static str {
    match a {
        Arch::X86_64 => "x86_64", Arch::AArch64 => "aarch64",
        Arch::RiscV64 => "riscv64", Arch::Mips64 => "mips64",
        Arch::Mips => "mips", Arch::Arm => "arm",
    }
}
pub fn os_name(o: Os) -> &'static str {
    match o { Os::Linux => "linux", Os::Freestanding => "freestanding",
              Os::Windows => "windows", Os::Macos => "darwin" }
}
pub fn abi_name(a: Abi) -> &'static str {
    match a { Abi::Gnu => "gnu", Abi::Musl => "musl", Abi::Eabi => "eabi",
              Abi::Eabihf => "eabihf", Abi::Msvc => "msvc", Abi::Darwin => "darwin" }
}

pub fn parse(args: &[String]) -> Result<Config, CliError> {
    let mut input: Option<PathBuf> = None;
    let mut output = PathBuf::from("a.out");
    let mut opt: u8 = 0;
    let mut std = StdMode::Gnu23;   // 第一性原理:默认 gnu23,跟现代 GCC 行为一致
    let mut target = TargetTriple::host();
    let mut freestanding = false;
    let mut include_paths: Vec<PathBuf> = vec![];
    let mut system_paths: Vec<PathBuf> = vec![];
    let mut defines: Vec<(String, Option<String>)> = vec![];
    let mut dump_ast = false;
    let mut verbose = false;
    let mut relocatable = false;
    let mut preprocess_only = false;
    let mut force_includes: Vec<String> = vec![];

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "-O0" => opt = 0,
            "-O1" => opt = 1,
            "-O2" => opt = 2,
            "-O3" => opt = 3,
            "-Os" => opt = 2,    // 体积优化合并到 O2
            "-o" => {
                i += 1;
                output = PathBuf::from(args.get(i)
                    .ok_or_else(|| CliError::new("missing arg after -o"))?);
            }
            "-c" | "-S" => { relocatable = true; } // -S: 汇编输出，DCC 直接生成 ELF
            "-E" => { preprocess_only = true; }
            "-pipe" => {} // GCC -pipe: 使用管道代替临时文件，DCC 忽略
            "-g" | "-g0" | "-g1" | "-g2" | "-g3" => {} // 调试信息: 忽略
            "-r" => { relocatable = true; } // -r: 可重定位链接
            // -L, -l, -T 等链接器选项
            "-L" | "-l" | "-T" => { i += 1; } // 跳过参数
            a if a.starts_with("-L") || a.starts_with("-l") || a.starts_with("-T") => {} // 内联形式
            // -Wl,* : 传递给链接器的选项
            a if a.starts_with("-Wl,") => {} // 忽略
            // -Wp,* : 传递给预处理器的选项
            a if a.starts_with("-Wp,") => {} // 忽略
            "-x" => {
                i += 1;
                args.get(i)
                    .ok_or_else(|| CliError::new("missing arg after -x"))?;
            }
            "--target" => {
                i += 1;
                target = TargetTriple::parse(args.get(i)
                    .ok_or_else(|| CliError::new("missing arg after --target"))?)
                    .map_err(|e| CliError::new(&e))?;
            }
            "-std=c89"   => std = StdMode::C89,
            "-std=c90"   => std = StdMode::C89,   // c90 = c89 别名
            "-std=iso9899:1990" => std = StdMode::C89,
            "-std=c99"   => std = StdMode::C99,
            "-std=c9x"   => std = StdMode::C99,   // c9x = c99 别名
            "-std=iso9899:1999" => std = StdMode::C99,
            "-std=c11"   => std = StdMode::C11,
            "-std=c1x"   => std = StdMode::C11,   // c1x = c11 别名
            "-std=iso9899:2011" => std = StdMode::C11,
            "-std=c17"   => std = StdMode::C17,
            "-std=c18"   => std = StdMode::C17,   // c18 = c17 别名
            "-std=iso9899:2017" => std = StdMode::C17,
            "-std=c23"   => std = StdMode::C23,
            "-std=c2x"   => std = StdMode::C23,   // c2x = c23 别名
            "-std=iso9899:2024" => std = StdMode::C23,
            "-std=gnu89" => std = StdMode::Gnu89,
            "-std=gnu90" => std = StdMode::Gnu89, // gnu90 = gnu89 别名
            "-std=gnu99" => std = StdMode::Gnu99,
            "-std=gnu11" => std = StdMode::Gnu11,
            "-std=gnu17" => std = StdMode::Gnu17,
            "-std=gnu18" => std = StdMode::Gnu17, // gnu18 = gnu17 别名
            "-std=gnu23" => std = StdMode::Gnu23,
            "-ffreestanding" => freestanding = true,
            "-I" => {
                i += 1;
                include_paths.push(PathBuf::from(args.get(i)
                    .ok_or_else(|| CliError::new("missing arg after -I"))?));
            }
            a if a.starts_with("-I") => {
                include_paths.push(PathBuf::from(&a[2..]));
            }
            "-D" => {
                i += 1;
                let def = args.get(i)
                    .ok_or_else(|| CliError::new("missing arg after -D"))?;
                if let Some(eq) = def.find('=') {
                    defines.push((def[..eq].to_string(), Some(def[eq+1..].to_string())));
                } else {
                    defines.push((def.clone(), Some("1".to_string())));
                }
            }
            a if a.starts_with("-D") => {
                let def = &a[2..];
                if let Some(eq) = def.find('=') {
                    defines.push((def[..eq].to_string(), Some(def[eq+1..].to_string())));
                } else {
                    defines.push((def.to_string(), Some("1".to_string())));
                }
            }
            "-fno-builtin"   => {}
            // -Wa,* : 传递给汇编器的选项
            a if a.starts_with("-Wa,") => {
                let as_opts = &a[4..];
                // 特殊处理 -Wa,--version : 调用系统汇编器并输出版本信息
                if as_opts.contains("--version") {
                    if let Ok(out) = std::process::Command::new("as")
                        .arg("--version")
                        .output() {
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
            // Linux 内核编译常用的 GCC flags - 忽略但接受
            "-mno-red-zone" | "-fno-pic" | "-fno-stack-protector" | "-fno-strict-aliasing"
            | "-fno-common" | "-fno-delete-null-pointer-checks" | "-fno-strict-overflow"
            | "-fno-merge-all-constants" | "-fno-omit-frame-pointer"
            | "-mno-sse" | "-mno-mmx" | "-mno-sse2" | "-mno-80387" | "-mno-fp-ret-in-387"
            | "-mno-3dnow" | "-mno-3dnowa" | "-nostdinc" | "-nostdlib" | "-nodefaultlibs"
            | "-static" | "-shared" | "-pie" | "-no-pie"
            | "-pg" | "-pthread" => {}
            "-P" => {} // 抑制 #line 指令，DCC 预处理器本身就不输出它们
            // -W* 警告选项 - 忽略
            a if a.starts_with("-W") => {}
            // -m* 机器选项 - 忽略
            a if a.starts_with("-m") => {}
            // -f* 其他选项 - 忽略
            a if a.starts_with("-f") => {}
            // -std=* 已经处理过了
            // -include, -isystem 等
            "-include" => {
                i += 1;
                let f = args.get(i)
                    .ok_or_else(|| CliError::new("missing arg after -include"))?;
                force_includes.push(f.clone());
            }
            "-isystem" => {
                i += 1;
                system_paths.push(PathBuf::from(args.get(i)
                    .ok_or_else(|| CliError::new("missing arg after -isystem"))?));
            }
            a if a.starts_with("-include") => {
                force_includes.push(a[8..].to_string());
            }
            a if a.starts_with("-isystem") => { system_paths.push(PathBuf::from(&a[8..])); }
            // -MF, -MP, -MD 等依赖生成选项
            "-MF" | "-MT" | "-MQ" | "-MP" | "-MD" | "-MMD" => { if a != "-MP" && a != "-MD" && a != "-MMD" { i += 1; } }
            "-dump-ast"  => dump_ast = true,
            "-v" | "--verbose" => verbose = true,
            // -print-file-name=<file>: GCC 用于查找库/插件/头文件路径
            a if a.starts_with("-print-file-name=") => {
                let filename = &a[17..];
                // 对于 include, 返回 GCC 的 include 目录
                if filename == "include" {
                    if let Ok(out) = std::process::Command::new("gcc")
                        .arg("-print-file-name=include").output() {
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
                println!("");
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
                input = Some(PathBuf::from("-"));
            }
            a if a.starts_with('-') => return Err(CliError::new(&format!("unknown flag: {}", a))),
            _ => {
                // 多文件支持: 只编译第一个文件，其余忽略 (内核构建通常一次一个文件)
                if input.is_some() && !relocatable { return Err(CliError::new("only one input file supported in M0")); }
                if input.is_none() {
                    input = Some(PathBuf::from(a));
                }
            }
        }
        i += 1;
    }
    // -E 模式允许没有输入文件 (从 stdin 读取)
    if !preprocess_only {
        let input = input.ok_or_else(|| CliError::new("no input file"))?;
        // -c 模式默认输出名为源文件名去后缀加 .o
        if relocatable && output == PathBuf::from("a.out") {
            let stem = input.file_stem().unwrap_or_default().to_string_lossy();
            output = PathBuf::from(format!("{}.o", stem));
        }
        Ok(Config { input, output, opt, std, target, freestanding, include_paths, system_paths, defines, dump_ast, verbose, relocatable, preprocess_only, force_includes })
    } else {
        let input = input.unwrap_or_else(|| PathBuf::from("-"));
        Ok(Config { input, output, opt, std, target, freestanding, include_paths, system_paths, defines, dump_ast, verbose, relocatable, preprocess_only, force_includes })
    }
}

fn print_help() {
    println!("dcc - suckless C compiler");
    println!();
    println!("USAGE:");
    println!("    dcc [options] <input.c>");
    println!();
    println!("OPTIONS:");
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
pub struct CliError { pub msg: String }
impl CliError {
    pub fn new(m: &str) -> Self { Self { msg: m.into() } }
    pub fn print(&self) {
        eprintln!("dcc: error: {}", self.msg);
    }
}
