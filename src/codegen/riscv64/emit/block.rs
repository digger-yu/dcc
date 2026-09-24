// RISC-V 64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::ast::{BlockItem, Decl, Expr, Stmt};
use crate::codegen::riscv64::codegen::{Riscv64Codegen, aggregate_size, classify_rv_hfa};
use crate::codegen::shared::asm_constraints::{self, resolve_asm_operands};
use crate::ir::{TypedStmt, ValueType};

impl Riscv64Codegen {
    pub(crate) fn gen_block_item(&mut self, item: &BlockItem, ir: &TypedStmt) {
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

    pub(crate) fn gen_stmt(&mut self, stmt: &Stmt, ir: &TypedStmt) {
        // 记录 DWARF 行号表条目
        let pc = self.code.len() as u64;
        let line = ir.span.line;
        if line > 0 {
            self.line_entries.push((pc, line));
        }
        match stmt {
            Stmt::Return { val, .. } => {
                let ret_ty = self.current_return_ty.clone().unwrap_or(ValueType::Void);
                let sret_size = aggregate_size(&ret_ty).filter(|s| *s > 16);
                let hfa = classify_rv_hfa(&ret_ty, &self.aggregate_fields);
                if let Some(size) = sret_size {
                    // 大结构体返回: 求值到栈临时区, 再 memcpy 到 [a0]
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
            Stmt::Expr { e, .. } => {
                if let Some(expr) = ir.expressions.first() {
                    let _ = self.gen_expr(expr);
                } else {
                    self.gen_expr_from_ast(e);
                }
            }
            Stmt::If { then_, else_, .. } => {
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
            Stmt::While { body, .. } | Stmt::Do { body, .. } => {
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
            Stmt::For {
                init,
                cond,
                step,
                body,
                ..
            } => {
                let body_idx = if init.is_some() { 1 } else { 0 };
                if let Some(s) = init {
                    self.gen_stmt(s, &ir.nested[0]);
                }
                let lbl_loop = self.fresh_label("Lfor");
                let lbl_cont = self.fresh_label("Lfor_cont");
                let lbl_exit = self.fresh_label("Lfend");
                self.maybe_align_loop();
                self.label(&lbl_loop);
                self.break_continue_stack.push((lbl_exit.clone(), lbl_cont.clone()));
                if cond.is_some() {
                    if let Some(ir_cond) = ir.expressions.first() {
                        self.gen_expr(ir_cond);
                        self.beqz_a0(&lbl_exit);
                    }
                }
                self.gen_stmt(body, &ir.nested[body_idx]);
                self.label(&lbl_cont);
                if step.is_some() {
                    if let Some(step_ir) = ir.expressions.get(1) {
                        let _ = self.gen_expr(step_ir);
                    }
                }
                self.j(&lbl_loop);
                self.label(&lbl_exit);
                self.break_continue_stack.pop();
            }
            Stmt::Block { b, .. } => {
                for (item, nested_ir) in b.items.iter().zip(ir.nested.iter()) {
                    self.gen_block_item(item, nested_ir);
                }
            }
            Stmt::AsmGoto {
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
                ..
            } => {
                let label_pairs: Vec<(String, String)> = goto_labels
                    .iter()
                    .map(|l| (l.name.clone(), l.name.clone()))
                    .collect();
                // Load input values to allocated registers
                let pool = asm_constraints::riscv64_reg_pool();
                let resolved_operands = resolve_asm_operands(
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
            Stmt::Switch { expr, cases, .. } => {
                // switch: 计算表达式值存入栈临时槽, 逐一比较 case 值
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
                let mut case_labels: Vec<(String, &crate::ast::Case)> = Vec::new();
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
                for (l_case, case) in &case_labels {
                    self.label(l_case);
                    for (item, nested_ir) in case.body.items.iter().zip(ir.nested.iter()) {
                        self.gen_block_item(item, nested_ir);
                    }
                }
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
            Stmt::Break { .. } => {
                if let Some((l_break, _)) = self.break_continue_stack.last().cloned() {
                    self.j(&l_break);
                }
            }
            Stmt::Continue { .. } => {
                if let Some((_, l_cont)) = self.break_continue_stack.last().cloned() {
                    if !l_cont.is_empty() {
                        self.j(&l_cont);
                    }
                }
            }
            _ => {}
        }
    }

}
