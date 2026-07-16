// src/codegen/aarch64.rs - AArch64 最小后端 (M7 hello world)
// 第一性原理: M7 先跑通 AAPCS64 返回常量，再扩展完整 IR lowering。

use crate::ast::{Decl, Expr, Program, Stmt, BlockItem};
use crate::codegen::elf::{self, ElfRela, ElfSym, STB_GLOBAL, STT_FUNC};
use crate::target::Target;

pub struct Aarch64Codegen {
    code: Vec<u8>,
    entry: u64,
    relocatable: bool,
    symbols: Vec<ElfSym>,
    relocations: Vec<ElfRela>,
    func_sizes: std::collections::HashMap<String, u64>,
    labels: std::collections::HashMap<String, u64>,
    _target: Target,
}

impl Aarch64Codegen {
    pub fn new(target: Target, relocatable: bool) -> Self {
        Self {
            code: Vec::new(),
            entry: if relocatable { 0 } else { 0x400000 + 64 + 56 },
            relocatable,
            symbols: Vec::new(),
            relocations: Vec::new(),
            func_sizes: std::collections::HashMap::new(),
            labels: std::collections::HashMap::new(),
            _target: target,
        }
    }

    pub fn compile(&mut self, prog: &Program) {
        if self.relocatable {
            self.compile_rel(prog);
        } else {
            self.compile_exec(prog);
        }
    }

    fn compile_exec(&mut self, prog: &Program) {
        let main_ret = main_return_value(prog).unwrap_or(0);
        self.label("_start");
        let bl_pos = self.pos();
        self.emit_bl_placeholder();
        self.mov_x8_imm(93);
        self.svc_zero();
        self.label("main");
        let main_start = self.pos();
        self.mov_w0_imm(main_ret as u32);
        self.ret();
        self.func_sizes.insert("main".to_string(), self.pos() - main_start);
        self.patch_bl(bl_pos, main_start);
    }

    fn compile_rel(&mut self, prog: &Program) {
        let main_ret = main_return_value(prog).unwrap_or(0);
        self.label("main");
        let main_start = self.pos();
        self.mov_w0_imm(main_ret as u32);
        self.ret();
        let size = self.pos() - main_start;
        self.func_sizes.insert("main".to_string(), size);
        self.symbols.push(ElfSym {
            name: "main".to_string(),
            section: 1,
            value: 0,
            size,
            bind: STB_GLOBAL,
            ty: STT_FUNC,
            visibility: elf::STV_DEFAULT,
        });
    }

    pub fn build_output(&self) -> Vec<u8> {
        if self.relocatable {
            elf::build_elf_rel_aarch64(&self.code, &self.symbols, &self.relocations)
        } else {
            elf::build_elf_aarch64(&self.code, self.entry)
        }
    }

    fn pos(&self) -> u64 {
        self.code.len() as u64
    }

    fn label(&mut self, name: &str) {
        self.labels.insert(name.to_string(), self.pos());
    }

    fn emit(&mut self, bytes: &[u8]) {
        self.code.extend_from_slice(bytes);
    }

    /// MOVZ Wd, #imm16
    fn mov_w0_imm(&mut self, imm: u32) {
        let insn = 0x5280_0000u32 | ((imm & 0xffff) << 5);
        self.emit(&insn.to_le_bytes());
    }

    fn mov_x8_imm(&mut self, imm: u64) {
        let insn = 0xd280_0000u32 | (((imm & 0xffff) as u32) << 5) | 8;
        self.emit(&insn.to_le_bytes());
    }

    fn ret(&mut self) {
        self.emit(&[0xc0, 0x03, 0x5f, 0xd6]);
    }

    fn svc_zero(&mut self) {
        self.emit(&[0x01, 0x00, 0x00, 0xd4]);
    }

    fn emit_bl_placeholder(&mut self) {
        self.emit(&[0x00, 0x00, 0x00, 0x94]);
    }

    fn patch_bl(&mut self, at: u64, target: u64) {
        let pc = at + 4;
        let imm26 = ((target as i64 - pc as i64) >> 2) as u32 & 0x03ff_ffff;
        let insn = 0x9400_0000u32 | imm26;
        let off = at as usize;
        self.code[off..off + 4].copy_from_slice(&insn.to_le_bytes());
    }
}

fn main_return_value(prog: &Program) -> Option<i128> {
    let main_fn = prog.decls.iter().find_map(|d| match d {
        Decl::Func(f) if f.name.name == "main" => Some(f),
        _ => None,
    })?;
    let body = main_fn.body.as_ref()?;
    for item in &body.items {
        if let BlockItem::Stmt(Stmt::Return { val: Some(e), .. }) = item {
            if let Expr::Int { v, .. } = e {
                return Some(*v);
            }
            if let Ok(v) = crate::const_eval::eval(e) {
                return Some(v);
            }
        }
    }
    None
}
