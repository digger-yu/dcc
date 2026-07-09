// src/codegen/mod.rs - 代码生成调度
// 第一性原理: codegen 入口只做"按 target 选后端"。
// suckless 准则: 一行分发。

pub mod elf;
pub mod x86_64;

use crate::ast::Program;
use crate::target::Target;

pub fn emit(prog: &Program, target: &Target) -> (Vec<u8>, u64) {
    match target.arch {
        crate::cli::Arch::X86_64 => {
            let mut cg = x86_64::Codegen::new(target.clone());
            cg.compile(prog);
            let bytes = elf::build_elf_x86_64(&cg.code, cg.entry);
            (bytes, cg.entry)
        }
        a => panic!("arch {:?} not yet implemented (M0: x86_64 only)", a),
    }
}
