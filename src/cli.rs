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
    pub dump_ast: bool,
    pub verbose:  bool,
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
        // 兜底:假装 x86_64 linux
        Self { arch: Arch::X86_64, vendor: "unknown".into(),
               os: Os::Linux, abi: Abi::Gnu }
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
    let mut dump_ast = false;
    let mut verbose = false;

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
            "-c" => { /* 编译不链接 - M0 暂忽略 */ }
            "--target" => {
                i += 1;
                target = TargetTriple::parse(args.get(i)
                    .ok_or_else(|| CliError::new("missing arg after --target"))?)
                    .map_err(|e| CliError::new(&e))?;
            }
            "-std=c89"   => std = StdMode::C89,
            "-std=c99"   => std = StdMode::C99,
            "-std=c11"   => std = StdMode::C11,
            "-std=c17"   => std = StdMode::C17,
            "-std=c23"   => std = StdMode::C23,
            "-std=gnu89" => std = StdMode::Gnu89,
            "-std=gnu99" => std = StdMode::Gnu99,
            "-std=gnu11" => std = StdMode::Gnu11,
            "-std=gnu17" => std = StdMode::Gnu17,
            "-std=gnu23" => std = StdMode::Gnu23,
            "-ffreestanding" => freestanding = true,
            "-fno-builtin"   => {}
            "-dump-ast"  => dump_ast = true,
            "-v" | "--verbose" => verbose = true,
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
            a if a.starts_with('-') => return Err(CliError::new(&format!("unknown flag: {}", a))),
            _ => {
                if input.is_some() { return Err(CliError::new("only one input file supported in M0")); }
                input = Some(PathBuf::from(a));
            }
        }
        i += 1;
    }
    let input = input.ok_or_else(|| CliError::new("no input file"))?;
    Ok(Config { input, output, opt, std, target, freestanding, dump_ast, verbose })
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
