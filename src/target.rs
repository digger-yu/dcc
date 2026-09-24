// src/target.rs - 目标平台抽象
// 第一性原理: 后端是"数据驱动"——不同架构共享一套 IR,只换指令编码表。
// suckless 准则: 不为每种架构写一个 codegen.rs;而是 codegen 跑在 target 描述之上。

use crate::cli::{Arch, Os, StackGuardReg, TargetTriple};
use crate::cpuid::{CpuFeatureOverrides, CpuFeatures};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endian {
    Little,
    Big,
}

/// 目标的过程调用约定 (callee-saved 寄存器集合的名字)。
///
/// ⚠ 当前**无任何生产消费者**: `callee_save` 字段的 7 个读取点全部落在本文件
/// `#[cfg(test)]` 的断言里, 各架构的保存/恢复序列是各自后端硬写死的 (x86_64 用
/// `callee_saved_pool`, ARM/ARM64/MIPS 各自写自己的序言)。所以这个枚举现在只是一句
/// 可被测试锁定的“声明”。要真正驱动寄存器分配, 必须先把 MIPS O32/N64 与 ARM AAPCS 的
/// callee-saved 表接进各自后端, 否则改它不会改变任何产物字节。
///
/// 为什么 MIPS 不共用 `AAPCS`: AAPCS 是 **ARM** Architecture Procedure Call Standard。
/// 之前 MIPS/MIPS64 被标成 AAPCS 是纯粹的标签说谎——两个 MIPS ABI 的 callee-saved
/// 集合与 ARM 完全不同, 现在各自有一个能说实话的名字 (Arm 仍保留 `AAPCS`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::upper_case_acronyms)]
pub enum CalleeSave {
    SysV,
    Win64,
    AAPCS64,
    AAPCS,
    /// MIPS O32 (32 位 ABI): $s0-$s7 / $fp / $gp / $sp / $ra 及 $f20-$f31
    MipsO32,
    /// MIPS N64 (mips64-linux-gnuabi64, LP64): $s0-$s5 / $fp / $gp / $sp / $ra 及 $f20-$f31
    MipsN64,
}

#[derive(Clone)]
pub struct Target {
    pub triple: TargetTriple,
    pub arch: Arch,
    pub ptr_size: u8,   // 字节,通常 4 或 8
    pub int_size: u8,   // int 字节
    pub long_size: u8,  // long 字节
    pub llong_size: u8, // long long 字节
    pub endian: Endian,
    pub callee_save: CalleeSave,
    /// 默认对齐表 (字节)
    ///
    /// 每个架构的 ABI 规定了基本类型的自然对齐要求。
    /// 这些值用于 struct/union 布局计算和 `__alignof__` 求值。
    pub ptr_align: u8,       // 指针类型对齐
    pub int_align: u8,       // int 对齐 (通常 = int_size)
    pub long_align: u8,      // long 对齐
    pub llong_align: u8,     // long long 对齐
    pub double_align: u8,    // double 对齐
    pub long_double_align: u8, // long double 对齐
    pub is_linux: bool,
    pub red_zone: bool,
    pub frame_pointer: bool,
    pub sse: bool,
    pub kernel_code_model: bool,
    pub auto_var_init_zero: bool,
    pub stack_protector: bool,
    pub unwind_tables: bool,
    pub function_return_thunk_extern: bool,
    pub indirect_branch_thunk_extern: bool,
    pub patchable_function_entry: Option<(usize, usize)>,
    /// ftrace instrumentation mode
    pub ftrace_mode: crate::cli::FtraceMode,
    /// -finstrument-functions
    pub instrument_functions: bool,
    /// Stack protector guard register
    pub stack_protector_guard_reg: StackGuardReg,
    /// -fstack-clash-protection
    pub stack_clash_protection: bool,
    /// -fno-strict-aliasing
    pub strict_aliasing: bool,
    /// -fno-strict-overflow
    pub strict_overflow: bool,
    /// -fshort-wchar
    pub short_wchar: bool,
    /// -fno-common
    pub no_common: bool,
    /// CPU 指令集特性 (运行时检测)
    pub cpu_features: CpuFeatures,
    /// -march=<name> 目标微架构
    pub march: Option<String>,
    /// -mtune=<name> 调度优化目标
    pub mtune: Option<String>,
    /// -ffast-math: 激进浮点优化
    pub fast_math: bool,
    /// -ffp-contract=: FP 收缩模式
    pub fp_contract_mode: crate::cli::FpContractMode,
    /// ARM 架构版本 ("armv6-m", "armv7-m", "armv7e-m", "armv8-m.base",
    /// "armv8-m.main", "armv8.1-m.main")
    pub arm_arch_version: Option<String>,
    /// ARM Thumb-2 指令集可用
    pub has_thumb2: bool,
    /// ARM 硬件 FPU 可用 (VFPv4/FPv5)
    pub has_fpu: bool,
    /// ARM DSP/SIMD 指令可用 (ARMv7E-M Helium)
    pub has_dsp: bool,
    /// ARM TrustZone-M 安全扩展可用
    pub has_trustzone: bool,
    /// ARM 指令集模式 (-mthumb/-marm)
    pub arm_instruction_mode: crate::cli::ArmInstructionMode,
    /// 浮点 ABI 模式 (-mfloat-abi=)
    pub float_abi: crate::cli::FloatAbi,
    /// RISC-V M 扩展 (整数乘除法)
    pub riscv_has_m: bool,
    /// RISC-V A 扩展 (原子指令)
    pub riscv_has_a: bool,
    /// RISC-V F 扩展 (单精度浮点)
    pub riscv_has_f: bool,
    /// RISC-V D 扩展 (双精度浮点)
    pub riscv_has_d: bool,
    /// RISC-V C 扩展 (压缩指令)
    pub riscv_has_c: bool,
    /// RISC-V B 扩展 (位操作)
    pub riscv_has_b: bool,
    /// RISC-V V 扩展 (向量指令)
    pub riscv_has_v: bool,
    /// AArch64 NEON (Advanced SIMD / ASIMD)
    pub aarch64_has_neon: bool,
    /// AArch64 SVE (Scalable Vector Extension)
    pub aarch64_has_sve: bool,
    /// AArch64 SVE2
    pub aarch64_has_sve2: bool,
    /// AArch64 LSE (Large System Extensions, ARMv8.1)
    pub aarch64_has_lse: bool,
    /// AArch64 RDM (Rounding Double Multiply Accumulate)
    pub aarch64_has_rdm: bool,
    /// AArch64 DP (Dot Product)
    pub aarch64_has_dp: bool,
    /// AArch64 CRC32
    pub aarch64_has_crc32: bool,
    /// MIPS MSA (MIPS SIMD Architecture)
    pub mips_has_msa: bool,
    /// MIPS DSP ASE
    pub mips_has_dsp: bool,
    /// -falign-loops=N: 循环头部对齐边界 (2 的幂, 0=不对齐)
    pub align_loops: u32,
    /// -falign-jumps=N: 跳转目标对齐边界 (2 的幂, 0=不对齐)
    pub align_jumps: u32,
}

