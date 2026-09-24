// src/codegen/registry.rs - Backend registration and discovery
// 第一性原理：单一 truth 源，避免 has_backend 手写谎言

use crate::cli::Arch;
use crate::codegen::shared::backend::{Backend, CommonImage, CompileInput};
use crate::codegen::shared::features::FeatureSet;
use crate::codegen::x86_64;
use crate::diagnostics::Diagnostic;
use crate::target::Target;

use crate::codegen::mips;
use crate::codegen::mips::mips64;
use crate::codegen::riscv64;
use crate::codegen::{aarch64, arm};

/// Backend implementation wrapper for type erasure
pub enum AnyBackend {
    X86_64(x86_64::Codegen),
    AArch64(aarch64::Aarch64Codegen),
    Arm(arm::ArmCodegen),
    Mips(mips::MipsCodegen),
    Mips64(mips64::Mips64Codegen),
    RiscV64(riscv64::Riscv64Codegen),
}

impl Backend for AnyBackend {
    fn arch(&self) -> Arch {
        match self {
            AnyBackend::X86_64(_) => Arch::X86_64,
            AnyBackend::AArch64(_) => Arch::AArch64,
            AnyBackend::Arm(_) => Arch::Arm,
            AnyBackend::Mips(_) => Arch::Mips,
            AnyBackend::Mips64(_) => Arch::Mips64,
            AnyBackend::RiscV64(_) => Arch::RiscV64,
        }
    }

    fn features(&self) -> FeatureSet {
        // 单一 truth 源: 能力清单由各 arch 自己的 `impl Backend` 持有,
        // 包装层不得再抄一份.
        match self {
            AnyBackend::X86_64(cg) => cg.features(),
            AnyBackend::AArch64(cg) => cg.features(),
            AnyBackend::Arm(cg) => cg.features(),
            AnyBackend::Mips(cg) => cg.features(),
            AnyBackend::Mips64(cg) => cg.features(),
            AnyBackend::RiscV64(cg) => cg.features(),
        }
    }

    fn compile(&mut self, input: &CompileInput<'_>) -> Result<CommonImage, Vec<Diagnostic>> {
        // UFCS: 各 arch 的 Codegen 都有 inherent `compile(&mut self, module: &Module)`,
        // 方法调用语法会优先选中 inherent 版本而导致类型不匹配, 必须显式走 trait.
        match self {
            AnyBackend::X86_64(cg) => <x86_64::Codegen as Backend>::compile(cg, input),
            AnyBackend::AArch64(cg) => <aarch64::Aarch64Codegen as Backend>::compile(cg, input),
            AnyBackend::Arm(cg) => <arm::ArmCodegen as Backend>::compile(cg, input),
            AnyBackend::Mips(cg) => <mips::MipsCodegen as Backend>::compile(cg, input),
            AnyBackend::Mips64(cg) => <mips64::Mips64Codegen as Backend>::compile(cg, input),
            AnyBackend::RiscV64(cg) => <riscv64::Riscv64Codegen as Backend>::compile(cg, input),
        }
    }

    fn render_asm(&mut self, input: &CompileInput<'_>) -> Result<Vec<u8>, Vec<Diagnostic>> {
        match self {
            AnyBackend::X86_64(cg) => <x86_64::Codegen as Backend>::render_asm(cg, input),
            AnyBackend::AArch64(cg) => <aarch64::Aarch64Codegen as Backend>::render_asm(cg, input),
            AnyBackend::Arm(cg) => <arm::ArmCodegen as Backend>::render_asm(cg, input),
            AnyBackend::Mips(cg) => <mips::MipsCodegen as Backend>::render_asm(cg, input),
            AnyBackend::Mips64(cg) => <mips64::Mips64Codegen as Backend>::render_asm(cg, input),
            AnyBackend::RiscV64(cg) => <riscv64::Riscv64Codegen as Backend>::render_asm(cg, input),
        }
    }

    fn build_object(&mut self) -> Result<Vec<u8>, Vec<Diagnostic>> {
        match self {
            AnyBackend::X86_64(cg) => <x86_64::Codegen as Backend>::build_object(cg),
            AnyBackend::AArch64(cg) => <aarch64::Aarch64Codegen as Backend>::build_object(cg),
            AnyBackend::Arm(cg) => <arm::ArmCodegen as Backend>::build_object(cg),
            AnyBackend::Mips(cg) => <mips::MipsCodegen as Backend>::build_object(cg),
            AnyBackend::Mips64(cg) => <mips64::Mips64Codegen as Backend>::build_object(cg),
            AnyBackend::RiscV64(cg) => <riscv64::Riscv64Codegen as Backend>::build_object(cg),
        }
    }
}

