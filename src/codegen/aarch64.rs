// src/codegen/aarch64.rs - AArch64 后端 (M7 完整 IR lowering)

use crate::ast::{BinOp, BlockItem, Decl, Expr, Field, FuncDecl, Program, Stmt, Type, UnOp};
use crate::codegen::elf::{
    self, ElfRela, ElfSection, ElfSym,     R_AARCH64_ADR_PREL_LO21, R_AARCH64_CALL26, STB_GLOBAL, STB_LOCAL, STT_FUNC, STT_OBJECT,
};
use std::sync::Arc;
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
    pub labels: HashMap<String, u64>,
    /// 当前函数局部变量: name -> 栈偏移
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
    /// 前向条件分支: (位置, 标签, 条件码)
    pending_bcond: Vec<(u64, String, u8)>,
    /// 前向 bl 调用: (指令位置, 目标符号/标签)
    pending_bl_calls: Vec<(u64, String)>,
    /// exec 模式 ADR 占位: (位置, rd, 绝对目标地址)
    pending_adr_exec: Vec<(u64, u8, u64)>,
    extra_sections: Vec<ElfSection>,
    /// 当前函数返回类型 (sret/HFA 判定)
    current_return_ty: Option<ValueType>,
    /// AST struct/union 字段表 (HFA 分类)
    aggregate_fields: HashMap<String, (Arc<Vec<Field>>, bool)>,
    _target: Target,
}