impl Target {
    pub fn resolve(t: &TargetTriple) -> Self {
        let (ptr_size, long_size, int_size, llong_size, endian, callee_save) = match t.arch {
            Arch::X86_64 => (8, 8, 4, 8, Endian::Little, CalleeSave::SysV),
            Arch::AArch64 => (8, 8, 4, 8, Endian::Little, CalleeSave::AAPCS64),
            Arch::RiscV64 => (8, 8, 4, 8, Endian::Little, CalleeSave::SysV),
            Arch::RiscV32 => (4, 4, 4, 8, Endian::Little, CalleeSave::SysV),
            Arch::Mips64 => (8, 8, 4, 8, Endian::Big, CalleeSave::MipsN64),
            Arch::Mips => (4, 4, 4, 8, Endian::Big, CalleeSave::MipsO32),
            Arch::Arm => (4, 4, 4, 8, Endian::Little, CalleeSave::AAPCS),
        };
        // 默认对齐表: 根据架构 ABI 确定各基本类型的自然对齐
        //
        // LP64 (x86_64/aarch64/riscv64/mips64 N64):
        //   - long/pointer/double: 8 字节对齐
        //   - long long: 8 字节对齐
        //   - long double: 16 字节对齐 (x86_64 80-bit 扩展精度)
        //
        // ILP32 (mips O32/arm AAPCS):
        //   - long/pointer: 4 字节对齐 (= size)
        //   - long long: 8 字节对齐 (ABI 要求 even-register 对齐)
        //   - double: 8 字节对齐 (ABI 要求 even-register 对齐)
        //   - long double: 8 字节对齐 (= double 大小)
        let (ptr_align, int_align, long_align, llong_align, double_align, long_double_align) =
            match t.arch {
                Arch::X86_64 => (8, 4, 8, 8, 8, 16),
                Arch::AArch64 => (8, 4, 8, 8, 8, 16),
                Arch::RiscV64 => (8, 4, 8, 8, 8, 16),
                Arch::RiscV32 => (4, 4, 4, 8, 8, 8),
                Arch::Mips64 => (8, 4, 8, 8, 8, 16),
                Arch::Mips => (4, 4, 4, 8, 8, 8),
                Arch::Arm => (4, 4, 4, 8, 8, 8),
            };
        // 第一性原理: Target::resolve() 只负责"几何"参数 (指针/字节序/调用约定),
        // 不管 codegen 后端是否实现。是否真的能 emit 是 codegen::emit() 关心的事。
        //
        // 交叉编译: red_zone / sse 仅在 x86_64 上有意义, 其余架构默认关闭.
        let is_x86_64 = t.arch == Arch::X86_64;
        // 非 x86 架构: 检测宿主 CPU 特性 (仅在对应架构宿主上返回真实值)
        let non_x86 = if !is_x86_64 {
            CpuFeatures::detect_non_x86(t.arch)
        } else {
            crate::cpuid::NonX86Features::default()
        };
        Self {
            triple: t.clone(),
            arch: t.arch,
            ptr_size,
            int_size,
            long_size,
            llong_size,
            endian,
            callee_save,
            ptr_align,
            int_align,
            long_align,
            llong_align,
            double_align,
            long_double_align,
            is_linux: matches!(t.os, Os::Linux | Os::Freestanding),
            red_zone: is_x86_64,
            frame_pointer: false,
            sse: is_x86_64,
            kernel_code_model: false,
            auto_var_init_zero: false,
            stack_protector: false,
            unwind_tables: true,
            function_return_thunk_extern: false,
            indirect_branch_thunk_extern: false,
            patchable_function_entry: None,
            ftrace_mode: crate::cli::FtraceMode::None,
            instrument_functions: false,
            stack_protector_guard_reg: StackGuardReg::Fs,
            stack_clash_protection: false,
            strict_aliasing: true,
            strict_overflow: true,
            short_wchar: false,
            no_common: false,
            cpu_features: if is_x86_64 {
                CpuFeatures::detect()
            } else {
                CpuFeatures::default()
            },
            march: None,
            mtune: None,
            fast_math: false,
            fp_contract_mode: crate::cli::FpContractMode::Default,
            arm_arch_version: None,
            has_thumb2: non_x86.thumb2,
            has_fpu: non_x86.vfpv4 || non_x86.arm_neon,
            has_dsp: non_x86.mips_dsp,
            has_trustzone: false,
            arm_instruction_mode: crate::cli::ArmInstructionMode::default(),
            float_abi: crate::cli::FloatAbi::default(),
            riscv_has_m: non_x86.rv_m,
            riscv_has_a: non_x86.rv_a,
            riscv_has_f: non_x86.rv_f,
            riscv_has_d: non_x86.rv_d,
            riscv_has_c: non_x86.rv_c,
            riscv_has_b: non_x86.rv_b,
            riscv_has_v: non_x86.rv_v,
            aarch64_has_neon: non_x86.neon,
            aarch64_has_sve: non_x86.sve,
            aarch64_has_sve2: non_x86.sve2,
            aarch64_has_lse: non_x86.lse,
            aarch64_has_rdm: non_x86.rdm,
            aarch64_has_dp: non_x86.dp,
            aarch64_has_crc32: non_x86.crc32,
            mips_has_msa: non_x86.msa,
            mips_has_dsp: non_x86.mips_dsp,
            align_loops: 0,
            align_jumps: 0,
        }
    }

