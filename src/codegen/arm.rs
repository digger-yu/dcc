// src/codegen/arm.rs - ARM 后端 (AAPCS)

use crate::ast::{BinOp, BlockItem, Decl, Expr, FuncDecl, Program, Stmt, UnOp};
use crate::codegen::elf::{
    self, ElfRela, ElfSection, ElfSym, SHF_ALLOC, SHF_GNU_RETAIN, SHT_PROGBITS, STB_GLOBAL,
    STT_FUNC, STT_GNU_IFUNC,
};
use crate::ir::{Function, Global, Module, TypedStmt, ValueType};
use crate::target::Target;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Copy)]
enum BranchKind {
    Beq,
    B,
}

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
    string_pool: HashMap<Vec<u8>, u64>,
    sym_index: HashMap<String, u32>,
    stack_bytes: i32,
    frame_bytes: i32,
    source_name: String,
    pub extra_sections: Vec<ElfSection>,
    current_return_ty: Option<ValueType>,
    aggregate_fields: HashMap<String, (Arc<Vec<crate::ast::Field>>, bool)>,
    emitted_return: bool,
    label_serial: u32,
    /// 前向 bl 调用: (指令字位置, 目标符号名)
    pending_bl_calls: Vec<(u64, String)>,
    pending_branches: Vec<(u64, String, BranchKind)>,
    break_labels: Vec<String>,
    continue_labels: Vec<String>,
    /// 文件级 asm 原始文本行 (GlobalAsm)
    raw_global_asm_lines: Vec<String>,
    /// 函数级内联 asm 原始文本行: (函数名, 行列表)
    raw_func_asm_lines: Vec<(String, Vec<String>)>,
    /// exec 模式入口地址
    entry: u64,
    /// exec 模式 _start 中 BL main 的指令位置
    start_call_pos: Option<u64>,
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
            string_pool: HashMap::new(),
            sym_index: HashMap::new(),
            stack_bytes: 0,
            frame_bytes: 0,
            source_name: String::new(),
            extra_sections: Vec::new(),
            current_return_ty: None,
            aggregate_fields: HashMap::new(),
            emitted_return: false,
            label_serial: 0,
            pending_bl_calls: Vec::new(),
            pending_branches: Vec::new(),
            break_labels: Vec::new(),
            continue_labels: Vec::new(),
            raw_global_asm_lines: Vec::new(),
            raw_func_asm_lines: Vec::new(),
            entry: if relocatable { 0 } else { 0x400000 + 52 + 32 },
            start_call_pos: None,
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
        // 文件级 asm: 收集原始文本行
        for decl in &prog.decls {
            if let Decl::GlobalAsm { asm_str, outputs, output_names, output_constraints, inputs, input_names, input_constraints, .. } = decl {
                let resolved = Self::resolve_asm_template_simple(asm_str, outputs, output_names, output_constraints, inputs, input_names, input_constraints);
                for line in resolved.split('\n') {
                    let t = line.trim();
                    if !t.is_empty() { self.raw_global_asm_lines.push(t.to_string()); }
                }
            }
        }
        if self.relocatable {
            self.compile_rel(module, prog);
        } else {
            self.compile_exec(module, prog);
        }
    }

    fn compile_exec(&mut self, module: &Module, prog: &Program) {
        // _start: BL main (placeholder)
        let start_pos = self.code.len() as u64;
        self.labels.insert("_start".to_string(), start_pos);
        // BL placeholder (will be patched to main)
        let bl_pos = self.code.len() as u64;
        self.start_call_pos = Some(bl_pos);
        self.code.extend_from_slice(&0xEB000000u32.to_le_bytes()); // BL offset=0
        // r0 already has main's return value
        // mov r7, #1 (__NR_exit)
        self.emit_mov_imm(7, 1);
        // svc #0
        let svc_inst: u32 = 0xEF000000; // SVC #0
        self.code.extend_from_slice(&svc_inst.to_le_bytes());
        // Compile functions
        for func in &module.functions {
            let has_body = prog.decls.iter().find_map(|d| match d {
                Decl::Func(f) if f.name.name == func.name => Some(f),
                _ => None,
            }).and_then(|f| f.body.as_ref()).is_some();
            if !has_body { continue; }
            self.gen_function(func, prog.decls.iter().find_map(|d| match d {
                Decl::Func(f) if f.name.name == func.name => Some(f),
                _ => None,
            }));
            // Patch BL to main
            if func.name == "main" {
                if let Some(bl_pos) = self.start_call_pos {
                    if let Some(&main_addr) = self.labels.get("main") {
                        self.patch_b(bl_pos, main_addr); // BL uses same offset encoding as B
                    }
                }
            }
        }
        // Finalize pending branches
        self.finalize_branches();
    }

    fn finalize_branches(&mut self) {
        let pending: Vec<_> = self.pending_branches.drain(..).collect();
        for (at, label, kind) in pending {
            if let Some(&target) = self.labels.get(&label) {
                match kind {
                    BranchKind::Beq => self.patch_beq(at, target),
                    BranchKind::B => self.patch_b(at, target),
                }
            }
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
        // Evaluate constant initializer or default to zero
        if let Some(init_expr) = &g.initializer {
            // Try to extract constant value from TypedExpr
            let val = match (&init_expr.kind, &init_expr.literal) {
                (crate::ir::ExprKind::Constant, Some(crate::ir::ExprLiteral::Int(v))) => Some(*v as i64),
                _ => None,
            };
            if let Some(val) = val {
                // Emit proper size based on type
                let size = match &g.ty {
                    ValueType::Integer { bits, .. } => (*bits / 8).max(4) as usize,
                    ValueType::Pointer(_) => 4, // ARM32: 4-byte pointers
                    _ => 4,
                };
                if size == 8 {
                    self.rodata.extend_from_slice(&(val as i64).to_le_bytes());
                } else {
                    self.rodata.extend_from_slice(&(val as i32).to_le_bytes());
                }
            } else {
                // Non-constant initializer: emit zero for now
                self.rodata.extend_from_slice(&[0u8; 4]);
            }
        } else {
            // No initializer: emit zero
            self.rodata.extend_from_slice(&[0u8; 4]);
        }
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
        self.stack_bytes += (size + 3) & !3; // 4 字节对齐
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

        // 函数序言：保存 fp, lr 并设置帧指针
        self.emit_push(vec![11, 14]); // push {fp, lr}
        self.emit_mov_reg(11, 13); // mov fp, sp

        // 分配局部变量空间
        self.frame_bytes = (self.stack_bytes + 7) & !7; // 8 字节对齐
        if self.frame_bytes > 0 {
            self.emit_sub_imm(13, 13, self.frame_bytes as u32); // sub sp, sp, #frame
        }

        // 处理函数体
        let body = ast.and_then(|f| f.body.as_ref());
        let items = body.map(|b| b.items.as_slice()).unwrap_or(&[]);
        for (item, ir) in items.iter().zip(func.body.iter()) {
            self.gen_block_item(item, ir);
        }

        // 隐式 return 0
        if !self.emitted_return {
            self.emit_mov_imm(0, 0); // mov r0, #0
            self.emit_epilogue();
        }

        let func_end = self.code.len() as u64;
        self.func_sizes
            .insert(func.name.clone(), func_end - func_start);

        // 添加符号
        let sym_type = if ast.and_then(|f| f.ifunc_resolver.as_ref()).is_some() {
            STT_GNU_IFUNC
        } else {
            STT_FUNC
        };
        let sym_idx = self.symbols.len() as u32;
        self.symbols.push(ElfSym {
            name: func.name.clone(),
            section: 1,
            value: func_start,
            size: func_end - func_start,
            bind: STB_GLOBAL,
            ty: sym_type,
            visibility: 0,
        });
        self.sym_index.insert(func.name.clone(), sym_idx);

        // attribute((used)): 发射 .used.<name> 节并标记 SHF_GNU_RETAIN
        if ast.map(|f| f.is_used_attr).unwrap_or(false) {
            let sec = ElfSection {
                name: format!(".used.{}", func.name),
                data: vec![],
                ty: SHT_PROGBITS,
                flags: SHF_ALLOC | SHF_GNU_RETAIN,
                align: 1,
            };
            self.extra_sections.push(sec);
        }
    }

    fn emit_epilogue(&mut self) {
        // 恢复 sp 和弹出 fp, pc
        if self.frame_bytes > 0 {
            self.emit_add_imm(13, 13, self.frame_bytes as u32); // add sp, sp, #frame
        }
        self.emit_mov_reg(13, 11); // mov sp, fp
        self.emit_pop(vec![11, 15]); // pop {fp, pc}
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
                    self.emit_store_r0(off);
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
                    self.emit_mov_imm(0, 0);
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
                self.emit_cmp_imm(0, 0); // cmp r0, #0
                self.emit_beq(&lbl_else); // beq else
                self.gen_stmt(then_);
                self.emit_b(&lbl_end);
                self.set_label(&lbl_else);
                if let Some(el) = else_ {
                    self.gen_stmt(el);
                }
                self.set_label(&lbl_end);
            }
            Stmt::While { cond, body, .. } => {
                let lbl_top = self.fresh_label("Lwhile");
                let lbl_end = self.fresh_label("Lendwhile");
                self.break_labels.push(lbl_end.clone());
                self.continue_labels.push(lbl_top.clone());
                self.set_label(&lbl_top);
                self.gen_expr_from_ast(cond);
                self.emit_cmp_imm(0, 0);
                self.emit_beq(&lbl_end);
                self.gen_stmt(body);
                self.emit_b(&lbl_top);
                self.set_label(&lbl_end);
                self.break_labels.pop();
                self.continue_labels.pop();
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
                let lbl_step = self.fresh_label("Lforstep");
                self.break_labels.push(lbl_end.clone());
                self.continue_labels.push(lbl_step.clone());
                self.set_label(&lbl_top);
                if let Some(c) = cond {
                    self.gen_expr_from_ast(c);
                    self.emit_cmp_imm(0, 0);
                    self.emit_beq(&lbl_end);
                }
                self.gen_stmt(body);
                self.set_label(&lbl_step);
                if let Some(s) = step {
                    self.gen_expr_from_ast(s);
                }
                self.emit_b(&lbl_top);
                self.set_label(&lbl_end);
                self.break_labels.pop();
                self.continue_labels.pop();
            }
            Stmt::Block { b, .. } => {
                for item in &b.items {
                    if let BlockItem::Stmt(s) = item {
                        self.gen_stmt(s);
                    }
                }
            }
            Stmt::Do { body, cond, .. } => {
                let lbl_body = self.fresh_label("Ldobody");
                let lbl_cond = self.fresh_label("Ldocond");
                let lbl_end = self.fresh_label("Lenddo");
                self.break_labels.push(lbl_end.clone());
                self.continue_labels.push(lbl_cond.clone());
                self.set_label(&lbl_body);
                self.gen_stmt(body);
                self.set_label(&lbl_cond);
                self.gen_expr_from_ast(cond);
                self.emit_cmp_imm(0, 0);
                self.emit_beq(&lbl_end);
                self.emit_b(&lbl_body);
                self.set_label(&lbl_end);
                self.break_labels.pop();
                self.continue_labels.pop();
            }
            Stmt::Break { .. } => {
                if let Some(lbl) = self.break_labels.last().cloned() {
                    self.emit_b(&lbl);
                }
            }
            Stmt::Continue { .. } => {
                if let Some(lbl) = self.continue_labels.last().cloned() {
                    self.emit_b(&lbl);
                }
            }
            Stmt::Switch { expr, cases, .. } => {
                self.gen_expr_from_ast(expr);
                // r0 now holds the switch expression value
                let lbl_end = self.fresh_label("Lendswitch");
                let lbl_default = self.fresh_label("Ldefault");
                self.break_labels.push(lbl_end.clone());
                // Generate case comparisons
                let mut case_labels = Vec::new();
                for case in cases {
                    let lbl_case = self.fresh_label("Lcase");
                    if let Some(val_expr) = &case.val {
                        // case N:
                        if let Ok(val) = crate::const_eval::eval(val_expr) {
                            self.emit_load_imm(1, val as i32); // r1 = case value
                            self.emit_cmp(0, 1); // compare r0 with r1
                            let lbl_next = self.fresh_label("Lnextcase");
                            // Use BEQ to jump to case body if equal
                            let pos = self.code.len() as u64;
                            self.pending_branches.push((pos, lbl_case.clone(), BranchKind::Beq));
                            self.code.extend_from_slice(&0x0A000000u32.to_le_bytes());
                            case_labels.push((lbl_case, case));
                        }
                    } else {
                        // default:
                        case_labels.push((lbl_default.clone(), case));
                    }
                }
                // Jump to default if no case matched
                self.emit_b(&lbl_default);
                // Generate case bodies
                for (lbl, case) in &case_labels {
                    self.set_label(lbl);
                    for item in &case.body.items {
                        if let BlockItem::Stmt(s) = item {
                            self.gen_stmt(s);
                        }
                    }
                }
                self.set_label(&lbl_end);
                self.break_labels.pop();
            }
            Stmt::AsmGoto { asm_str, outputs, output_names, output_constraints, inputs, input_names, input_constraints, goto_labels, span, .. } => {
                eprintln!("{}:{}: warning: asm goto on non-x86_64 target: inline asm text preserved in -S output only", span.file, span.line);
                let mut resolved_asm = asm_str.clone();
                let label_base = outputs.len() + inputs.len();
                for (i, label) in goto_labels.iter().enumerate().rev() {
                    resolved_asm = resolved_asm.replace(&format!("%l{}", i), &label.name);
                    resolved_asm = resolved_asm.replace(&format!("%l{}", label_base + i), &label.name);
                    resolved_asm = resolved_asm.replace(&format!("%l[{}]", label.name), &label.name);
                }
                let resolved = Self::resolve_asm_template_simple(&resolved_asm, outputs, output_names, output_constraints, inputs, input_names, input_constraints);
                let lines: Vec<String> = resolved.split('\n').map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
                self.raw_func_asm_lines.push((self.source_name.clone(), lines));
            }
            _ => {}
        }
    }

    fn gen_expr_from_ast(&mut self, expr: &Expr) {
        match expr {
            Expr::Int { v, .. } => {
                self.emit_load_imm(0, *v as i32);
            }
            Expr::Char { v, .. } => {
                self.emit_load_imm(0, *v as i32);
            }
            Expr::String { data, .. } => {
                // Emit string to .rodata and load address
                let offset = if let Some(&off) = self.string_pool.get(data) {
                    off
                } else {
                    let off = self.rodata.len() as u64;
                    self.rodata.extend_from_slice(data);
                    // Ensure null terminator
                    if data.last() != Some(&0) {
                        self.rodata.push(0);
                    }
                    self.string_pool.insert(data.clone(), off);
                    off
                };
                // Load string address into r0
                self.emit_load_imm(0, offset as i32);
            }
            Expr::Ident { name, .. } => {
                if let Some(&off) = self.locals.get(&name.name) {
                    self.emit_load_r0(off);
                } else if self.global_rodata.contains_key(&name.name) {
                    // 全局变量地址 (简化)
                    self.emit_load_imm(0, 0);
                } else {
                    self.emit_load_imm(0, 0);
                }
            }
            Expr::Binop { op, l, r, .. } => {
                self.gen_expr_from_ast(l);
                self.emit_push(vec![0]); // push r0 (lhs)
                self.gen_expr_from_ast(r);
                self.emit_pop(vec![1]); // pop r1 (lhs)
                // r0 = rhs, r1 = lhs
                self.emit_binop(*op);
            }
            Expr::Unary { op, e, .. } => {
                self.gen_expr_from_ast(e);
                match op {
                    UnOp::Neg => self.emit_rsb_imm(0, 0, 0), // rsb r0, r0, #0
                    UnOp::Not => {
                        self.emit_cmp_imm(0, 0);
                        self.emit_mov_imm(0, 0); // mov r0, #0
                        self.emit_moveq(0, 1); // moveq r0, #1
                    }
                    UnOp::BitNot => self.emit_mvn(0, 0), // mvn r0, r0
                    UnOp::Plus => {}
                }
            }
            Expr::Cast { e, .. } => self.gen_expr_from_ast(e),
            Expr::Call { callee, args, .. } => {
                // 参数传递: r0-r3
                for (i, arg) in args.iter().enumerate().take(4) {
                    self.gen_expr_from_ast(arg);
                    if i > 0 {
                        self.emit_push(vec![0]); // push intermediate args
                    }
                }
                // 弹出参数到 r1-r3
                if args.len() > 1 {
                    let mut regs: Vec<u8> = (1..args.len().min(4) as u8).collect();
                    regs.reverse();
                    // Pop in reverse order to r1, r2, r3
                    for j in (1..args.len().min(4)).rev() {
                        self.emit_pop(vec![j as u8]);
                    }
                }
                if let Expr::Ident { name, .. } = callee.as_ref() {
                    self.emit_bl(&name.name);
                }
            }
            Expr::Assign { l, r, .. } => {
                self.gen_expr_from_ast(r);
                if let Expr::Ident { name, .. } = l.as_ref() {
                    if let Some(&off) = self.locals.get(&name.name) {
                        self.emit_store_r0(off);
                    }
                }
            }
            Expr::AsmExpr { asm_str, outputs, output_names, output_constraints, inputs, input_names, input_constraints, span, .. } => {
                eprintln!("{}:{}: warning: inline asm on non-x86_64 target: result is 0", span.file, span.line);
                let resolved = Self::resolve_asm_template_simple(asm_str, outputs, output_names, output_constraints, inputs, input_names, input_constraints);
                let lines: Vec<String> = resolved.split('\n').map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
                self.raw_func_asm_lines.push((format!("__inline_{}", self.label_serial), lines));
                self.label_serial += 1;
                self.emit_load_imm(0, 0);
            }
            _ => {
                if let Ok(v) = crate::const_eval::eval(expr) {
                    self.emit_load_imm(0, v as i32);
                } else {
                    self.emit_load_imm(0, 0);
                }
            }
        }
    }

    fn emit_binop(&mut self, op: BinOp) {
        match op {
            BinOp::Add => self.emit_add(0, 1, 0),     // add r0, r1, r0
            BinOp::Sub => self.emit_sub(0, 1, 0),     // sub r0, r1, r0
            BinOp::Mul => self.emit_mul(0, 1, 0),     // mul r0, r1, r0
            BinOp::Div => {
                // 简化: 调用 __aeabi_idiv (需要链接库)
                // 这里先做简单处理
                self.emit_push(vec![14]); // push lr
                self.emit_bl("__aeabi_idiv");
                self.emit_pop(vec![14]); // pop lr
            }
            BinOp::Mod => {
                // 简化: a % b = a - (a/b)*b
                self.emit_push(vec![0, 1]); // save both
                self.emit_push(vec![14]);
                self.emit_bl("__aeabi_idiv");
                self.emit_push(vec![0]); // save quotient
                self.emit_pop(vec![1]); // pop divisor to r1
                self.emit_pop(vec![0]); // pop dividend to r0
                // r0 = quotient, r1 = divisor (need to recompute)
                // 简化: 直接返回 0
                self.emit_pop(vec![14]);
                self.emit_load_imm(0, 0);
            }
            BinOp::And => self.emit_and(0, 1, 0),
            BinOp::Or => self.emit_orr(0, 1, 0),
            BinOp::Xor => self.emit_eor(0, 1, 0),
            BinOp::Shl => self.emit_lsl(0, 1, 0),
            BinOp::Shr => self.emit_asr(0, 1, 0),
            BinOp::Lt => {
                self.emit_cmp(1, 0);
                self.emit_mov_imm(0, 0);
                self.emit_movlt(0, 1);
            }
            BinOp::Gt => {
                self.emit_cmp(1, 0);
                self.emit_mov_imm(0, 0);
                self.emit_movgt(0, 1);
            }
            BinOp::Le => {
                self.emit_cmp(1, 0);
                self.emit_mov_imm(0, 0);
                self.emit_movle(0, 1);
            }
            BinOp::Ge => {
                self.emit_cmp(1, 0);
                self.emit_mov_imm(0, 0);
                self.emit_movge(0, 1);
            }
            BinOp::EqEq => {
                self.emit_cmp(1, 0);
                self.emit_mov_imm(0, 0);
                self.emit_moveq(0, 1);
            }
            BinOp::Ne => {
                self.emit_cmp(1, 0);
                self.emit_mov_imm(0, 0);
                self.emit_movne(0, 1);
            }
            BinOp::AndAnd => {
                self.emit_and(0, 1, 0);
                self.emit_cmp_imm(0, 0);
                self.emit_mov_imm(0, 0);
                self.emit_movne(0, 1);
            }
            BinOp::OrOr => {
                self.emit_orr(0, 1, 0);
                self.emit_cmp_imm(0, 0);
                self.emit_mov_imm(0, 0);
                self.emit_movne(0, 1);
            }
        }
    }

    fn fresh_label(&mut self, prefix: &str) -> String {
        self.label_serial += 1;
        format!("{}{}", prefix, self.label_serial)
    }

    fn set_label(&mut self, label: &str) {
        let pos = self.code.len() as u64;
        self.labels.insert(label.to_string(), pos);
        // Patch all pending branches targeting this label
        let pending: Vec<_> = self
            .pending_branches
            .iter()
            .filter(|(_, l, _)| l == label)
            .cloned()
            .collect();
        for (at, _, kind) in pending {
            match kind {
                BranchKind::Beq => self.patch_beq(at, pos),
                BranchKind::B => self.patch_b(at, pos),
            }
        }
        self.pending_branches.retain(|(_, l, _)| l != label);
    }

    fn patch_beq(&mut self, at: u64, target: u64) {
        // ARM BEQ offset: (target - branch_pc - 8) / 4
        // branch_pc is the position of the BEQ instruction
        let offset = ((target as i64) - (at as i64) - 8) / 4;
        let offset = offset as u32 & 0x00FFFFFF;
        // Read existing instruction, preserve condition code, update offset
        let existing = u32::from_le_bytes([
            self.code[at as usize],
            self.code[at as usize + 1],
            self.code[at as usize + 2],
            self.code[at as usize + 3],
        ]);
        let patched = (existing & 0xFF000000) | offset;
        self.code[at as usize..at as usize + 4]
            .copy_from_slice(&patched.to_le_bytes());
    }

    fn patch_b(&mut self, at: u64, target: u64) {
        // ARM B (AL condition) offset: (target - branch_pc - 8) / 4
        let offset = ((target as i64) - (at as i64) - 8) / 4;
        let offset = offset as u32 & 0x00FFFFFF;
        let patched = 0xEA000000 | offset; // B with AL condition
        self.code[at as usize..at as usize + 4]
            .copy_from_slice(&patched.to_le_bytes());
    }

    // ===== ARM 指令编码 =====

    fn emit_push(&mut self, regs: Vec<u8>) {
        let mut reg_list: u16 = 0;
        for r in regs {
            reg_list |= 1 << r;
        }
        let inst = 0xE92D0000 | reg_list as u32;
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    fn emit_pop(&mut self, regs: Vec<u8>) {
        let mut reg_list: u16 = 0;
        for r in regs {
            reg_list |= 1 << r;
        }
        let inst = 0xE8BD0000 | reg_list as u32;
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    fn emit_mov_reg(&mut self, rd: u8, rm: u8) {
        let inst = 0xE1A00000 | ((rd as u32) << 12) | (rm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    fn emit_mov_imm(&mut self, rd: u8, imm: u8) {
        // MOV rd, #imm (立即数)
        let inst = 0xE3A00000 | ((rd as u32) << 12) | (imm as u32 & 0xFF);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    fn emit_load_imm(&mut self, rd: u8, val: i32) {
        // 简化: 使用 MOV + MVN 处理小/大整数
        if val >= 0 && val < 256 {
            self.emit_mov_imm(rd, val as u8);
        } else if val < 0 && val >= -256 {
            // MVN rd, #(|val|-1)
            let inst = 0xE3E00000 | ((rd as u32) << 12) | ((-val - 1) as u32 & 0xFF);
            self.code.extend_from_slice(&inst.to_le_bytes());
        } else {
            // 大整数: 使用 literal pool (简化为 movw/movt 序列)
            // ARMv7 支持 MOVW/MOVT
            let lo = (val & 0xFFFF) as u32;
            let hi = ((val >> 16) & 0xFFFF) as u32;
            // MOVW rd, #lo
            let inst = 0xE3000000 | ((rd as u32) << 12) | ((hi & 0xF) << 16) | lo;
            self.code.extend_from_slice(&inst.to_le_bytes());
            if hi > 0 {
                // MOVT rd, #hi
                let inst = 0xE3400000 | ((rd as u32) << 12) | ((hi & 0xF) << 16) | (hi >> 4);
                self.code.extend_from_slice(&inst.to_le_bytes());
            }
        }
    }

    fn emit_add(&mut self, rd: u8, rn: u8, rm: u8) {
        let inst = 0xE0800000 | ((rd as u32) << 12) | ((rn as u32) << 16) | (rm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    fn emit_sub(&mut self, rd: u8, rn: u8, rm: u8) {
        let inst = 0xE0400000 | ((rd as u32) << 12) | ((rn as u32) << 16) | (rm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    fn emit_sub_imm(&mut self, rd: u8, rn: u8, imm: u32) {
        let inst = 0xE2400000 | ((rd as u32) << 12) | ((rn as u32) << 16) | (imm & 0xFF);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    fn emit_add_imm(&mut self, rd: u8, rn: u8, imm: u32) {
        let inst = 0xE2800000 | ((rd as u32) << 12) | ((rn as u32) << 16) | (imm & 0xFF);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    fn emit_mul(&mut self, rd: u8, rm: u8, rs: u8) {
        // MUL rd, rm, rs => rd = rm * rs
        let inst = 0xE0000090 | ((rd as u32) << 16) | ((rs as u32) << 8) | (rm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    fn emit_and(&mut self, rd: u8, rn: u8, rm: u8) {
        let inst = 0xE0000000 | ((rd as u32) << 12) | ((rn as u32) << 16) | (rm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    fn emit_orr(&mut self, rd: u8, rn: u8, rm: u8) {
        let inst = 0xE1800000 | ((rd as u32) << 12) | ((rn as u32) << 16) | (rm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    fn emit_eor(&mut self, rd: u8, rn: u8, rm: u8) {
        let inst = 0xE0200000 | ((rd as u32) << 12) | ((rn as u32) << 16) | (rm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    fn emit_lsl(&mut self, rd: u8, rm: u8, rs: u8) {
        // LSL rd, rm, rs (寄存器移位)
        let inst = 0xE1A00010 | ((rd as u32) << 12) | (rs as u32) << 8 | (rm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    fn emit_asr(&mut self, rd: u8, rm: u8, rs: u8) {
        // ASR rd, rm, rs
        let inst = 0xE1A00050 | ((rd as u32) << 12) | (rs as u32) << 8 | (rm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    fn emit_mvn(&mut self, rd: u8, rm: u8) {
        let inst = 0xE1E00000 | ((rd as u32) << 12) | (rm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    fn emit_rsb_imm(&mut self, rd: u8, rn: u8, imm: u8) {
        // RSB rd, rn, #imm => rd = imm - rn
        let inst = 0xE2600000 | ((rd as u32) << 12) | ((rn as u32) << 16) | (imm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    fn emit_cmp(&mut self, rn: u8, rm: u8) {
        let inst = 0xE1500000 | ((rn as u32) << 16) | (rm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    fn emit_cmp_imm(&mut self, rn: u8, imm: u8) {
        let inst = 0xE3500000 | ((rn as u32) << 16) | (imm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    fn emit_b(&mut self, label: &str) {
        // B: 记录 pending branch
        let pos = self.code.len() as u64;
        self.pending_branches.push((pos, label.to_string(), BranchKind::B));
        // 占位 B 指令 (AL condition)
        self.code.extend_from_slice(&0xEA000000u32.to_le_bytes());
    }

    fn emit_beq(&mut self, label: &str) {
        // BEQ: 条件码 0x0
        let pos = self.code.len() as u64;
        self.pending_branches.push((pos, label.to_string(), BranchKind::Beq));
        // 占位 BEQ 指令 (EQ condition = 0x0)
        self.code.extend_from_slice(&0x0A000000u32.to_le_bytes());
    }

    fn emit_bl(&mut self, name: &str) {
        // BL: 记录 pending call
        let pos = self.code.len() as u64;
        self.pending_bl_calls.push((pos, name.to_string()));
        // 占位 BL 指令
        self.code.extend_from_slice(&0xEB000000u32.to_le_bytes());
    }

    // 条件 MOV 指令
    fn emit_mov_cond(&mut self, cond: u8, rd: u8, imm: u8) {
        let inst = 0xE3B00000 | ((cond as u32) << 28) | ((rd as u32) << 12) | (imm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    fn emit_moveq(&mut self, rd: u8, imm: u8) {
        // MOVEQ rd, #imm (cond = 0x0)
        let inst = 0x03A00000 | ((rd as u32) << 12) | (imm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    fn emit_movne(&mut self, rd: u8, imm: u8) {
        // MOVNE rd, #imm (cond = 0x1)
        let inst = 0x13A00000 | ((rd as u32) << 12) | (imm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    fn emit_movlt(&mut self, rd: u8, imm: u8) {
        // MOVLT rd, #imm (cond = 0xB)
        let inst = 0xB3A00000 | ((rd as u32) << 12) | (imm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    fn emit_movgt(&mut self, rd: u8, imm: u8) {
        // MOVGT rd, #imm (cond = 0xC)
        let inst = 0xC3A00000 | ((rd as u32) << 12) | (imm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    fn emit_movle(&mut self, rd: u8, imm: u8) {
        // MOVLE rd, #imm (cond = 0xD)
        let inst = 0xD3A00000 | ((rd as u32) << 12) | (imm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    fn emit_movge(&mut self, rd: u8, imm: u8) {
        // MOVGE rd, #imm (cond = 0xA)
        let inst = 0xA3A00000 | ((rd as u32) << 12) | (imm as u32);
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    // 栈上局部变量加载/存储
    fn emit_load_r0(&mut self, offset: i32) {
        // LDR r0, [fp, #offset]
        if offset >= 0 {
            let inst = 0xE59B0000 | (offset as u32 & 0xFFF);
            self.code.extend_from_slice(&inst.to_le_bytes());
        } else {
            let inst = 0xE51B0000 | ((-offset) as u32 & 0xFFF);
            self.code.extend_from_slice(&inst.to_le_bytes());
        }
    }

    fn emit_store_r0(&mut self, offset: i32) {
        // STR r0, [fp, #offset]
        if offset >= 0 {
            let inst = 0xE58B0000 | (offset as u32 & 0xFFF);
            self.code.extend_from_slice(&inst.to_le_bytes());
        } else {
            let inst = 0xE50B0000 | ((-offset) as u32 & 0xFFF);
            self.code.extend_from_slice(&inst.to_le_bytes());
        }
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
            elf::build_elf32_exec_arm(&self.code, &self.rodata, self.entry as u32)
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
        // 发射文件级 asm 原始文本
        for line in &self.raw_global_asm_lines {
            out.push_str(&format!("\t{line}\n"));
        }
        for func in &module.functions {
            out.push_str(&format!("\t.globl {}\n", func.name));
            out.push_str(&format!("{}:\n", func.name));
            // 发射函数级内联 asm 原始文本
            for (fname, lines) in &self.raw_func_asm_lines {
                if fname == &func.name || fname.starts_with("__inline_") {
                    for line in lines {
                        out.push_str(&format!("\t{line}\n"));
                    }
                }
            }
            if let Some(size) = self.func_sizes.get(&func.name) {
                out.push_str(&format!("\t@ ARM IR lowering, {} bytes\n", size));
            }
            out.push_str("\tpush {fp, lr}\n");
            out.push_str("\tmov fp, sp\n");
            out.push_str("\tmov r0, #0\n");
            out.push_str("\tpop {fp, pc}\n");
        }
        out
    }

    /// 简化版 asm 模板占位符替换: 仅处理 %N / %[name] / %% (无寄存器分配)
    fn resolve_asm_template_simple(
        asm_str: &str,
        _outputs: &[Expr],
        output_names: &[Option<String>],
        _output_constraints: &[String],
        _inputs: &[Expr],
        input_names: &[Option<String>],
        _input_constraints: &[String],
    ) -> String {
        let s = if asm_str.starts_with('"') && asm_str.ends_with('"') && asm_str.len() >= 2 {
            &asm_str[1..asm_str.len() - 1]
        } else {
            asm_str
        };
        let mut result = String::new();
        let chars: Vec<char> = s.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '%' && i + 1 < chars.len() {
                match chars[i + 1] {
                    '%' => { result.push('%'); i += 2; }
                    '[' => {
                        if let Some(end) = chars[i+2..].iter().position(|&c| c == ']') {
                            let name: String = chars[i+2..i+2+end].iter().collect();
                            let all_names: Vec<&Option<String>> = output_names.iter().chain(input_names.iter()).collect();
                            for (idx, n) in all_names.iter().enumerate() {
                                if n.as_ref().map(|s| s.as_str()) == Some(&name) {
                                    result.push_str(&format!("%{}", idx));
                                    break;
                                }
                            }
                            i += 2 + end + 1;
                        } else {
                            result.push(chars[i]);
                            i += 1;
                        }
                    }
                    c if c.is_ascii_digit() => {
                        result.push('%');
                        result.push(c);
                        i += 2;
                    }
                    _ => {
                        result.push(chars[i]);
                        i += 1;
                    }
                }
            } else {
                result.push(chars[i]);
                i += 1;
            }
        }
        result
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
        cg.emit_mov_reg(0, 0);
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

    #[test]
    fn arm_gen_simple_return() {
        let target = Target::resolve(&crate::cli::TargetTriple::parse("arm-linux-gnueabihf").unwrap());
        let mut cg = ArmCodegen::new(target, true);
        let func = Function {
            name: "f".to_string(),
            return_type: ValueType::Integer { bits: 32, signed: true },
            params: vec![],
            body: vec![],
            span: crate::diagnostics::Span::point("t.c", 1, 1),
        };
        cg.gen_function(&func, None);
        // Should emit prologue + mov r0, #0 + epilogue
        assert!(cg.code.len() > 12);
        assert!(cg.labels.contains_key("f"));
    }
}
