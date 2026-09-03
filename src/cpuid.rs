// src/cpuid.rs - CPU 指令集特性动态检测
//
// 第一性原理: 编译器应知道当前 CPU 能跑什么指令, 而不是硬编码默认值.
// 通过 x86 CPUID 指令在运行时检测 AVX/AVX2/AVX-512/CET 等特性.
// 非 x86 平台返回空特性集 (由 Target::resolve 按架构决定默认值).

/// x86 CPU 指令集特性集合.
///
/// 每个字段对应一个 CPUID 可检测的硬件特性.
/// `detect()` 在 x86_64 上通过 CPUID 指令填充;
/// 非 x86 平台全部为 false.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuFeatures {
    // --- CPUID leaf 1, ECX ---
    pub sse3: bool,
    pub pclmulqdq: bool,
    pub ssse3: bool,
    pub sse4_1: bool,
    pub sse4_2: bool,
    pub popcnt: bool,
    pub aes: bool,
    pub avx: bool,
    pub f16c: bool,
    pub fma: bool,

    // --- CPUID leaf 7 sub-leaf 0, EBX ---
    pub bmi1: bool,
    pub avx2: bool,
    pub bmi2: bool,
    pub erms: bool,
    pub avx512f: bool,
    pub avx512dq: bool,
    pub avx512_ifma: bool,
    pub avx512pf: bool,
    pub avx512er: bool,
    pub avx512cd: bool,
    pub avx512bw: bool,
    pub avx512vl: bool,

    // --- CPUID leaf 7 sub-leaf 0, ECX ---
    pub avx512_vbmi: bool,
    pub avx512_vbmi2: bool,
    pub avx512_vnni: bool,
    pub avx512_bitalg: bool,
    pub avx512_vpopcntdq: bool,

    // --- AVX-VNNI (VEX form, non-512) ---
    // CPUID leaf 7 sub-leaf 0, ECX[bit 4]
    pub avx_vnni: bool,

    // --- CPUID leaf 7 sub-leaf 0, EDX ---
    pub avx512_4vnniw: bool,
    pub avx512_4fmaps: bool,

    // --- CET (Control-flow Enforcement Technology) ---
    pub cet_ss: bool,     // CPUID leaf 7, ECX[bit 7]: CET_SS
    pub cet_ibt: bool,    // CPUID leaf 7, EDX[bit 20]: CET_IBT

    // --- AVX10 (CPUID leaf 7 sub-leaf 1) ---
    pub avx10_1: bool,    // AVX10.1 support: leaf 7 sub-1 EDX[bit 19]
    pub avx10_256: bool,  // AVX10.1-256: leaf 7 sub-1 EBX[bit 17]
    pub avx10_512: bool,  // AVX10.1-512: leaf 7 sub-1 EBX[bit 18]

    // --- APX Foundation ---
    pub apx_f: bool,      // CPUID leaf 7 sub-1 EDX[bit 21]

    // --- 其他 ---
    pub movbe: bool,      // CPUID leaf 1, ECX[bit 22]
    pub xsave: bool,      // CPUID leaf 1, ECX[bit 26]
    pub osxsave: bool,    // CPUID leaf 1, ECX[bit 27]
}

/// CLI 显式指定的 CPU 特性覆盖.
///
/// 每个字段为 `Option<bool>`:
/// - `None` = 用户未指定, 保留 CPUID 检测结果
/// - `Some(true)` = 用户显式启用 (-mavx)
/// - `Some(false)` = 用户显式禁用 (-mno-avx)
#[derive(Debug, Clone, Copy, Default)]
pub struct CpuFeatureOverrides {
    pub avx: Option<bool>,
    pub avx2: Option<bool>,
    pub avx512f: Option<bool>,
    pub avx512bw: Option<bool>,
    pub avx512vl: Option<bool>,
    pub avx512dq: Option<bool>,
    pub fma: Option<bool>,
    pub bmi1: Option<bool>,
    pub bmi2: Option<bool>,
    pub cet_ss: Option<bool>,
    pub cet_ibt: Option<bool>,
    pub avx_vnni: Option<bool>,
    pub avx10_1: Option<bool>,
    pub avx10_512: Option<bool>,
    pub apx_f: Option<bool>,
}