    pub fn apply_codegen_options(&mut self, red_zone: bool, frame_pointer: bool, sse: bool) {
        self.red_zone = red_zone;
        self.frame_pointer = frame_pointer;
        self.sse = sse;
    }

    pub fn set_stack_protector(&mut self, enable: bool) {
        self.stack_protector = enable;
    }

    /// 应用 CLI 显式指定的 CPU 特性覆盖.
    ///
    /// 用户通过 -mavx / -mno-avx 等标志显式控制,
    /// 覆盖 CPUID 动态检测的结果.
    pub fn apply_cpu_feature_overrides(&mut self, overrides: &CpuFeatureOverrides) {
        if let Some(v) = overrides.avx {
            self.cpu_features.avx = v;
        }
        if let Some(v) = overrides.avx2 {
            self.cpu_features.avx2 = v;
        }
        if let Some(v) = overrides.avx512f {
            self.cpu_features.avx512f = v;
        }
        if let Some(v) = overrides.avx512bw {
            self.cpu_features.avx512bw = v;
        }
        if let Some(v) = overrides.avx512vl {
            self.cpu_features.avx512vl = v;
        }
        if let Some(v) = overrides.avx512dq {
            self.cpu_features.avx512dq = v;
        }
        if let Some(v) = overrides.fma {
            self.cpu_features.fma = v;
        }
        if let Some(v) = overrides.bmi1 {
            self.cpu_features.bmi1 = v;
        }
        if let Some(v) = overrides.bmi2 {
            self.cpu_features.bmi2 = v;
        }
        if let Some(v) = overrides.cet_ss {
            self.cpu_features.cet_ss = v;
        }
        if let Some(v) = overrides.cet_ibt {
            self.cpu_features.cet_ibt = v;
        }
        if let Some(v) = overrides.avx_vnni {
            self.cpu_features.avx_vnni = v;
        }
        if let Some(v) = overrides.avx10_1 {
            self.cpu_features.avx10_1 = v;
        }
        if let Some(v) = overrides.avx10_512 {
            self.cpu_features.avx10_512 = v;
            // AVX10-512 implies AVX10.1
            if v {
                self.cpu_features.avx10_1 = true;
            }
        }
        if let Some(v) = overrides.apx_f {
            self.cpu_features.apx_f = v;
        }
        // AVX2 依赖 AVX: 若 AVX 被禁用, AVX2 也必须禁用
        if !self.cpu_features.avx {
            self.cpu_features.avx2 = false;
            self.cpu_features.fma = false;
            self.cpu_features.avx_vnni = false;
            self.cpu_features.avx10_1 = false;
            self.cpu_features.avx10_256 = false;
            self.cpu_features.avx10_512 = false;
        }
        // AVX-512 依赖 AVX: 若 AVX 被禁用, AVX-512 也必须禁用
        if !self.cpu_features.avx {
            self.cpu_features.avx512f = false;
            self.cpu_features.avx512dq = false;
            self.cpu_features.avx512bw = false;
            self.cpu_features.avx512vl = false;
        }
    }

