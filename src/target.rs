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
}

impl Target {
    pub fn resolve(t: &TargetTriple) -> Self {
        let (ptr_size, long_size, int_size, llong_size, endian, callee_save) = match t.arch {
            Arch::X86_64 => (8, 8, 4, 8, Endian::Little, CalleeSave::SysV),
            Arch::AArch64 => (8, 8, 4, 8, Endian::Little, CalleeSave::AAPCS64),
            Arch::RiscV64 => (8, 8, 4, 8, Endian::Little, CalleeSave::SysV),
            Arch::Mips64 => (8, 8, 4, 8, Endian::Big, CalleeSave::AAPCS),
            Arch::Mips => (4, 4, 4, 8, Endian::Big, CalleeSave::AAPCS),
            Arch::Arm => (4, 4, 4, 8, Endian::Little, CalleeSave::AAPCS),
        };
        // 第一性原理: Target::resolve() 只负责"几何"参数 (指针/字节序/调用约定),
        // 不管 codegen 后端是否实现。是否真的能 emit 是 codegen::emit() 关心的事。
        Self {
            triple: t.clone(),
            arch: t.arch,
            ptr_size,
            int_size,
            long_size,
            llong_size,
            endian,
            callee_save,
            is_linux: matches!(t.os, Os::Linux | Os::Freestanding),
            red_zone: true,
            frame_pointer: false,
            sse: true,
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
            cpu_features: if t.arch == Arch::X86_64 {
                CpuFeatures::detect()
            } else {
                CpuFeatures::default()
            },
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
        // AVX2 依赖 AVX: 若 AVX 被禁用, AVX2 也必须禁用
        if !self.cpu_features.avx {
            self.cpu_features.avx2 = false;
            self.cpu_features.fma = false;
        }
        // AVX-512 依赖 AVX: 若 AVX 被禁用, AVX-512 也必须禁用
        if !self.cpu_features.avx {
            self.cpu_features.avx512f = false;
            self.cpu_features.avx512dq = false;
            self.cpu_features.avx512bw = false;
            self.cpu_features.avx512vl = false;
        }
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
}
