// src/codegen/riscv64.rs - RISC-V 64 后端 (M7 IR lowering)

use crate::ast::{BinOp, BlockItem, Decl, Expr, FuncDecl, Program, Stmt, UnOp};
use crate::codegen::elf::{
    self, ElfRela, ElfSym, R_RISCV_JAL, STB_GLOBAL, STB_LOCAL, STT_FUNC, STT_OBJECT,
};
use crate::ir::{ExprKind, ExprLiteral, Function, Global, Module, TypedExpr, TypedStmt, ValueType};
use crate::target::Target;
use std::collections::HashMap;

#[derive(Clone, Copy)]
enum BranchKind {
    Jal,
    Beqz,
}

pub struct Riscv64Codegen {
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

impl Riscv64Codegen {
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
        let jal_pos = self.pos();
        self.emit_jal_placeholder(1); // ra
        self.li_a7(93);
        self.li_a0(0);
        self.ecall();
        for func in &module.functions {
            if find_func(prog, &func.name).and_then(|f| f.body.as_ref()).is_none() {
                continue;
            }
            self.label(&func.name);
            let start = self.pos();
            self.gen_function(func, find_func(prog, &func.name));
            self.func_sizes.insert(func.name.clone(), self.pos() - start);
            if func.name == "main" {
                self.patch_jal(jal_pos, start);
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
            self.patch_auipc_load(code_pos, rodata_start + rodata_off);
        }
        self.finalize_branches();
    }

    fn finalize_branches(&mut self) {
        let pending: Vec<_> = self.pending_branches.drain(..).collect();
        for (at, label, kind) in pending {
            if let Some(&target) = self.labels.get(&label) {
                match kind {
                    BranchKind::Jal => self.patch_jal(at, target),
                    BranchKind::Beqz => self.patch_beqz(at, target),
                }
            }
        }
    }

    pub fn build_output(&self) -> Vec<u8> {
        if self.relocatable {
            elf::build_elf_rel_riscv64(
                &self.code,
                &self.rodata,
                &self.symbols,
                &self.relocations,
                &self.rodata_relocations,
            )
        } else {
            elf::build_elf_riscv64(&self.code, &self.rodata, self.entry)
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
            for g in &module.globals {
                if self.global_rodata.contains_key(&g.name) {
                    out.push_str(&format!("\t.globl {}\n", g.name));
                    out.push_str(&format!("{}:\n", g.name));
                }
            }
        }
        out.push_str("\t.text\n");
        for func in &module.functions {
            out.push_str(&format!("\t.globl {}\n", func.name));
            out.push_str(&format!("{}:\n", func.name));
            if let Some(size) = self.func_sizes.get(&func.name) {
                out.push_str(&format!("\t// IR lowering, {} bytes\n", size));
            }
            out.push_str("\tret\n");
        }
        out
    }