    /// 应用 -march=<name> 设置对应的 CPU 特性组合.
    ///
    /// 已知 x86-64 微架构级别:
    /// - x86-64: 基线 (SSE2)
    /// - x86-64-v2: + SSE4.2 + SSSE3 + POPCNT + CMPXCHG16B
    /// - x86-64-v3: + AVX + AVX2 + BMI1 + BMI2 + FMA + F16C
    /// - x86-64-v4: + AVX512F + AVX512BW + AVX512DQ + AVX512VL
    /// - native: 使用 CPUID 检测结果
    /// - 具体微架构名: sandybridge/haswell/skylake/等
    pub fn apply_march(&mut self, arch_name: &str) {
        self.march = Some(arch_name.to_string());
        match arch_name {
            "native" => {
                // 使用 CPUID 检测结果, 不做覆盖
                if self.arch == crate::cli::Arch::X86_64 {
                    self.cpu_features = CpuFeatures::detect();
                }
            }
            "x86-64" | "x86-64-v1" => {
                // 基线: 不做任何额外启用
            }
            "x86-64-v2" => {
                self.cpu_features.ssse3 = true;
                self.cpu_features.sse4_1 = true;
                self.cpu_features.sse4_2 = true;
                self.cpu_features.popcnt = true;
            }
            "x86-64-v3" => {
                // v2 + AVX family
                self.cpu_features.ssse3 = true;
                self.cpu_features.sse4_1 = true;
                self.cpu_features.sse4_2 = true;
                self.cpu_features.popcnt = true;
                self.cpu_features.avx = true;
                self.cpu_features.avx2 = true;
                self.cpu_features.bmi1 = true;
                self.cpu_features.bmi2 = true;
                self.cpu_features.fma = true;
                self.cpu_features.f16c = true;
            }
            "x86-64-v4" => {
                // v3 + AVX-512
                self.cpu_features.ssse3 = true;
                self.cpu_features.sse4_1 = true;
                self.cpu_features.sse4_2 = true;
                self.cpu_features.popcnt = true;
                self.cpu_features.avx = true;
                self.cpu_features.avx2 = true;
                self.cpu_features.bmi1 = true;
                self.cpu_features.bmi2 = true;
                self.cpu_features.fma = true;
                self.cpu_features.f16c = true;
                self.cpu_features.avx512f = true;
                self.cpu_features.avx512bw = true;
                self.cpu_features.avx512dq = true;
                self.cpu_features.avx512vl = true;
            }
            // Intel micro-architectures
            "sandybridge" => {
                self.cpu_features.avx = true;
                self.cpu_features.sse4_1 = true;
                self.cpu_features.sse4_2 = true;
                self.cpu_features.popcnt = true;
            }
            "haswell" => {
                self.cpu_features.avx = true;
                self.cpu_features.avx2 = true;
                self.cpu_features.bmi1 = true;
                self.cpu_features.bmi2 = true;
                self.cpu_features.fma = true;
                self.cpu_features.f16c = true;
                self.cpu_features.sse4_1 = true;
                self.cpu_features.sse4_2 = true;
                self.cpu_features.popcnt = true;
            }
            "skylake" | "skylake-avx512" => {
                self.cpu_features.avx = true;
                self.cpu_features.avx2 = true;
                self.cpu_features.bmi1 = true;
                self.cpu_features.bmi2 = true;
                self.cpu_features.fma = true;
                self.cpu_features.f16c = true;
                self.cpu_features.sse4_1 = true;
                self.cpu_features.sse4_2 = true;
                self.cpu_features.popcnt = true;
                if arch_name == "skylake-avx512" {
                    self.cpu_features.avx512f = true;
                    self.cpu_features.avx512bw = true;
                    self.cpu_features.avx512dq = true;
                    self.cpu_features.avx512vl = true;
                }
            }
            "icelake-server" | "icelake-client" => {
                self.cpu_features.avx = true;
                self.cpu_features.avx2 = true;
                self.cpu_features.bmi1 = true;
                self.cpu_features.bmi2 = true;
                self.cpu_features.fma = true;
                self.cpu_features.f16c = true;
                self.cpu_features.sse4_1 = true;
                self.cpu_features.sse4_2 = true;
                self.cpu_features.popcnt = true;
                self.cpu_features.avx512f = true;
                self.cpu_features.avx512bw = true;
                self.cpu_features.avx512dq = true;
                self.cpu_features.avx512vl = true;
                self.cpu_features.avx512_vnni = true;
            }
            "sapphirerapids" | "granite-rapids" => {
                self.cpu_features.avx = true;
                self.cpu_features.avx2 = true;
                self.cpu_features.bmi1 = true;
                self.cpu_features.bmi2 = true;
                self.cpu_features.fma = true;
                self.cpu_features.f16c = true;
                self.cpu_features.sse4_1 = true;
                self.cpu_features.sse4_2 = true;
                self.cpu_features.popcnt = true;
                self.cpu_features.avx512f = true;
                self.cpu_features.avx512bw = true;
                self.cpu_features.avx512dq = true;
                self.cpu_features.avx512vl = true;
                self.cpu_features.avx512_vnni = true;
                self.cpu_features.avx_vnni = true;
                self.cpu_features.avx10_1 = true;
                self.cpu_features.apx_f = true;
            }
            // ARM micro-architectures (mcpu)
            "cortex-a53" | "cortex-a72" | "cortex-a76" => {
                // ARMv8.0 A-profile baseline
            }
            "neoverse-n1" | "neoverse-v1" => {
                // ARMv8.2+ with crypto extensions
            }
            // ARM Cortex-M embedded cores (bare-metal)
            "cortex-m0" | "cortex-m0plus" | "cortex-m1" => {
                // ARMv6-M: Thumb-1 only, no Thumb-2, soft-float
                self.arm_arch_version = Some("armv6-m".to_string());
                self.has_thumb2 = false;
                self.has_fpu = false;
                self.has_dsp = false;
                self.has_trustzone = false;
            }
            "cortex-m3" => {
                // ARMv7-M: Thumb-2, soft-float, no DSP
                self.arm_arch_version = Some("armv7-m".to_string());
                self.has_thumb2 = true;
                self.has_fpu = false;
                self.has_dsp = false;
                self.has_trustzone = false;
            }
            "cortex-m4" => {
                // ARMv7E-M: Thumb-2, optional single-precision FP, DSP
                self.arm_arch_version = Some("armv7e-m".to_string());
                self.has_thumb2 = true;
                self.has_fpu = true; // optional, enabled by default
                self.has_dsp = true;
                self.has_trustzone = false;
            }
            "cortex-m7" => {
                // ARMv7E-M: Thumb-2, optional single/double FP, DSP
                self.arm_arch_version = Some("armv7e-m".to_string());
                self.has_thumb2 = true;
                self.has_fpu = true;
                self.has_dsp = true;
                self.has_trustzone = false;
            }
            "cortex-m23" => {
                // ARMv8-M Baseline: Thumb-1/2, TrustZone
                self.arm_arch_version = Some("armv8-m.base".to_string());
                self.has_thumb2 = true;
                self.has_fpu = false;
                self.has_dsp = false;
                self.has_trustzone = true;
            }
            "cortex-m33" => {
                // ARMv8-M Mainline: Thumb-2, FP, DSP, TrustZone
                self.arm_arch_version = Some("armv8-m.main".to_string());
                self.has_thumb2 = true;
                self.has_fpu = true;
                self.has_dsp = true;
                self.has_trustzone = true;
            }
            "cortex-m55" => {
                // ARMv8.1-M: Helium DSP, FP, TrustZone
                self.arm_arch_version = Some("armv8.1-m.main".to_string());
                self.has_thumb2 = true;
                self.has_fpu = true;
                self.has_dsp = true;
                self.has_trustzone = true;
            }
            // RISC-V extension parsing (rv32/rv64 + extensions)
            name if name.starts_with("rv32") || name.starts_with("rv64") => {
                // Parse RISC-V ISA string: rv32/rv64 + extension letters
                // Examples: rv32imc, rv32imac, rv32g, rv64gc
                let ext_str = &name[4..]; // skip "rv32" or "rv64"
                self.riscv_has_m = ext_str.contains('m');
                self.riscv_has_a = ext_str.contains('a');
                self.riscv_has_f = ext_str.contains('f');
                self.riscv_has_d = ext_str.contains('d');
                self.riscv_has_c = ext_str.contains('c');
                self.riscv_has_b = ext_str.contains('b');
                self.riscv_has_v = ext_str.contains('v');
                // 'g' is shorthand for imafd (general)
                if ext_str.contains('g') {
                    self.riscv_has_m = true;
                    self.riscv_has_a = true;
                    self.riscv_has_f = true;
                    self.riscv_has_d = true;
                }
            }
            _ => {
                // Unknown arch name: accept but no feature changes
            }
        }
    }