impl Default for CpuFeatures {
    fn default() -> Self {
        Self {
            sse3: false,
            pclmulqdq: false,
            ssse3: false,
            sse4_1: false,
            sse4_2: false,
            popcnt: false,
            aes: false,
            avx: false,
            f16c: false,
            fma: false,
            bmi1: false,
            avx2: false,
            bmi2: false,
            erms: false,
            avx512f: false,
            avx512dq: false,
            avx512_ifma: false,
            avx512pf: false,
            avx512er: false,
            avx512cd: false,
            avx512bw: false,
            avx512vl: false,
            avx512_vbmi: false,
            avx512_vbmi2: false,
            avx512_vnni: false,
            avx_vnni: false,
            avx512_bitalg: false,
            avx512_vpopcntdq: false,
            avx512_4vnniw: false,
            avx512_4fmaps: false,
            cet_ss: false,
            cet_ibt: false,
            avx10_1: false,
            avx10_256: false,
            avx10_512: false,
            apx_f: false,
            movbe: false,
            xsave: false,
            osxsave: false,
        }
    }
}

impl CpuFeatures {
    /// 检测当前 CPU 的指令集特性.
    ///
    /// 在 x86_64 上通过 CPUID 指令检测; 非 x86_64 平台返回全 false.
    /// AVX/AVX-512 特性还需要检查 OS 是否启用了 XSAVE 支持
    /// (通过 XCR0 寄存器), 否则即使 CPU 支持也不能使用.
    #[allow(clippy::let_and_return)]
    pub fn detect() -> Self {
        #[cfg(all(
            any(target_arch = "x86", target_arch = "x86_64"),
            not(miri)
        ))]
        {
            return detect_x86();
        }
        #[cfg(not(all(
            any(target_arch = "x86", target_arch = "x86_64"),
            not(miri)
        )))]
        {
            Self::default()
        }
    }

    /// SSE 是否可用 (SSE + SSE2, x86_64 始终为 true).
    pub fn has_sse(&self) -> bool {
        // x86_64 保证 SSE/SSE2 可用
        true
    }

    /// AVX 系列是否可用 (AVX 蕴含 SSE4.2 等).
    pub fn has_avx(&self) -> bool {
        self.avx
    }

    /// AVX2 是否可用.
    pub fn has_avx2(&self) -> bool {
        self.avx2
    }

    /// AVX-VNNI (VEX form, non-512) 是否可用.
    pub fn has_avx_vnni(&self) -> bool {
        self.avx_vnni
    }

    /// AVX-512 Foundation 是否可用.
    pub fn has_avx512f(&self) -> bool {
        self.avx512f
    }

    /// AVX10 是否可用.
    pub fn has_avx10(&self) -> bool {
        self.avx10_1
    }

    /// APX Foundation 是否可用.
    pub fn has_apx_f(&self) -> bool {
        self.apx_f
    }

    /// CET Shadow Stack 是否可用.
    pub fn has_cet_ss(&self) -> bool {
        self.cet_ss
    }

    /// CET Indirect Branch Tracking 是否可用.
    pub fn has_cet_ibt(&self) -> bool {
        self.cet_ibt
    }

    /// 生成 GCC 兼容的 -march=native 风格特性字符串列表.
    ///
    /// 用于诊断输出或传递给后端.
    pub fn to_feature_strings(&self) -> Vec<&'static str> {
        let mut feats = Vec::new();
        if self.sse3 { feats.push("sse3"); }
        if self.pclmulqdq { feats.push("pclmul"); }
        if self.ssse3 { feats.push("ssse3"); }
        if self.sse4_1 { feats.push("sse4.1"); }
        if self.sse4_2 { feats.push("sse4.2"); }
        if self.popcnt { feats.push("popcnt"); }
        if self.aes { feats.push("aes"); }
        if self.avx { feats.push("avx"); }
        if self.f16c { feats.push("f16c"); }
        if self.fma { feats.push("fma"); }
        if self.bmi1 { feats.push("bmi1"); }
        if self.avx2 { feats.push("avx2"); }
        if self.bmi2 { feats.push("bmi2"); }
        if self.erms { feats.push("erms"); }
        if self.avx512f { feats.push("avx512f"); }
        if self.avx512dq { feats.push("avx512dq"); }
        if self.avx512_ifma { feats.push("avx512ifma"); }
        if self.avx512pf { feats.push("avx512pf"); }
        if self.avx512er { feats.push("avx512er"); }
        if self.avx512cd { feats.push("avx512cd"); }
        if self.avx512bw { feats.push("avx512bw"); }
        if self.avx512vl { feats.push("avx512vl"); }
        if self.avx512_vbmi { feats.push("avx512vbmi"); }
        if self.avx512_vbmi2 { feats.push("avx512vbmi2"); }
        if self.avx512_vnni { feats.push("avx512vnni"); }
        if self.avx_vnni { feats.push("avxvnni"); }
        if self.avx512_bitalg { feats.push("avx512bitalg"); }
        if self.avx512_vpopcntdq { feats.push("avx512vpopcntdq"); }
        if self.avx512_4vnniw { feats.push("avx5124vnniw"); }
        if self.avx512_4fmaps { feats.push("avx5124fmaps"); }
        if self.cet_ss { feats.push("cet-ss"); }
        if self.cet_ibt { feats.push("ibt"); }
        if self.avx10_1 { feats.push("avx10.1"); }
        if self.avx10_256 { feats.push("avx10.1-256"); }
        if self.avx10_512 { feats.push("avx10.1-512"); }
        if self.apx_f { feats.push("apxf"); }
        if self.movbe { feats.push("movbe"); }
        feats
    }

    /// 检测非 x86 架构的 CPU 特性.
    ///
    /// 委托给 `NonX86Features::detect()` 并将结果转换为统一结构.
    pub fn detect_non_x86(arch: crate::cli::Arch) -> NonX86Features {
        NonX86Features::detect(arch)
    }
}

