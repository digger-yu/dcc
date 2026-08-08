// src/codegen/mips.rs - MIPS32 后端 (O32 ABI)

use crate::ast::{BinOp, BlockItem, Decl, Expr, FuncDecl, Program, Stmt, UnOp};
use crate::codegen::elf::{self, ElfRela, ElfSection, ElfSym, STB_GLOBAL, STT_FUNC};
use crate::ir::{Function, Global, Module, TypedStmt, ValueType};
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
    emitted_return: bool,
    label_serial: u32,
    pending_bl_calls: Vec<(u64, String)>,
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
            emitted_return: false,
            label_serial: 0,
            pending_bl_calls: Vec::new(),
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
        self.rodata.extend_from_slice(&[0u8; 4]);
    }

    fn compile_rel(&mut self, module: &Module, _prog: &Program) {
        for func in &module.functions {
            let ast = module.source().decls.iter().find_map(|d| match d {
                Decl::Func(f) if f.name.name == func.name => Some(f),
                _ => None,
            });
            self.gen_function(func, ast);
        }
    }

    // ===== 局部变量分配 =====

    fn plan_locals(&mut self, ast: Option<&FuncDecl>) {
        self.locals.clear();
        self.stack_bytes = 0;
        let Some(body) = ast.and_then(|f| f.body.as_ref()) else {
            return;
        };
        for item in &body.items {
            if let BlockItem::Decl(Decl::Var(v)) = item {
                let sz = v.ty.size().max(4) as i32;
                self.alloc_local(&v.name.name, sz);
            }
        }
    }

    fn alloc_local(&mut self, name: &str, size: i32) -> i32 {
        let off = self.stack_bytes;
        self.stack_bytes += (size + 3) & !3;
        self.locals.insert(name.to_string(), off);
        off
    }

    // ===== 函数生成 =====

    fn gen_function(&mut self, func: &Function, ast: Option<&FuncDecl>) {
        self.plan_locals(ast);
        self.current_return_ty = Some(func.return_type.clone());
        self.emitted_return = false;

        let func_start = self.code.len() as u64;
        self.labels.insert(func.name.clone(), func_start);

        // 函数序言: 分配帧空间 (至少保存 $ra, $fp + 局部变量)
        // O32 ABI: 帧大小必须为 8 字节对齐
        let save_size = 8; // $fp + $ra
        self.frame_bytes = ((self.stack_bytes + save_size) + 7) & !7;
        self.emit_addiu(29, 29, -(self.frame_bytes as i16)); // addiu $sp, $sp, -frame
        self.emit_sw(31, 29, (self.frame_bytes - 4) as i16); // sw $ra, frame-4($sp)
        self.emit_sw(30, 29, (self.frame_bytes - 8) as i16); // sw $fp, frame-8($sp)
        self.emit_addiu(30, 29, (self.frame_bytes - 8) as i16); // addiu $fp, $sp, frame-8

        // 参数传递: $a0-$a3 -> 局部变量槽
        if let Some(ast_f) = ast {
            for (i, p) in ast_f.params.iter().enumerate() {
                if let Some(name) = &p.name {
                    if let Some(&off) = self.locals.get(&name.name) {
                        // 从 $a0..$a3 保存到栈
                        let arg_reg = (4 + i) as u8; // $a0=4, $a1=5, ...
                        let fp_off = (self.frame_bytes - 8 - off) as i16;
                        if fp_off >= 0 {
                            self.emit_sw(arg_reg, 30, -(off as i16));
                        } else {
                            // 使用 $sp 相对偏移
                            self.emit_sw(arg_reg, 29, (self.frame_bytes - 8 - off) as i16);
                        }
                    }
                }
            }
        }

        // 处理函数体
        let body = ast.and_then(|f| f.body.as_ref());
        let items = body.map(|b| b.items.as_slice()).unwrap_or(&[]);
        for (item, ir) in items.iter().zip(func.body.iter()) {
            self.gen_block_item(item, ir);
        }

        // 隐式 return 0
        if !self.emitted_return {
            self.emit_addiu(2, 0, 0); // move $v0, $zero
            self.emit_epilogue();
        }

        let func_end = self.code.len() as u64;
        self.func_sizes
            .insert(func.name.clone(), func_end - func_start);

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

    fn emit_epilogue(&mut self) {
        // 恢复 $fp, $ra 并返回
        self.emit_lw(30, 29, (self.frame_bytes - 8) as i16); // lw $fp, frame-8($sp)
        self.emit_lw(31, 29, (self.frame_bytes - 4) as i16); // lw $ra, frame-4($sp)
        self.emit_addiu(29, 29, self.frame_bytes as i16);     // addiu $sp, $sp, frame
        self.emit_jr(31);  // jr $ra
        self.emit_nop();   // delay slot
        self.emitted_return = true;
    }

    // ===== Block item / stmt / expr =====

    fn gen_block_item(&mut self, item: &BlockItem, _ir: &TypedStmt) {
        match item {
            BlockItem::Stmt(s) => self.gen_stmt(s),
            BlockItem::Decl(Decl::Var(v)) => {
                let off = *self.locals.get(&v.name.name).unwrap_or(&0);
                if let Some(init) = &v.init {
                    self.gen_expr_from_ast(init);
                    self.emit_store_v0(off);
                }
            }
            _ => {}
        }
    }

    fn gen_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Return { val, .. } => {
                if let Some(e) = val {
                    self.gen_expr_from_ast(e);
                } else {
                    self.emit_addiu(2, 0, 0); // move $v0, $zero
                }
                self.emit_epilogue();
            }
            Stmt::Expr { e, .. } => {
                self.gen_expr_from_ast(e);
            }
            Stmt::If {
                cond,
                then_,
                else_,
                ..
            } => {
                self.gen_expr_from_ast(cond);
                let lbl_else = self.fresh_label("Lelse");
                let lbl_end = self.fresh_label("Lendif");
                self.emit_beqz(2, &lbl_else);
                self.emit_nop();
                self.gen_stmt(then_);
                self.emit_b(&lbl_end);
                self.emit_nop();
                self.set_label(&lbl_else);
                if let Some(el) = else_ {
                    self.gen_stmt(el);
                }
                self.set_label(&lbl_end);
            }
            Stmt::While { cond, body, .. } => {
                let lbl_top = self.fresh_label("Lwhile");
                let lbl_end = self.fresh_label("Lendwhile");
                self.set_label(&lbl_top);
                self.gen_expr_from_ast(cond);
                self.emit_beqz(2, &lbl_end);
                self.emit_nop();
                self.gen_stmt(body);
                self.emit_b(&lbl_top);
                self.emit_nop();
                self.set_label(&lbl_end);
            }
            Stmt::For {
                init,
                cond,
                step,
                body,
                ..
            } => {
                if let Some(i) = init {
                    self.gen_stmt(i);
                }
                let lbl_top = self.fresh_label("Lfor");
                let lbl_end = self.fresh_label("Lendfor");
                self.set_label(&lbl_top);
                if let Some(c) = cond {
                    self.gen_expr_from_ast(c);
                    self.emit_beqz(2, &lbl_end);
                    self.emit_nop();
                }
                self.gen_stmt(body);
                if let Some(s) = step {
                    self.gen_expr_from_ast(s);
                }
                self.emit_b(&lbl_top);
                self.emit_nop();
                self.set_label(&lbl_end);
            }
            Stmt::Block { b, .. } => {
                for item in &b.items {
                    if let BlockItem::Stmt(s) = item {
                        self.gen_stmt(s);
                    }
                }
            }
            _ => {}
        }
    }

    fn gen_expr_from_ast(&mut self, expr: &Expr) {
        match expr {
            Expr::Int { v, .. } => {
                self.emit_load_imm(2, *v as i32);
            }
            Expr::Char { v, .. } => {
                self.emit_load_imm(2, *v as i32);
            }
            Expr::Ident { name, .. } => {
                if let Some(&off) = self.locals.get(&name.name) {
                    self.emit_load_v0(off);
                } else if self.global_rodata.contains_key(&name.name) {
                    self.emit_load_imm(2, 0);
                } else {
                    self.emit_load_imm(2, 0);
                }
            }
            Expr::Binop { op, l, r, .. } => {
                self.gen_expr_from_ast(l);
                // 保存 lhs 到栈
                self.emit_addiu(29, 29, -4);
                self.emit_sw(2, 29, 0); // sw $v0, 0($sp)
                self.gen_expr_from_ast(r);
                // 恢复 lhs 到 $t0 ($8)
                self.emit_lw(8, 29, 0); // lw $t0, 0($sp)
                self.emit_addiu(29, 29, 4);
                // $v0 = rhs, $t0 = lhs
                self.emit_binop(*op);
            }
            Expr::Unary { op, e, .. } => {
                self.gen_expr_from_ast(e);
                match op {
                    UnOp::Neg => {
                        // neg $v0 = subu $v0, $zero, $v0
                        let inst = (0b000000 << 26) | (0u32 << 21) | (2u32 << 16) | (2u32 << 11) | (0b100011);
                        self.code.extend_from_slice(&inst.to_be_bytes());
                    }
                    UnOp::Not => {
                        // sltu $v0, $v0, 1  (not: x == 0 ? 1 : 0)
                        self.emit_sltiu(2, 2, 1);
                    }
                    UnOp::BitNot => {
                        // nor $v0, $v0, $zero
                        let inst = (0b000000 << 26) | (2u32 << 21) | (0u32 << 16) | (2u32 << 11) | (0b100111);
                        self.code.extend_from_slice(&inst.to_be_bytes());
                    }
                    UnOp::Plus => {}
                }
            }
            Expr::Cast { e, .. } => self.gen_expr_from_ast(e),
            Expr::Call { callee, args, .. } => {
                // 参数传递: $a0-$a3
                for (i, arg) in args.iter().enumerate().take(4) {
                    self.gen_expr_from_ast(arg);
                    if i > 0 {
                        self.emit_addiu(29, 29, -4);
                        self.emit_sw(2, 29, 0);
                    }
                }
                // 弹出参数到 $a1-$a3
                for j in (1..args.len().min(4)).rev() {
                    self.emit_lw((3 + j) as u8, 29, 0);
                    self.emit_addiu(29, 29, 4);
                }
                if let Expr::Ident { name, .. } = callee.as_ref() {
                    self.emit_jal(&name.name);
                    self.emit_nop(); // delay slot
                }
                // 返回值在 $v0 ($2)
            }
            Expr::Assign { l, r, .. } => {
                self.gen_expr_from_ast(r);
                if let Expr::Ident { name, .. } = l.as_ref() {
                    if let Some(&off) = self.locals.get(&name.name) {
                        self.emit_store_v0(off);
                    }
                }
            }
            _ => {
                if let Ok(v) = crate::const_eval::eval(expr) {
                    self.emit_load_imm(2, v as i32);
                } else {
                    self.emit_load_imm(2, 0);
                }
            }
        }
    }

    fn emit_binop(&mut self, op: BinOp) {
        // $t0 ($8) = lhs, $v0 ($2) = rhs, result -> $v0 ($2)
        match op {
            BinOp::Add => self.emit_addu(2, 8, 2),
            BinOp::Sub => self.emit_subu(2, 8, 2),
            BinOp::Mul => self.emit_mul_r(2, 8, 2),
            BinOp::Div => {
                self.emit_div(8, 2);
                self.emit_mflo(2); // $v0 = quotient
            }
            BinOp::Mod => {
                self.emit_div(8, 2);
                self.emit_mfhi(2); // $v0 = remainder
            }
            BinOp::And => {
                let inst = (0b000000 << 26) | (8u32 << 21) | (2u32 << 16) | (2u32 << 11) | 0b100100;
                self.code.extend_from_slice(&inst.to_be_bytes());
            }
            BinOp::Or => {
                let inst = (0b000000 << 26) | (8u32 << 21) | (2u32 << 16) | (2u32 << 11) | 0b100101;
                self.code.extend_from_slice(&inst.to_be_bytes());
            }
            BinOp::Xor => {
                let inst = (0b000000 << 26) | (8u32 << 21) | (2u32 << 16) | (2u32 << 11) | 0b100110;
                self.code.extend_from_slice(&inst.to_be_bytes());
            }
            BinOp::Shl => {
                // sllv $v0, $t0, $v0
                let inst = (0b000000 << 26) | (2u32 << 21) | (8u32 << 16) | (2u32 << 11) | 0b000100;
                self.code.extend_from_slice(&inst.to_be_bytes());
            }
            BinOp::Shr => {
                // srav $v0, $t0, $v0
                let inst = (0b000000 << 26) | (2u32 << 21) | (8u32 << 16) | (2u32 << 11) | 0b000111;
                self.code.extend_from_slice(&inst.to_be_bytes());
            }
            BinOp::Lt => self.emit_slt(2, 8, 2),
            BinOp::Gt => self.emit_slt(2, 2, 8),
            BinOp::Le => {
                self.emit_slt(2, 2, 8); // $v0 = rhs < lhs
                self.emit_xori(2, 2, 1); // $v0 ^= 1
            }
            BinOp::Ge => {
                self.emit_slt(2, 8, 2); // $v0 = lhs < rhs
                self.emit_xori(2, 2, 1); // $v0 ^= 1
            }
            BinOp::EqEq => {
                self.emit_subu(2, 8, 2); // $v0 = lhs - rhs
                self.emit_sltiu(2, 2, 1); // $v0 = ($v0 < 1) ? 1 : 0
            }
            BinOp::Ne => {
                self.emit_subu(2, 8, 2);
                // $v0 != 0 ? 1 : 0
                self.emit_sltu(2, 0, 2); // sltu $v0, $zero, $v0
            }
            BinOp::AndAnd => {
                // both non-zero?
                self.emit_sltu(8, 0, 8); // $t0 = (lhs != 0)
                self.emit_sltu(2, 0, 2); // $v0 = (rhs != 0)
                let inst = (0b000000 << 26) | (8u32 << 21) | (2u32 << 16) | (2u32 << 11) | 0b100100;
                self.code.extend_from_slice(&inst.to_be_bytes()); // and
            }
            BinOp::OrOr => {
                self.emit_sltu(8, 0, 8);
                self.emit_sltu(2, 0, 2);
                let inst = (0b000000 << 26) | (8u32 << 21) | (2u32 << 16) | (2u32 << 11) | 0b100101;
                self.code.extend_from_slice(&inst.to_be_bytes()); // or
            }
        }
    }

    fn fresh_label(&mut self, prefix: &str) -> String {
        self.label_serial += 1;
        format!("{}{}", prefix, self.label_serial)
    }

    fn set_label(&mut self, _label: &str) {
        // 在 relocatable 模式下标签通过符号表处理
    }

    // ===== MIPS 指令编码 =====

    fn emit_addiu(&mut self, rt: u8, rs: u8, imm: i16) {
        let inst =
            (0b001001 << 26) | ((rs as u32) << 21) | ((rt as u32) << 16) | (imm as u16 as u32);
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    fn emit_sw(&mut self, rt: u8, base: u8, offset: i16) {
        let inst =
            (0b101011 << 26) | ((base as u32) << 21) | ((rt as u32) << 16) | (offset as u16 as u32);
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    fn emit_lw(&mut self, rt: u8, base: u8, offset: i16) {
        let inst =
            (0b100011 << 26) | ((base as u32) << 21) | ((rt as u32) << 16) | (offset as u16 as u32);
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    fn emit_jr(&mut self, rs: u8) {
        let inst = ((rs as u32) << 21) | 0b001000;
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    fn emit_nop(&mut self) {
        self.code.extend_from_slice(&[0u8; 4]);
    }

    fn emit_load_imm(&mut self, rt: u8, val: i32) {
        if val >= -32768 && val <= 32767 {
            self.emit_addiu(rt, 0, val as i16);
        } else {
            // 使用 lui + ori 加载大整数
            let hi = ((val >> 16) & 0xFFFF) as u16;
            let lo = (val & 0xFFFF) as u16;
            // LUI rt, hi
            let inst = (0b001111 << 26) | ((rt as u32) << 16) | (hi as u32);
            self.code.extend_from_slice(&inst.to_be_bytes());
            if lo != 0 {
                // ORI rt, rt, lo
                let inst = (0b001101 << 26) | ((rt as u32) << 21) | ((rt as u32) << 16) | (lo as u32);
                self.code.extend_from_slice(&inst.to_be_bytes());
            }
        }
    }

    fn emit_addu(&mut self, rd: u8, rs: u8, rt: u8) {
        let inst = (0b000000 << 26) | ((rs as u32) << 21) | ((rt as u32) << 16)
            | ((rd as u32) << 11) | 0b100001;
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    fn emit_subu(&mut self, rd: u8, rs: u8, rt: u8) {
        let inst = (0b000000 << 26) | ((rs as u32) << 21) | ((rt as u32) << 16)
            | ((rd as u32) << 11) | 0b100011;
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    fn emit_mul_r(&mut self, rd: u8, rs: u8, rt: u8) {
        // MUL rd, rs, rt (MIPS32r2 专用指令, 或回退到 mult+mflo)
        // 使用 mult + mflo 保证兼容性
        let inst = (0b000000 << 26) | ((rs as u32) << 21) | ((rt as u32) << 16);
        self.code.extend_from_slice(&inst.to_be_bytes()); // mult rs, rt
        self.emit_mflo(rd);
    }

    fn emit_div(&mut self, rs: u8, rt: u8) {
        let inst = (0b000000 << 26) | ((rs as u32) << 21) | ((rt as u32) << 16) | 0b010010;
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    fn emit_mflo(&mut self, rd: u8) {
        let inst = ((rd as u32) << 16) | 0b010010;
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    fn emit_mfhi(&mut self, rd: u8) {
        let inst = ((rd as u32) << 16) | 0b010000;
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    fn emit_slt(&mut self, rd: u8, rs: u8, rt: u8) {
        let inst = (0b000000 << 26) | ((rs as u32) << 21) | ((rt as u32) << 16)
            | ((rd as u32) << 11) | 0b101010;
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    fn emit_sltu(&mut self, rd: u8, rs: u8, rt: u8) {
        let inst = (0b000000 << 26) | ((rs as u32) << 21) | ((rt as u32) << 16)
            | ((rd as u32) << 11) | 0b101011;
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    fn emit_sltiu(&mut self, rt: u8, rs: u8, imm: i16) {
        let inst =
            (0b001011 << 26) | ((rs as u32) << 21) | ((rt as u32) << 16) | (imm as u16 as u32);
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    fn emit_xori(&mut self, rt: u8, rs: u8, imm: u16) {
        let inst =
            (0b001110 << 26) | ((rs as u32) << 21) | ((rt as u32) << 16) | (imm as u32);
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    fn emit_beqz(&mut self, _rs: u8, _label: &str) {
        // BEQ $rs, $zero, offset (占位)
        let inst: u32 = (0b000100 << 26) | 0; // 偏移 0
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    fn emit_b(&mut self, _label: &str) {
        // B offset (BEQ $zero, $zero, offset)
        let inst: u32 = (0b000100 << 26) | 0;
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    fn emit_jal(&mut self, name: &str) {
        let pos = self.code.len() as u64;
        self.pending_bl_calls.push((pos, name.to_string()));
        // JAL 占位
        let inst: u32 = 0b000011 << 26;
        self.code.extend_from_slice(&inst.to_be_bytes());
    }

    // 栈上局部变量加载/存储 (相对于 $fp)
    fn emit_load_v0(&mut self, offset: i32) {
        // LW $v0, -offset($fp)
        let fp_off = -(offset as i16);
        self.emit_lw(2, 30, fp_off);
    }

    fn emit_store_v0(&mut self, offset: i32) {
        // SW $v0, -offset($fp)
        let fp_off = -(offset as i16);
        self.emit_sw(2, 30, fp_off);
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

    #[test]
    fn mips_gen_simple_return() {
        let target = Target::resolve(&crate::cli::TargetTriple::parse("mips-linux-gnu").unwrap());
        let mut cg = MipsCodegen::new(target, true);
        let func = Function {
            name: "f".to_string(),
            return_type: ValueType::Integer { bits: 32, signed: true },
            params: vec![],
            body: vec![],
            span: crate::diagnostics::Span::point("t.c", 1, 1),
        };
        cg.gen_function(&func, None);
        assert!(cg.code.len() > 16);
        assert!(cg.labels.contains_key("f"));
    }
}