    /// 应用 -ffast-math: 关闭严格别名/溢出, 启用激进浮点优化.
    pub fn apply_fast_math(&mut self) {
        self.fast_math = true;
        self.strict_aliasing = false;
        self.strict_overflow = false;
    }

    /// 检查是否有 VNNI (Vector Neural Network Instructions) 支持.
    /// 包括 AVX-VNNI (VEX 形式) 或 AVX-512VNNI.
    pub fn has_vnni_support(&self) -> bool {
        self.cpu_features.avx_vnni || self.cpu_features.avx512_vnni
    }

    /// 返回基本类型的默认对齐 (字节)。
    ///
    /// 根据目标架构 ABI 返回各基本类型的自然对齐要求。
    /// 用于 struct/union 布局计算和 `__alignof__` 求值。
    ///
    /// 参数 `type_kind` 标识类型类别:
    /// - "ptr": 指针类型
    /// - "int": int 类型
    /// - "long": long 类型
    /// - "llong": long long 类型
    /// - "double": double 类型
    /// - "longdouble": long double 类型
    /// - "int128": __int128 类型
    pub fn base_type_align(&self, type_kind: &str) -> usize {
        match type_kind {
            "ptr" => self.ptr_align as usize,
            "int" => self.int_align as usize,
            "long" => self.long_align as usize,
            "llong" => self.llong_align as usize,
            "double" => self.double_align as usize,
            "longdouble" => self.long_double_align as usize,
            "int128" => 16,
            _ => 1,
        }
    }

    /// 返回目标架构的最大对齐要求 (字节)。
    ///
    /// 用于确定 struct 的最大自然对齐上界。
    /// 通常是 long double 的对齐 (LP64: 16, ILP32: 8)。
    pub fn max_align(&self) -> usize {
        self.long_double_align as usize
    }

    /// 是否为 LP64 数据模型 (64-bit long + 64-bit pointer).
    ///
    /// x86_64 / aarch64 / riscv64 / mips64 N64 均为 LP64.
    pub fn is_lp64(&self) -> bool {
        self.ptr_size == 8 && self.long_size == 8
    }

    /// 是否为 ILP32 数据模型 (32-bit int + 32-bit long + 32-bit pointer).
    ///
    /// mips O32 / arm AAPCS 均为 ILP32.
    pub fn is_ilp32(&self) -> bool {
        self.ptr_size == 4 && self.int_size == 4 && self.long_size == 4
    }

    /// 是否为大端架构.
    ///
    /// mips / mips64 默认大端; 其余为小端.
    pub fn is_big_endian(&self) -> bool {
        matches!(self.endian, Endian::Big)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Abi, Arch, Os, TargetTriple};

    fn trip(arch: Arch, os: Os, abi: Abi) -> TargetTriple {
        TargetTriple {
            arch,
            vendor: "unknown".into(),
            os,
            abi,
        }
    }

    #[test]
    fn x86_64_gnu_has_8byte_ptr_little_endian_sysv() {
        let t = Target::resolve(&trip(Arch::X86_64, Os::Linux, Abi::Gnu));
        assert_eq!(t.arch, Arch::X86_64);
        assert_eq!(t.ptr_size, 8);
        assert_eq!(t.long_size, 8);
        assert_eq!(t.int_size, 4);
        assert_eq!(t.llong_size, 8);
        assert_eq!(t.endian, Endian::Little);
        assert_eq!(t.callee_save, CalleeSave::SysV);
        assert!(t.is_linux);
        assert!(t.arch.has_backend());
    }

    #[test]
    fn aarch64_uses_aapcs64_little_endian() {
        let t = Target::resolve(&trip(Arch::AArch64, Os::Linux, Abi::Gnu));
        assert_eq!(t.ptr_size, 8);
        assert_eq!(t.endian, Endian::Little);
        assert_eq!(t.callee_save, CalleeSave::AAPCS64);
        #[cfg(feature = "arm")]
        assert!(t.arch.has_backend());
        #[cfg(not(feature = "arm"))]
        assert!(!t.arch.has_backend());
    }

    #[test]
    fn riscv64_uses_sysv_little_endian() {
        let t = Target::resolve(&trip(Arch::RiscV64, Os::Linux, Abi::Gnu));
        assert_eq!(t.ptr_size, 8);
        assert_eq!(t.endian, Endian::Little);
        assert_eq!(t.callee_save, CalleeSave::SysV);
        #[cfg(feature = "mips")]
        assert!(t.arch.has_backend());
        #[cfg(not(feature = "mips"))]
        assert!(!t.arch.has_backend());
    }

    #[test]
    fn riscv32_ilp32_little_endian_sysv() {
        let t = Target::resolve(&trip(Arch::RiscV32, Os::Freestanding, Abi::Eabi));
        assert_eq!(t.arch, Arch::RiscV32);
        assert_eq!(t.ptr_size, 4);
        assert_eq!(t.long_size, 4);
        assert_eq!(t.int_size, 4);
        assert_eq!(t.llong_size, 8);
        assert_eq!(t.endian, Endian::Little);
        assert_eq!(t.callee_save, CalleeSave::SysV);
        assert!(!t.arch.has_backend());  // RiscV32 not implemented
        assert!(t.is_ilp32());
        assert!(!t.is_lp64());
    }