/// Backend Registry - Single source of truth for supported backends
///
/// **Usage:**
/// 1. `backend_for(arch, target, relocatable)` returns Option<Box<dyn Backend>>
/// 2. If None, the backend is disabled (not just limited)
/// 3. `has_backend()` must stay consistent with the match arms above
///
/// **Uninstall procedure:**
/// When removing a backend:
/// - Remove from this match statement
/// - Update `has_backend()` to return false
/// - Delete the `emit` arm in mod.rs
/// - Update CI matrix and documentation
/// - Remove from parity test table
#[inline]
#[allow(unused_variables)] // relocatable is forwarded to arm/mips/riscv backends, which are gated by cargo features
pub fn backend_for(
    arch: Arch,
    target: &Target,
    relocatable: bool,
) -> Option<Box<dyn Backend + 'static>> {
    match arch {
        Arch::X86_64 => Some(Box::new(AnyBackend::X86_64(x86_64::Codegen::new(target.clone())))),
        Arch::AArch64 => Some(Box::new(AnyBackend::AArch64(aarch64::Aarch64Codegen::new(
            target.clone(),
            relocatable,
        )))),
        Arch::Arm => {
            Some(Box::new(AnyBackend::Arm(arm::ArmCodegen::new(target.clone(), relocatable))))
        },
        Arch::Mips => {
            Some(Box::new(AnyBackend::Mips(mips::MipsCodegen::new(target.clone(), relocatable))))
        },
        Arch::Mips64 => Some(Box::new(AnyBackend::Mips64(mips64::Mips64Codegen::new(
            target.clone(),
            relocatable,
        )))),
        Arch::RiscV64 => Some(Box::new(AnyBackend::RiscV64(riscv64::Riscv64Codegen::new(
            target.clone(),
            relocatable,
        )))),

        // Disabled backends:
        Arch::RiscV32 => None, // See emit E0399 "Not Yet Implemented"

                               // Future platforms can be added here
    }
}

/// Check if a backend exists for the given architecture
///
/// 本函数不维持自己的清单，而是直接委托给 [`crate::cli::Arch::has_backend`]——
/// `Arch` 是定义方、位于最低层，无循环依赖。以前这里是一份手写的
/// `matches!`，与 `cli.rs` 里那份并存而仅靠人肉同步，现在收敛到只一份。
///
/// [`backend_for`] 的 match 分支因为需构造具体类型而无法表达，由
/// `registry_backend_list_is_consistent` 测试机性地钉住它与清单的一致。
///
/// # Examples
///
/// ```
/// use dcc::cli::Arch;
/// use dcc::codegen::shared::registry::has_backend;
///
/// assert!(has_backend(Arch::X86_64));
/// assert!(!has_backend(Arch::RiscV32));
/// ```
pub fn has_backend(arch: Arch) -> bool {
    arch.has_backend()
}

/// Get list of all enabled backends
///
/// 由 `Arch::ALL` + `has_backend()` 推导，不再硬编码一份 Vec (那是第三份清单)。
pub fn enabled_backends() -> Vec<Arch> {
    Arch::ALL
        .iter()
        .copied()
        .filter(|arch| has_backend(*arch))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Abi, Os, TargetTriple};

    /// `backend_for()` 的 match 分支集合必须与 `Arch::has_backend()` 清单严格一致。
    ///
    /// 这是"卸一个后端只需改一处"能真正成立的护栏: 往 `backend_for` 加了 arm
    /// 却忘改 `has_backend` (或反过来)，本测试直接红。
    #[test]
    fn registry_backend_list_is_consistent() {
        let target = Target::resolve(&TargetTriple {
            arch: Arch::X86_64,
            vendor: "unknown".into(),
            os: Os::Linux,
            abi: Abi::Gnu,
        });
        for arch in Arch::ALL {
            let registered = backend_for(arch, &target, true).is_some();
            assert_eq!(
                registered,
                has_backend(arch),
                "{arch:?}: backend_for() 与 has_backend() 不一致 (一个已注册/一个未注册)"
            );
        }
    }

    /// `enabled_backends()` 不得漏项也不得多报 disabled 后端。
    #[test]
    fn enabled_backends_matches_arch_list() {
        let enabled = enabled_backends();
        assert_eq!(enabled.len(), Arch::ALL.iter().filter(|a| a.has_backend()).count());
        assert!(enabled.contains(&Arch::X86_64));
        assert!(!enabled.contains(&Arch::RiscV32));
    }

    /// 能力清单必须由 arch impl 自己回答，包装层不得改变结果。
    #[test]
    fn anybackend_features_delegates_to_inner() {
        let target = Target::resolve(&TargetTriple {
            arch: Arch::X86_64,
            vendor: "unknown".into(),
            os: Os::Linux,
            abi: Abi::Gnu,
        });
        let boxed = backend_for(Arch::X86_64, &target, true).expect("x86_64 must be registered");
        let via_any = boxed.features();
        let via_direct = crate::codegen::x86_64::Codegen::new(target.clone()).features();
        assert_eq!(via_any.features, via_direct.features);
    }
}
