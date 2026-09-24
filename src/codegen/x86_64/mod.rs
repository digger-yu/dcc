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
use crate::semantics::{ObjectIdentity, StorageClass, StorageInfo, StorageType};
use crate::target::Target;
use std::collections::{HashMap, HashSet};

mod abi;
/// x86_64 代码生成器 - 所有子模块声明
mod asm;
mod backend;
mod codegen;
mod elf;
mod emit;
mod instructions;
mod mem;
mod parallel;
mod special;
mod stubs;

// ============================================================================
// Core Public Exports (from extracted modules)
// All production exports flow through this section.
// ============================================================================
pub use self::codegen::{
    AbiClass, ArgLoc, Codegen, CustomSecRel, DeferredPushsectionFixup, FuncAsmLayout,
    FuncEmitChunk, GlobalVar, JumpKind, ParallelEmitCtx, ParallelGlobalSnap, PendingCodeRel,
    PendingJump, VaListAddress,
};

// ============================================================================
// ASM Register Constants and Utilities
// ============================================================================
pub use self::asm::{
    parse_asm_operand_str, strip_asm_comment, AsmDirectiveExpander, AsmLocalLabels, AsmOperand,
    ASM_REG_CR, ASM_REG_DR, ASM_REG_K, ASM_REG_MMX, ASM_REG_ST, ASM_REG_TMM, ASM_REG_XMM,
    ASM_REG_YMM, ASM_REG_ZMM,
};

// No _impl modules - functionality extracted to dedicated modules (asm, codegen, etc.)

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