    #[test]
    fn mips_parse_only_4byte_ptr_big_endian_mipso32() {
        // mips 有 triple 解析和 codegen 后端
        let t = Target::resolve(&trip(Arch::Mips, Os::Linux, Abi::Gnu));
        assert_eq!(t.arch, Arch::Mips);
        assert_eq!(t.ptr_size, 4);
        assert_eq!(t.long_size, 4);
        assert_eq!(t.int_size, 4);
        assert_eq!(t.llong_size, 8);
        assert_eq!(t.endian, Endian::Big);
        // 锁的是“不是 ARM 的 AAPCS”这一事实: MIPS O32 自有 callee-saved 集
        assert_eq!(t.callee_save, CalleeSave::MipsO32);
        #[cfg(feature = "mips")]
        assert!(t.arch.has_backend());
        #[cfg(not(feature = "mips"))]
        assert!(!t.arch.has_backend());
    }

    #[test]
    fn mips64_parse_only_8byte_ptr_big_endian_mipsn64() {
        let t = Target::resolve(&trip(Arch::Mips64, Os::Linux, Abi::Gnu));
        assert_eq!(t.arch, Arch::Mips64);
        assert_eq!(t.ptr_size, 8);
        assert_eq!(t.long_size, 8);
        assert_eq!(t.endian, Endian::Big);
        assert_eq!(t.callee_save, CalleeSave::MipsN64);
        #[cfg(feature = "mips")]
        assert!(t.arch.has_backend());
        #[cfg(not(feature = "mips"))]
        assert!(!t.arch.has_backend());
    }

    #[test]
    fn arm_parse_only_4byte_ptr_little_endian_aapcs() {
        let t = Target::resolve(&trip(Arch::Arm, Os::Linux, Abi::Eabihf));
        assert_eq!(t.arch, Arch::Arm);
        assert_eq!(t.ptr_size, 4);
        assert_eq!(t.long_size, 4);
        assert_eq!(t.int_size, 4);
        assert_eq!(t.llong_size, 8);
        assert_eq!(t.endian, Endian::Little);
        assert_eq!(t.callee_save, CalleeSave::AAPCS);
        #[cfg(feature = "arm")]
        assert!(t.arch.has_backend());
        #[cfg(not(feature = "arm"))]
        assert!(!t.arch.has_backend());
    }

    #[test]
    fn apply_codegen_options_overrides_target_defaults() {
        let mut t = Target::resolve(&trip(Arch::X86_64, Os::Linux, Abi::Gnu));
        t.apply_codegen_options(false, true, false);
        assert!(!t.red_zone);
        assert!(t.frame_pointer);
        assert!(!t.sse);
    }

    #[test]
    fn x86_64_target_has_cpu_features_detected() {
        let t = Target::resolve(&trip(Arch::X86_64, Os::Linux, Abi::Gnu));
        // x86_64 should have SSE (always available)
        assert!(t.cpu_features.has_sse());
        // Other features depend on the actual CPU
        let _ = t.cpu_features.has_avx();
        let _ = t.cpu_features.has_avx2();
        let _ = t.cpu_features.has_avx512f();
    }

    #[test]
    fn non_x86_target_has_empty_cpu_features() {
        let t = Target::resolve(&trip(Arch::AArch64, Os::Linux, Abi::Gnu));
        // Non-x86 targets should have all features as default (false)
        assert!(!t.cpu_features.avx);
        assert!(!t.cpu_features.avx2);
        assert!(!t.cpu_features.avx512f);
        assert!(!t.cpu_features.cet_ss);
    }

    #[test]
    fn cpu_feature_override_avx_disable() {
        let mut t = Target::resolve(&trip(Arch::X86_64, Os::Linux, Abi::Gnu));
        // Simulate -mno-avx
        let overrides = CpuFeatureOverrides {
            avx: Some(false),
            ..Default::default()
        };
        t.apply_cpu_feature_overrides(&overrides);
        assert!(!t.cpu_features.avx);
        // AVX2 and FMA depend on AVX, should also be disabled
        assert!(!t.cpu_features.avx2);
        assert!(!t.cpu_features.fma);
    }

    #[test]
    fn cpu_feature_override_avx2_enable() {
        let mut t = Target::resolve(&trip(Arch::X86_64, Os::Linux, Abi::Gnu));
        // Simulate -mavx2
        let overrides = CpuFeatureOverrides {
            avx: Some(true),
            avx2: Some(true),
            ..Default::default()
        };
        t.apply_cpu_feature_overrides(&overrides);
        assert!(t.cpu_features.avx);
        assert!(t.cpu_features.avx2);
    }

    #[test]
    fn cpu_feature_override_cet() {
        let mut t = Target::resolve(&trip(Arch::X86_64, Os::Linux, Abi::Gnu));
        let overrides = CpuFeatureOverrides {
            cet_ss: Some(true),
            cet_ibt: Some(true),
            ..Default::default()
        };
        t.apply_cpu_feature_overrides(&overrides);
        assert!(t.cpu_features.cet_ss);
        assert!(t.cpu_features.cet_ibt);
    }

    // === 默认对齐表测试 ===

    #[test]
    fn x86_64_lp64_alignment_table() {
        let t = Target::resolve(&trip(Arch::X86_64, Os::Linux, Abi::Gnu));
        // LP64: long/ptr/double = 8, long long = 8, long double = 16
        assert_eq!(t.ptr_align, 8);
        assert_eq!(t.int_align, 4);
        assert_eq!(t.long_align, 8);
        assert_eq!(t.llong_align, 8);
        assert_eq!(t.double_align, 8);
        assert_eq!(t.long_double_align, 16);
        assert_eq!(t.max_align(), 16);
    }

