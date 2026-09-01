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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::upper_case_acronyms)]
pub enum CalleeSave {
    SysV,
    Win64,
    AAPCS64,
    AAPCS,
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
}

impl Target {
    pub fn resolve(t: &TargetTriple) -> Self {
        let (ptr_size, long_size, int_size, llong_size, endian, callee_save) = match t.arch {
            Arch::X86_64 => (8, 8, 4, 8, Endian::Little, CalleeSave::SysV),
            Arch::AArch64 => (8, 8, 4, 8, Endian::Little, CalleeSave::AAPCS64),
            Arch::RiscV64 => (8, 8, 4, 8, Endian::Little, CalleeSave::SysV),
            Arch::RiscV32 => (4, 4, 4, 8, Endian::Little, CalleeSave::SysV),
            Arch::Mips64 => (8, 8, 4, 8, Endian::Big, CalleeSave::AAPCS),
            Arch::Mips => (4, 4, 4, 8, Endian::Big, CalleeSave::AAPCS),
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
                // ARMv8.0 baseline
            }
            "neoverse-n1" | "neoverse-v1" => {
                // ARMv8.2+ with crypto extensions
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
        assert!(t.arch.has_backend());
    }

    #[test]
    fn riscv64_uses_sysv_little_endian() {
        let t = Target::resolve(&trip(Arch::RiscV64, Os::Linux, Abi::Gnu));
        assert_eq!(t.ptr_size, 8);
        assert_eq!(t.endian, Endian::Little);
        assert_eq!(t.callee_save, CalleeSave::SysV);
        assert!(t.arch.has_backend());
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
        assert!(t.arch.has_backend());
        assert!(t.is_ilp32());
        assert!(!t.is_lp64());
    }

    #[test]
    fn mips_parse_only_4byte_ptr_big_endian_aapcs() {
        // mips 有 triple 解析和 codegen 后端
        let t = Target::resolve(&trip(Arch::Mips, Os::Linux, Abi::Gnu));
        assert_eq!(t.arch, Arch::Mips);
        assert_eq!(t.ptr_size, 4);
        assert_eq!(t.long_size, 4);
        assert_eq!(t.int_size, 4);
        assert_eq!(t.llong_size, 8);
        assert_eq!(t.endian, Endian::Big);
        assert_eq!(t.callee_save, CalleeSave::AAPCS);
        assert!(t.arch.has_backend());
    }

    #[test]
    fn mips64_parse_only_8byte_ptr_big_endian_aapcs() {
        let t = Target::resolve(&trip(Arch::Mips64, Os::Linux, Abi::Gnu));
        assert_eq!(t.arch, Arch::Mips64);
        assert_eq!(t.ptr_size, 8);
        assert_eq!(t.long_size, 8);
        assert_eq!(t.endian, Endian::Big);
        assert_eq!(t.callee_save, CalleeSave::AAPCS);
        assert!(t.arch.has_backend());
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
        assert!(t.arch.has_backend());
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
}