/// x86/x86_64 CPUID 检测实现.
#[cfg(all(any(target_arch = "x86", target_arch = "x86_64"), not(miri)))]
fn detect_x86() -> CpuFeatures {
    use std::arch::x86_64::__cpuid;

    let mut feats = CpuFeatures::default();

    // Leaf 0: 获取最大基本 leaf
    let leaf0 = __cpuid(0);
    let max_leaf = leaf0.eax;

    if max_leaf < 1 {
        return feats;
    }

    // Leaf 1: 基础特性
    let leaf1 = __cpuid(1);
    let ecx1 = leaf1.ecx;
    let _edx1 = leaf1.edx;

    feats.sse3 = bit(ecx1, 0);
    feats.pclmulqdq = bit(ecx1, 1);
    feats.ssse3 = bit(ecx1, 9);
    feats.sse4_1 = bit(ecx1, 19);
    feats.sse4_2 = bit(ecx1, 20);
    feats.movbe = bit(ecx1, 22);
    feats.popcnt = bit(ecx1, 23);
    feats.aes = bit(ecx1, 25);
    feats.xsave = bit(ecx1, 26);
    feats.osxsave = bit(ecx1, 27);
    feats.f16c = bit(ecx1, 29);

    // AVX 需要: CPU 支持 + OS 已启用 XSAVE (XCR0[2:1] == 0b11)
    let cpu_has_avx = bit(ecx1, 28);
    let os_avx_ready = feats.osxsave && check_xcr0_avx();
    feats.avx = cpu_has_avx && os_avx_ready;
    feats.fma = bit(ecx1, 12) && feats.avx;

    // Leaf 7 sub-leaf 0: 扩展特性
    if max_leaf >= 7 {
        let leaf7 = __cpuid(7);
        let ebx7 = leaf7.ebx;
        let ecx7 = leaf7.ecx;
        let edx7 = leaf7.edx;

        feats.bmi1 = bit(ebx7, 3);
        feats.avx2 = bit(ebx7, 5) && feats.avx;
        feats.bmi2 = bit(ebx7, 8);
        feats.erms = bit(ebx7, 9);

        // AVX-512 需要 OS 启用 AVX-512 (XCR0[7:5] == 0b111)
        let os_avx512_ready = os_avx_ready && check_xcr0_avx512();
        feats.avx512f = bit(ebx7, 16) && feats.avx && os_avx512_ready;
        feats.avx512dq = bit(ebx7, 17) && feats.avx512f;
        feats.avx512_ifma = bit(ebx7, 21) && feats.avx512f;
        feats.avx512pf = bit(ebx7, 26) && feats.avx512f;
        feats.avx512er = bit(ebx7, 27) && feats.avx512f;
        feats.avx512cd = bit(ebx7, 28) && feats.avx512f;
        feats.avx512bw = bit(ebx7, 30) && feats.avx512f;
        feats.avx512vl = bit(ebx7, 31) && feats.avx512f;

        // CET
        feats.cet_ss = bit(ecx7, 7);
        feats.cet_ibt = bit(edx7, 20);

        // AVX-512 扩展 (ECX)
        feats.avx512_vbmi = bit(ecx7, 1) && feats.avx512f;
        feats.avx512_vbmi2 = bit(ecx7, 6) && feats.avx512f;
        feats.avx512_vnni = bit(ecx7, 11) && feats.avx512f;
        // AVX-VNNI (VEX form): ECX[bit 4], requires AVX (not AVX-512)
        feats.avx_vnni = bit(ecx7, 4) && feats.avx;
        feats.avx512_bitalg = bit(ecx7, 12) && feats.avx512f;
        feats.avx512_vpopcntdq = bit(ecx7, 14) && feats.avx512f;

        // AVX-512 扩展 (EDX)
        feats.avx512_4vnniw = bit(edx7, 2) && feats.avx512f;
        feats.avx512_4fmaps = bit(edx7, 3) && feats.avx512f;

        // Leaf 7 sub-leaf 1: AVX10 / APX
        // __cpuid_count(eax=7, ecx=1) for sub-leaf 1
        let max_subleaf = leaf7.eax; // EAX of leaf 7 = max sub-leaf
        if max_subleaf >= 1 {
            let leaf7_1 = std::arch::x86_64::__cpuid_count(7, 1);
            let ebx7_1 = leaf7_1.ebx;
            let edx7_1 = leaf7_1.edx;

            // AVX10.1 support: EDX[bit 19]
            let cpu_has_avx10 = bit(edx7_1, 19);
            feats.avx10_1 = cpu_has_avx10 && feats.avx;
            // AVX10.1-256: EBX[bit 17], requires AVX2
            feats.avx10_256 = bit(ebx7_1, 17) && feats.avx10_1 && feats.avx2;
            // AVX10.1-512: EBX[bit 18], requires AVX-512F
            feats.avx10_512 =
                bit(ebx7_1, 18) && feats.avx10_1 && feats.avx512f;

            // APX Foundation: EDX[bit 21]
            feats.apx_f = bit(edx7_1, 21);
        }
    }

    feats
}