    #[test]
    fn aarch64_lp64_alignment_table() {
        let t = Target::resolve(&trip(Arch::AArch64, Os::Linux, Abi::Gnu));
        assert_eq!(t.ptr_align, 8);
        assert_eq!(t.int_align, 4);
        assert_eq!(t.long_align, 8);
        assert_eq!(t.llong_align, 8);
        assert_eq!(t.double_align, 8);
        assert_eq!(t.long_double_align, 16);
        assert_eq!(t.max_align(), 16);
    }

    #[test]
    fn riscv64_lp64_alignment_table() {
        let t = Target::resolve(&trip(Arch::RiscV64, Os::Linux, Abi::Gnu));
        assert_eq!(t.ptr_align, 8);
        assert_eq!(t.long_align, 8);
        assert_eq!(t.llong_align, 8);
        assert_eq!(t.double_align, 8);
        assert_eq!(t.long_double_align, 16);
    }

    #[test]
    fn riscv32_ilp32_alignment_table() {
        let t = Target::resolve(&trip(Arch::RiscV32, Os::Freestanding, Abi::Eabi));
        // ILP32: long/ptr = 4, long long/double = 8, long double = 8
        assert_eq!(t.ptr_align, 4);
        assert_eq!(t.int_align, 4);
        assert_eq!(t.long_align, 4);
        assert_eq!(t.llong_align, 8);
        assert_eq!(t.double_align, 8);
        assert_eq!(t.long_double_align, 8);
        assert_eq!(t.max_align(), 8);
    }

    #[test]
    fn mips64_lp64_alignment_table() {
        let t = Target::resolve(&trip(Arch::Mips64, Os::Linux, Abi::Gnu));
        assert_eq!(t.ptr_align, 8);
        assert_eq!(t.long_align, 8);
        assert_eq!(t.llong_align, 8);
        assert_eq!(t.double_align, 8);
        assert_eq!(t.long_double_align, 16);
    }

    #[test]
    fn mips_ilp32_alignment_table() {
        let t = Target::resolve(&trip(Arch::Mips, Os::Linux, Abi::Gnu));
        // ILP32: long/ptr = 4, long long/double = 8, long double = 8
        assert_eq!(t.ptr_align, 4);
        assert_eq!(t.int_align, 4);
        assert_eq!(t.long_align, 4);
        assert_eq!(t.llong_align, 8);
        assert_eq!(t.double_align, 8);
        assert_eq!(t.long_double_align, 8);
        assert_eq!(t.max_align(), 8);
    }

    #[test]
    fn arm_ilp32_alignment_table() {
        let t = Target::resolve(&trip(Arch::Arm, Os::Linux, Abi::Eabihf));
        // AAPCS: long/ptr = 4, long long/double = 8, long double = 8
        assert_eq!(t.ptr_align, 4);
        assert_eq!(t.int_align, 4);
        assert_eq!(t.long_align, 4);
        assert_eq!(t.llong_align, 8);
        assert_eq!(t.double_align, 8);
        assert_eq!(t.long_double_align, 8);
        assert_eq!(t.max_align(), 8);
    }

    #[test]
    fn base_type_align_query() {
        let t = Target::resolve(&trip(Arch::X86_64, Os::Linux, Abi::Gnu));
        assert_eq!(t.base_type_align("ptr"), 8);
        assert_eq!(t.base_type_align("int"), 4);
        assert_eq!(t.base_type_align("long"), 8);
        assert_eq!(t.base_type_align("llong"), 8);
        assert_eq!(t.base_type_align("double"), 8);
        assert_eq!(t.base_type_align("longdouble"), 16);
        assert_eq!(t.base_type_align("int128"), 16);
        assert_eq!(t.base_type_align("unknown"), 1);
    }

    #[test]
    fn ilp32_lp64_alignment_divergence() {
        // 验证 ILP32 和 LP64 对齐差异
        let lp64 = Target::resolve(&trip(Arch::X86_64, Os::Linux, Abi::Gnu));
        let ilp32 = Target::resolve(&trip(Arch::Arm, Os::Linux, Abi::Eabihf));
        // ptr/long 在 ILP32 是 4, LP64 是 8
        assert_eq!(lp64.ptr_align, 8);
        assert_eq!(ilp32.ptr_align, 4);
        assert_eq!(lp64.long_align, 8);
        assert_eq!(ilp32.long_align, 4);
        // long long/double 在两种模型下都是 8
        assert_eq!(lp64.llong_align, ilp32.llong_align);
        assert_eq!(lp64.double_align, ilp32.double_align);
        // long double 在 LP64 是 16, ILP32 是 8
        assert_eq!(lp64.long_double_align, 16);
        assert_eq!(ilp32.long_double_align, 8);
    }

    // === 数据模型辅助方法测试 ===

    #[test]
    fn lp64_architectures() {
        for arch in [Arch::X86_64, Arch::AArch64, Arch::RiscV64, Arch::Mips64] {
            let t = Target::resolve(&trip(arch, Os::Linux, Abi::Gnu));
            assert!(t.is_lp64(), "{:?} should be LP64", arch);
            assert!(!t.is_ilp32(), "{:?} should not be ILP32", arch);
        }
    }

    #[test]
    fn ilp32_architectures() {
        for arch in [Arch::Mips, Arch::Arm] {
            let t = Target::resolve(&trip(arch, Os::Linux, Abi::Gnu));
            assert!(t.is_ilp32(), "{:?} should be ILP32", arch);
            assert!(!t.is_lp64(), "{:?} should not be LP64", arch);
        }
    }

    #[test]
    fn endian_detection() {
        let x86 = Target::resolve(&trip(Arch::X86_64, Os::Linux, Abi::Gnu));
        assert!(!x86.is_big_endian());
        let aarch64 = Target::resolve(&trip(Arch::AArch64, Os::Linux, Abi::Gnu));
        assert!(!aarch64.is_big_endian());
        let mips = Target::resolve(&trip(Arch::Mips, Os::Linux, Abi::Gnu));
        assert!(mips.is_big_endian());
        let mips64 = Target::resolve(&trip(Arch::Mips64, Os::Linux, Abi::Gnu));
        assert!(mips64.is_big_endian());
        let arm = Target::resolve(&trip(Arch::Arm, Os::Linux, Abi::Eabihf));
        assert!(!arm.is_big_endian());
    }

