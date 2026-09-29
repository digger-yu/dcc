// AArch64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::ast::{BlockItem, Decl, Expr, Stmt};
use crate::codegen::aarch64::codegen::{
    aggregate_size, classify_hfa, is_float_ty, local_stack_size, Aarch64Codegen,
};
use crate::codegen::shared::asm_constraints::{self, resolve_asm_operands};
use crate::ir::{TypedStmt, ValueType};

impl Aarch64Codegen {
    pub(crate) fn gen_block_item(&mut self, item: &BlockItem, ir: &TypedStmt) {
        match item {
            BlockItem::Stmt(s) => self.gen_stmt(s, ir),
            BlockItem::Decl(Decl::Var(v)) => {
                let off = self
                    .locals
                    .get(&v.name.name)
                    .copied()
                    .unwrap_or_else(|| self.alloc_local(&v.name.name, local_stack_size(&v.ty)));
                if let Some(expr) = ir.expressions.first() {
                    self.gen_expr(expr);
                    self.store_result(off, &v.ty);
                } else if let Some(init) = &v.init {
                    self.gen_expr_from_ast(init);
                    self.store_result(off, &v.ty);
                }
            },
            _ => {},
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
                let hfa = classify_hfa(&ret_ty, &self.aggregate_fields);
                if let Some(size) = sret_size {
                    // 大结构体返回: 求值到栈临时区, 再 memcpy 到 sret 目标
                    // 嵌套安全: 使用 gen_function 入口保存的 <sret_ptr> 而非 x0
                    // (x0 可能在嵌套调用中被覆盖)
                    let tmp = self.alloc_local("<ret_tmp>", size as i32);
                    if let Some(expr) = ir.expressions.first() {
                        self.gen_expr(expr);
                    } else if let Some(e) = val.as_ref() {
                        self.gen_expr_from_ast(e);
                    }
                    self.store_result(tmp, &crate::ast::Type::Int);
                    if let Some(sret_off) = self.saved_sret_ptr {
                        // 从保存的 sret 指针加载目标地址到 x0, 然后 memcpy
                        self.load_w0(sret_off);
                        self.memcpy_sp_to_x0(tmp, size);
                    } else {
                        // 顶层函数或未保存 sret 指针: 直接用 x0 (兼容旧路径)
                        self.memcpy_sp_to_x0(tmp, size);
                    }
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
            },
            Stmt::Expr { e, .. } => {
                // 内联汇编在 IR 里没有对应形态 (它不是求值节点)。若只看
                // `ir.expressions`, 语句位置的 asm 会被整条丢掉 —— naked 路径
                // 不走 IR 所以侥幸正确。这里按 AST 形态显式分流,
                // 保证 `__asm__` 在任何函数形态下都真的落地。
                if matches!(e, Expr::AsmExpr { .. }) {
                    self.gen_expr_from_ast(e);
                } else if let Some(expr) = ir.expressions.first() {
                    let _ = self.gen_expr(expr);
                } else {
                    self.gen_expr_from_ast(e);
                }
            },
            Stmt::If { then_, else_, .. } => {
                if let Some(cond) = ir.expressions.first() {
                    self.gen_expr(cond);
                }
                let lbl_else = self.fresh_label("Lelse");
                let lbl_end = self.fresh_label("Lendif");
                self.cbz_w0(&lbl_else);
                self.maybe_align_jump();
                self.gen_stmt(then_, &ir.nested[0]);
                self.b(&lbl_end);
                self.maybe_align_jump();
                self.label(&lbl_else);
                if let Some(el) = else_ {
                    if ir.nested.len() > 1 {
                        self.gen_stmt(el, &ir.nested[1]);
                    } else {
                        self.gen_stmt(
                            el,
                            &TypedStmt {
                                expressions: Vec::new(),
                                nested: Vec::new(),
                                span: ir.span.clone(),
                            },
                        );
                    }
                }
                self.label(&lbl_end);
            },
            Stmt::While { body, .. } | Stmt::Do { body, .. } => {
                let lbl_loop = self.fresh_label("Lwhile");
                let lbl_exit = self.fresh_label("Lwend");
                self.maybe_align_loop();
                self.label(&lbl_loop);
                self.break_continue_stack
                    .push((lbl_exit.clone(), lbl_loop.clone()));
                if let Some(cond) = ir.expressions.first() {
                    self.gen_expr(cond);
                    self.cbz_w0(&lbl_exit);
                }
                self.gen_stmt(body, &ir.nested[0]);
                self.b(&lbl_loop);
                self.label(&lbl_exit);
                self.break_continue_stack.pop();
            },
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
                self.break_continue_stack
                    .push((lbl_exit.clone(), lbl_cont.clone()));
                if cond.is_some() {
                    if let Some(ir_cond) = ir.expressions.first() {
                        self.gen_expr(ir_cond);
                        self.cbz_w0(&lbl_exit);
                    }
                }
                self.gen_stmt(body, &ir.nested[body_idx]);
                self.label(&lbl_cont);
                if step.is_some() {
                    if let Some(step_ir) = ir.expressions.get(1) {
                        let _ = self.gen_expr(step_ir);
                    }
                }
                self.b(&lbl_loop);
                self.label(&lbl_exit);
                self.break_continue_stack.pop();
            },
            Stmt::Block { b, .. } => {
                for (item, nested_ir) in b.items.iter().zip(ir.nested.iter()) {
                    self.gen_block_item(item, nested_ir);
                }
            },
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
                // Build goto label mapping for template substitution
                let label_pairs: Vec<(String, String)> = goto_labels
                    .iter()
                    .map(|l| (l.name.clone(), l.name.clone()))
                    .collect();
                // Load input values to allocated registers
                let pool = asm_constraints::aarch64_reg_pool();
                let resolved_operands = resolve_asm_operands(
                    output_constraints,
                    input_constraints,
                    output_names,
                    input_names,
                    &pool,
                );
                // Load inout input values
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
                // Load pure input values
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
                // 文本与机器码出自同一次解析 (详见 asm_source::record_asm_stmt)。
                self.encode_and_emit_asm(&resolved, &span.file, span.line);
                // Writeback output registers to C variables
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
                    self.emit_dmb_ish();
                }
            },
            Stmt::Switch { expr, cases, .. } => {
                // switch: 计算表达式值存入栈临时槽, 逐一比较 case 值
                let l_end = self.fresh_label("Lsw_end");
                let l_default = self.fresh_label("Lsw_def");
                // 分配栈临时保存 switch 值
                let sw_tmp = self.alloc_local("<sw_val>", 4);
                // 计算 switch 表达式
                if let Some(sw_ir) = ir.expressions.first() {
                    self.gen_expr(sw_ir);
                } else {
                    self.gen_expr_from_ast(expr);
                }
                // w0 -> 栈临时
                self.store_w0(sw_tmp);
                self.break_continue_stack
                    .push((l_end.clone(), String::new()));
                let mut has_default = false;
                let mut case_labels: Vec<(String, &crate::ast::Case)> = Vec::new();
                for case in cases {
                    if let Some(ref val_expr) = &case.val {
                        let l_case = self.fresh_label("Lsw_case");
                        // 重新加载 switch 值到 w14
                        self.load_w0(sw_tmp);
                        self.mov_w14_w0(); // w14 = switch value
                        if let Some(ref hi) = case.range_end {
                            // GNU case low ... high: 范围匹配
                            self.gen_expr_from_ast(val_expr); // w0 = low
                            self.mov_w1_w0(); // w1 = low
                            self.gen_expr_from_ast(hi); // w0 = high
                            self.mov_w2_w0(); // w2 = high
                                              // cmp w14, w1 (switch vs low)
                            self.emit_cmp_w_reg(14, 1);
                            let l_skip_lo = self.fresh_label("Lsw_skip_lo");
                            self.emit_bcond(&l_skip_lo, 0xd); // GE: switch >= low
                                                              // switch < low => skip
                            self.b(&l_skip_lo);
                            self.label(&l_skip_lo);
                            // cmp w14, w2 (switch vs high)
                            self.emit_cmp_w_reg(14, 2);
                            let l_skip = self.fresh_label("Lsw_skip");
                            self.emit_bcond(&l_skip, 0xc); // GT: switch > high
                            self.b(&l_case);
                            self.label(&l_skip);
                        } else {
                            // case N: 精确匹配
                            self.gen_expr_from_ast(val_expr); // w0 = case value
                            self.mov_w1_w0(); // w1 = case value
                                              // cmp w14, w1
                            self.emit_cmp_w_reg(14, 1);
                            // b.eq l_case
                            self.emit_bcond(&l_case, 0x0); // EQ
                        }
                        case_labels.push((l_case, case));
                    } else {
                        has_default = true;
                    }
                }
                // 无匹配: 跳 default 或 end
                if has_default {
                    self.b(&l_default);
                } else {
                    self.b(&l_end);
                }
                // case 体
                for (l_case, case) in &case_labels {
                    self.label(l_case);
                    for (item, nested_ir) in case.body.items.iter().zip(ir.nested.iter()) {
                        self.gen_block_item(item, nested_ir);
                    }
                }
                // default 体
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
                // break 出口
                self.label(&l_end);
                self.break_continue_stack.pop();
            },
            Stmt::Break { .. } => {
                if let Some((l_break, _)) = self.break_continue_stack.last().cloned() {
                    self.b(&l_break);
                }
            },
            Stmt::Continue { .. } => {
                if let Some((_, l_cont)) = self.break_continue_stack.last().cloned() {
                    if !l_cont.is_empty() {
                        self.b(&l_cont);
                    }
                }
            },
            _ => {},
        }
    }
}
