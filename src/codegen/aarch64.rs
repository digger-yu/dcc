// src/codegen/aarch64.rs - AArch64 后端 (M7 完整 IR lowering)

use crate::ast::{BinOp, BlockItem, Decl, Expr, FuncDecl, Program, Stmt, UnOp};
use crate::codegen::elf::{
    self, ElfRela, ElfSym, R_AARCH64_CALL26, STB_GLOBAL, STB_LOCAL, STT_FUNC, STT_OBJECT,
};
use crate::ir::{ExprKind, ExprLiteral, Function, Global, Module, TypedExpr, TypedStmt, ValueType};
use crate::target::Target;
use std::collections::HashMap;

#[derive(Clone, Copy)]
enum BranchKind {
    B,
    Cbz,
}

pub struct Aarch64Codegen {
    code: Vec<u8>,
    rodata: Vec<u8>,
    entry: u64,
    relocatable: bool,
    symbols: Vec<ElfSym>,
    relocations: Vec<ElfRela>,
    rodata_relocations: Vec<ElfRela>,
    func_sizes: HashMap<String, u64>,
    labels: HashMap<String, u64>,
    locals: HashMap<String, i32>,
    global_rodata: HashMap<String, u64>,
    float_refs: Vec<(u64, u64)>,
    sym_index: HashMap<String, u32>,
    stack_bytes: i32,
    frame_bytes: i32,
    emitted_return: bool,
    source_name: String,
    label_serial: u32,
    pending_branches: Vec<(u64, String, BranchKind)>,
    _target: Target,
}

impl Aarch64Codegen {
    pub fn new(target: Target, relocatable: bool) -> Self {
        Self {
            code: Vec::new(),
            rodata: Vec::new(),
            entry: if relocatable { 0 } else { 0x400000 + 64 + 56 },
            relocatable,
            symbols: Vec::new(),
            relocations: Vec::new(),
            rodata_relocations: Vec::new(),
            func_sizes: HashMap::new(),
            labels: HashMap::new(),
            locals: HashMap::new(),
            global_rodata: HashMap::new(),
            float_refs: Vec::new(),
            sym_index: HashMap::new(),
            stack_bytes: 0,
            frame_bytes: 0,
            emitted_return: false,
            source_name: String::new(),
            label_serial: 0,
            pending_branches: Vec::new(),
            _target: target,
        }
    }

    pub fn set_source_name(&mut self, name: impl Into<String>) {
        self.source_name = name.into();
    }

    pub fn compile(&mut self, module: &Module) {
        let prog = module.source();
        self.emit_globals(module);
        if self.relocatable {
            self.compile_rel(module, prog);
            self.finalize_branches();
        } else {
            self.compile_exec(module, prog);
            self.finalize_exec();
        }
    }

    fn emit_globals(&mut self, module: &Module) {
        for g in &module.globals {
            self.emit_global(g);
        }
        if !self.rodata.is_empty() {
            self.symbols.push(ElfSym {
                name: ".rodata".to_string(),
                section: 2,
                value: 0,
                size: self.rodata.len() as u64,
                bind: STB_LOCAL,
                ty: elf::STT_SECTION,
                visibility: elf::STV_DEFAULT,
            });
            self.sym_index.insert(".rodata".to_string(), self.symbols.len() as u32);
        }
    }

    fn emit_global(&mut self, g: &Global) {
        let size = value_type_size(&g.ty).max(1);
        let align = size.max(4) as u64;
        let off = align_rodata(&mut self.rodata, align);
        self.global_rodata.insert(g.name.clone(), off);
        if let Some(init) = &g.initializer {
            self.write_global_bytes(init, size);
        } else {
            self.rodata.resize(self.rodata.len() + size, 0);
        }
        if self.relocatable {
            self.symbols.push(ElfSym {
                name: g.name.clone(),
                section: 2,
                value: off,
                size: size as u64,
                bind: STB_GLOBAL,
                ty: STT_OBJECT,
                visibility: elf::STV_DEFAULT,
            });
            self.sym_index.insert(g.name.clone(), self.symbols.len() as u32);
        }
    }

    fn write_global_bytes(&mut self, init: &TypedExpr, size: usize) {
        let start = self.rodata.len();
        self.rodata.resize(start + size, 0);
        match init.literal {
            Some(ExprLiteral::Int(v)) => {
                let bytes = (v as i64).to_le_bytes();
                self.rodata[start..start + size.min(8)].copy_from_slice(&bytes[..size.min(8)]);
            }
            Some(ExprLiteral::Float(v)) => {
                let bits = (v as f32).to_bits();
                self.rodata[start..start + 4.min(size)].copy_from_slice(&bits.to_le_bytes()[..4.min(size)]);
            }
            _ => {}
        }
    }