    // === 交叉编译默认值测试 ===

    #[test]
    fn red_zone_only_x86_64() {
        let x86 = Target::resolve(&trip(Arch::X86_64, Os::Linux, Abi::Gnu));
        assert!(x86.red_zone, "x86_64 should have red_zone by default");
        let aarch64 = Target::resolve(&trip(Arch::AArch64, Os::Linux, Abi::Gnu));
        assert!(!aarch64.red_zone, "aarch64 should not have red_zone");
        let riscv = Target::resolve(&trip(Arch::RiscV64, Os::Linux, Abi::Gnu));
        assert!(!riscv.red_zone, "riscv64 should not have red_zone");
        let arm = Target::resolve(&trip(Arch::Arm, Os::Linux, Abi::Eabihf));
        assert!(!arm.red_zone, "arm should not have red_zone");
    }

    #[test]
    fn sse_only_x86_64() {
        let x86 = Target::resolve(&trip(Arch::X86_64, Os::Linux, Abi::Gnu));
        assert!(x86.sse, "x86_64 should have SSE by default");
        let aarch64 = Target::resolve(&trip(Arch::AArch64, Os::Linux, Abi::Gnu));
        assert!(!aarch64.sse, "aarch64 should not have SSE");
        let mips = Target::resolve(&trip(Arch::Mips, Os::Linux, Abi::Gnu));
        assert!(!mips.sse, "mips should not have SSE");
    }

    // === Cortex-M CPU recognition tests ===

    #[test]
    fn cortex_m0_armv6m_no_thumb2_no_fpu() {
        let mut t = Target::resolve(&trip(Arch::Arm, Os::Freestanding, Abi::Eabi));
        t.apply_march("cortex-m0");
        assert_eq!(t.arm_arch_version.as_deref(), Some("armv6-m"));
        assert!(!t.has_thumb2);
        assert!(!t.has_fpu);
        assert!(!t.has_dsp);
        assert!(!t.has_trustzone);
    }

    #[test]
    fn cortex_m3_armv7m_thumb2_no_fpu() {
        let mut t = Target::resolve(&trip(Arch::Arm, Os::Freestanding, Abi::Eabi));
        t.apply_march("cortex-m3");
        assert_eq!(t.arm_arch_version.as_deref(), Some("armv7-m"));
        assert!(t.has_thumb2);
        assert!(!t.has_fpu);
        assert!(!t.has_dsp);
    }

    #[test]
    fn cortex_m4_armv7em_thumb2_fpu_dsp() {
        let mut t = Target::resolve(&trip(Arch::Arm, Os::Freestanding, Abi::Eabi));
        t.apply_march("cortex-m4");
        assert_eq!(t.arm_arch_version.as_deref(), Some("armv7e-m"));
        assert!(t.has_thumb2);
        assert!(t.has_fpu);
        assert!(t.has_dsp);
        assert!(!t.has_trustzone);
    }

    #[test]
    fn cortex_m33_armv8m_main_trustzone() {
        let mut t = Target::resolve(&trip(Arch::Arm, Os::Freestanding, Abi::Eabi));
        t.apply_march("cortex-m33");
        assert_eq!(t.arm_arch_version.as_deref(), Some("armv8-m.main"));
        assert!(t.has_thumb2);
        assert!(t.has_fpu);
        assert!(t.has_dsp);
        assert!(t.has_trustzone);
    }

    #[test]
    fn cortex_m55_armv81m_helium() {
        let mut t = Target::resolve(&trip(Arch::Arm, Os::Freestanding, Abi::Eabi));
        t.apply_march("cortex-m55");
        assert_eq!(t.arm_arch_version.as_deref(), Some("armv8.1-m.main"));
        assert!(t.has_thumb2);
        assert!(t.has_fpu);
        assert!(t.has_dsp);
        assert!(t.has_trustzone);
    }

    // === RISC-V extension parsing tests ===

    #[test]
    fn riscv32_imc_extensions() {
        let mut t = Target::resolve(&trip(Arch::RiscV32, Os::Freestanding, Abi::Eabi));
        t.apply_march("rv32imc");
        assert!(t.riscv_has_m);
        assert!(!t.riscv_has_a);
        assert!(!t.riscv_has_f);
        assert!(!t.riscv_has_d);
        assert!(t.riscv_has_c);
        assert!(!t.riscv_has_b);
        assert!(!t.riscv_has_v);
    }

    #[test]
    fn riscv32_imac_extensions() {
        let mut t = Target::resolve(&trip(Arch::RiscV32, Os::Freestanding, Abi::Eabi));
        t.apply_march("rv32imac");
        assert!(t.riscv_has_m);
        assert!(t.riscv_has_a);
        assert!(!t.riscv_has_f);
        assert!(!t.riscv_has_d);
        assert!(t.riscv_has_c);
    }

    #[test]
    fn riscv32_g_shorthand() {
        let mut t = Target::resolve(&trip(Arch::RiscV32, Os::Freestanding, Abi::Eabi));
        t.apply_march("rv32g");
        // 'g' = imafd
        assert!(t.riscv_has_m);
        assert!(t.riscv_has_a);
        assert!(t.riscv_has_f);
        assert!(t.riscv_has_d);
        assert!(!t.riscv_has_c);
    }

    #[test]
    fn riscv64_gc_extensions() {
        let mut t = Target::resolve(&trip(Arch::RiscV64, Os::Linux, Abi::Gnu));
        t.apply_march("rv64gc");
        // 'g' = imafd, plus 'c'
        assert!(t.riscv_has_m);
        assert!(t.riscv_has_a);
        assert!(t.riscv_has_f);
        assert!(t.riscv_has_d);
        assert!(t.riscv_has_c);
    }
}
