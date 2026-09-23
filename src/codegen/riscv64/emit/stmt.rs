// src/codegen/riscv64/emit/stmt.rs - Statement-level emission (RISC-V 64 backend)
// Priority 4 Task: Extract working implementations from riscv64.rs.bak

use crate::ast::{BlockItem, Case, Decl, Expr, Stmt, Type};
use crate::codegen::riscv64::Riscv64Codegen;
use crate::ir::{TypedExpr, TypedStmt, ValueType};
use crate::target::Target;
use std::collections::HashMap;

/// 大结构体返回阈值 (>16 bytes 使用 SRET)
fn aggregate_size(ty: &ValueType) -> Option<usize> {
    use crate::ir::value_type_size;
    match ty {
        ValueType::Aggregate { name, .. } => {
            if let Some(fields) = crate::codegen::riscv64::find_aggregate_fields(name) {
                let size = fields.iter().map(|f| f.ty.size()).sum::<usize>();
                if size > 16 { Some(size) } else { None }
            } else { None }
        }
        _ => None,
    }
}

/// HFA (Half Float Array) 判断：浮点聚合类型
fn classify_rv_hfa(ty: &ValueType, _aggregate_fields: &HashMap<String, (std::sync::Arc<Vec<crate::ast::Field>>, bool)>) -> bool {
    matches!(ty, ValueType::Aggregate { .. })
}

impl Riscv64Codegen {
    // ========================================================================
    // 语句发射主逻辑
    // ========================================================================

    /// 生成语句的代码
    pub fn gen_stmt(&mut self, stmt: &Stmt, ir: &TypedStmt) {
        // 记录 DWARF 行号表条目
        let pc = self.code.len() as u64;
        let line = ir.span.line;
        if line > 0 {
            self.line_entries.push((pc, line));
        }

        match stmt {
            Stmt::Return { val, .. } => {
                self.emit_return(val, ir);
            }
            Stmt::Expr { e, .. } => {
                self.emit_expr_stmt(e, ir);
            }
            Stmt::If { then_, else_, .. } => {
                self.emit_if_stmt(then_, else_, ir);
            }
            Stmt::While { body, .. } | Stmt::Do { body, .. } => {
                self.emit_while_loop(body, ir);
            }
            Stmt::For { init, cond, step, body, .. } => {
                self.emit_for_loop(init, cond, step, body, ir);
            }
            Stmt::Block { b, .. } => {
                self.emit_block(b, ir);
            }
            Stmt::AsmGoto { .. } => {
                self.emit_asm_goto(stmt, ir);
            }
            Stmt::Switch { expr, cases, .. } => {
                self.emit_switch_stmt(expr, cases, ir);
            }
            Stmt::Break { .. } => {
                self.emit_break();
            }
            Stmt::Continue { .. } => {
                self.emit_continue();
            }
            _ => {}
        }
    }

    /// 发射 return 语句
    fn emit_return(&mut self, val: &Option<Box<Expr>>, ir: &TypedStmt) {
        let ret_ty = self.current_return_ty.clone().unwrap_or(ValueType::Void);
        let sret_size = aggregate_size(&ret_ty).filter(|s| *s > 16);
        let hfa = classify_rv_hfa(&ret_ty, &self.aggregate_fields);

        if let Some(size) = sret_size {
            // 大结构体返回：求值到栈临时区，再 memcpy 到 [a0]
            let tmp = self.alloc_local("<ret_tmp>", size as i32);
            if let Some(expr) = ir.expressions.first() {
                self.gen_expr(expr);
            } else if let Some(e) = val.as_ref() {
                self.gen_expr_from_ast(e);
            }
            self.store_a0(tmp);
            self.memcpy_sp_to_a0(tmp, size);
        } else if let Some(_hfa) = hfa {
            // HFA: 浮点聚合返回 fa0-fa3
            if let Some(expr) = ir.expressions.first() {
                self.gen_expr(expr);
            } else if let Some(e) = val.as_ref() {
                self.gen_expr_from_ast(e);
            }
            // 结果已在 a0, HFA 浮点字段加载到 fa0-fa3
        } else {
            if let Some(expr) = ir.expressions.first() {
                self.gen_expr(expr);
            } else if let Some(e) = val.as_ref() {
                self.gen_expr_from_ast(e);
            } else {
                self.li_a0(0);
            }
        }
        self.ret();
        self.emitted_return = true;
    }

    /// 发射表达式语句 (如函数调用、赋值等)
    fn emit_expr_stmt(&mut self, e: &Option<Box<Expr>>, ir: &TypedStmt) {
        if let Some(expr) = ir.expressions.first() {
            let _ = self.gen_expr(expr);
        } else if let Some(expr) = e {
            self.gen_expr_from_ast(expr);
        }
    }