/// 检查 XCR0 寄存器是否启用了 AVX (XCR0[2:1] == 0b11).
#[cfg(all(any(target_arch = "x86", target_arch = "x86_64"), not(miri)))]
fn check_xcr0_avx() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        let xcr0 = unsafe { std::arch::x86_64::_xgetbv(0) };
        // XCR0[1] = SSE state, XCR0[2] = AVX state
        (xcr0 & 0x6) == 0x6
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

/// 检查 XCR0 寄存器是否启用了 AVX-512 (XCR0[7:5] == 0b111).
#[cfg(all(any(target_arch = "x86", target_arch = "x86_64"), not(miri)))]
fn check_xcr0_avx512() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        let xcr0 = unsafe { std::arch::x86_64::_xgetbv(0) };
        // XCR0[5] = opmask, XCR0[6] = ZMM_Hi256, XCR0[7] = Hi16_ZMM
        (xcr0 & 0xE0) == 0xE0
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

/// 提取 u32 中的第 n 位.
#[inline]
fn bit(val: u32, n: u32) -> bool {
    (val >> n) & 1 != 0
}

/// 非 x86 架构 CPU 特性集合.
///
/// 通过读取 /proc/cpuinfo 检测 ARM/AArch64/RISC-V/MIPS 的硬件特性.
/// 仅在目标架构与宿主架构匹配时才有意义.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NonX86Features {
    // AArch64
    pub neon: bool,
    pub sve: bool,
    pub sve2: bool,
    pub lse: bool,
    pub rdm: bool,
    pub dp: bool,
    pub crc32: bool,
    // ARM
    pub thumb2: bool,
    pub arm_neon: bool,
    pub vfpv4: bool,
    // RISC-V
    pub rv_m: bool,
    pub rv_a: bool,
    pub rv_f: bool,
    pub rv_d: bool,
    pub rv_c: bool,
    pub rv_b: bool,
    pub rv_v: bool,
    // MIPS
    pub msa: bool,
    pub mips_dsp: bool,
}

