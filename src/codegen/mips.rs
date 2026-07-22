// src/codegen/mips.rs - MIPS32 后端 (O32 ABI)

use crate::ast::{FuncDecl, Program};
use crate::codegen::elf::{
    self, ElfRela, ElfSection, ElfSym, STB_GLOBAL, STT_FUNC,
};
use crate::ir::{Function, Global, Module, ValueType};
use crate::target::Target;
use std::collections::HashMap;
use std::sync::Arc;

/// MIPS32 O32 ABI 后端
pub struct MipsCodegen {
    pub code: Vec<u8>,
    pub rodata: Vec<u8>,
    pub relocatable: bool,
    pub symbols: Vec<ElfSym>,
    pub relocations: Vec<ElfRela>,
    pub rodata_relocations: Vec<ElfRela>,
    pub func_sizes: HashMap<String, u64>,
    pub labels: HashMap<String, u64>,
    locals: HashMap<String, i32>,
    global_rodata: HashMap<String, u64>,
    sym_index: HashMap<String, u32>,
    stack_bytes: i32,
    frame_bytes: i32,
    source_name: String,
    pub extra_sections: Vec<ElfSection>,
    current_return_ty: Option<ValueType>,
    aggregate_fields: HashMap<String, (Arc<Vec<crate::ast::Field>>, bool)>,
    is_64bit: bool,
    _target: Target,
}

impl MipsCodegen {
    pub fn new(target: Target, relocatable: bool) -> Self {
        Self {
            code: Vec::new(),
            rodata: Vec::new(),
            relocatable,
            symbols: Vec::new(),
            relocations: Vec::new(),
            rodata_relocations: Vec::new(),
            func_sizes: HashMap::new(),
            labels: HashMap::new(),
            locals: HashMap::new(),
            global_rodata: HashMap::new(),
            sym_index: HashMap::new(),
            stack_bytes: 0,
            frame_bytes: 0,
            source_name: String::new(),
            extra_sections: Vec::new(),
            current_return_ty: None,
            aggregate_fields: HashMap::new(),
            is_64bit: false,
            _target: target,
        }
    }

    pub fn set_source_name(&mut self, name: impl Into<String>) {
        self.source_name = name.into();
    }

    pub fn set_64bit(&mut self) {
        self.is_64bit = true;
    }

    pub fn push_extra_section(&mut self, sec: ElfSection) {
        self.extra_sections.push(sec);
    }

    pub fn func_frames(&self, module: &Module) -> Vec<crate::debug::FuncFrame> {
        module
            .functions
            .iter()
            .filter_map(|f| {
                self.labels.get(&f.name).and_then(|&pc| {
                    self.func_sizes.get(&f.name).map(|&sz| crate::debug::FuncFrame {
                        pc,
                        size: sz as u32,
                    })
                })
            })
            .collect()
    }

    pub fn compile(&mut self, module: &Module) {
        let prog = module.source();
        self.emit_globals(module);
        if self.relocatable {
            self.compile_rel(module, prog);
        }
    }

    fn emit_globals(&mut self, module: &Module) {
        for g in &module.globals {
            if g.initializer.is_some() {
                self.emit_global_init(g);
            }
        }
    }

    fn emit_global_init(&mut self, g: &Global) {
        let offset = self.rodata.len() as u64;
        self.global_rodata.insert(g.name.clone(), offset);
        // 简化：全局变量初始化为 0
        self.rodata.extend_from_slice(&[0u8; 4]);
    }

    fn compile_rel(&mut self, module: &Module, _prog: &Program) {
        for func in &module.functions {
            self.gen_function(func, None);
        }
    }