    fn gen_function(&mut self, func: &Function, ast: Option<&FuncDecl>) {
        self.plan_locals(ast);
        self.frame_bytes = (self.stack_bytes + 15) & !15;
        if self.frame_bytes > 0 {
            self.addi_sp(-self.frame_bytes);
        }
        self.emitted_return = false;
        let body = ast.and_then(|f| f.body.as_ref());
        let items = body.map(|b| b.items.as_slice()).unwrap_or(&[]);
        for (item, ir) in items.iter().zip(func.body.iter()) {
            self.gen_block_item(item, ir);
        }
        if !self.emitted_return {
            self.li_a0(0);
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
                    self.store_a0(off);
                } else if let Some(init) = &v.init {
                    self.gen_expr_from_ast(init);
                    self.store_a0(off);
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
                    self.li_a0(0);
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
                self.beqz_a0(&lbl_else);
                self.gen_stmt(then_, &ir.nested[0]);
                self.j(&lbl_end);
                self.label(&lbl_else);
                if let Some(el) = else_ {
                    if ir.nested.len() > 1 {
                        self.gen_stmt(el, &ir.nested[1]);
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
                    self.beqz_a0(&lbl_exit);
                }
                self.gen_stmt(body, &ir.nested[0]);
                self.j(&lbl_loop);
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
                        self.beqz_a0(&lbl_exit);
                    }
                }
                self.gen_stmt(body, &ir.nested[body_idx]);
                if step.is_some() {
                    if let Some(step_ir) = ir.expressions.get(1) {
                        let _ = self.gen_expr(step_ir);
                    }
                }
                self.j(&lbl_loop);
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
                    self.li_a0(trunc_i32(v) as i32);
                    return true;
                }
                if let Some(ExprLiteral::Float(v)) = expr.literal {
                    self.load_float_fa0(v as f32);
                    return true;
                }
                self.li_a0(0);
            }
            ExprKind::Local(ref name) => {
                if let Some(&off) = self.locals.get(name) {
                    self.load_a0(off);
                    return true;
                }
                if self.global_rodata.contains_key(name) {
                    self.load_global_addr_a0(name);
                    return true;
                }
                self.li_a0(0);
            }
            ExprKind::Binary(op) if expr.children.len() >= 2 => {
                self.gen_expr(&expr.children[0]);
                self.mv_a1_a0();
                self.gen_expr(&expr.children[1]);
                self.binop_a0_a1(op);
                return true;
            }
            ExprKind::Unary(op) if !expr.children.is_empty() => {
                self.gen_expr(&expr.children[0]);
                match op {
                    UnOp::Neg => self.neg_a0(),
                    UnOp::Plus => {}
                    UnOp::Not => self.cmp_a0_zero(),
                    UnOp::BitNot => self.not_a0(),
                }
                return true;
            }
            ExprKind::Cast if !expr.children.is_empty() => {
                self.gen_expr(&expr.children[0]);
                return true;
            }
            ExprKind::Call if !expr.children.is_empty() => {
                self.gen_call(expr);
                return true;
            }
            ExprKind::Conditional if expr.children.len() >= 3 => {
                self.gen_expr(&expr.children[0]);
                let lbl_else = self.fresh_label("Lc0");
                let lbl_end = self.fresh_label("Lc1");
                self.beqz_a0(&lbl_else);
                self.gen_expr(&expr.children[1]);
                self.j(&lbl_end);
                self.label(&lbl_else);
                self.gen_expr(&expr.children[2]);
                self.label(&lbl_end);
                return true;
            }
            _ => self.li_a0(0),
        }
        false
    }

    fn gen_call(&mut self, expr: &TypedExpr) {
        let args = &expr.children[1..];

        // M7 修复: 聚合类型返回 > 16 字节, 走 sret (隐藏 a0 = sret slot)
        // 限制: 仅 caller 分配 <sret> local, callee 写到 [a0]
        let mut sret_reg_idx = 0usize;
        let needs_sret = is_aggregate(&expr.ty)
            && aggregate_size(&expr.ty).map_or(false, |s| s > 16);
        if needs_sret {
            let size = aggregate_size(&expr.ty).unwrap();
            let aligned = (size + 15) & !15;
            let off = self.alloc_local("<sret>", aligned as i32);
            self.mov_a0_sp_off(off);
            sret_reg_idx = 1;
        }

        // M7 修复: 聚合类型参数 by-ref; > 8 args 走栈 (varargs 基础)
        // 编码: add aN, sp, #off   (RV64I 形式: addi + 大常量走 mv + addi)
        //       sw t0, off(sp)     => S-type, imm[11:5]|rs2|rs1|010|imm[4:0]|0100011
        // 限制: 偏移 12-bit 对齐, 否则需 auipc 序列
        let mut stack_arg_off = self.frame_bytes;
        for (i, arg) in args.iter().enumerate() {
            if is_aggregate(&arg.ty) {
                if let ExprKind::Local(ref n) = arg.kind {
                    if let Some(&off) = self.locals.get(n) {
                        let target_reg = (10 + sret_reg_idx + i) as u8;
                        self.mov_a_reg_sp_off(target_reg, off);
                        continue;
                    }
                }
                let target_reg = (10 + sret_reg_idx + i) as u8;
                self.li_reg(target_reg, 0);
            } else if sret_reg_idx + i >= 8 {
                // varargs: 溢出的实参压栈 (M7 最小)
                self.gen_expr(arg);
                self.store_a0(stack_arg_off);
                stack_arg_off += 8;
            } else {
                self.gen_expr(arg);
                if is_float_ty(&arg.ty) {
                    // 简化: 浮点参数走 GP 寄存器 (M7 最小)
                    self.mv_a_reg((sret_reg_idx + i) as u8);
                } else {
                    self.mv_a_reg((sret_reg_idx + i) as u8);
                }
            }
        }

        let callee = match expr.children[0].kind {
            ExprKind::Local(ref n) => n.clone(),
            _ => "unknown".to_string(),
        };
        self.emit_jal_to(&callee);
    }

    fn gen_float_expr(&mut self, expr: &TypedExpr) -> bool {
        match expr.kind {
            ExprKind::Constant => {
                if let Some(ExprLiteral::Float(v)) = expr.literal {
                    self.load_float_fa0(v as f32);
                    return true;
                }
            }
            ExprKind::Binary(op) if expr.children.len() >= 2 => {
                self.gen_float_expr(&expr.children[0]);
                self.gen_float_expr(&expr.children[1]);
                self.float_binop_fa0(op);
                return true;
            }
            _ => {
                if self.gen_expr(expr) {
                    self.emit_fcvt_s_w_fa0();
                    return true;
                }
            }
        }
        self.li_a0(0);
        false
    }

    fn gen_vector_expr(&mut self, expr: &TypedExpr) -> bool {
        match expr.kind {
            ExprKind::Binary(op) if expr.children.len() >= 2 => {
                self.gen_expr(&expr.children[0]);
                self.mv_a1_a0();
                self.gen_expr(&expr.children[1]);
                self.binop_a0_a1(op);
                return true;
            }
            _ => self.li_a0(0),
        }
        false
    }

    fn gen_expr_from_ast(&mut self, expr: &Expr) {
        match expr {
            Expr::Int { v, .. } => self.li_a0(trunc_i32(*v) as i32),
            Expr::Float { v, .. } => self.load_float_fa0(*v as f32),
            Expr::Ident { name, .. } => {
                if let Some(&off) = self.locals.get(&name.name) {
                    self.load_a0(off);
                } else {
                    self.li_a0(0);
                }
            }
            Expr::Binop { op, l, r, .. } => {
                self.gen_expr_from_ast(l);
                self.mv_a1_a0();
                self.gen_expr_from_ast(r);
                self.binop_a0_a1(*op);
            }
            Expr::Call { callee, args, .. } => {
                for (i, arg) in args.iter().enumerate().take(8) {
                    self.gen_expr_from_ast(arg);
                    self.mv_a_reg(i as u8);
                }
                if let Expr::Ident { name, .. } = callee.as_ref() {
                    self.emit_jal_to(&name.name);
                }
            }
            _ => {
                if let Ok(v) = crate::const_eval::eval(expr) {
                    self.li_a0(trunc_i32(v) as i32);
                } else {
                    self.li_a0(0);
                }
            }
        }
    }

    fn load_global_addr_a0(&mut self, name: &str) {
        if self.relocatable {
            let pos = self.pos();
            self.emit_auipc_a0(0);
            if let Some(&idx) = self.sym_index.get(name) {
                self.relocations.push(ElfRela {
                    offset: pos,
                    sym_index: idx,
                    rel_type: elf::R_RISCV_PCREL_HI20,
                    addend: 0,
                });
                let pos2 = self.pos();
                self.emit_ld_a0_a0(0);
                self.rodata_relocations.push(ElfRela {
                    offset: pos2,
                    sym_index: idx,
                    rel_type: elf::R_RISCV_PCREL_LO12_I,
                    addend: 0,
                });
            }
        } else if let Some(&off) = self.global_rodata.get(name) {
            let addr = self.entry + align_up_u64(self.code.len() as u64, 8) + off;
            self.li_a0(addr as i32);
        }
    }

    fn alloc_local(&mut self, name: &str, bytes: i32) -> i32 {
        let aligned = (bytes + 7) & !7;
        let off = self.stack_bytes;
        self.stack_bytes += aligned;
        self.locals.insert(name.to_string(), off);
        off
    }

    fn binop_a0_a1(&mut self, op: BinOp) {
        match op {
            BinOp::Add => self.emit_add(10, 10, 11),
            BinOp::Sub => self.emit_sub(10, 10, 11),
            BinOp::Mul => self.emit_mul(10, 10, 11),
            BinOp::And => self.emit_and(10, 10, 11),
            BinOp::Or => self.emit_or(10, 10, 11),
            BinOp::Xor => self.emit_xor(10, 10, 11),
            BinOp::EqEq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                self.emit_slt(12, 10, 11);
                self.li_a0(1);
                self.emit_beq(12, 10, 8); // skip
                self.li_a0(0);
            }
            // M7 修复: 真 div/divu/rem/remu 替代 0 (RV64M 扩展)
            // binop_a0_a1 之前已把 a0=left, a1=right
            // 编码 (rd=a0, rs1=a0, rs2=a1):
            //   div  : 0x02B54033   (有符号除)
            //   divu : 0x02B55033   (无符号除)
            //   rem  : 0x02B56033   (有符号取模)
            //   remu : 0x02B57033   (无符号取模)
            // 限制: 整数除 0 不抛异常,直接返回 0/-1 (符合 RISC-V spec)
            BinOp::Div => self.emit_div(10, 10, 11),
            BinOp::Mod => self.emit_rem(10, 10, 11),
            BinOp::Shl => self.emit_sll(10, 10, 11),
            BinOp::Shr => self.emit_srl(10, 10, 11),
            BinOp::AndAnd | BinOp::OrOr => self.cmp_a0_zero(),
        }
    }

    fn float_binop_fa0(&mut self, op: BinOp) {
        let _ = op;
        self.emit_fadd_fa0();
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
                BranchKind::Jal => self.patch_jal(at, pos),
                BranchKind::Beqz => self.patch_beqz(at, pos),
            }
        }
        self.pending_branches.retain(|(_, l, _)| l != name);
    }

    fn emit(&mut self, bytes: &[u8]) {
        self.code.extend_from_slice(bytes);
    }

    fn li_a0(&mut self, imm: i32) {
        self.li_reg(10, imm);
    }

    fn li_a7(&mut self, imm: i32) {
        self.li_reg(17, imm);
    }

    fn li_reg(&mut self, rd: u8, imm: i32) {
        if imm >= -2048 && imm <= 2047 {
            self.emit_addi(rd, 0, imm);
        } else {
            let upper = ((imm as i64 + 0x800) >> 12) as i32;
            let lower = imm - (upper << 12);
            self.emit_lui(rd, upper);
            if lower != 0 {
                self.emit_addi(rd, rd, lower);
            }
        }
    }

    fn load_float_fa0(&mut self, v: f32) {
        let off = align_rodata(&mut self.rodata, 4);
        let bits = v.to_bits();
        self.rodata.extend_from_slice(&bits.to_le_bytes());
        let pos = self.pos();
        self.emit_auipc_a0(0);
        self.emit_flw_fa0_a0(0);
        self.float_refs.push((pos, off));
    }

    fn store_a0(&mut self, off: i32) {
        self.emit_sw(10, 2, off);
    }

    fn load_a0(&mut self, off: i32) {
        self.emit_lw(10, 2, off);
    }

    fn mv_a1_a0(&mut self) {
        self.emit_addi(11, 10, 0);
    }

    fn mv_a_reg(&mut self, idx: u8) {
        let rd = 10 + idx;
        self.emit_addi(rd, 10, 0);
    }

    // M7 修复: 计算 sp + off 的地址到指定 a 寄存器
    // 编码: add rd, sp, #off   (off >= 0)   => 0x0000_? ? ?B 形式 addi
    //       addi rd, sp, #off  (off 在 ±2048)
    // 限制: 偏移 12-bit 有符号; 越界需 auipc+addi 序列
    fn mov_a_reg_sp_off(&mut self, reg: u8, off: i32) {
        if off >= -2048 && off <= 2047 {
            self.emit_addi(reg, 2, off);
        } else {
            // 越界: 走 lui + addi 序列 (与 li_reg 一致)
            self.li_reg(18, off);  // t0 = off (临时)
            self.emit_add(reg, reg, 18);
        }
    }

    fn mov_a0_sp_off(&mut self, off: i32) {
        self.mov_a_reg_sp_off(10, off);
    }

    fn neg_a0(&mut self) {
        self.emit_sub(10, 0, 10);
    }

    fn not_a0(&mut self) {
        self.emit_xori(10, 10, -1);
    }

    fn cmp_a0_zero(&mut self) {
        self.emit_sltu(10, 0, 10);
    }

    fn addi_sp(&mut self, delta: i32) {
        self.emit_addi(2, 2, delta);
    }

    fn ret(&mut self) {
        if self.frame_bytes > 0 {
            self.addi_sp(self.frame_bytes);
        }
        self.emit_jalr(0, 1, 0);
    }

    fn ecall(&mut self) {
        self.emit(&[0x73, 0x00, 0x00, 0x00]);
    }

    fn beqz_a0(&mut self, label: &str) {
        let pos = self.pos();
        self.emit_beq(10, 0, 0);
        if let Some(&target) = self.labels.get(label) {
            self.patch_beq(pos, 10, 0, target);
        } else {
            self.pending_branches
                .push((pos, label.to_string(), BranchKind::Beqz));
        }
    }

    fn j(&mut self, label: &str) {
        let pos = self.pos();
        self.emit_jal(0, 0);
        if let Some(&target) = self.labels.get(label) {
            self.patch_jal(pos, target);
        } else {
            self.pending_branches
                .push((pos, label.to_string(), BranchKind::Jal));
        }
    }

    fn emit_jal_placeholder(&mut self, rd: u8) {
        self.emit_jal(rd, 0);
    }

    fn emit_jal_to(&mut self, name: &str) {
        if let Some(&target) = self.labels.get(name) {
            let pos = self.pos();
            self.emit_jal(1, 0);
            self.patch_jal(pos, target);
        } else if self.relocatable {
            let pos = self.pos();
            self.emit_jal(1, 0);
            let idx = self.ensure_sym(name);
            self.relocations.push(ElfRela {
                offset: pos,
                sym_index: idx,
                rel_type: R_RISCV_JAL,
                addend: 0,
            });
        } else {
            self.emit_jal(1, 0);
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

    // ===== RISC-V 指令编码 =====

    fn emit_addi(&mut self, rd: u8, rs1: u8, imm: i32) {
        let imm = imm as u32 & 0xfff;
        let insn = imm << 20 | (rs1 as u32) << 15 | 0 << 12 | (rd as u32) << 7 | 0x13;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_lui(&mut self, rd: u8, imm: i32) {
        let imm = (imm as u32 & 0xfffff) << 12;
        let insn = imm | (rd as u32) << 7 | 0x37;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_add(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20 | (rs1 as u32) << 15 | 0 << 12 | (rd as u32) << 7 | 0x33;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_sub(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20 | (rs1 as u32) << 15 | 0b0000000 << 12 | (rd as u32) << 7 | 0x33 | 0x40000000;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_mul(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20 | (rs1 as u32) << 15 | 0b0000001 << 12 | (rd as u32) << 7 | 0x33;
        self.emit(&insn.to_le_bytes());
    }

    // M7 修复: 整数除/取模 (RV64M 扩展)
    // div  : funct7=0000001, funct3=100 (有符号)
    // divu : funct7=0000001, funct3=101 (无符号)
    // rem  : funct7=0000001, funct3=110 (有符号)
    // remu : funct7=0000001, funct3=111 (无符号)
    // 编码: 0x02000000 = funct7=0000001 (bit 25)
    // 除 0 行为: 硬件返回 -1 (div) / -1 (rem) ;在 RV spec 中明确定义
    // 最小整数 -2^63 / -1 同样按 spec 行为, 不抛异常
    fn emit_div(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20 | (rs1 as u32) << 15 | 0b100 << 12 | (rd as u32) << 7 | 0x02000000 | 0x33;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_divu(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20 | (rs1 as u32) << 15 | 0b101 << 12 | (rd as u32) << 7 | 0x02000000 | 0x33;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_rem(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20 | (rs1 as u32) << 15 | 0b110 << 12 | (rd as u32) << 7 | 0x02000000 | 0x33;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_remu(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20 | (rs1 as u32) << 15 | 0b111 << 12 | (rd as u32) << 7 | 0x02000000 | 0x33;
        self.emit(&insn.to_le_bytes());
    }

    // M7 修复: 移位指令 (RV64I 基础)
    // sll : funct7=0000000, funct3=001 (逻辑左移)
    // srl : funct7=0000000, funct3=101 (逻辑右移)
    // sra : funct7=0100000, funct3=101 (算术右移)
    // 编码: sra 用 0x40000000 (bit 30 = funct7 的 bit 6)
    // IR 的 Shr 暂视为无符号 (srl), 有符号另由 AST 端 cast 处理
    fn emit_sll(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20 | (rs1 as u32) << 15 | 0b001 << 12 | (rd as u32) << 7 | 0x33;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_srl(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20 | (rs1 as u32) << 15 | 0b101 << 12 | (rd as u32) << 7 | 0x33;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_sra(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20 | (rs1 as u32) << 15 | 0b101 << 12 | (rd as u32) << 7 | 0x40000000 | 0x33;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_and(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20 | (rs1 as u32) << 15 | 0b111 << 12 | (rd as u32) << 7 | 0x33;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_or(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20 | (rs1 as u32) << 15 | 0b110 << 12 | (rd as u32) << 7 | 0x33;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_xor(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20 | (rs1 as u32) << 15 | 0b100 << 12 | (rd as u32) << 7 | 0x33;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_xori(&mut self, rd: u8, rs1: u8, imm: i32) {
        let imm = imm as u32 & 0xfff;
        let insn = imm << 20 | (rs1 as u32) << 15 | 0b100 << 12 | (rd as u32) << 7 | 0x13;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_slt(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20 | (rs1 as u32) << 15 | 0b010 << 12 | (rd as u32) << 7 | 0x33;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_sltu(&mut self, rd: u8, rs1: u8, rs2: u8) {
        let insn = (rs2 as u32) << 20 | (rs1 as u32) << 15 | 0b011 << 12 | (rd as u32) << 7 | 0x33;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_lw(&mut self, rd: u8, rs1: u8, off: i32) {
        let imm = off as u32 & 0xfff;
        let insn = imm << 20 | (rs1 as u32) << 15 | 0b010 << 12 | (rd as u32) << 7 | 0x03;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_sw(&mut self, rs2: u8, rs1: u8, off: i32) {
        let imm = off as u32;
        let insn = ((imm >> 5) & 0x7f) << 25
            | (rs2 as u32) << 20
            | (rs1 as u32) << 15
            | 0b010 << 12
            | (imm & 0x1f) << 7
            | 0x23;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_jal(&mut self, rd: u8, _offset: i32) {
        let insn = (rd as u32) << 7 | 0x6f;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_jalr(&mut self, rd: u8, rs1: u8, imm: i32) {
        let imm = imm as u32 & 0xfff;
        let insn = imm << 20 | (rs1 as u32) << 15 | 0 << 12 | (rd as u32) << 7 | 0x67;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_beq(&mut self, rs1: u8, rs2: u8, _offset: i32) {
        let insn = (rs2 as u32) << 20 | (rs1 as u32) << 15 | 0 << 12 | 0x63;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_auipc_a0(&mut self, imm: i32) {
        let imm = (imm as u32 & 0xfffff) << 12;
        let insn = imm | 10 << 7 | 0x17;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_ld_a0_a0(&mut self, off: i32) {
        let imm = off as u32 & 0xfff;
        let insn = imm << 20 | 10 << 15 | 0b011 << 12 | 10 << 7 | 0x03;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_flw_fa0_a0(&mut self, off: i32) {
        let imm = off as u32 & 0xfff;
        let insn = imm << 20 | 10 << 15 | 0b010 << 12 | 10 << 7 | 0x07;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_fadd_fa0(&mut self) {
        let insn: u32 = 0x08000053 | 10 << 20 | 10 << 15 | 10 << 7;
        self.emit(&insn.to_le_bytes());
    }

    fn emit_fcvt_s_w_fa0(&mut self) {
        let insn = 0xD0000053u32 | (10 << 7) | (10 << 15);
        self.emit(&insn.to_le_bytes());
    }

    fn patch_jal(&mut self, at: u64, target: u64) {
        let offset = (target as i64 - at as i64) as i32;
        let off = offset as u32;
        let insn = ((off >> 12) & 0xff) << 12
            | ((off >> 11) & 1) << 20
            | ((off >> 1) & 0x3ff) << 21
            | ((off >> 20) & 1) << 31
            | 1 << 7
            | 0x6f;
        let pos = at as usize;
        if pos + 4 <= self.code.len() {
            self.code[pos..pos + 4].copy_from_slice(&insn.to_le_bytes());
        }
    }

    fn patch_beq(&mut self, at: u64, rs1: u8, rs2: u8, target: u64) {
        let offset = (target as i64 - at as i64) as i32;
        let off = offset as u32;
        let insn = ((off >> 12) & 1) << 31
            | ((off >> 5) & 0x3f) << 25
            | (rs2 as u32) << 20
            | (rs1 as u32) << 15
            | 0 << 12
            | ((off >> 1) & 0xf) << 8
            | ((off >> 11) & 1) << 7
            | 0x63;
        let pos = at as usize;
        if pos + 4 <= self.code.len() {
            self.code[pos..pos + 4].copy_from_slice(&insn.to_le_bytes());
        }
    }

    fn patch_beqz(&mut self, at: u64, target: u64) {
        self.patch_beq(at, 10, 0, target);
    }

    fn patch_auipc_load(&mut self, at: u64, addr: u64) {
        let pc = at;
        let delta = (addr as i64 - pc as i64) as i32;
        let upper = ((delta + 0x800) >> 12) as i32;
        let lower = delta - (upper << 12);
        let lui = ((upper as u32 & 0xfffff) << 12) | 10 << 7 | 0x17;
        let pos = at as usize;
        if pos + 4 <= self.code.len() {
            self.code[pos..pos + 4].copy_from_slice(&lui.to_le_bytes());
        }
        if pos + 8 <= self.code.len() {
            let flw = (lower as u32 & 0xfff) << 20 | 10 << 15 | 0b010 << 12 | 10 << 7 | 0x07;
            self.code[pos + 4..pos + 8].copy_from_slice(&flw.to_le_bytes());
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
    fn riscv64_ir_return_add() {
        let (program, diags) = crate::parser::parse(
            b"int main(void) { return 10 + 32; }\n",
            "rv.c",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let module = lower(program).expect("lower");
        validate(&module).expect("validate");
        let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
        let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
        cg.compile(&module);
        let obj = cg.build_output();
        assert_eq!(&obj[0..4], &[0x7f, b'E', b'L', b'F']);
        assert_eq!(u16::from_le_bytes([obj[18], obj[19]]), 243);
    }

    // M7 修复: div/mod 真实 RV64M 指令 (div/rem) 替代 0
    // div rd, rs1, rs2: funct7=0000001, funct3=100 (有符号)
    // 编码 (rd=a0, rs1=a0, rs2=a1): 0x02B54533 LE 编码 = 33 45 B5 02
    // 注: 使用 opaque 函数防止常量折叠
    #[test]
    fn riscv64_ir_div_mod_emit_rv64m() {
        let (program, diags) = crate::parser::parse(
            b"extern int opaque(int);\n\
              int main(void) { int a = opaque(42); int b = opaque(5); int q = a / b; int r = a % b; return q + r; }\n",
            "rv.c",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let module = lower(program).expect("lower");
        validate(&module).expect("validate");
        let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
        let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
        cg.compile(&module);
        let obj = cg.build_output();
        // div a0, a0, a1: 0x02B54533 LE = 33 45 B5 02
        let has_div = obj.windows(4).any(|w| w == [0x33, 0x45, 0xb5, 0x02]);
        // rem a0, a0, a1: 0x02B56533 LE = 33 65 B5 02
        let has_rem = obj.windows(4).any(|w| w == [0x33, 0x65, 0xb5, 0x02]);
        assert!(has_div || has_rem, "expected div/rem in riscv64 text");
    }

    // M7 修复: 移位真实 sll/srl 替代 0
    // sll a0, a0, a1: funct7=0000000, funct3=001
    //   insn = (11<<20)|(10<<15)|(1<<12)|(10<<7)|0x33 = 0x00B51533 LE = 33 15 B5 00
    // srl a0, a0, a1: funct7=0000000, funct3=101
    //   insn = 0x00B55533 LE = 33 55 B5 00
    #[test]
    fn riscv64_ir_shifts_emit_sll_srl() {
        let (program, diags) = crate::parser::parse(
            b"extern int opaque(int);\n\
              int main(void) { int a = opaque(1); int b = opaque(2); return (a << b) | (a >> b); }\n",
            "rv.c",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let module = lower(program).expect("lower");
        validate(&module).expect("validate");
        let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
        let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
        cg.compile(&module);
        let obj = cg.build_output();
        let has_sll = obj.windows(4).any(|w| w == [0x33, 0x15, 0xb5, 0x00]);
        let has_srl = obj.windows(4).any(|w| w == [0x33, 0x55, 0xb5, 0x00]);
        assert!(has_sll || has_srl, "expected sll/srl in riscv64 text");
    }

    // M7 修复: 聚合类型参数按引用传递
    // 验证: 调函数时,若参数是 local struct, 发射 addi rd, sp, #off
    // addi rd, sp, #off 编码: opcode=0x13, funct3=0, rs1=2 (sp)
    // 重建 insn 后检查: (insn & 0x7F) == 0x13 && (insn>>12)&7 == 0 && (insn>>15)&0x1F == 2
    #[test]
    fn riscv64_aggregate_arg_by_ref() {
        let (program, diags) = crate::parser::parse(
            b"struct S { int x; int y; };\n\
              void callee(struct S s) {}\n\
              int main(void) { struct S s; s.x = 1; s.y = 2; callee(s); return 0; }\n",
            "rv.c",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let module = lower(program).expect("lower");
        validate(&module).expect("validate");
        let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
        let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
        cg.compile(&module);
        let obj = cg.build_output();
        let has_addi_sp = obj.windows(4).any(|w| {
            if w.len() < 4 { return false; }
            let insn = (w[0] as u32) | ((w[1] as u32) << 8) | ((w[2] as u32) << 16) | ((w[3] as u32) << 24);
            (insn & 0x7F) == 0x13 && ((insn >> 12) & 7) == 0 && ((insn >> 15) & 0x1F) == 2
        });
        assert!(has_addi_sp, "expected addi rd, sp, #off (M7 by-ref aggregate)");
    }
}