impl NonX86Features {
    /// 检测当前 CPU 的非 x86 架构特性.
    ///
    /// 仅在对应架构的宿主上运行时才返回真实检测结果;
    /// 其他情况返回全 false.
    pub fn detect(arch: crate::cli::Arch) -> Self {
        match arch {
            crate::cli::Arch::AArch64 => detect_aarch64(),
            crate::cli::Arch::Arm => detect_arm(),
            crate::cli::Arch::RiscV64 | crate::cli::Arch::RiscV32 => detect_riscv(),
            crate::cli::Arch::Mips64 | crate::cli::Arch::Mips => detect_mips(),
            _ => Self::default(),
        }
    }
}

/// 读取 /proc/cpuinfo 内容. 失败时返回 None.
fn read_proc_cpuinfo() -> Option<String> {
    std::fs::read_to_string("/proc/cpuinfo").ok()
}

/// AArch64 特性检测: 读取 /proc/cpuinfo 的 Features 行.
///
/// 已知特性关键字:
/// - `asimd` -> NEON (Advanced SIMD)
/// - `sve` -> SVE
/// - `sve2` -> SVE2
/// - `atomics` -> LSE (ARMv8.1)
/// - `asimdrdm` -> RDM
/// - `asimddp` -> Dot Product
/// - `crc32` -> CRC32
#[cfg(target_arch = "aarch64")]
fn detect_aarch64() -> NonX86Features {
    let mut f = NonX86Features::default();
    if let Some(cpuinfo) = read_proc_cpuinfo() {
        for line in cpuinfo.lines() {
            if line.starts_with("Features") {
                let features = line.split(':').nth(1).unwrap_or("");
                let feats: Vec<&str> = features.split_whitespace().collect();
                f.neon = feats.contains(&"asimd");
                f.sve = feats.contains(&"sve");
                f.sve2 = feats.contains(&"sve2");
                f.lse = feats.contains(&"atomics");
                f.rdm = feats.contains(&"asimdrdm");
                f.dp = feats.contains(&"asimddp");
                f.crc32 = feats.contains(&"crc32");
                break;
            }
        }
    }
    f
}

#[cfg(not(target_arch = "aarch64"))]
fn detect_aarch64() -> NonX86Features {
    NonX86Features::default()
}

/// ARM (32-bit) 特性检测: 读取 /proc/cpuinfo 的 Features 行.
///
/// 已知特性关键字:
/// - `thumb` -> Thumb (Thumb-2 通过 `idiva`/`idivt` 或 `arch` 行判断)
/// - `neon` -> NEON
/// - `vfpv4` -> VFPv4
#[cfg(target_arch = "arm")]
fn detect_arm() -> NonX86Features {
    let mut f = NonX86Features::default();
    if let Some(cpuinfo) = read_proc_cpuinfo() {
        for line in cpuinfo.lines() {
            if line.starts_with("Features") {
                let features = line.split(':').nth(1).unwrap_or("");
                let feats: Vec<&str> = features.split_whitespace().collect();
                f.thumb2 = feats.contains(&"thumb") || feats.contains(&"edsp");
                f.arm_neon = feats.contains(&"neon");
                f.vfpv4 = feats.contains(&"vfpv4") || feats.contains(&"vfpv3");
                break;
            }
        }
    }
    f
}