    fn gen_function(&mut self, func: &Function, _ast: Option<&FuncDecl>) {
        let func_start = self.code.len() as u64;
        self.labels.insert(func.name.clone(), func_start);

        // 函数序言：保存 $ra 和 $fp
        self.emit_addiu(29, 29, -8); // addiu $sp, $sp, -8
        self.emit_sw(31, 29, 4);     // sw $ra, 4($fp)
        self.emit_sw(30, 29, 0);     // sw $fp, 0($fp)
        self.emit_addiu(30, 29, 0);  // move $fp, $sp

        // 简化：函数体只生成返回 0
        self.emit_addiu(2, 0, 0);    // move $v0, $zero

        // 函数尾声
        self.emit_lw(30, 29, 0);     // lw $fp, 0($sp)
        self.emit_lw(31, 29, 4);     // lw $ra, 4($sp)
        self.emit_addiu(29, 29, 8);  // addiu $sp, $sp, 8
        self.emit_jr(31);            // jr $ra
        self.emit_nop();             // delay slot

        let func_end = self.code.len() as u64;
        self.func_sizes.insert(func.name.clone(), func_end - func_start);

        // 添加符号
        let sym_idx = self.symbols.len() as u32;
        self.symbols.push(ElfSym {
            name: func.name.clone(),
            section: 1,
            value: func_start,
            size: func_end - func_start,
            bind: STB_GLOBAL,
            ty: STT_FUNC,
            visibility: 0,
        });
        self.sym_index.insert(func.name.clone(), sym_idx);
    }

    // MIPS 指令编码
    fn emit_addiu(&mut self, rt: u8, rs: u8, imm: i16) {
        let inst = (0b001001 << 26) | ((rs as u32) << 21) | ((rt as u32) << 16) | (imm as u16 as u32);
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    fn emit_sw(&mut self, rt: u8, base: u8, offset: i16) {
        let inst = (0b101011 << 26) | ((base as u32) << 21) | ((rt as u32) << 16) | (offset as u16 as u32);
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    fn emit_lw(&mut self, rt: u8, base: u8, offset: i16) {
        let inst = (0b100011 << 26) | ((base as u32) << 21) | ((rt as u32) << 16) | (offset as u16 as u32);
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    fn emit_jr(&mut self, rs: u8) {
        let inst = ((rs as u32) << 21) | 0b001000;
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    fn emit_nop(&mut self) {
        self.code.extend_from_slice(&[0u8; 4]);
    }

    pub fn build_output(&self) -> Vec<u8> {
        if self.relocatable {
            if self.is_64bit {
                elf::build_elf_rel_mips64(
                    &self.code,
                    &self.rodata,
                    &self.symbols,
                    &self.relocations,
                    &self.rodata_relocations,
                    &self.extra_sections,
                )
            } else {
                elf::build_elf32_mips(
                    &self.code,
                    &self.rodata,
                    &self.symbols,
                    &self.relocations,
                    &self.rodata_relocations,
                    &self.extra_sections,
                )
            }
        } else {
            // 非 relocatable 模式暂不支持
            panic!("MIPS executable output not yet supported");
        }
    }

    pub fn build_asm_output(&self, module: &Module) -> String {
        let mut out = String::new();
        let name = if self.source_name.is_empty() {
            "input.c".to_string()
        } else {
            self.source_name.clone()
        };
        out.push_str(&format!("\t.file \"{name}\"\n"));
        if !self.rodata.is_empty() {
            out.push_str("\t.section .rodata\n");
        }
        out.push_str("\t.text\n");
        for func in &module.functions {
            out.push_str(&format!("\t.globl {}\n", func.name));
            out.push_str(&format!("{}:\n", func.name));
            if let Some(size) = self.func_sizes.get(&func.name) {
                out.push_str(&format!("\t# MIPS IR lowering, {} bytes\n", size));
            }
            out.push_str("\tjr $ra\n");
            out.push_str("\tnop\n");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mips_addiu_encoding() {
        let mut cg = MipsCodegen::new(
            Target::resolve(&crate::cli::TargetTriple::parse("mips-linux-gnu").unwrap()),
            true,
        );
        cg.emit_addiu(29, 29, -8);
        assert_eq!(cg.code.len(), 4);
        // addiu $sp, $sp, -8 => 0x27bdfff8
        assert_eq!(cg.code, vec![0x27, 0xbd, 0xff, 0xf8]);
    }

    #[test]
    fn mips_jr_ra_encoding() {
        let mut cg = MipsCodegen::new(
            Target::resolve(&crate::cli::TargetTriple::parse("mips-linux-gnu").unwrap()),
            true,
        );
        cg.emit_jr(31);
        // jr $ra => 0x03e00008
        assert_eq!(cg.code, vec![0x03, 0xe0, 0x00, 0x08]);
    }

    #[test]
    fn mips_nop_encoding() {
        let mut cg = MipsCodegen::new(
            Target::resolve(&crate::cli::TargetTriple::parse("mips-linux-gnu").unwrap()),
            true,
        );
        cg.emit_nop();
        assert_eq!(cg.code, vec![0, 0, 0, 0]);
    }
}