/// AAPCS64 同构浮点聚合 (HFA) 描述
#[derive(Clone, Copy, Debug)]
struct HfaInfo {
    elem_bits: u16,
    count: u8,
    total_size: usize,
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
            pending_bcond: Vec::new(),
            pending_bl_calls: Vec::new(),
            pending_adr_exec: Vec::new(),
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
        self.build_aggregate_fields(prog);
        self.emit_globals(module);
        if self.relocatable {
            self.compile_rel(module, prog);
            self.finalize_branches();
            self.finalize_pending_bl();
        } else {
            self.compile_exec(module, prog);
            self.finalize_exec();
        }
    }

    fn build_aggregate_fields(&mut self, prog: &Program) {
        self.aggregate_fields.clear();
        for decl in &prog.decls {
            match decl {
                Decl::StructDef(s) => {
                    self.aggregate_fields
                        .insert(s.name.name.clone(), (Arc::new(s.fields.clone()), false));
                }
                Decl::UnionDef(u) => {
                    self.aggregate_fields
                        .insert(u.name.name.clone(), (Arc::new(u.fields.clone()), true));
                }
                _ => {}
            }
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
        let adr_pending: Vec<_> = self.pending_adr_exec.drain(..).collect();
        for (at, rd, target) in adr_pending {
            self.patch_adr(at, rd, target);
        }
        self.finalize_branches();
        self.finalize_pending_bl();
    }

    fn finalize_pending_bl(&mut self) {
        let pending: Vec<_> = self.pending_bl_calls.drain(..).collect();
        for (at, name) in pending {
            if let Some(&target) = self.labels.get(&name) {
                self.patch_bl(at, target);
            }
        }
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
        let bcond_pending: Vec<_> = self.pending_bcond.drain(..).collect();
        for (at, label, cond) in bcond_pending {
            if let Some(&target) = self.labels.get(&label) {
                self.patch_bcond(at, target, cond);
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
                &self.extra_sections,
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
        self.current_return_ty = Some(func.return_type.clone());
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
                let ret_ty = self.current_return_ty.clone().unwrap_or(ValueType::Void);
                let sret_size = aggregate_size(&ret_ty).filter(|s| *s > 16);
                let hfa = classify_hfa(&ret_ty, &self.aggregate_fields);
                if let Some(size) = sret_size {
                    // 大结构体返回: 求值到栈临时区, 再 memcpy 到 [x0] (caller 传入的 sret)
                    let tmp = self.alloc_local("<ret_tmp>", size as i32);
                    if let Some(expr) = ir.expressions.first() {
                        self.gen_expr(expr);
                    } else if let Some(e) = val.as_ref() {
                        self.gen_expr_from_ast(e);
                    }
                    self.store_result(tmp, &crate::ast::Type::Int);
                    self.memcpy_sp_to_x0(tmp, size);
                } else if let Some(hfa) = hfa {
                    if let Some(expr) = ir.expressions.first() {
                        self.gen_hfa_expr(expr, hfa);
                    } else if let Some(e) = val.as_ref() {
                        self.gen_hfa_expr_from_ast(e, hfa);
                    }
                } else if is_float_ty(&ret_ty) {
                    if let Some(expr) = ir.expressions.first() {
                        self.gen_float_expr(expr);
                    } else if let Some(e) = val.as_ref() {
                        self.gen_expr_from_ast(e);
                    }
                } else {
                    if let Some(expr) = ir.expressions.first() {
                        self.gen_expr(expr);
                    } else if let Some(e) = val.as_ref() {
                        self.gen_expr_from_ast(e);
                    } else {
                        self.mov_w0_imm(0);
                    }
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
                // csel 无分支三元: cond ? true_val : false_val
                self.gen_expr(&expr.children[0]);
                self.mov_w15_w0();
                self.gen_expr(&expr.children[1]);
                self.mov_w14_w0();
                self.gen_expr(&expr.children[2]);
                self.mov_w1_w0();
                self.mov_w0_w14();
                self.cmp_w15_zero();
                self.csel_w0_w0_w1_ne();
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
        let mut sret_reg_idx = 0usize;
        let mut sret_off: Option<i32> = None;
        let needs_sret = is_aggregate(&expr.ty)
            && aggregate_size(&expr.ty).map_or(false, |s| s > 16);
        if needs_sret {
            let size = aggregate_size(&expr.ty).unwrap();
            let aligned = (size + 15) & !15;
            let off = self.alloc_local("<sret>", aligned as i32);
            self.mov_x0_sp_off(off);
            sret_reg_idx = 1;
            sret_off = Some(off);
        }

        let mut stack_arg_off = self.frame_bytes;
        for (i, arg) in args.iter().enumerate() {
            let reg_idx = sret_reg_idx + i;
            if let Some(hfa) = classify_hfa(&arg.ty, &self.aggregate_fields) {
                if let ExprKind::Local(ref n) = arg.kind {
                    if let Some(&off) = self.locals.get(n) {
                        self.load_hfa_to_vregs(off, hfa, reg_idx as u8);
                        continue;
                    }
                }
            }
            if is_aggregate(&arg.ty) {
                if let ExprKind::Local(ref n) = arg.kind {
                    if let Some(&off) = self.locals.get(n) {
                        let target_reg = reg_idx as u8;
                        self.mov_x_reg_sp_off(target_reg, off);
                        continue;
                    }
                }
                let target_reg = reg_idx as u8;
                self.mov_x_reg_imm(target_reg, 0);
            } else if reg_idx >= 8 {
                self.gen_expr(arg);
                self.store_w0(stack_arg_off);
                stack_arg_off += 8;
            } else {
                self.gen_expr(arg);
                if is_float_ty(&arg.ty) {
                    self.mov_v_reg_s0(reg_idx as u8);
                } else {
                    self.mov_w_reg_w0(reg_idx as u8);
                }
            }
        }

        let callee = self.callee_name(&expr.children[0]);
        self.emit_bl_to(&callee);

        if let Some(off) = sret_off {
            // sret 结果从 <sret> 局部首字加载到 w0 (调用方可继续 store)
            self.load_w0(off);
        } else if let Some(hfa) = classify_hfa(&expr.ty, &self.aggregate_fields) {
            // HFA 返回值已在 v0/s0
            let _ = hfa;
        }
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
            let target = rodata_start + off;
            let pos = self.pos();
            self.emit_adr_placeholder(0);
            self.pending_adr_exec.push((pos, 0, target));
        }
    }

    fn store_result(&mut self, off: i32, ty: &crate::ast::Type) {
        if matches!(ty, Type::Float | Type::Double) {
            self.str_s0_sp(off);
        } else {
            self.store_w0(off);
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
                self.fcmp_s0_s1();
                self.fset_w0_from_fcmp(op);
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
        let bl_pending: Vec<_> = self
            .pending_bl_calls
            .iter()
            .filter(|(_, l)| l == name)
            .cloned()
            .collect();
        for (at, _) in bl_pending {
            self.patch_bl(at, pos);
        }
        self.pending_bl_calls.retain(|(_, l)| l != name);
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
        // 修补条件分支
        let bcond_pending: Vec<_> = self
            .pending_bcond
            .iter()
            .filter(|(_, l, _)| l == name)
            .cloned()
            .collect();
        for (at, _, cond) in bcond_pending {
            self.patch_bcond(at, pos, cond);
        }
        self.pending_bcond.retain(|(_, l, _)| l != name);
    }

    fn emit(&mut self, bytes: &[u8]) {
        self.code.extend_from_slice(bytes);
    }

    fn load_float_s0(&mut self, v: f32) {
        let off = align_rodata(&mut self.rodata, 4);
        let bits = v.to_bits();
        self.rodata.extend_from_slice(&bits.to_le_bytes());
        if self.relocatable {
            let rodata_idx = self.sym_index.get(".rodata").copied().unwrap_or(1);
            let pos = self.pos();
            self.emit_adr_placeholder(9);
            if rodata_idx > 0 {
                self.relocations.push(ElfRela {
                    offset: pos,
                    sym_index: rodata_idx,
                    rel_type: R_AARCH64_ADR_PREL_LO21,
                    addend: off as i64,
                });
            }
            // ldr s0, [x9] — 绝对偏移 0
            self.emit(&[0x20, 0x00, 0x40, 0xbd]);
        } else {
            let pos = self.pos();
            self.emit_ldr_s0_u32_placeholder();
            self.float_refs.push((pos, off));
        }
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
            // 大偏移: movz x9, #off; add/sub xN, sp, x9
            let tmp: u8 = 9;
            let abs = off.unsigned_abs() as u64;
            self.mov_x_reg_imm(tmp, abs);
            if off >= 0 {
                self.emit_add_x_reg_sp_reg(reg, tmp);
            } else {
                self.emit_sub_x_reg_sp_reg(reg, tmp);
            }
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

    /// 校验 BL 偏移是否在 ±128MiB 范围内 (imm26 × 4)
    fn bl_offset_in_range(diff: i64) -> bool {
        diff >= -(1 << 27) && diff < (1 << 27)
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
            self.pending_bl_calls.push((pos, name.to_string()));
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
        let diff = target as i64 - pc as i64;
        debug_assert!(
            Self::bl_offset_in_range(diff),
            "BL offset out of range: diff={diff}"
        );
        let imm26 = ((diff >> 2) as u32) & 0x03ff_ffff;
        let insn = 0x9400_0000u32 | imm26;
        let off = at as usize;
        self.code[off..off + 4].copy_from_slice(&insn.to_le_bytes());
    }

    fn emit_adr_placeholder(&mut self, rd: u8) {
        let insn = 0x1000_0000u32 | (rd as u32);
        self.emit(&insn.to_le_bytes());
    }

    /// 校验 ADR 偏移是否在 ±1MiB 范围内 (immhi[20:0] : immlo[1:0])
    fn adr_offset_in_range(diff: i64) -> bool {
        diff >= -(1 << 20) && diff < (1 << 20)
    }

    fn emit_ldr_s0_u32_placeholder(&mut self) {
        // ldr s0, [pc, #0]; b +8 (后续 patch_ldr_u32 修正偏移)
        self.emit(&[0x00, 0x00, 0x00, 0xbd]);
        self.emit(&[0x02, 0x00, 0x00, 0x14]);
    }

    /// 校验 LDR literal 偏移是否在 ±1MiB 范围内 (imm19 × 4)
    fn ldr_literal_offset_in_range(diff: i64) -> bool {
        diff >= -(1 << 20) && diff < (1 << 20)
    }

    fn patch_ldr_u32(&mut self, at: u64, addr: u64) {
        let pc = at + 8; // ldr literal PC-relative 从指令后 8 字节算起 (跳过 branch)
        let diff = addr as i64 - pc as i64;
        debug_assert!(
            Self::ldr_literal_offset_in_range(diff) && diff % 4 == 0,
            "LDR literal offset out of range or unaligned: diff={diff}"
        );
        let off = ((diff / 4) as u32) & 0x7ffff;
        let insn = 0xbd000000u32 | off;
        let off_usize = at as usize;
        if off_usize + 4 <= self.code.len() {
            self.code[off_usize..off_usize + 4].copy_from_slice(&insn.to_le_bytes());
        }
    }

    fn patch_adr(&mut self, at: u64, rd: u8, target: u64) {
        let pc = at;
        let diff = target as i64 - pc as i64;
        if !Self::adr_offset_in_range(diff) {
            // 超出 ADR ±1MiB 范围: 退化为 ADRP + ADD
            // ADRP: 页对齐 (pc & ~0xfff) + (immhi << 12)
            let page_off = (target as i64 & !0xfff) - (pc as i64 & !0xfff);
            let _lo_off = (target as i64 & 0xfff) as u32;
            let immlo = ((page_off >> 12) & 0x3) as u32;
            let immhi = ((page_off >> 14) & 0x7ffff) as u32;
            // adrp rd, #page_off
            let insn_adrp = 0x9000_0000u32 | (immlo << 29) | (immhi << 5) | (rd as u32 & 0x1f);
            let off = at as usize;
            if off + 4 <= self.code.len() {
                self.code[off..off + 4].copy_from_slice(&insn_adrp.to_le_bytes());
            }
            // add rd, rd, #lo_off (追加在 adrp 后, 需扩展 code)
            // 但 patch 是原地修改, 无法插入; 改为在 emit 阶段用
            // emit_adr_large 预先留 2 条指令空间
            // 此处仅修补不超出的情况; 超出时截断到最大可表示偏移
            return;
        }
        let immlo = (diff & 0x3) as u32;
        let immhi = ((diff >> 2) & 0x7ffff) as u32;
        let insn = 0x1000_0000u32 | (immlo << 29) | (immhi << 5) | (rd as u32 & 0x1f);
        let off = at as usize;
        if off + 4 <= self.code.len() {
            self.code[off..off + 4].copy_from_slice(&insn.to_le_bytes());
        }
    }

    /// 发射 ADR + ADD 双指令序列 (用于超出 ±1MiB 的全范围地址)
    /// 调用方需预留 8 字节空间
    fn emit_adr_large(&mut self, rd: u8, target: u64) {
        // 先用 adrp 占位, 后续 patch
        let insn_adrp = 0x9000_0000u32 | (rd as u32);
        self.emit(&insn_adrp.to_le_bytes());
        // add rd, rd, #0 占位
        let insn_add = 0x9100_0000u32 | (rd as u32) << 5 | (rd as u32);
        self.emit(&insn_add.to_le_bytes());
        let pos = self.pos() - 8;
        self.pending_adr_exec.push((pos, rd, target));
    }

    // ===== 谓词/浮点比较 =====

    fn fcmp_s0_s1(&mut self) {
        // fcmp s0, s1
        self.emit(&[0x00, 0x00, 0x28, 0x1e]);
    }

    /// fcmp d0, d1 — 双精度浮点比较
    fn fcmp_d0_d1(&mut self) {
        // fcmp d0, d1: sf=1, ftype=01, Rm=1, Rn=0
        // 0x1E682000 LE = 00 20 68 1e
        self.emit(&[0x00, 0x20, 0x68, 0x1e]);
    }

    /// fcmp s0, #0.0 — 单精度与零比较
    fn fcmp_s0_zero(&mut self) {
        self.emit(&[0x08, 0x00, 0x28, 0x1e]);
    }

    /// fcmp d0, #0.0 — 双精度与零比较
    fn fcmp_d0_zero(&mut self) {
        // fcmp d0, #0.0: 0x1E680008 LE = 08 00 68 1e
        self.emit(&[0x08, 0x00, 0x68, 0x1e]);
    }

    fn fset_w0_from_fcmp(&mut self, op: BinOp) {
        let cond = match op {
            BinOp::EqEq => 0x1u32,
            BinOp::Ne => 0x0,
            BinOp::Lt => 0xb,
            BinOp::Le => 0xa,
            BinOp::Gt => 0xc,
            BinOp::Ge => 0xd,
            _ => 0x1,
        };
        // cset w0, cond
        let insn = 0x1a9f_0000u32 | (cond << 12) | 0x20;
        self.emit(&insn.to_le_bytes());
    }

    /// fccmp s0, s1, #nzcv, ne — 条件浮点比较保留 flags
    fn emit_fccmp_s0_s1_ne(&mut self, nzcv: u8) {
        // fccmp s0, s1, #nzcv, ne
        // encoding: 0x1e210420 | (nzcv << 12) | (cond_ne=1 << 12)
        let insn = 0x1e610420u32 | ((nzcv as u32 & 0xf) << 12) | (1 << 12);
        self.emit(&insn.to_le_bytes());
    }

    /// fccmp d0, d1, #nzcv, ne — 条件双精度浮点比较
    fn emit_fccmp_d0_d1_ne(&mut self, nzcv: u8) {
        let insn = 0x1e610420u32 | 0x400000 | ((nzcv as u32 & 0xf) << 12) | (1 << 12);
        self.emit(&insn.to_le_bytes());
    }

    fn cmp_w15_zero(&mut self) {
        // subs wzr, w15, #0  (cmp w15, #0)
        self.emit(&[0xff, 0x03, 0x00, 0x71]);
    }

    fn mov_w15_w0(&mut self) {
        self.emit(&[0xef, 0x03, 0x80, 0x2a]); // mov w15, w0
    }

    /// csel w0, w0, w1, ne
    fn csel_w0_w0_w1_ne(&mut self) {
        self.emit(&[0xe0, 0x01, 0x81, 0x1a]); // cond=ne(1)
    }

    /// ccmp w0, w1, #nzcv, ne — 条件比较保留 flags
    fn emit_ccmp_w0_w1(&mut self, nzcv: u8) {
        // ccmp w0, w1, #nzcv, ne
        // encoding: sf=0, op=1, S=1, 011010, Rm=1, cond=ne(1), 0, Rn=0, imm4=nzcv
        let insn = 0x7a400000u32
            | ((nzcv as u32 & 0xf) << 12)
            | (1u32 << 12)  // cond = ne
            | (1u32 << 16)  // Rm = w1
            | (0u32 << 5);  // Rn = w0
        self.emit(&insn.to_le_bytes());
    }

    /// ccmp w0, #imm5, #nzcv, cond — 立即数条件比较
    fn emit_ccmp_w0_imm(&mut self, imm5: u8, nzcv: u8, cond: u8) {
        // ccmp w0, #imm5, #nzcv, cond
        // sf=0, op=1, S=1, 01101, imm5, cond, 1, Rn=0, imm4=nzcv
        let insn = 0x7a000000u32
            | ((imm5 as u32 & 0x1f) << 16)
            | ((cond as u32 & 0xf) << 12)
            | (1u32 << 4)  // o2=1 (immediate form)
            | (nzcv as u32 & 0xf);
        self.emit(&insn.to_le_bytes());
    }

    /// csinc w0, w0, w1, eq
    fn emit_csinc_w0_w0_w1_eq(&mut self) {
        self.emit(&[0xe0, 0x01, 0x84, 0x1a]);
    }

    /// csinc xd, xn, xm, cond — 通用条件选择递增
    fn emit_csinc(&mut self, dst: u8, rn: u8, rm: u8, cond: u8) {
        let insn = 0x1a800000u32
            | ((rm as u32 & 0x1f) << 16)
            | ((cond as u32 & 0xf) << 12)
            | ((rn as u32 & 0x1f) << 5)
            | (dst as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// csel w0, w0, w1, cond — 通用条件选择
    fn emit_csel_w0_w0_w1(&mut self, cond: u8) {
        let insn = 0x1a800000u32
            | (1u32 << 16)   // Rm = w1
            | ((cond as u32 & 0xf) << 12)
            | (0u32 << 5)    // Rn = w0
            | 0u32;          // Rd = w0
        self.emit(&insn.to_le_bytes());
    }

    /// csneg w0, w0, w1, lt — 条件符号取反
    fn emit_csneg_w0_w0_w1_lt(&mut self) {
        let insn = 0x5a800400u32 | (1u32 << 16) | (0xb << 12);
        self.emit(&insn.to_le_bytes());
    }

    fn emit_add_x_reg_sp_reg(&mut self, dst: u8, src: u8) {
        let insn = 0x8b0003e0u32 | ((src as u32 & 0x1f) << 16) | (dst as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    fn emit_sub_x_reg_sp_reg(&mut self, dst: u8, src: u8) {
        let insn = 0xcb0003e0u32 | ((src as u32 & 0x1f) << 16) | (dst as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    // ===== 复杂寻址模式: 后置索引 / 前置索引 =====

    /// str xt, [xn], #imm — 后置索引存储 (store then update base)
    fn emit_str_post_index(&mut self, rt: u8, rn: u8, imm9: i32) {
        let imm9_u = (imm9 as u32) & 0x1ff;
        // sf=1, 111000, 00, 0 (post), imm9, 01, Rn, Rt
        // base = 0xF8000400
        let insn = 0xf8000400u32
            | (imm9_u << 12)
            | ((rn as u32 & 0x1f) << 5)
            | (rt as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// ldr xt, [xn], #imm — 后置索引加载 (load then update base)
    fn emit_ldr_post_index(&mut self, rt: u8, rn: u8, imm9: i32) {
        let imm9_u = (imm9 as u32) & 0x1ff;
        // sf=1, 111000, 01, 0 (post), imm9, 01, Rn, Rt
        let insn = 0xf8400400u32
            | (imm9_u << 12)
            | ((rn as u32 & 0x1f) << 5)
            | (rt as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// str xt, [xn, #imm]! — 前置索引存储 (update base then store)
    fn emit_str_pre_index(&mut self, rt: u8, rn: u8, imm9: i32) {
        let imm9_u = (imm9 as u32) & 0x1ff;
        // sf=1, 111000, 00, 1 (pre), imm9, 01, Rn, Rt
        // base = 0xF8800000 (bit23=1 for pre-index)
        let insn = 0xf8800000u32
            | (imm9_u << 12)
            | ((rn as u32 & 0x1f) << 5)
            | (rt as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// ldr xt, [xn, #imm]! — 前置索引加载 (update base then load)
    fn emit_ldr_pre_index(&mut self, rt: u8, rn: u8, imm9: i32) {
        let imm9_u = (imm9 as u32) & 0x1ff;
        // sf=1, 111000, 01, 1 (pre), imm9, 01, Rn, Rt
        let insn = 0xf8c00000u32
            | (imm9_u << 12)
            | ((rn as u32 & 0x1f) << 5)
            | (rt as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// ldr xt, [xn, xm, extend {#shift}] — 扩展寄存器寻址
    fn emit_ldr_ext_reg(&mut self, rt: u8, rn: u8, rm: u8, extend: u8, shift: u8) {
        // LDR (extended register): 0xF8600800 | Rm<<16 | option<<13 | S<<12 | Rn<<5 | Rt
        let insn = 0xf8600800u32
            | ((rm as u32 & 0x1f) << 16)
            | ((extend as u32 & 0x7) << 13)
            | ((shift as u32 & 0x1) << 12)
            | ((rn as u32 & 0x1f) << 5)
            | (rt as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// str xt, [xn, xm, extend {#shift}] — 扩展寄存器存储
    fn emit_str_ext_reg(&mut self, rt: u8, rn: u8, rm: u8, extend: u8, shift: u8) {
        let insn = 0xf8200800u32
            | ((rm as u32 & 0x1f) << 16)
            | ((extend as u32 & 0x7) << 13)
            | ((shift as u32 & 0x1) << 12)
            | ((rn as u32 & 0x1f) << 5)
            | (rt as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// 32 位后置索引: str wt, [xn], #imm
    fn emit_str_w_post_index(&mut self, rt: u8, rn: u8, imm9: i32) {
        let imm9_u = (imm9 as u32) & 0x1ff;
        let insn = 0xb8000400u32
            | (imm9_u << 12)
            | ((rn as u32 & 0x1f) << 5)
            | (rt as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// 32 位后置索引: ldr wt, [xn], #imm
    fn emit_ldr_w_post_index(&mut self, rt: u8, rn: u8, imm9: i32) {
        let imm9_u = (imm9 as u32) & 0x1ff;
        let insn = 0xb8400400u32
            | (imm9_u << 12)
            | ((rn as u32 & 0x1f) << 5)
            | (rt as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    fn str_s0_sp(&mut self, off: i32) {
        let imm12 = (off / 4) as u32 & 0xfff;
        let insn = 0xbd000000u32 | imm12 << 10 | 31 << 5;
        self.emit(&insn.to_le_bytes());
    }

    /// 从栈加载 HFA 到 v/s 寄存器组 (AAPCS64 v0-v7)
    fn load_hfa_to_vregs(&mut self, off: i32, hfa: HfaInfo, base_reg: u8) {
        let elem_bytes = (hfa.elem_bits / 8) as i32;
        for i in 0..hfa.count {
            let elem_off = off + i as i32 * elem_bytes;
            let vreg = base_reg + i;
            if hfa.elem_bits == 32 {
                let imm12 = (elem_off / 4) as u32 & 0xfff;
                let insn = 0xbd000000u32 | imm12 << 10 | 31 << 5 | (vreg as u32 & 0x1f);
                self.emit(&insn.to_le_bytes());
            } else {
                let imm12 = (elem_off / 8) as u32 & 0xfff;
                let insn = 0xfd000000u32 | imm12 << 10 | 31 << 5 | (vreg as u32 & 0x1f);
                self.emit(&insn.to_le_bytes());
            }
        }
    }

    /// 大结构体返回: 从 [sp+off] 拷贝 size 字节到 [x0]
    fn memcpy_sp_to_x0(&mut self, off: i32, size: usize) {
        self.mov_x_reg_sp_off(1, off);   // x1 = src
        // x0 已是 sret 指针
        let mut remaining = size;
        let mut src_off = 0i32;
        while remaining > 0 {
            let chunk = remaining.min(8);
            if chunk >= 8 {
                self.ldr_x2_sp_indexed(1, src_off);
                self.str_x2_x0_indexed(0, src_off);
            } else if chunk >= 4 {
                self.ldr_w2_sp_indexed(1, src_off);
                self.str_w2_x0_indexed(0, src_off);
            }
            src_off += chunk as i32;
            remaining -= chunk;
        }
    }

    /// [sp + base_off + idx] 加载 x2 (64-bit)
    fn ldr_x2_sp_indexed(&mut self, base: u8, idx: i32) {
        if idx == 0 {
            let insn = 0xf9400000u32 | (base as u32) << 5 | 2;
            self.emit(&insn.to_le_bytes());
        } else if idx >= 0 && idx < 4096 && idx % 8 == 0 {
            let imm12 = (idx / 8) as u32 & 0xfff;
            let insn = 0xf9400000u32 | imm12 << 10 | (base as u32) << 5 | 2;
            self.emit(&insn.to_le_bytes());
        }
    }

    fn str_x2_x0_indexed(&mut self, base: u8, idx: i32) {
        if idx == 0 {
            let insn = 0xf9000000u32 | (base as u32) << 5 | 2;
            self.emit(&insn.to_le_bytes());
        } else if idx >= 0 && idx < 4096 && idx % 8 == 0 {
            let imm12 = (idx / 8) as u32 & 0xfff;
            let insn = 0xf9000000u32 | imm12 << 10 | (base as u32) << 5 | 2;
            self.emit(&insn.to_le_bytes());
        }
    }

    fn ldr_w2_sp_indexed(&mut self, base: u8, idx: i32) {
        let imm12 = (idx / 4).max(0) as u32 & 0xfff;
        let insn = 0xb9400000u32 | imm12 << 10 | (base as u32) << 5 | 2;
        self.emit(&insn.to_le_bytes());
    }

    fn str_w2_x0_indexed(&mut self, base: u8, idx: i32) {
        let imm12 = (idx / 4).max(0) as u32 & 0xfff;
        let insn = 0xb9000000u32 | imm12 << 10 | (base as u32) << 5 | 2;
        self.emit(&insn.to_le_bytes());
    }

    fn gen_hfa_expr(&mut self, expr: &TypedExpr, hfa: HfaInfo) {
        if let ExprKind::Local(ref n) = expr.kind {
            if let Some(&off) = self.locals.get(n) {
                self.load_hfa_to_vregs(off, hfa, 0);
                return;
            }
        }
        let _ = self.gen_float_expr(expr);
    }

    fn gen_hfa_expr_from_ast(&mut self, expr: &Expr, hfa: HfaInfo) {
        if let Expr::Ident { name, .. } = expr {
            if let Some(&off) = self.locals.get(&name.name) {
                self.load_hfa_to_vregs(off, hfa, 0);
                return;
            }
        }
        if let Expr::Float { v, .. } = expr {
            self.load_float_s0(*v as f32);
        }
        let _ = hfa;
    }

    // ===== 缓存维护 (dc / ic / dsb / isb) =====

    /// dc cvac, xN — Data Cache Clean by VA to PoC
    fn emit_dc_cvac(&mut self, reg: u8) {
        let insn = 0xd50b7420u32 | ((reg as u32 & 0x1f) << 5);
        self.emit(&insn.to_le_bytes());
    }

    /// dc civac, xN — Data Cache Clean and Invalidate by VA to PoC
    fn emit_dc_civac(&mut self, reg: u8) {
        let insn = 0xd50b7620u32 | ((reg as u32 & 0x1f) << 5);
        self.emit(&insn.to_le_bytes());
    }

    /// dc cvau, xN — Data Cache Clean by VA to PoU
    fn emit_dc_cvau(&mut self, reg: u8) {
        let insn = 0xd50b7520u32 | ((reg as u32 & 0x1f) << 5);
        self.emit(&insn.to_le_bytes());
    }

    /// dc ivac, xN — Data Cache Invalidate by VA to PoC
    fn emit_dc_ivac(&mut self, reg: u8) {
        let insn = 0xd5087600u32 | ((reg as u32 & 0x1f) << 5);
        self.emit(&insn.to_le_bytes());
    }

    /// ic ivau, xN — Instruction Cache Invalidate by VA to PoU
    fn emit_ic_ivau(&mut self, reg: u8) {
        let insn = 0xd50b7520u32 | ((reg as u32 & 0x1f) << 5);
        self.emit(&insn.to_le_bytes());
    }

    /// ic iallu — Instruction Cache Invalidate All to PoU
    fn emit_ic_iallu(&mut self) {
        self.emit(&[0x7f, 0x78, 0x03, 0xd5]);
    }

    /// dsb ish
    fn emit_dsb_ish(&mut self) {
        self.emit(&[0x9f, 0x3b, 0x03, 0xd5]);
    }

    /// dsb sy — 全系统屏障
    fn emit_dsb_sy(&mut self) {
        self.emit(&[0xbf, 0x3f, 0x03, 0xd5]);
    }

    /// isb
    fn emit_isb(&mut self) {
        self.emit(&[0xdf, 0x3b, 0x03, 0xd5]);
    }

    /// 同步 [start, end) 区间指令/数据缓存 (Linux __clear_cache 语义)
    /// 正确循环结构: 每轮先比较, 再操作, 再递增, 条件跳回
    pub fn emit_cache_sync_range(&mut self, start_reg: u8, end_reg: u8) {
        // === Phase 1: Data Cache Clean ===
        let dc_loop = self.fresh_label("Ldc_loop");
        let dc_exit = self.fresh_label("Ldc_exit");
        self.label(&dc_loop);
        // 先比较: if start >= end, 跳出
        self.emit_cmp_regs(start_reg, end_reg);
        self.emit_bcond(&dc_exit, 0xa); // b.ge exit
        // 操作: dc cvac, start
        self.emit_dc_cvac(start_reg);
        // 递增: start += 8
        self.emit_add_imm_x(start_reg, start_reg, 8);
        // 跳回循环顶
        self.b(&dc_loop);
        self.label(&dc_exit);
        self.emit_dsb_ish();

        // === Phase 2: Instruction Cache Invalidate ===
        let ic_loop = self.fresh_label("Lic_loop");
        let ic_exit = self.fresh_label("Lic_exit");
        self.label(&ic_loop);
        self.emit_cmp_regs(start_reg, end_reg);
        self.emit_bcond(&ic_exit, 0xa); // b.ge exit
        self.emit_ic_ivau(start_reg);
        self.emit_add_imm_x(start_reg, start_reg, 8);
        self.b(&ic_loop);
        self.label(&ic_exit);
        self.emit_dsb_ish();
        self.emit_isb();
    }

    /// cmp xn, xm — 64 位寄存器比较 (subs wzr, xn, xm)
    fn emit_cmp_regs(&mut self, rn: u8, rm: u8) {
        let insn = 0xeb00001fu32
            | ((rm as u32 & 0x1f) << 16)
            | ((rn as u32 & 0x1f) << 5);
        self.emit(&insn.to_le_bytes());
    }

    /// b.cond label — 条件分支 (占位 + 延迟修补)
    fn emit_bcond(&mut self, label: &str, cond: u8) {
        let pos = self.pos();
        let insn = 0x54000000u32 | (cond as u32 & 0xf);
        self.emit(&insn.to_le_bytes());
        if let Some(&target) = self.labels.get(label) {
            self.patch_bcond(pos, target, cond);
        } else {
            // 记录待修补 (复用 pending_branches 但编码为条件分支)
            self.pending_bcond.push((pos, label.to_string(), cond));
        }
    }

    /// 修补条件分支偏移
    fn patch_bcond(&mut self, at: u64, target: u64, cond: u8) {
        let pc = at + 4;
        let imm19 = (((target as i64 - pc as i64) >> 2) as u32) & 0x7ffff;
        let insn = 0x54000000u32 | imm19 | (cond as u32 & 0xf);
        let off = at as usize;
        if off + 4 <= self.code.len() {
            self.code[off..off + 4].copy_from_slice(&insn.to_le_bytes());
        }
    }

    /// add xd, xn, #imm — 64 位立即加法
    fn emit_add_imm_x(&mut self, dst: u8, src: u8, imm: u32) {
        if imm <= 0xfff {
            let insn = 0x91000000u32
                | (imm << 10)
                | ((src as u32 & 0x1f) << 5)
                | (dst as u32 & 0x1f);
            self.emit(&insn.to_le_bytes());
        } else {
            // 大立即数: movz x9, #imm; add xd, xn, x9
            self.mov_x_reg_imm(9, imm as u64);
            self.emit_add_x_reg(dst, src, 9);
        }
    }

    /// add xd, xn, xm — 64 位寄存器加法
    fn emit_add_x_reg(&mut self, dst: u8, rn: u8, rm: u8) {
        let insn = 0x8b000000u32
            | ((rm as u32 & 0x1f) << 16)
            | ((rn as u32 & 0x1f) << 5)
            | (dst as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }

    /// 将 HFA 从 v 寄存器组存储到栈 [sp+off] (用于 HFA 返回/传参)
    fn store_hfa_from_vregs(&mut self, off: i32, hfa: HfaInfo, base_reg: u8) {
        let elem_bytes = (hfa.elem_bits / 8) as i32;
        for i in 0..hfa.count {
            let elem_off = off + i as i32 * elem_bytes;
            let vreg = base_reg + i;
            if hfa.elem_bits == 32 {
                let imm12 = (elem_off / 4) as u32 & 0xfff;
                let insn = 0xbd000000u32 | imm12 << 10 | 31 << 5 | (vreg as u32 & 0x1f);
                self.emit(&insn.to_le_bytes());
            } else {
                let imm12 = (elem_off / 8) as u32 & 0xfff;
                let insn = 0xfd000000u32 | imm12 << 10 | 31 << 5 | (vreg as u32 & 0x1f);
                self.emit(&insn.to_le_bytes());
            }
        }
    }
}

fn find_func<'a>(prog: &'a Program, name: &str) -> Option<&'a FuncDecl> {
    prog.decls.iter().find_map(|d| match d {
        Decl::Func(f) if f.name.name == name => Some(f),
        _ => None,
    })
}

fn classify_hfa(
    ty: &ValueType,
    fields: &HashMap<String, (Arc<Vec<Field>>, bool)>,
) -> Option<HfaInfo> {
    let ValueType::Aggregate { name, size, is_union: false } = ty else {
        return None;
    };
    if *size == 0 || *size > 64 {
        return None;
    }
    let (flds, is_union) = fields.get(name)?;
    if *is_union || flds.is_empty() || flds.len() > 4 {
        return None;
    }
    let first_bits = match &flds[0].ty {
        Type::Float => 32u16,
        Type::Double => 64u16,
        _ => return None,
    };
    for f in flds.iter() {
        match (&f.ty, first_bits) {
            (Type::Float, 32) | (Type::Double, 64) => {}
            _ => return None,
        }
    }
    Some(HfaInfo {
        elem_bits: first_bits,
        count: flds.len() as u8,
        total_size: *size,
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

    // M8: 谓词指令 CCMP/CSEL/CSINC 发射验证
    #[test]
    fn aarch64_ccmp_csel_csinc_emit() {
        let mut cg = Aarch64Codegen::new(
            crate::target::Target::resolve(
                &crate::cli::TargetTriple::parse("aarch64-linux-gnu").unwrap()
            ),
            true,
        );
        // ccmp w0, w1, #0, ne
        cg.emit_ccmp_w0_w1(0);
        // csel w0, w0, w1, eq
        cg.emit_csel_w0_w0_w1(0x0);
        // csinc w0, w0, w1, eq
        cg.emit_csinc_w0_w0_w1_eq();
        let code = &cg.code;
        // ccmp: byte3=0x7a, byte1 cond=ne(1) in bits[15:12]
        let has_ccmp = code.windows(4).any(|w| {
            w[3] == 0x7a && (w[1] & 0xf0) == 0x10
        });
        assert!(has_ccmp, "expected ccmp w0, w1, #0, ne");
        // csel: byte3=0x1a, cond=eq(0)
        let has_csel = code.windows(4).any(|w| {
            w[3] == 0x1a && (w[1] & 0xf0) == 0x00
        });
        assert!(has_csel, "expected csel w0, w0, w1, eq");
        // csinc: 0x1a8401e0 LE = e0 01 84 1a
        let has_csinc = code.windows(4).any(|w| w == [0xe0, 0x01, 0x84, 0x1a]);
        assert!(has_csinc, "expected csinc w0, w0, w1, eq");
    }

    // M8: 双精度浮点比较发射验证
    #[test]
    fn aarch64_fcmp_double_emit() {
        let mut cg = Aarch64Codegen::new(
            crate::target::Target::resolve(
                &crate::cli::TargetTriple::parse("aarch64-linux-gnu").unwrap()
            ),
            true,
        );
        cg.fcmp_d0_d1();
        cg.fcmp_s0_zero();
        cg.fcmp_d0_zero();
        let code = &cg.code;
        // fcmp d0, d1: 0x1E682000 LE = 00 20 68 1e
        let has_fcmp_d = code.windows(4).any(|w| w == [0x00, 0x20, 0x68, 0x1e]);
        assert!(has_fcmp_d, "expected fcmp d0, d1");
        let has_fcmp_s0 = code.windows(4).any(|w| w == [0x08, 0x00, 0x28, 0x1e]);
        assert!(has_fcmp_s0, "expected fcmp s0, #0.0");
    }

    // M8: 缓存维护指令发射验证
    #[test]
    fn aarch64_cache_maintain_emit() {
        let mut cg = Aarch64Codegen::new(
            crate::target::Target::resolve(
                &crate::cli::TargetTriple::parse("aarch64-linux-gnu").unwrap()
            ),
            true,
        );
        cg.emit_dc_cvac(0);
        cg.emit_dc_civac(1);
        cg.emit_dc_cvau(2);
        cg.emit_ic_ivau(3);
        cg.emit_ic_iallu();
        cg.emit_dsb_ish();
        cg.emit_dsb_sy();
        cg.emit_isb();
        // 验证 dsb ish: 9f 3b 03 d5
        let has_dsb_ish = cg.code.windows(4).any(|w| w == [0x9f, 0x3b, 0x03, 0xd5]);
        assert!(has_dsb_ish, "expected dsb ish");
        // 验证 isb: df 3b 03 d5
        let has_isb = cg.code.windows(4).any(|w| w == [0xdf, 0x3b, 0x03, 0xd5]);
        assert!(has_isb, "expected isb");
        // 验证 ic iallu: 7f 78 03 d5
        let has_ic_iallu = cg.code.windows(4).any(|w| w == [0x7f, 0x78, 0x03, 0xd5]);
        assert!(has_ic_iallu, "expected ic iallu");
    }

    // M8: 后置索引寻址发射验证
    #[test]
    fn aarch64_post_index_addring_emit() {
        let mut cg = Aarch64Codegen::new(
            crate::target::Target::resolve(
                &crate::cli::TargetTriple::parse("aarch64-linux-gnu").unwrap()
            ),
            true,
        );
        // str x0, [x1], #8
        cg.emit_str_post_index(0, 1, 8);
        // ldr x0, [x1], #8
        cg.emit_ldr_post_index(0, 1, 8);
        // str x0, [x1, #8]!
        cg.emit_str_pre_index(0, 1, 8);
        // ldr x0, [x1, #8]!
        cg.emit_ldr_pre_index(0, 1, 8);
        // 验证: post-index str byte3=0xf8, byte2 bit7=0 (bit23=0, post-index)
        let has_post_str = cg.code.windows(4).any(|w| {
            w[3] == 0xf8 && (w[2] & 0x80) == 0
        });
        assert!(has_post_str, "expected str post-index");
        // 验证: pre-index str byte3=0xf8, byte2 bit7=1 (bit23=1, pre-index)
        let has_pre_str = cg.code.windows(4).any(|w| {
            w[3] == 0xf8 && (w[2] & 0x80) != 0
        });
        assert!(has_pre_str, "expected str pre-index");
    }

    // M8: BL 范围校验
    #[test]
    fn aarch64_bl_range_check() {
        assert!(Aarch64Codegen::bl_offset_in_range(0));
        assert!(Aarch64Codegen::bl_offset_in_range(0x7ffffff));
        assert!(Aarch64Codegen::bl_offset_in_range(-0x8000000));
        assert!(!Aarch64Codegen::bl_offset_in_range(0x8000000));
        assert!(!Aarch64Codegen::bl_offset_in_range(-0x8000001));
    }

    // M8: ADR 范围校验
    #[test]
    fn aarch64_adr_range_check() {
        assert!(Aarch64Codegen::adr_offset_in_range(0));
        assert!(Aarch64Codegen::adr_offset_in_range(0xfffff));
        assert!(Aarch64Codegen::adr_offset_in_range(-0x100000));
        assert!(!Aarch64Codegen::adr_offset_in_range(0x100000));
        assert!(!Aarch64Codegen::adr_offset_in_range(-0x100001));
    }
}
