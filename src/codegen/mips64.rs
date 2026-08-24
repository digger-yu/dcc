// src/codegen/mips64.rs - MIPS64 后端 (N64 ABI, 复用 mips.rs)

use crate::codegen::mips::MipsCodegen;
use crate::ir::Module;
use crate::target::Target;

/// MIPS64 N64 ABI 后端
/// 复用 MipsCodegen 核心结构，设置 is_64bit 标志
pub struct Mips64Codegen {
    inner: MipsCodegen,
}

impl Mips64Codegen {
    pub fn new(target: Target, relocatable: bool) -> Self {
        let mut inner = MipsCodegen::new(target, relocatable);
        inner.set_64bit();
        Self { inner }
    }

    pub fn set_source_name(&mut self, name: impl Into<String>) {
        self.inner.set_source_name(name);
    }

    pub fn set_asm_text_mode(&mut self, v: bool) {
        self.inner.set_asm_text_mode(v);
    }

    pub fn push_extra_section(&mut self, sec: crate::codegen::elf::ElfSection) {
        self.inner.push_extra_section(sec);
    }

    pub fn func_frames(&self, module: &Module) -> Vec<crate::debug::FuncFrame> {
        self.inner.func_frames(module)
    }

    pub fn compile(&mut self, module: &Module) {
        self.inner.compile(module);
    }

    pub fn build_output(&self) -> Vec<u8> {
        self.inner.build_output()
    }

    pub fn build_asm_output(&self, module: &Module) -> String {
        self.inner.build_asm_output(module)
    }

    pub fn labels(&self) -> &std::collections::HashMap<String, u64> {
        &self.inner.labels
    }

    pub fn func_sizes(&self) -> &std::collections::HashMap<String, u64> {
        &self.inner.func_sizes
    }
}
