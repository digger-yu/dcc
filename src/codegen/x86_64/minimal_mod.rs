// src/codegen/x86_64.rs - x86_64 代码生成模块入口
// 第一性原理：x86_64 指令编码固定，寄存器按 SysV ABI 选。
// suckless：直接 emit 字节，不引入汇编库。
#![allow(clippy::too_many_arguments)]
#![allow(dead_code)]
#![allow(unused_imports)]

use crate::ast::*;
use crate::codegen::shared::elf::{
    ElfRela, ElfSection, ElfSym, R_X86_64_32, R_X86_64_32S, R_X86_64_64, R_X86_64_PC32,
    R_X86_64_PLT32, SHF_ALLOC, SHF_EXECINSTR, SHF_GNU_RETAIN, SHF_WRITE, SHN_UNDEF, SHT_PROGBITS,
    STB_GLOBAL, STB_LOCAL, STB_WEAK, STT_FUNC, STT_GNU_IFUNC, STT_NOTYPE, STT_OBJECT, STT_SECTION,
    STT_TLS, STV_DEFAULT, STV_HIDDEN, STV_PROTECTED,
};
use crate::const_eval::{self, ConstValue};
use crate::diagnostics::{Diagnostic, Span};
use crate::preproc::LineSpan;
use crate::target::Target;
use crate::semantics::{ObjectIdentity, StorageClass, StorageInfo, StorageType};
use std::collections::{HashMap, HashSet};

/// x86_64 代码生成器 - 公共 API 重新导出链
mod asm;
mod backend;
mod codegen;
mod abi;
mod emit;
mod mem;
mod parallel;
mod special;

// ============================================================================
// Struct Definitions (must remain in mod.rs for public visibility)
// ============================================================================
pub use codegen::Codegen;

// ============================================================================
// Public Type Exports from Original mod.rs (moved to codegen module)
// These are re-exported through the codegen submodule.
// ============================================================================
pub use self::codegen::{JumpKind, AbiClass, ArgLoc};

// ============================================================================
// Additional Types (if they exist in extracted modules)
// ============================================================================
pub use self::codegen::{PendingJump, CustomSecRel, DeferredPushsectionFixup, FuncAsmLayout};

// ============================================================================
// ASM Constants (re-exported from extracted asm module if needed)
// ============================================================================
pub use self::asm::{
    ASM_REG_CR, ASM_REG_DR, ASM_REG_K, ASM_REG_MMX, ASM_REG_ST, ASM_REG_TMM,
    ASM_REG_XMM, ASM_REG_YMM, ASM_REG_ZMM,
};

// ============================================================================
// Minimal Structural Markers
// ============================================================================
impl Codegen {
    // Placeholder impl block - no methods (all delegated to extracted modules)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Abi, Arch, Os, TargetTriple};

    fn x86_codegen() -> Codegen {
        let triple = TargetTriple {
            arch: Arch::X86_64,
            vendor: "unknown".into(),
            os: Os::Linux,
            abi: Abi::Gnu,
        };
        Codegen::new(Target::resolve(&triple))
    }
}