#[cfg(not(target_arch = "arm"))]
fn detect_arm() -> NonX86Features {
    NonX86Features::default()
}

/// RISC-V 特性检测: 读取 /proc/cpuinfo 的 isa 行.
///
/// 解析 ISA 字符串中的扩展字母:
/// - M: 整数乘除
/// - A: 原子指令
/// - F: 单精度浮点
/// - D: 双精度浮点
/// - C: 压缩指令
/// - B: 位操作 (Zbb 等)
/// - V: 向量指令
#[cfg(any(target_arch = "riscv64", target_arch = "riscv32"))]
fn detect_riscv() -> NonX86Features {
    let mut f = NonX86Features::default();
    if let Some(cpuinfo) = read_proc_cpuinfo() {
        for line in cpuinfo.lines() {
            if line.starts_with("isa") {
                let isa = line.split(':').nth(1).unwrap_or("").trim();
                // ISA 字符串格式: rv64imafdcvh...
                // 跳过 "rv32"/"rv64" 前缀后解析扩展字母
                let exts = if isa.starts_with("rv64") {
                    &isa[4..]
                } else if isa.starts_with("rv32") {
                    &isa[4..]
                } else {
                    isa
                };
                // 只检查基本扩展字母 (在 Z/x 等后缀扩展之前)
                for ch in exts.chars() {
                    match ch {
                        'm' => f.rv_m = true,
                        'a' => f.rv_a = true,
                        'f' => f.rv_f = true,
                        'd' => f.rv_d = true,
                        'c' => f.rv_c = true,
                        'v' => f.rv_v = true,
                        // 遇到大写 Z 或 x 表示自定义/标准扩展, 停止基本扫描
                        'Z' | 'z' | 'x' | 'X' | 'S' | 's' => break,
                        _ => {}
                    }
                }
                // 检测 B 扩展 (可能以 Zba/Zbb/Zbc 形式出现)
                f.rv_b = exts.contains('b')
                    || exts.contains("zbb")
                    || exts.contains("zba")
                    || exts.contains("zbc");
                break;
            }
        }
    }
    f
}

#[cfg(not(any(target_arch = "riscv64", target_arch = "riscv32")))]
fn detect_riscv() -> NonX86Features {
    NonX86Features::default()
}

/// MIPS 特性检测: 读取 /proc/cpuinfo 的 ASEs 行.
///
/// 已知 ASE 关键字:
/// - `msa` -> MIPS SIMD Architecture
/// - `dsp` / `dspr2` -> DSP ASE
#[cfg(any(target_arch = "mips", target_arch = "mips64"))]
fn detect_mips() -> NonX86Features {
    let mut f = NonX86Features::default();
    if let Some(cpuinfo) = read_proc_cpuinfo() {
        for line in cpuinfo.lines() {
            let lower = line.to_lowercase();
            if lower.starts_with("ases implemented") {
                let ases = line.split(':').nth(1).unwrap_or("");
                let ase_list: Vec<&str> = ases.split_whitespace().collect();
                f.msa = ase_list.iter().any(|a| a.eq_ignore_ascii_case("msa"));
                f.mips_dsp = ase_list.iter().any(|a| {
                    a.eq_ignore_ascii_case("dsp") || a.eq_ignore_ascii_case("dspr2")
                });
                break;
            }
        }
    }
    f
}