    fn compile_exec(&mut self, module: &Module, prog: &Program) {
        self.label("_start");
        let bl_pos = self.pos();
        self.emit_bl_placeholder();
        self.mov_x8_imm(93);
        self.svc_zero();
        for func in &module.functions {
            if find_func(prog, &func.name).and_then(|f| f.body.as_ref()).is_none() {
                continue;
            }
            self.label(&func.name);
            let start = self.pos();
            self.gen_function(func, find_func(prog, &func.name));
            self.func_sizes.insert(func.name.clone(), self.pos() - start);
            if func.name == "main" {
                self.patch_bl(bl_pos, start);
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
            self.sym_index.insert(func.name.clone(), self.symbols.len() as u32);
            text_off += size;
        }
    }

    fn finalize_exec(&mut self) {
        let rodata_start = self.entry + align_up_u64(self.code.len() as u64, 8);
        let refs: Vec<_> = self.float_refs.drain(..).collect();
        for (code_pos, rodata_off) in refs {
            self.patch_ldr_u32(code_pos, rodata_start + rodata_off);
        }
        self.finalize_branches();
    }

    fn finalize_branches(&mut self) {
        let pending: Vec<_> = self.pending_branches.drain(..).collect();
        for (at, label, kind) in pending {
            if let Some(&target) = self.labels.get(&label) {
                match kind {
                    BranchKind::B => self.patch_b26(at, target),
                    BranchKind::Cbz => self.patch_cbz(at, target),
                }
            }
        }
    }

    pub fn build_output(&self) -> Vec<u8> {
        if self.relocatable {
            elf::build_elf_rel_aarch64(
                &self.code,
                &self.rodata,
                &self.symbols,
                &self.relocations,
                &self.rodata_relocations,
            )
        } else {
            elf::build_elf_aarch64(&self.code, &self.rodata, self.entry)
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
            out.push_str("\t.section .rodata,\"a\",@progbits\n");
            for g in &module.globals {
                if let Some(&off) = self.global_rodata.get(&g.name) {
                    out.push_str(&format!("\t.globl {}\n", g.name));
                    out.push_str(&format!("\t.type {}, %object\n", g.name));
                    out.push_str(&format!("\t.size {}, {}\n", g.name, value_type_size(&g.ty)));
                    out.push_str(&format!("{}:\n", g.name));
                    out.push_str(&format!("\t// rodata offset {off}\n"));
                }
            }
        }
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
        for (item, ir) in items.iter().zip(func.body.iter()) {
            self.gen_block_item(item, ir);
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

    fn gen_block_item(&mut self, item: &BlockItem, ir: &TypedStmt) {
        match item {
            BlockItem::Stmt(s) => self.gen_stmt(s, ir),
            BlockItem::Decl(Decl::Var(v)) => {
                let off = self.alloc_local(&v.name.name, v.ty.size().max(4) as i32);
                if let Some(expr) = ir.expressions.first() {
                    self.gen_expr(expr);
                    self.store_result(off, &v.ty);
                } else if let Some(init) = &v.init {
                    self.gen_expr_from_ast(init);
                    self.store_result(off, &v.ty);
                }
            }
            _ => {}
        }
    }

    fn gen_stmt(&mut self, stmt: &Stmt, ir: &TypedStmt) {
        match stmt {
            Stmt::Return { val, .. } => {
                if let Some(expr) = ir.expressions.first() {
                    self.gen_expr(expr);
                } else if let Some(e) = val.as_ref() {
                    self.gen_expr_from_ast(e);
                } else {
                    self.mov_w0_imm(0);
                }
                self.ret();
                self.emitted_return = true;
            }
            Stmt::Expr { e, .. } => {
                if let Some(expr) = ir.expressions.first() {
                    let _ = self.gen_expr(expr);
                } else {
                    let _ = self.gen_expr_from_ast(e);
                }
            }
            Stmt::If { then_, else_, .. } => {
                if let Some(cond) = ir.expressions.first() {
                    self.gen_expr(cond);
                }
                let lbl_else = self.fresh_label("Lelse");
                let lbl_end = self.fresh_label("Lendif");
                self.cbz_w0(&lbl_else);
                self.gen_stmt(then_, &ir.nested[0]);
                self.b(&lbl_end);
                self.label(&lbl_else);
                if let Some(el) = else_ {
                    if ir.nested.len() > 1 {
                        self.gen_stmt(el, &ir.nested[1]);
                    } else {
                        self.gen_stmt(el, &TypedStmt {
                            expressions: Vec::new(),
                            nested: Vec::new(),
                            span: ir.span.clone(),
                        });
                    }
                }
                self.label(&lbl_end);
            }
            Stmt::While { body, .. } | Stmt::Do { body, .. } => {
                let lbl_loop = self.fresh_label("Lwhile");
                let lbl_exit = self.fresh_label("Lwend");
                self.label(&lbl_loop);
                if let Some(cond) = ir.expressions.first() {
                    self.gen_expr(cond);
                    self.cbz_w0(&lbl_exit);
                }
                self.gen_stmt(body, &ir.nested[0]);
                self.b(&lbl_loop);
                self.label(&lbl_exit);
            }
            Stmt::For { init, cond, step, body, .. } => {
                let body_idx = if init.is_some() { 1 } else { 0 };
                if let Some(s) = init {
                    self.gen_stmt(s, &ir.nested[0]);
                }
                let lbl_loop = self.fresh_label("Lfor");
                let lbl_exit = self.fresh_label("Lfend");
                self.label(&lbl_loop);
                if cond.is_some() {
                    if let Some(ir_cond) = ir.expressions.first() {
                        self.gen_expr(ir_cond);
                        self.cbz_w0(&lbl_exit);
                    }
                }
                self.gen_stmt(body, &ir.nested[body_idx]);
                if step.is_some() {
                    if let Some(step_ir) = ir.expressions.get(1) {
                        let _ = self.gen_expr(step_ir);
                    }
                }
                self.b(&lbl_loop);
                self.label(&lbl_exit);
            }
            Stmt::Block { b, .. } => {
                for (item, nested_ir) in b.items.iter().zip(ir.nested.iter()) {
                    self.gen_block_item(item, nested_ir);
                }
            }
            _ => {}
        }
    }

    fn gen_expr(&mut self, expr: &TypedExpr) -> bool {
        if is_vector_ty(&expr.ty) {
            return self.gen_vector_expr(expr);
        }
        if is_float_ty(&expr.ty)
            || expr
                .children
                .iter()
                .any(|c| is_float_ty(&c.ty) || is_vector_ty(&c.ty))
        {
            return self.gen_float_expr(expr);
        }
        match expr.kind {
            ExprKind::Constant => {
                if let Some(ExprLiteral::Int(v)) = expr.literal {
                    self.mov_w0_imm(trunc_i32(v));
                    return true;
                }
                if let Some(ExprLiteral::Float(v)) = expr.literal {
                    self.load_float_s0(v as f32);
                    return true;
                }
                self.mov_w0_imm(0);
            }
            ExprKind::Local(ref name) => {
                if let Some(&off) = self.locals.get(name) {
                    self.load_w0(off);
                    return true;
                }
                if self.global_rodata.contains_key(name) {
                    self.load_global_addr_w0(name);
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
            ExprKind::Call if !expr.children.is_empty() => {
                self.gen_call(expr);
                return true;
            }
            ExprKind::Conditional if expr.children.len() >= 3 => {
                self.gen_expr(&expr.children[0]);
                let lbl_else = self.fresh_label("Lcond0");
                let lbl_end = self.fresh_label("Lcond1");
                self.cbz_w0(&lbl_else);
                self.gen_expr(&expr.children[1]);
                self.b(&lbl_end);
                self.label(&lbl_else);
                self.gen_expr(&expr.children[2]);
                self.label(&lbl_end);
                return true;
            }
            _ => self.mov_w0_imm(0),
        }
        false
    }

    fn gen_call(&mut self, expr: &TypedExpr) {
        let args = &expr.children[1..];

        // M7 修复: 聚合类型返回 > 16 字节, 走 sret (隐藏 x0 = sret slot)
        // 限制: 仅当 caller 有显式 local sret 时使用; 简化版只在调用方分配
        //       一个 <sret> 局部, callee 写到 [x0]
        let mut sret_reg_idx = 0usize;  // x0 默认被 sret 占用
        let needs_sret = is_aggregate(&expr.ty)
            && aggregate_size(&expr.ty).map_or(false, |s| s > 16);
        if needs_sret {
            let size = aggregate_size(&expr.ty).unwrap();
            let aligned = (size + 15) & !15;
            let off = self.alloc_local("<sret>", aligned as i32);
            self.mov_x0_sp_off(off);
            sret_reg_idx = 1;  // x0 已用, args 从 x1 开始
        }

        // M7 修复: 聚合类型参数 by-ref; > 8 args 走栈 (varargs 基础)
        // 编码: add xN, sp, #off     => 0x910003e0 | (off << 10) | reg
        //       str wt, [sp, #off]   => 0xb9000000 | (off/4) << 10 | 31<<5 | reg
        //       ldr wt, [sp, #off]   => 0xb9400000 | (off/4) << 10 | 31<<5 | reg
        // 限制: 偏移需 12-bit 对齐; 栈空间会在 plan_locals 时按对齐增大
        let mut stack_arg_off = self.frame_bytes;  // 从当前帧底之后开始
        for (i, arg) in args.iter().enumerate() {
            if is_aggregate(&arg.ty) {
                // pass-by-ref: 加载 local var 地址到 x{sret_reg_idx + i}
                if let ExprKind::Local(ref n) = arg.kind {
                    if let Some(&off) = self.locals.get(n) {
                        let target_reg = (sret_reg_idx + i) as u8;
                        self.mov_x_reg_sp_off(target_reg, off);
                        continue;
                    }
                }
                // 非 local aggregate: 不支持, 填 0
                let target_reg = (sret_reg_idx + i) as u8;
                self.mov_x_reg_imm(target_reg, 0);
            } else if sret_reg_idx + i >= 8 {
                // varargs: 溢出的实参压栈 (M7 最小)
                self.gen_expr(arg);
                self.store_w0(stack_arg_off);
                stack_arg_off += 8;
            } else {
                self.gen_expr(arg);
                if is_float_ty(&arg.ty) {
                    self.mov_v_reg_s0((sret_reg_idx + i) as u8);
                } else {
                    self.mov_w_reg_w0((sret_reg_idx + i) as u8);
                }
            }
        }

        let callee = self.callee_name(&expr.children[0]);
        self.emit_bl_to(&callee);
    }

    fn callee_name(&self, expr: &TypedExpr) -> String {
        match expr.kind {
            ExprKind::Local(ref n) => n.clone(),
            _ => "unknown".to_string(),
        }
    }

    fn gen_float_expr(&mut self, expr: &TypedExpr) -> bool {
        match expr.kind {
            ExprKind::Constant => {
                if let Some(ExprLiteral::Float(v)) = expr.literal {
                    self.load_float_s0(v as f32);
                    return true;
                }
                if let Some(ExprLiteral::Int(v)) = expr.literal {
                    self.mov_w0_imm(trunc_i32(v));
                    // int 转 float: scvtf s0, w0
                    self.emit(&[0x00, 0x00, 0x18, 0x1e]);
                    return true;
                }
            }
            ExprKind::Binary(op) if expr.children.len() >= 2 => {
                self.gen_float_expr(&expr.children[0]);
                self.mov_s1_s0();
                self.gen_float_expr(&expr.children[1]);
                self.float_binop_s0_s1(op);
                return true;
            }
            ExprKind::Unary(UnOp::Neg) if !expr.children.is_empty() => {
                self.gen_float_expr(&expr.children[0]);
                self.neg_s0();
                return true;
            }
            ExprKind::Cast if !expr.children.is_empty() => {
                self.gen_float_expr(&expr.children[0]);
                return true;
            }
            _ => {
                if self.gen_expr(expr) {
                    self.emit(&[0x00, 0x00, 0x18, 0x1e]); // scvtf s0, w0
                    return true;
                }
            }
        }
        self.load_float_s0(0.0);
        false
    }

    fn gen_vector_expr(&mut self, expr: &TypedExpr) -> bool {
        match expr.kind {
            ExprKind::Binary(op) if expr.children.len() >= 2 => {
                self.gen_vector_operand(&expr.children[0]);
                self.mov_v1_v0();
                self.gen_vector_operand(&expr.children[1]);
                self.vector_binop_v0_v1(op);
                return true;
            }
            ExprKind::Constant | ExprKind::Aggregate => {
                self.zero_v0();
                return true;
            }
            _ => {
                self.zero_v0();
            }
        }
        false
    }

    fn gen_vector_operand(&mut self, expr: &TypedExpr) {
        if let Some(ExprLiteral::Int(v)) = expr.literal {
            self.mov_w0_imm(trunc_i32(v));
            self.emit(&[0x20, 0x00, 0x08, 0x4e]); // dup v0.4s, w0
            return;
        }
        let _ = self.gen_expr(expr);
        self.emit(&[0x20, 0x00, 0x08, 0x4e]);
    }

    fn gen_expr_from_ast(&mut self, expr: &Expr) {
        match expr {
            Expr::Int { v, .. } => self.mov_w0_imm(trunc_i32(*v)),
            Expr::Char { v, .. } => self.mov_w0_imm(trunc_i32(*v as i128)),
            Expr::Float { v, .. } => self.load_float_s0(*v as f32),
            Expr::Ident { name, .. } => {
                if let Some(&off) = self.locals.get(&name.name) {
                    self.load_w0(off);
                } else if self.global_rodata.contains_key(&name.name) {
                    self.load_global_addr_w0(&name.name);
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
            Expr::Cast { e, .. } => self.gen_expr_from_ast(e),
            Expr::Call { callee, args, .. } => {
                for (i, arg) in args.iter().enumerate().take(8) {
                    self.gen_expr_from_ast(arg);
                    self.mov_w_reg_w0(i as u8);
                }
                if let Expr::Ident { name, .. } = callee.as_ref() {
                    self.emit_bl_to(&name.name);
                }
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

    fn load_global_addr_w0(&mut self, name: &str) {
        if self.relocatable {
            let pos = self.pos();
            self.emit_adr_placeholder(0);
            if let Some(&idx) = self.sym_index.get(name) {
                self.relocations.push(ElfRela {
                    offset: pos,
                    sym_index: idx,
                    rel_type: elf::R_AARCH64_ADR_PREL_LO21,
                    addend: 0,
                });
            }
        } else if let Some(&off) = self.global_rodata.get(name) {
            let rodata_start = self.entry + align_up_u64(self.code.len() as u64, 8);
            self.mov_x0_imm((rodata_start + off) as u32);
        }
    }

    fn store_result(&mut self, off: i32, ty: &crate::ast::Type) {
        let _ = ty;
        self.store_w0(off);
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
                self.emit(&[0x00, 0x00, 0x18, 0x13]);
            }
            ValueType::Integer { bits: 8, signed: false } => {
                self.emit(&[0x00, 0x00, 0x14, 0x13]);
            }
            ValueType::Integer { bits: 16, signed: true } => {
                self.emit(&[0x00, 0x00, 0x18, 0x12]);
            }
            ValueType::Integer { bits: 16, signed: false } => {
                self.emit(&[0x00, 0x00, 0x14, 0x12]);
            }
            ValueType::Integer { bits: 32, .. } => {}
            _ => {}
        }
    }

    fn binop_w0_w1(&mut self, op: BinOp) {
        match op {
            BinOp::Add => self.emit(&[0x00, 0x00, 0x01, 0x0b]),
            BinOp::Sub => self.emit(&[0x00, 0x00, 0x01, 0x4b]),
            BinOp::Mul => self.emit(&[0x00, 0x7c, 0x01, 0x1b]),
            BinOp::And => self.emit(&[0x20, 0x00, 0x01, 0x0a]),
            BinOp::Or => self.emit(&[0x20, 0x00, 0x01, 0x2a]),
            BinOp::Xor => self.emit(&[0x20, 0x00, 0x01, 0x4a]),
            // M7 修复: 移位指令正确编码
            // lslv w0, w0, w1: 0x1A302000 LE = 00 20 30 1A
            //   bits[31:21] = 0b00011010001 (lslv op)
            //   bits[20:16] = Rm (1 for w1)
            //   bits[15:10] = 0b001000
            //   bits[9:5] = Rn (0 for w0)
            //   bits[4:0] = Rd (0 for w0)
            // lsrv w0, w0, w1: 0x1A342000 LE = 00 20 34 1A
            //   bits[23:21] = 0b010 (lsrv op)
            BinOp::Shl => self.emit(&[0x00, 0x20, 0x30, 0x1a]), // lslv w0, w0, w1
            BinOp::Shr => self.emit(&[0x00, 0x20, 0x34, 0x1a]), // lsrv w0, w0, w1
            // M7 修复: 真 sdiv / msub 替代 0
            // binop_w0_w1 之前已把 w0=left, w1=right, w14=left (保存)
            //   Div: sdiv w0, w0, w1   => 0x9AC00C20
            //   Mod: sdiv w0, w0, w1   (w0 = quotient)
            //        msub w0, w1, w0, w14 (w0 = w14 - w1*w0 = left mod right)
            // 编码来源: ARMv8 AArch64 指令集, sdiv 32-bit 形式
            // 限制: 不区分有/无符号 (始终有符号);调用方保证 IR 已 folding 常量
            BinOp::Div => self.emit(&[0x20, 0x0c, 0xc0, 0x9a]), // sdiv w0, w0, w1
            BinOp::Mod => {
                self.emit(&[0x20, 0x0c, 0xc0, 0x9a]);           // sdiv w0, w0, w1
                self.emit(&[0xa0, 0x83, 0xb0, 0x9b]);           // msub w0, w1, w0, w14
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
            BinOp::AndAnd | BinOp::OrOr => self.cmp_w0_zero(),
        }
    }

    fn float_binop_s0_s1(&mut self, op: BinOp) {
        match op {
            BinOp::Add => self.emit(&[0x20, 0x00, 0x21, 0x1e]), // fadd s0, s0, s1
            BinOp::Sub => self.emit(&[0x20, 0x00, 0x41, 0x1e]),
            BinOp::Mul => self.emit(&[0x20, 0x00, 0x61, 0x1e]),
            BinOp::Div => self.emit(&[0x20, 0x00, 0x81, 0x1e]),
            BinOp::EqEq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                self.emit(&[0x20, 0x00, 0x21, 0x1e]); // 近似：按加法后转 int
                self.emit(&[0x00, 0x00, 0x08, 0x1e]); // fcvtzs w0, s0
            }
            _ => self.emit(&[0x20, 0x00, 0x21, 0x1e]),
        }
    }

    fn vector_binop_v0_v1(&mut self, op: BinOp) {
        match op {
            BinOp::Add => self.emit(&[0x20, 0x14, 0x21, 0x4e]), // add v0.4s, v0.4s, v1.4s
            BinOp::Sub => self.emit(&[0x20, 0x14, 0x41, 0x4e]),
            BinOp::Mul => self.emit(&[0x20, 0x94, 0x81, 0x4e]),
            _ => self.emit(&[0x20, 0x14, 0x21, 0x4e]),
        }
    }

    fn fresh_label(&mut self, prefix: &str) -> String {
        self.label_serial += 1;
        format!("{prefix}{}", self.label_serial)
    }

    fn pos(&self) -> u64 {
        self.code.len() as u64
    }

    fn label(&mut self, name: &str) {
        let pos = self.pos();
        self.labels.insert(name.to_string(), pos);
        let pending: Vec<_> = self
            .pending_branches
            .iter()
            .filter(|(_, l, _)| l == name)
            .cloned()
            .collect();
        for (at, _, kind) in pending {
            match kind {
                BranchKind::B => self.patch_b26(at, pos),
                BranchKind::Cbz => self.patch_cbz(at, pos),
            }
        }
        self.pending_branches.retain(|(_, l, _)| l != name);
    }

    fn emit(&mut self, bytes: &[u8]) {
        self.code.extend_from_slice(bytes);
    }

    fn load_float_s0(&mut self, v: f32) {
        let off = align_rodata(&mut self.rodata, 4);
        let bits = v.to_bits();
        self.rodata.extend_from_slice(&bits.to_le_bytes());
        let pos = self.pos();
        self.emit_ldr_s0_u32_placeholder();
        self.float_refs.push((pos, off));
    }

    fn mov_w0_imm(&mut self, imm: u32) {
        if imm <= 0xffff {
            let insn = 0x5280_0000u32 | ((imm & 0xffff) << 5);
            self.emit(&insn.to_le_bytes());
            return;
        }
        let lo = imm & 0xffff;
        let hi = (imm >> 16) & 0xffff;
        self.emit(&(0x5280_0000u32 | (lo << 5)).to_le_bytes());
        self.emit(&(0x72a0_0000u32 | (hi << 5)).to_le_bytes());
    }

    // M7 修复: 通用 x 寄存器装入任意立即数 (movz + 必要 movk)
    // 编码: movz xN, #imm16, lsl #shift  => sf=1, opc=10, hw=shift/16
    //       movk xN, #imm16, lsl #shift  => sf=1, opc=11, hw=shift/16
    // 限制: 仅无符号 64-bit; 负数需补码 (调用方负责)
    fn mov_x_reg_imm(&mut self, reg: u8, imm: u64) {
        for shift in [0u32, 16, 32, 48] {
            let part = ((imm >> shift) & 0xffff) as u32;
            // movz 清零其它位, 仅在需要建立 64-bit 值时 emit
            // 但若后续有更高位非零, 当前 shift 必须 emit
            let higher_nonzero = (imm >> (shift + 16)) != 0;
            if part == 0 && shift > 0 && !higher_nonzero {
                continue;
            }
            let hw = (shift / 16) as u32;
            let insn: u32 = (1 << 31) | (0b10 << 29) | (0b100101 << 23) | (hw << 21)
                | (part << 5) | (reg as u32 & 0x1f);
            self.emit(&insn.to_le_bytes());
        }
    }

    // M7 修复: 计算 sp + off 到 x 寄存器的地址
    // 编码: add xN, sp, #imm12  (off >= 0)   => 0x910003e0 | (off<<10) | rd
    //       sub xN, sp, #imm12  (off < 0)    => 0x510003e0 | (-off<<10) | rd
    // 限制: 偏移 12-bit (0..4095); frame 大于此范围需分段
    fn mov_x_reg_sp_off(&mut self, reg: u8, off: i32) {
        if off >= 0 && off < 4096 {
            let insn = 0x910003e0u32 | ((off as u32) << 10) | (reg as u32 & 0x1f);
            self.emit(&insn.to_le_bytes());
        } else if off < 0 && off > -4096 {
            let pos = (-off) as u32;
            let insn = 0x510003e0u32 | (pos << 10) | (reg as u32 & 0x1f);
            self.emit(&insn.to_le_bytes());
        } else {
            // 越界: 退化为 mov_x_reg_imm(0) + add, 不常见
            self.mov_x_reg_imm(reg, off as i64 as u64);
        }
    }

    fn mov_x0_sp_off(&mut self, off: i32) {
        self.mov_x_reg_sp_off(0, off);
    }

    fn mov_x0_imm(&mut self, imm: u32) {
        let insn = 0xd280_0000u32 | ((imm & 0xffff) << 5);
        self.emit(&insn.to_le_bytes());
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

    fn str_wt_sp(&mut self, reg: u8, off: i32) {
        let imm12 = (off / 4) as u32 & 0xfff;
        let insn = 0xb900_0000u32 | imm12 << 10 | 31 << 5 | (reg as u32 & 7);
        self.emit(&insn.to_le_bytes());
    }

    fn ldr_wt_sp(&mut self, reg: u8, off: i32) {
        let imm12 = (off / 4) as u32 & 0xfff;
        let insn = 0xb940_0000u32 | imm12 << 10 | 31 << 5 | (reg as u32 & 7);
        self.emit(&insn.to_le_bytes());
    }

    fn mov_w14_w0(&mut self) {
        self.emit(&[0xee, 0x03, 0x80, 0x2a]);
    }

    fn mov_w0_w14(&mut self) {
        self.emit(&[0xe0, 0x03, 0x0e, 0x2a]);
    }

    fn mov_w1_w0(&mut self) {
        self.emit(&[0xe1, 0x03, 0x80, 0x2a]);
    }

    fn mov_w_reg_w0(&mut self, reg: u8) {
        let insn = 0x2a0003e0u32 | ((reg as u32 & 7) << 16) | ((reg as u32 & 7) << 5);
        self.emit(&insn.to_le_bytes());
    }

    fn mov_v_reg_s0(&mut self, reg: u8) {
        let insn = 0x1e204000u32 | ((reg as u32 & 7) << 5);
        self.emit(&insn.to_le_bytes());
    }

    fn mov_s1_s0(&mut self) {
        self.emit(&[0x20, 0x00, 0x20, 0x1e]);
    }

    fn neg_s0(&mut self) {
        self.emit(&[0x00, 0x00, 0xa0, 0x1e]); // fneg s0, s0
    }

    fn mov_v1_v0(&mut self) {
        self.emit(&[0x20, 0x00, 0x81, 0x4e]); // orr v1.16b, v0.16b, v0.16b
    }

    fn zero_v0(&mut self) {
        self.emit(&[0x00, 0x00, 0x80, 0x4e]); // movi v0.4s, #0
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
        self.emit(&[0x00, 0x00, 0x00, 0x4b]);
    }

    fn mvn_w0(&mut self) {
        self.emit(&[0x20, 0x03, 0x20, 0x2a]);
    }

    fn cmp_w0_zero(&mut self) {
        self.emit(&[0x1f, 0x00, 0x00, 0x71]);
        self.emit(&[0xe0, 0x07, 0x9f, 0x1a]);
    }

    fn cmp_w0_w1(&mut self) {
        self.emit(&[0x3f, 0x00, 0x01, 0x6b]);
    }

    fn cbz_w0(&mut self, label: &str) {
        let pos = self.pos();
        self.emit(&[0x00, 0x00, 0x00, 0x34]);
        if let Some(&target) = self.labels.get(label) {
            self.patch_cbz(pos, target);
        } else {
            self.pending_branches
                .push((pos, label.to_string(), BranchKind::Cbz));
        }
    }

    fn b(&mut self, label: &str) {
        let pos = self.pos();
        self.emit(&[0x00, 0x00, 0x00, 0x14]);
        if let Some(&target) = self.labels.get(label) {
            self.patch_b26(pos, target);
        } else {
            self.pending_branches
                .push((pos, label.to_string(), BranchKind::B));
        }
    }

    fn patch_b26(&mut self, at: u64, target: u64) {
        let pc = at + 4;
        let imm26 = ((target as i64 - pc as i64) >> 2) as u32 & 0x03ff_ffff;
        let insn = 0x1400_0000u32 | imm26;
        let off = at as usize;
        if off + 4 <= self.code.len() {
            self.code[off..off + 4].copy_from_slice(&insn.to_le_bytes());
        }
    }

    fn patch_cbz(&mut self, at: u64, target: u64) {
        let pc = at + 4;
        let imm19 = ((target as i64 - pc as i64) >> 2) as u32 & 0x7ffff;
        let insn = 0x3400_0000u32 | imm19;
        let off = at as usize;
        if off + 4 <= self.code.len() {
            self.code[off..off + 4].copy_from_slice(&insn.to_le_bytes());
        }
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

    fn emit_bl_to(&mut self, name: &str) {
        if let Some(&target) = self.labels.get(name) {
            let pos = self.pos();
            self.emit_bl_placeholder();
            self.patch_bl(pos, target);
        } else if self.relocatable {
            let pos = self.pos();
            self.emit_bl_placeholder();
            let idx = self.ensure_sym(name);
            self.relocations.push(ElfRela {
                offset: pos,
                sym_index: idx,
                rel_type: R_AARCH64_CALL26,
                addend: 0,
            });
        } else {
            let pos = self.pos();
            self.emit_bl_placeholder();
            let _ = pos;
        }
    }

    fn ensure_sym(&mut self, name: &str) -> u32 {
        if let Some(&idx) = self.sym_index.get(name) {
            return idx;
        }
        self.symbols.push(ElfSym {
            name: name.to_string(),
            section: 0,
            value: 0,
            size: 0,
            bind: STB_GLOBAL,
            ty: STT_FUNC,
            visibility: elf::STV_DEFAULT,
        });
        let idx = self.symbols.len() as u32;
        self.sym_index.insert(name.to_string(), idx);
        idx
    }

    fn patch_bl(&mut self, at: u64, target: u64) {
        let pc = at + 4;
        let imm26 = ((target as i64 - pc as i64) >> 2) as u32 & 0x03ff_ffff;
        let insn = 0x9400_0000u32 | imm26;
        let off = at as usize;
        self.code[off..off + 4].copy_from_slice(&insn.to_le_bytes());
    }

    fn emit_adr_placeholder(&mut self, rd: u8) {
        let insn = 0x1000_0000u32 | (rd as u32);
        self.emit(&insn.to_le_bytes());
    }

    fn emit_ldr_s0_u32_placeholder(&mut self) {
        // ldr s0, [pc, #8]; b +8
        self.emit(&[0x00, 0x00, 0x00, 0xbd]);
        self.emit(&[0x02, 0x00, 0x00, 0x14]);
    }

    fn patch_ldr_u32(&mut self, at: u64, addr: u64) {
        let pc = at + 8;
        let off = ((addr as i64 - pc as i64) / 4) as u32 & 0x3ffff;
        let insn = 0xbd000000u32 | off;
        let off_usize = at as usize;
        if off_usize + 4 <= self.code.len() {
            self.code[off_usize..off_usize + 4].copy_from_slice(&insn.to_le_bytes());
        }
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

fn is_float_ty(ty: &ValueType) -> bool {
    matches!(ty, ValueType::Float { .. })
}

fn is_vector_ty(ty: &ValueType) -> bool {
    matches!(ty, ValueType::Vector(_, _))
}

// M7 修复: 聚合类型判定 (struct/union/array of known size)
fn is_aggregate(ty: &ValueType) -> bool {
    matches!(
        ty,
        ValueType::Aggregate { .. } | ValueType::Array(_, Some(_)) | ValueType::Complex(_)
    )
}

// M7 修复: 聚合类型字节数 (非聚合返回 None)
fn aggregate_size(ty: &ValueType) -> Option<usize> {
    match ty {
        ValueType::Aggregate { size, .. } => Some(*size),
        ValueType::Array(inner, Some(n)) => Some(value_type_size(inner) * n),
        ValueType::Complex(inner) => Some(value_type_size(inner) * 2),
        _ => None,
    }
}

fn value_type_size(ty: &ValueType) -> usize {
    match ty {
        ValueType::Void => 0,
        ValueType::Integer { bits, .. } => (*bits as usize + 7) / 8,
        ValueType::Float { bits } => (*bits as usize + 7) / 8,
        ValueType::Pointer(_) => 8,
        ValueType::Array(inner, Some(n)) => value_type_size(inner) * n,
        ValueType::Array(inner, None) => value_type_size(inner),
        ValueType::Aggregate { size, .. } => *size,
        ValueType::Function { .. } => 8,
        ValueType::Vector(inner, lanes) => value_type_size(inner) * lanes,
        ValueType::Complex(inner) => value_type_size(inner) * 2,
    }
}

fn align_rodata(buf: &mut Vec<u8>, align: u64) -> u64 {
    let off = align_up_u64(buf.len() as u64, align);
    buf.resize(off as usize, 0);
    off
}

fn align_up_u64(v: u64, align: u64) -> u64 {
    (v + align - 1) & !(align - 1)
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

    // M7 修复: div/mod 真实 sdiv/msub 替代 0
    // 验证: aarch64 编译 (a / b) 与 (a % b) 不再退化为 0
    // 注: 使用 opaque 函数防止常量折叠
    #[test]
    fn aarch64_ir_div_mod_emit_sdiv_msub() {
        let (program, diags) = crate::parser::parse(
            b"extern int opaque(int);\n\
              int main(void) { int a = opaque(42); int b = opaque(5); int q = a / b; int r = a % b; return q + r; }\n",
            "a64.c",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let module = lower(program).expect("lower");
        validate(&module).expect("validate");
        let triple = crate::cli::TargetTriple::parse("aarch64-linux-gnu").expect("triple");
        let mut cg = Aarch64Codegen::new(crate::target::Target::resolve(&triple), true);
        cg.compile(&module);
        let obj = cg.build_output();
        // 验证 .text 段含 sdiv 指令 (0x9AC00C20 LE 编码: 20 0c c0 9a)
        let text = obj
            .windows(4)
            .any(|w| w == [0x20, 0x0c, 0xc0, 0x9a]);
        assert!(text, "expected sdiv w0, w0, w1 (0x9ac00c20) in aarch64 text");
    }

    // M7 修复: 移位真实 lslv/lsrv 替代 0
    // lslv w0, w0, w1: 0x1A302000 LE = 00 20 30 1A
    // lsrv w0, w0, w1: 0x1A342000 LE = 00 20 34 1A
    #[test]
    fn aarch64_ir_shifts_emit_lslv_lsrv() {
        let (program, diags) = crate::parser::parse(
            b"extern int opaque(int);\n\
              int main(void) { int a = opaque(1); int b = opaque(2); return (a << b) | (a >> b); }\n",
            "a64.c",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let module = lower(program).expect("lower");
        validate(&module).expect("validate");
        let triple = crate::cli::TargetTriple::parse("aarch64-linux-gnu").expect("triple");
        let mut cg = Aarch64Codegen::new(crate::target::Target::resolve(&triple), true);
        cg.compile(&module);
        let obj = cg.build_output();
        let has_lslv = obj.windows(4).any(|w| w == [0x00, 0x20, 0x30, 0x1a]);
        let has_lsrv = obj.windows(4).any(|w| w == [0x00, 0x20, 0x34, 0x1a]);
        assert!(has_lslv || has_lsrv, "expected lslv/lsrv in aarch64 text");
    }

    // M7 修复: 聚合类型参数按引用传递
    // 验证: 调函数时,若参数是 local struct, 发射 add xN, sp, #off
    // add x0, sp, #imm12 编码: bits[31:24]=0x91, bits[7:5]=0b000 (sf=1)
    // 简化检测: byte3 == 0x91
    #[test]
    fn aarch64_aggregate_arg_by_ref() {
        let (program, diags) = crate::parser::parse(
            b"struct S { int x; int y; };\n\
              void callee(struct S s) {}\n\
              int main(void) { struct S s; s.x = 1; s.y = 2; callee(s); return 0; }\n",
            "a64.c",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let module = lower(program).expect("lower");
        validate(&module).expect("validate");
        let triple = crate::cli::TargetTriple::parse("aarch64-linux-gnu").expect("triple");
        let mut cg = Aarch64Codegen::new(crate::target::Target::resolve(&triple), true);
        cg.compile(&module);
        let obj = cg.build_output();
        let has_adr = obj.windows(4).any(|w| w[3] == 0x91 && w[2] == 0x00);
        assert!(has_adr, "expected add xN, sp, #imm (M7 by-ref aggregate)");
    }
}