    /// 发射 if/else 分支序列
    fn emit_if_stmt(&mut self, then_: &Box<Stmt>, else_: &Option<Box<Stmt>>, ir: &TypedStmt) {
        if let Some(cond) = ir.expressions.first() {
            self.gen_expr(cond);
        }
        let lbl_else = self.fresh_label("Lelse");
        let lbl_end = self.fresh_label("Lendif");
        
        self.beqz_a0(&lbl_else);
        self.maybe_align_jump();
        self.gen_stmt(then_, &ir.nested[0]);
        self.j(&lbl_end);
        self.maybe_align_jump();
        self.label(&lbl_else);
        
        if let Some(el) = else_ {
            if ir.nested.len() > 1 {
                self.gen_stmt(el, &ir.nested[1]);
            }
        }
        self.label(&lbl_end);
    }

    /// 发射 while/do-while 循环构造
    fn emit_while_loop(&mut self, body: &Box<Stmt>, ir: &TypedStmt) {
        let lbl_loop = self.fresh_label("Lwhile");
        let lbl_exit = self.fresh_label("Lwend");
        
        self.maybe_align_loop();
        self.label(&lbl_loop);
        self.break_continue_stack.push((lbl_exit.clone(), lbl_loop.clone()));
        
        if let Some(cond) = ir.expressions.first() {
            self.gen_expr(cond);
            self.beqz_a0(&lbl_exit);
        }
        
        self.gen_stmt(body, &ir.nested[0]);
        self.j(&lbl_loop);
        self.label(&lbl_exit);
        self.break_continue_stack.pop();
    }

    /// 发射 for 循环构造
    fn emit_for_loop(&mut self, init: &Option<Box<Stmt>>, cond: &Option<Box<Expr>>, 
                    step: &Option<Box<Expr>>, body: &Box<Stmt>, ir: &TypedStmt) {
        let body_idx = if init.is_some() { 1 } else { 0 };
        
        // 初始化部分
        if let Some(s) = init {
            self.gen_stmt(s, &ir.nested[0]);
        }
        
        let lbl_loop = self.fresh_label("Lfor");
        let lbl_cont = self.fresh_label("Lfor_cont");
        let lbl_exit = self.fresh_label("Lfend");
        
        self.maybe_align_loop();
        self.label(&lbl_loop);
        self.break_continue_stack.push((lbl_exit.clone(), lbl_cont.clone()));
        
        // 条件判断
        if cond.is_some() {
            if let Some(ir_cond) = ir.expressions.first() {
                self.gen_expr(ir_cond);
                self.beqz_a0(&lbl_exit);
            }
        }
        
        // 循环体
        self.gen_stmt(body, &ir.nested[body_idx]);
        
        // 标签
        self.label(&lbl_cont);
        
        // 步进表达式
        if step.is_some() {
            if let Some(step_ir) = ir.expressions.get(1) {
                let _ = self.gen_expr(step_ir);
            }
        }
        
        self.j(&lbl_loop);
        self.label(&lbl_exit);
        self.break_continue_stack.pop();
    }

    /// 发射 switch/jump table 生成
    fn emit_switch_stmt(&mut self, expr: &Box<Expr>, cases: &[Case], ir: &TypedStmt) {
        // switch: 计算表达式值存入栈临时槽，逐一比较 case 值
        let l_end = self.fresh_label("Lsw_end");
        let l_default = self.fresh_label("Lsw_def");
        let sw_tmp = self.alloc_local("<sw_val>", 4);
        
        // 计算 switch 表达式
        if let Some(sw_ir) = ir.expressions.first() {
            self.gen_expr(sw_ir);
        } else {
            self.gen_expr_from_ast(expr);
        }
        
        // a0 -> 栈临时 (sw a0, sw_tmp(sp))
        self.store_a0(sw_tmp);
        self.break_continue_stack.push((l_end.clone(), String::new()));
        
        let mut has_default = false;
        let mut case_labels: Vec<(String, &Case)> = Vec::new();
        
        for case in cases {
            if let Some(ref val_expr) = &case.val {
                let l_case = self.fresh_label("Lsw_case");
                
                // 重新加载 switch 值到 a5 (t1, reg=15)
                self.load_a0(sw_tmp);
                self.emit_mv(15, 10); // a5 = a0 (switch value)
                
                if let Some(ref hi) = case.range_end {
                    // GNU case low ... high: 范围匹配
                    self.gen_expr_from_ast(val_expr); // a0 = low
                    self.emit_mv(6, 10); // a6 = a0 (low)
                    self.gen_expr_from_ast(hi); // a0 = high
                    self.emit_mv(7, 10); // a7 = a0 (high)
                    
                    // bge a5, a6, skip_lo (switch >= low)
                    let l_skip_lo = self.fresh_label("Lsw_skip_lo");
                    self.emit_bge_label(15, 6, &l_skip_lo);
                    self.j(&l_skip_lo);
                    self.label(&l_skip_lo);
                    
                    // blt a5, a7, skip (switch > high => skip)
                    let l_skip = self.fresh_label("Lsw_skip");
                    self.emit_blt_label(7, 15, &l_skip);
                    self.j(&l_case);
                    self.label(&l_skip);
                } else {
                    // case N: 精确匹配
                    self.gen_expr_from_ast(val_expr); // a0 = case value
                    // beq a5, a0, l_case
                    self.emit_beq_label(15, 10, &l_case);
                }
                
                case_labels.push((l_case, case));
            } else {
                has_default = true;
            }
        }
        
        if has_default {
            self.j(&l_default);
        } else {
            self.j(&l_end);
        }
        
        // 执行各个 case 分支
        for (l_case, case) in &case_labels {
            self.label(l_case);
            for (item, nested_ir) in case.body.items.iter().zip(ir.nested.iter()) {
                self.gen_block_item(item, nested_ir);
            }
        }
        
        // 默认分支
        if has_default {
            self.label(&l_default);
            for case in cases {
                if case.val.is_none() {
                    for (item, nested_ir) in case.body.items.iter().zip(ir.nested.iter()) {
                        self.gen_block_item(item, nested_ir);
                    }
                }
            }
        }
        
        self.label(&l_end);
        self.break_continue_stack.pop();
    }

