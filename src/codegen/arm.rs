// src/codegen/arm.rs - ARM 后端 (AAPCS)

use crate::ast::{FuncDecl, Program};
use crate::codegen::elf::{
    self, ElfRela, ElfSection, ElfSym, STB_GLOBAL, STT_FUNC,
};
use crate::ir::{Function, Global, Module, ValueType};
use crate::target::Target;
use std::collections::HashMap;
use std::sync::Arc;

/// ARM AAPCS 后端
pub struct ArmCodegen {
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
    _target: Target,
}

impl ArmCodegen {
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
            _target: target,
        }
    }

    pub fn set_source_name(&mut self, name: impl Into<String>) {
        self.source_name = name.into();
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
                    self.func_sizes
                        .get(&f.name)
                        .map(|&sz| crate::debug::FuncFrame::new(pc, sz as u32))
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

        // 函数序言：保存 lr 和 fp
        self.emit_push(vec![11, 14]); // push {fp, lr}
        self.emit_mov(11, 13);       // mov fp, sp

        // 简化：函数体只生成返回 0
        self.emit_mov(0, 0);         // mov r0, #0

        // 函数尾声
        self.emit_mov(13, 11);       // mov sp, fp
        self.emit_pop(vec![11, 15]); // pop {fp, pc}

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

    // ARM 指令编码
    fn emit_push(&mut self, regs: Vec<u8>) {
        // STMFD sp!, {regs}
        let mut reg_list: u16 = 0;
        for r in regs {
            reg_list |= 1 << r;
        }
        let inst = 0xE92D0000 | reg_list as u32;
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    fn emit_pop(&mut self, regs: Vec<u8>) {
        // LDMFD sp!, {regs}
        let mut reg_list: u16 = 0;
        for r in regs {
            reg_list |= 1 << r;
        }
        let inst = 0xE8BD0000 | reg_list as u32;
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    fn emit_mov(&mut self, rd: u8, rm: u8) {
        // MOV rd, rm (条件码 AL = 0xE)
        let inst = 0xE1A00000 | ((rd as u32) << 12) | (rm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    pub fn build_output(&self) -> Vec<u8> {
        if self.relocatable {
            elf::build_elf32_arm(
                &self.code,
                &self.rodata,
                &self.symbols,
                &self.relocations,
                &self.rodata_relocations,
                &self.extra_sections,
            )
        } else {
            panic!("ARM executable output not yet supported");
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
                out.push_str(&format!("\t@ ARM IR lowering, {} bytes\n", size));
            }
            out.push_str("\tpush {fp, lr}\n");
            out.push_str("\tmov r0, #0\n");
            out.push_str("\tpop {fp, pc}\n");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arm_push_encoding() {
        let mut cg = ArmCodegen::new(
            Target::resolve(&crate::cli::TargetTriple::parse("arm-linux-gnueabihf").unwrap()),
            true,
        );
        cg.emit_push(vec![11, 14]);
        assert_eq!(cg.code.len(), 4);
        // push {fp, lr} => 0xE92D4800
        let inst = u32::from_le_bytes(cg.code[0..4].try_into().unwrap());
        assert_eq!(inst, 0xE92D4800);
    }

    #[test]
    fn arm_mov_encoding() {
        let mut cg = ArmCodegen::new(
            Target::resolve(&crate::cli::TargetTriple::parse("arm-linux-gnueabihf").unwrap()),
            true,
        );
        cg.emit_mov(0, 0);
        // mov r0, r0 => 0xE1A00000
        let inst = u32::from_le_bytes(cg.code[0..4].try_into().unwrap());
        assert_eq!(inst, 0xE1A00000);
    }

    #[test]
    fn arm_pop_encoding() {
        let mut cg = ArmCodegen::new(
            Target::resolve(&crate::cli::TargetTriple::parse("arm-linux-gnueabihf").unwrap()),
            true,
        );
        cg.emit_pop(vec![11, 15]);
        // pop {fp, pc} => 0xE8BD8800
        let inst = u32::from_le_bytes(cg.code[0..4].try_into().unwrap());
        assert_eq!(inst, 0xE8BD8800);
    }
}
