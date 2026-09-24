// ARM backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::ast::{BlockItem, Decl, Stmt};
use crate::ir::TypedStmt;

use crate::codegen::arm::codegen::ArmCodegen;
use crate::codegen::arm::codegen::BranchKind;

impl ArmCodegen {
    pub(crate) fn gen_block_item(&mut self, item: &BlockItem, _ir: &TypedStmt) {
        match item {
            BlockItem::Stmt(s) => self.gen_stmt(s),
            BlockItem::Decl(Decl::Var(v)) => {
                let off = *self.locals.get(&v.name.name).unwrap_or(&0);
                if let Some(init) = &v.init {
                    self.gen_expr_from_ast(init);
                    self.emit_store_r0(off);
                }
            },
            _ => {},
        }
    }

    pub(crate) fn gen_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Return { val, .. } => {
                if let Some(e) = val {
                    self.gen_expr_from_ast(e);
                } else {
                    self.emit_mov_imm(0, 0);
                }
                self.emit_epilogue();
            },
            Stmt::Expr { e, .. } => {
                self.gen_expr_from_ast(e);
            },
            Stmt::If {
                cond, then_, else_, ..
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
            },
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
            },
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
            },
            Stmt::Block { b, .. } => {
                for item in &b.items {
                    if let BlockItem::Stmt(s) = item {
                        self.gen_stmt(s);
                    }
                }
            },
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
            },
            Stmt::Break { .. } => {
                if let Some(lbl) = self.break_labels.last().cloned() {
                    self.emit_b(&lbl);
                }
            },
            Stmt::Continue { .. } => {
                if let Some(lbl) = self.continue_labels.last().cloned() {
                    self.emit_b(&lbl);
                }
            },
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
                            let _lbl_next = self.fresh_label("Lnextcase");
                            // Use BEQ to jump to case body if equal
                            let pos = self.code.len() as u64;
                            self.pending_branches
                                .push((pos, lbl_case.clone(), BranchKind::Beq));
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
            },
            Stmt::AsmGoto {
                asm_str,
                outputs: _,
                output_names,
                output_constraints,
                inputs: _,
                input_names,
                input_constraints,
                goto_labels,
                span,
                ..
            } => {
                let label_pairs: Vec<(String, String)> = goto_labels
                    .iter()
                    .map(|l| (l.name.clone(), l.name.clone()))
                    .collect();
                let resolved = Self::resolve_asm_template_enhanced(
                    asm_str,
                    output_constraints,
                    input_constraints,
                    output_names,
                    input_names,
                    &label_pairs,
                );
                if self.asm_text_mode {
                    eprintln!("{}:{}: warning: asm goto on non-x86_64 target: inline asm text preserved in -S output only", span.file, span.line);
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
            },
            _ => {},
        }
    }
}