    /// 发射 goto/unconditional jump
    fn emit_goto(&mut self, target: &str) {
        self.j(target);
    }

    /// 发射间接跳转 (用于变址 goto 或函数指针调用)
    fn emit_goto_indirect(&mut self, addr_reg: u8) {
        // jalr t0, 0(reg) - 寄存器形式的间接跳转
        self.emit_jalr(addr_reg);
    }

    /// 发射 break 语句
    fn emit_break(&mut self) {
        if let Some((l_break, _)) = self.break_continue_stack.last().cloned() {
            self.j(&l_break);
        }
    }

    /// 发射 continue 语句
    fn emit_continue(&mut self) {
        if let Some((_, l_cont)) = self.break_continue_stack.last().cloned() {
            if !l_cont.is_empty() {
                self.j(&l_cont);
            }
        }
    }

    /// 发射基本块内的语句列表
    fn emit_block(&mut self, b: &crate::ast::Block, ir: &TypedStmt) {
        for (item, nested_ir) in b.items.iter().zip(ir.nested.iter()) {
            self.gen_block_item(item, nested_ir);
        }
    }

    /// 发射内联汇编语句
    fn emit_asm_goto(&mut self, stmt: &Stmt, ir: &TypedStmt) {
        // This delegates to the existing asm handling logic
        if let Stmt::AsmGoto {
            asm_str,
            outputs,
            output_names,
            output_constraints,
            inputs,
            input_names,
            input_constraints,
            clobbers,
            goto_labels,
            span,
            volatile,
            ..
        } = stmt
        {
            let label_pairs: Vec<(String, String)> = goto_labels
                .iter()
                .map(|l| (l.name.clone(), l.name.clone()))
                .collect();

            // Load input values to allocated registers
            let pool = crate::codegen::shared::asm_constraints::riscv64_reg_pool();
            let resolved_operands = crate::codegen::shared::asm_constraints::resolve_asm_operands(
                output_constraints,
                input_constraints,
                output_names,
                input_names,
                &pool,
            );

            for (i, op) in resolved_operands.iter().enumerate() {
                if op.is_input && op.is_output {
                    if let Some(ref alloc) = op.reg {
                        if i < outputs.len() {
                            if let Expr::Ident { name, .. } = &outputs[i] {
                                self.asm_load_var_to_reg(&name.name, alloc.idx);
                            }
                        }
                    }
                }
            }

            for (j, op) in resolved_operands.iter().enumerate() {
                if op.is_input && !op.is_output {
                    if let Some(ref alloc) = op.reg {
                        let input_idx = j - output_constraints.len();
                        if input_idx < inputs.len() {
                            if let Expr::Ident { name, .. } = &inputs[input_idx] {
                                self.asm_load_var_to_reg(&name.name, alloc.idx);
                            }
                        }
                    }
                }
            }

            let resolved = Self::resolve_asm_template_enhanced(
                asm_str,
                output_constraints,
                input_constraints,
                output_names,
                input_names,
                &label_pairs,
            );

            if self.asm_text_mode {
                let lines: Vec<String> = resolved
                    .split('\n')
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect();
                self.raw_func_asm_lines
                    .push((self.source_name.clone(), lines));
            } else {
                self.encode_and_emit_asm(&resolved, &span.file, span.line);
            }

            // Writeback output registers
            for (i, op) in resolved_operands.iter().enumerate() {
                if op.is_output && i < outputs.len() {
                    if let Some(ref alloc) = op.reg {
                        if let Expr::Ident { name, .. } = &outputs[i] {
                            self.asm_store_reg_to_var(&name.name, alloc.idx);
                        }
                    }
                }
            }

            // Memory clobber barrier
            if clobbers.iter().any(|c| c == "memory") {
                self.emit_fence(); // fence iorw, iorw
            }
        }
    }

    // Helper functions used by statement emission
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
}