#[cfg(not(any(target_arch = "mips", target_arch = "mips64")))]
fn detect_mips() -> NonX86Features {
    NonX86Features::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_features_all_false() {
        let f = CpuFeatures::default();
        assert!(!f.avx);
        assert!(!f.avx2);
        assert!(!f.avx512f);
        assert!(!f.cet_ss);
        assert!(!f.cet_ibt);
        assert!(!f.sse3);
        assert!(!f.fma);
        assert!(!f.avx_vnni);
    }

    #[test]
    fn detect_returns_valid_features() {
        let f = CpuFeatures::detect();
        // 在 x86_64 上, SSE 应该始终可用
        assert!(f.has_sse());
        // has_avx 的结果取决于实际 CPU, 但不应 panic
        let _ = f.has_avx();
        let _ = f.has_avx2();
        let _ = f.has_avx512f();
    }

    #[test]
    fn avx2_implies_avx() {
        let f = CpuFeatures::detect();
        if f.has_avx2() {
            assert!(f.has_avx(), "AVX2 implies AVX support");
        }
    }

    #[test]
    fn avx512_implies_avx() {
        let f = CpuFeatures::detect();
        if f.has_avx512f() {
            assert!(f.has_avx(), "AVX-512 implies AVX support");
        }
    }

    #[test]
    fn feature_strings_consistent() {
        let f = CpuFeatures::detect();
        let strs = f.to_feature_strings();
        // 如果 AVX2 被检测到, 字符串列表应包含 "avx2"
        if f.avx2 {
            assert!(strs.contains(&"avx2"));
        }
        // 如果 AVX 未检测到, 字符串列表不应包含 "avx"
        if !f.avx {
            assert!(!strs.contains(&"avx"));
        }
    }

    #[test]
    fn bit_extraction_works() {
        assert!(bit(1, 0));
        assert!(!bit(1, 1));
        assert!(bit(0b1000, 3));
        assert!(!bit(0b1000, 2));
        assert!(bit(0xFFFFFFFF, 31));
    }

    #[test]
    fn avx_vnni_default_false() {
        let f = CpuFeatures::default();
        assert!(!f.avx_vnni);
        assert!(!f.has_avx_vnni());
    }

    #[test]
    fn avx_vnni_override() {
        let mut t = crate::target::Target::resolve(
            &crate::cli::TargetTriple {
                arch: crate::cli::Arch::X86_64,
                vendor: "unknown".into(),
                os: crate::cli::Os::Linux,
                abi: crate::cli::Abi::Gnu,
            },
        );
        let overrides = CpuFeatureOverrides {
            avx: Some(true),
            avx_vnni: Some(true),
            ..Default::default()
        };
        t.apply_cpu_feature_overrides(&overrides);
        assert!(t.cpu_features.avx_vnni);
    }

    #[test]
    fn avx10_default_false() {
        let f = CpuFeatures::default();
        assert!(!f.avx10_1);
        assert!(!f.avx10_256);
        assert!(!f.avx10_512);
        assert!(!f.has_avx10());
    }

    #[test]
    fn apx_f_default_false() {
        let f = CpuFeatures::default();
        assert!(!f.apx_f);
        assert!(!f.has_apx_f());
    }

    #[test]
    fn avx10_override() {
        let mut t = crate::target::Target::resolve(
            &crate::cli::TargetTriple {
                arch: crate::cli::Arch::X86_64,
                vendor: "unknown".into(),
                os: crate::cli::Os::Linux,
                abi: crate::cli::Abi::Gnu,
            },
        );
        let overrides = CpuFeatureOverrides {
            avx: Some(true),
            avx10_1: Some(true),
            ..Default::default()
        };
        t.apply_cpu_feature_overrides(&overrides);
        assert!(t.cpu_features.avx10_1);
    }

    #[cfg(all(any(target_arch = "x86", target_arch = "x86_64"), not(miri)))]
    #[test]
    fn x86_detect_has_sse3_at_minimum() {
        // 几乎所有 x86_64 CPU 都支持 SSE3 (2004 年后)
        let f = detect_x86();
        // 不强制断言, 只打印诊断信息
        eprintln!(
            "CPU features: sse3={} ssse3={} sse4.1={} sse4.2={} \
             avx={} avx2={} avx512f={} cet_ss={} cet_ibt={}",
            f.sse3, f.ssse3, f.sse4_1, f.sse4_2,
            f.avx, f.avx2, f.avx512f, f.cet_ss, f.cet_ibt
        );
    }
}
