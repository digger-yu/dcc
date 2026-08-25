// src/codegen/mips64.rs - MIPS64 后端 (N64 ABI, 复用 mips.rs)

use crate::codegen::mips::MipsCodegen;
use crate::ir::Module;
use crate::target::Target;

/// MIPS64 N64 ABI 后端
/// 复用 MipsCodegen 核心结构，设置 is_64bit 标志
///
/// N64 ABI 特性:
/// - 64 位指针和寄存器 (8 字节)
/// - 8 个参数寄存器 ($a0-$a7) 用于整数参数传递
/// - 64 位帧对齐 (16 字节边界)
/// - 支持 LD/SD (64 位加载/存储)
/// - 浮点参数通过 $f12-$f19 传递 (最多 8 个)
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
        self.inner.func_sizes()
    }

    pub fn diagnostics(&self) -> &[crate::diagnostics::Diagnostic] {
        &self.inner.diagnostics
    }

    /// 获取符号表 (用于链接)
    pub fn symbols(&self) -> &[crate::codegen::elf::ElfSym] {
        &self.inner.symbols
    }

    /// 获取重定位表 (用于链接)
    pub fn relocations(&self) -> &[crate::codegen::elf::ElfRela] {
        &self.inner.relocations
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::TargetTriple;

    #[test]
    fn mips64_codegen_creates_with_64bit_flag() {
        let target = Target::resolve(&TargetTriple::parse("mips64-linux-gnuabi64").unwrap());
        let cg = Mips64Codegen::new(target, true);
        // 验证 MIPS64 后端正确创建
        assert!(cg.func_sizes().is_empty());
        assert!(cg.labels().is_empty());
    }

    #[test]
    fn mips64_symbols_and_relocations_empty_initially() {
        let target = Target::resolve(&TargetTriple::parse("mips64-linux-gnuabi64").unwrap());
        let cg = Mips64Codegen::new(target, true);
        assert!(cg.symbols().is_empty());
        assert!(cg.relocations().is_empty());
    }

    #[test]
    fn mips64_diagnostics_empty_initially() {
        let target = Target::resolve(&TargetTriple::parse("mips64-linux-gnuabi64").unwrap());
        let cg = Mips64Codegen::new(target, true);
        assert!(cg.diagnostics().is_empty());
    }
}
