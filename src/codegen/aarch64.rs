// src/codegen/aarch64.rs - AArch64 后端 (M7)
// 通过 typed IR 表达式图 lowering；语句结构仍对照 AST 以保持控制流语义。

use crate::ast::{BinOp, BlockItem, Decl, Expr, FuncDecl, Program, Stmt, UnOp};
use crate::codegen::elf::{self, ElfRela, ElfSym, STB_GLOBAL, STT_FUNC};
use crate::ir::{ExprKind, ExprLiteral, Function, Module, TypedExpr, ValueType};
use crate::target::Target;
use std::collections::HashMap;

pub struct Aarch64Codegen {
    code: Vec<u8>,
    entry: u64,
    relocatable: bool,
    symbols: Vec<ElfSym>,
    relocations: Vec<ElfRela>,
    func_sizes: HashMap<String, u64>,
    labels: HashMap<String, u64>,
    locals: HashMap<String, i32>,
    stack_bytes: i32,
    frame_bytes: i32,
    emitted_return: bool,
    source_name: String,
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
            func_sizes: HashMap::new(),
            labels: HashMap::new(),
            locals: HashMap::new(),
            stack_bytes: 0,
            frame_bytes: 0,
            emitted_return: false,
            source_name: String::new(),
            _target: target,
        }
    }

    pub fn set_source_name(&mut self, name: impl Into<String>) {
        self.source_name = name.into();
    }

    pub fn compile(&mut self, module: &Module) {
        let prog = module.source();
        if self.relocatable {
            self.compile_rel(module, prog);
        } else {
            self.compile_exec(module, prog);
        }
    }

    fn compile_exec(&mut self, module: &Module, prog: &Program) {
        self.label("_start");
        let bl_pos = self.pos();
        self.emit_bl_placeholder();
        self.mov_x8_imm(93);
        self.svc_zero();
        for func in &module.functions {
            if func.name == "main" {
                self.label("main");
                let start = self.pos();
                self.gen_function(func, find_func(prog, "main"));
                self.func_sizes.insert("main".to_string(), self.pos() - start);
                self.patch_bl(bl_pos, start);
                break;
            }
        }
    }

    fn compile_rel(&mut self, module: &Module, prog: &Program) {
        let mut text_off = 0u64;
        for func in &module.functions {
            if find_func(prog, &func.name).and_then(|f| f.body.as_ref()).is_none() {
                continue;
            }
            self.label(&func.name);
            let start = self.pos();
            self.gen_function(func, find_func(prog, &func.name));
            let size = self.pos() - start;
            self.func_sizes.insert(func.name.clone(), size);
            self.symbols.push(ElfSym {
                name: func.name.clone(),
                section: 1,
                value: text_off,
                size,
                bind: STB_GLOBAL,
                ty: STT_FUNC,
                visibility: elf::STV_DEFAULT,
            });
            text_off += size;
        }
    }

    pub fn build_output(&self) -> Vec<u8> {
        if self.relocatable {
            elf::build_elf_rel_aarch64(&self.code, &self.symbols, &self.relocations)
        } else {
            elf::build_elf_aarch64(&self.code, self.entry)
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
        out.push_str("\t.text\n");
        for func in &module.functions {
            out.push_str(&format!("\t.globl {}\n", func.name));
            out.push_str(&format!("\t.type {}, %function\n", func.name));
            out.push_str(&format!("{}:\n", func.name));
            if let Some(size) = self.func_sizes.get(&func.name) {
                out.push_str(&format!("\t// IR lowering, {} bytes machine code\n", size));
            }
            out.push_str("\tret\n");
            out.push_str(&format!("\t.size {}, .-{}\n", func.name, func.name));
        }
        out
    }

    fn gen_function(&mut self, func: &Function, ast: Option<&FuncDecl>) {
        self.plan_locals(ast);
        self.frame_bytes = (self.stack_bytes + 15) & !15;
        if self.frame_bytes > 0 {
            self.sub_sp_imm(self.frame_bytes as u32);
        }
        self.emitted_return = false;
        let body = ast.and_then(|f| f.body.as_ref());
        let items = body.map(|b| b.items.as_slice()).unwrap_or(&[]);
        for (item, ir_stmt) in items.iter().zip(func.body.iter()) {
            self.gen_item(item, ir_stmt.expressions.first());
        }
        if !self.emitted_return {
            self.mov_w0_imm(0);
            self.ret();
        }
    }

    fn plan_locals(&mut self, ast: Option<&FuncDecl>) {
        self.locals.clear();
        self.stack_bytes = 0;
        let Some(body) = ast.and_then(|f| f.body.as_ref()) else {
            return;
        };
        for item in &body.items {
            if let BlockItem::Decl(Decl::Var(v)) = item {
                self.alloc_local(&v.name.name, v.ty.size().max(4) as i32);
            }
        }
    }

    fn gen_item(&mut self, item: &BlockItem, ir_expr: Option<&TypedExpr>) {
        match item {
            BlockItem::Stmt(Stmt::Return { val, .. }) => {
                if let Some(expr) = ir_expr {
                    self.gen_expr(expr);
                } else if let Some(e) = val.as_ref() {
                    self.gen_expr_from_ast(e);
                } else {
                    self.mov_w0_imm(0);
                }
                self.ret();
                self.emitted_return = true;
            }
            BlockItem::Stmt(Stmt::Expr { e, .. }) => {
                if let Some(expr) = ir_expr {
                    let _ = self.gen_expr(expr);
                } else {
                    let _ = self.gen_expr_from_ast(e);
                }
            }
            BlockItem::Decl(Decl::Var(v)) => {
                let off = self.alloc_local(&v.name.name, v.ty.size().max(4) as i32);
                if let Some(expr) = ir_expr {
                    self.gen_expr(expr);
                    self.store_w0(off);
                } else if let Some(init) = &v.init {
                    self.gen_expr_from_ast(init);
                    self.store_w0(off);
                }
            }
            _ => {}
        }
    }

    fn gen_expr(&mut self, expr: &TypedExpr) -> bool {
        match expr.kind {
            ExprKind::Constant => {
                if let Some(ExprLiteral::Int(v)) = expr.literal {
                    self.mov_w0_imm(trunc_i32(v));
                    return true;
                }
                self.mov_w0_imm(0);
            }
            ExprKind::Local(ref name) => {
                if let Some(&off) = self.locals.get(name) {
                    self.load_w0(off);
                    return true;
                }
                self.mov_w0_imm(0);
            }
            ExprKind::Binary(op) if expr.children.len() >= 2 => {
                self.gen_expr(&expr.children[0]);
                self.mov_w14_w0();
                self.gen_expr(&expr.children[1]);
                self.mov_w1_w0();
                self.mov_w0_w14();
                self.binop_w0_w1(op);
                return true;
            }
            ExprKind::Unary(op) if !expr.children.is_empty() => {
                self.gen_expr(&expr.children[0]);
                match op {
                    UnOp::Neg => self.neg_w0(),
                    UnOp::Plus => {}
                    UnOp::Not => self.cmp_w0_zero(),
                    UnOp::BitNot => self.mvn_w0(),
                }
                return true;
            }
            ExprKind::Cast if !expr.children.is_empty() => {
                self.gen_expr(&expr.children[0]);
                self.truncate_w0_to(&expr.ty);
                return true;
            }
            _ => self.mov_w0_imm(0),
        }
        false
    }

    fn gen_expr_from_ast(&mut self, expr: &Expr) {
        match expr {
            Expr::Int { v, .. } => self.mov_w0_imm(trunc_i32(*v)),
            Expr::Char { v, .. } => self.mov_w0_imm(trunc_i32(*v as i128)),
            Expr::Ident { name, .. } => {
                if let Some(&off) = self.locals.get(&name.name) {
                    self.load_w0(off);
                } else {
                    self.mov_w0_imm(0);
                }
            }
            Expr::Binop { op, l, r, .. } => {
                self.gen_expr_from_ast(l);
                self.mov_w14_w0();
                self.gen_expr_from_ast(r);
                self.mov_w1_w0();
                self.mov_w0_w14();
                self.binop_w0_w1(*op);
            }
            Expr::Unary { op, e, .. } => {
                self.gen_expr_from_ast(e);
                match op {
                    UnOp::Neg => self.neg_w0(),
                    UnOp::Plus => {}
                    UnOp::Not => self.cmp_w0_zero(),
                    UnOp::BitNot => self.mvn_w0(),
                }
            }
            Expr::Cast { e, .. } => {
                self.gen_expr_from_ast(e);
            }
            _ => {
                if let Ok(v) = crate::const_eval::eval(expr) {
                    self.mov_w0_imm(trunc_i32(v));
                } else {
                    self.mov_w0_imm(0);
                }
            }
        }
    }

    fn alloc_local(&mut self, name: &str, bytes: i32) -> i32 {
        let aligned = (bytes + 7) & !7;
        let off = self.stack_bytes;
        self.stack_bytes += aligned;
        self.locals.insert(name.to_string(), off);
        off
    }

    fn truncate_w0_to(&mut self, ty: &ValueType) {
        match ty {
            ValueType::Integer { bits: 8, signed: true } => {
                self.emit(&[0x00, 0x00, 0x18, 0x13]); // sxtb w0, w0
            }
            ValueType::Integer { bits: 8, signed: false } => {
                self.emit(&[0x00, 0x00, 0x14, 0x13]); // uxtb w0, w0
            }
            ValueType::Integer { bits: 16, signed: true } => {
                self.emit(&[0x00, 0x00, 0x18, 0x12]); // sxth w0, w0
            }
            ValueType::Integer { bits: 16, signed: false } => {
                self.emit(&[0x00, 0x00, 0x14, 0x12]); // uxth w0, w0
            }
            ValueType::Integer { bits: 32, .. } => {}
            _ => {}
        }
    }

    fn binop_w0_w1(&mut self, op: BinOp) {
        match op {
            BinOp::Add => self.emit(&[0x00, 0x00, 0x01, 0x0b]), // add w0, w0, w1
            BinOp::Sub => self.emit(&[0x00, 0x00, 0x01, 0x4b]), // sub w0, w0, w1
            BinOp::Mul => self.emit(&[0x00, 0x7c, 0x01, 0x1b]), // mul w0, w0, w1
            BinOp::And => self.emit(&[0x20, 0x00, 0x01, 0x0a]), // and w0, w0, w1
            BinOp::Or => self.emit(&[0x20, 0x00, 0x01, 0x2a]),  // orr w0, w0, w1
            BinOp::Xor => self.emit(&[0x20, 0x00, 0x01, 0x4a]), // eor w0, w0, w1
            BinOp::Shl => self.emit(&[0x20, 0x00, 0x1f, 0x1a]), // lsl w0, w0, w1
            BinOp::Shr => self.emit(&[0x20, 0x00, 0x41, 0x1a]), // lsr w0, w0, w1
            BinOp::Div | BinOp::Mod => {
                // M7: 无硬件除法序列，回退为 0
                self.mov_w0_imm(0);
            }
            BinOp::EqEq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                self.cmp_w0_w1();
                let cond = match op {
                    BinOp::EqEq => 0x1,
                    BinOp::Ne => 0x0,
                    BinOp::Lt => 0xb,
                    BinOp::Le => 0xa,
                    BinOp::Gt => 0xc,
                    BinOp::Ge => 0xd,
                    _ => 0x1,
                };
                let insn = 0x1a9f_0000u32 | (cond << 12) | 0x20;
                self.emit(&insn.to_le_bytes());
            }
            BinOp::AndAnd | BinOp::OrOr => {
                self.cmp_w0_zero();
            }
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

    fn mov_w0_imm(&mut self, imm: u32) {
        if imm <= 0xffff {
            let insn = 0x5280_0000u32 | ((imm & 0xffff) << 5);
            self.emit(&insn.to_le_bytes());
            return;
        }
        let lo = imm & 0xffff;
        let hi = (imm >> 16) & 0xffff;
        let insn0 = 0x5280_0000u32 | (lo << 5);
        let insn1 = 0x72a0_0000u32 | (hi << 5);
        self.emit(&insn0.to_le_bytes());
        self.emit(&insn1.to_le_bytes());
    }

    fn mov_x8_imm(&mut self, imm: u64) {
        let insn = 0xd280_0000u32 | (((imm & 0xffff) as u32) << 5) | 8;
        self.emit(&insn.to_le_bytes());
    }

    fn store_w0(&mut self, off: i32) {
        self.str_wt_sp(0, off);
    }

    fn load_w0(&mut self, off: i32) {
        self.ldr_wt_sp(0, off);
    }

    fn load_w1(&mut self, off: i32) {
        self.ldr_wt_sp(1, off);
    }

    /// STR Wt, [SP, #off]
    fn str_wt_sp(&mut self, reg: u8, off: i32) {
        let imm12 = (off / 4) as u32 & 0xfff;
        let insn = 0xb900_0000u32 | imm12 << 10 | 31 << 5 | (reg as u32 & 7);
        self.emit(&insn.to_le_bytes());
    }

    /// LDR Wt, [SP, #off]
    fn ldr_wt_sp(&mut self, reg: u8, off: i32) {
        let imm12 = (off / 4) as u32 & 0xfff;
        let insn = 0xb940_0000u32 | imm12 << 10 | 31 << 5 | (reg as u32 & 7);
        self.emit(&insn.to_le_bytes());
    }

    fn mov_w14_w0(&mut self) {
        // orr w14, wzr, w0
        self.emit(&[0xee, 0x03, 0x80, 0x2a]);
    }

    fn mov_w0_w14(&mut self) {
        // orr w0, wzr, w14
        self.emit(&[0xe0, 0x03, 0x0e, 0x2a]);
    }

    fn mov_w1_w0(&mut self) {
        // orr w1, wzr, w0
        self.emit(&[0xe1, 0x03, 0x80, 0x2a]);
    }

    fn sub_sp_imm(&mut self, bytes: u32) {
        if bytes == 0 {
            return;
        }
        if bytes <= 0xfff {
            let insn = 0xd100_0000u32 | ((bytes & 0xfff) << 10) | (31 << 5) | 31;
            self.emit(&insn.to_le_bytes());
        }
    }

    fn add_sp_imm(&mut self, bytes: u32) {
        if bytes == 0 {
            return;
        }
        if bytes <= 0xfff {
            let insn = 0x9100_0000u32 | ((bytes & 0xfff) << 10) | (31 << 5) | 31;
            self.emit(&insn.to_le_bytes());
        }
    }

    fn neg_w0(&mut self) {
        self.emit(&[0x00, 0x00, 0x00, 0x4b]); // neg w0, w0
    }

    fn mvn_w0(&mut self) {
        self.emit(&[0x20, 0x03, 0x20, 0x2a]); // orn w0, wzr, w0
    }

    fn cmp_w0_zero(&mut self) {
        self.emit(&[0x1f, 0x00, 0x00, 0x71]); // cmp w0, #0
        self.emit(&[0xe0, 0x07, 0x9f, 0x1a]); // cset w0, ne
    }

    fn cmp_w0_w1(&mut self) {
        self.emit(&[0x3f, 0x00, 0x01, 0x6b]); // cmp w0, w1
    }

    fn ret(&mut self) {
        if self.frame_bytes > 0 {
            self.add_sp_imm(self.frame_bytes as u32);
        }
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

fn find_func<'a>(prog: &'a Program, name: &str) -> Option<&'a FuncDecl> {
    prog.decls.iter().find_map(|d| match d {
        Decl::Func(f) if f.name.name == name => Some(f),
        _ => None,
    })
}

fn trunc_i32(v: i128) -> u32 {
    (v as i32) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{lower, validate};

    #[test]
    fn aarch64_ir_return_add() {
        let (program, diags) = crate::parser::parse(
            b"int main(void) { return 10 + 32; }\n",
            "a64.c",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let module = lower(program).expect("lower");
        validate(&module).expect("validate");
        let triple = crate::cli::TargetTriple::parse("aarch64-linux-gnu").expect("triple");
        let mut cg = Aarch64Codegen::new(crate::target::Target::resolve(&triple), true);
        cg.compile(&module);
        let obj = cg.build_output();
        assert_eq!(&obj[0..4], &[0x7f, b'E', b'L', b'F']);
        assert_eq!(u16::from_le_bytes([obj[18], obj[19]]), 183);
    }
}
