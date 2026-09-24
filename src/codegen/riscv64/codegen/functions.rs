// RISC-V 64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::ast::{BlockItem, Decl, Expr, FuncDecl, Stmt};
use crate::codegen::riscv64::codegen::{Riscv64Codegen, stmt_has_call};
use crate::ir::{Function};

impl Riscv64Codegen {
    pub(crate) fn gen_function(&mut self, func: &Function, ast: Option<&FuncDecl>) {
        // __attribute__((naked)): 不生成 prologue/epilogue
        if ast.map(|f| f.is_naked).unwrap_or(false) {
            self.emitted_return = false;
            if let Some(body) = ast.and_then(|f| f.body.as_ref()) {
                for item in &body.items {
                    match item {
                        BlockItem::Stmt(Stmt::Expr { e, .. }) => {
                            self.gen_naked_asm_expr(e);
                        }
                        BlockItem::Stmt(Stmt::AsmGoto {
                            asm_str,
                            outputs,
                            output_names: _,
                            output_constraints: _,
                            inputs: _,
                            input_names: _,
                            input_constraints: _,
                            clobbers: _,
                            goto_labels,
                            volatile: _,
                            span,
                        }) => {
                            let mut resolved = asm_str.clone();
                            let label_base = outputs.len();
                            for (i, label) in goto_labels.iter().enumerate().rev() {
                                resolved = resolved.replace(
                                    &format!("%l{}", i),
                                    &label.name,
                                );
                                resolved = resolved.replace(
                                    &format!("%l{}", label_base + i),
                                    &label.name,
                                );
                                resolved = resolved.replace(
                                    &format!("%l[{}]", label.name),
                                    &label.name,
                                );
                            }
                            for line in resolved.split('\n') {
                                let line = line.trim();
                                if !line.is_empty() {
                                    self.emit_asm_line_naked(line, span);
                                }
                            }
                        }
                        _ => {} // naked 函数忽略非 asm 语句
                    }
                }
            }
            return;
        }
        self.plan_locals(ast);
        // 记录当前函数返回类型 (sret/HFA 判定)
        self.current_return_ty = Some(func.return_type.clone());
        // Stack protector: allocate canary slot before frame size calculation
        if self._target.stack_protector {
            self.canary_slot_off = self.alloc_local("<canary>", 8);
        } else {
            self.canary_slot_off = 0;
        }
        // 变参函数需要在 frame 顶部预留寄存器保存区:
        // a0..a7 (64 字节) + fa0..fa7 (64 字节) = 128 字节.
        let is_variadic = ast.map(|f| f.is_variadic).unwrap_or(false);
        self.is_current_func_variadic = is_variadic;
        self.va_gp_offs = 0;
        self.va_flt_offs = 0;
        if is_variadic {
            self.va_save_base_off = self.stack_bytes;
            self.stack_bytes += 128;
        } else {
            self.va_save_base_off = 0;
        }
        self.frame_bytes = (self.stack_bytes + 15) & !15;
        // 非叶子函数必须保存 ra/x1: jal 覆写 ra，本函数 ret 需要回退地址。叶子函数不需帧存
        // ra，保持帧尺寸不变 (不扰动已有产物字节)。ftrace/__fentry__/mcount 也是调用，
        // stack-protector 的 __stack_chk_fail 也是调用，同样需保存 ra。
        let ftrace_pos: Option<u64>;
        if matches!(self._target.ftrace_mode, crate::cli::FtraceMode::None) {
            ftrace_pos = None;
        } else {
            // Check call positions later; for now assume non-leaf if ftrace enabled
            if func.body.iter().any(stmt_has_call) {
                ftrace_pos = Some(0); // Placeholder to mark non-leaf
            } else {
                ftrace_pos = Some(1); // Will be overwritten
            }
        }
        let is_leaf = ftrace_pos.is_none() && !func.body.iter().any(stmt_has_call)
            && !(self._target.stack_protector && self.canary_slot_off > 0);
        self.ret_addr_off = if is_leaf {
            -1
        } else {
            let off = self.frame_bytes;
            self.frame_bytes += 8; // Frame top slot for ra (8 bytes aligned)
            off
        };
        if self.frame_bytes > 0 {
            if self._target.stack_clash_protection && self.frame_bytes > 4096 {
                self.emit_stack_clash_probes(self.frame_bytes as u32);
            } else {
                self.addi_sp(-self.frame_bytes);
            }
        }
        // -ftrivial-auto-var-init=zero: 零初始化整个栈帧
        if self._target.auto_var_init_zero && self.frame_bytes > 0 {
            self.emit_zero_init_frame(self.frame_bytes as usize);
        }
        // 存 ra 必须在帧零初始化之后，否则 ra 会被清零覆盖。
        if self.ret_addr_off >= 0 {
            self.emit_sd(1, 2, self.ret_addr_off); // sd x1, ret_addr_off(sp)
        }
        // Stack protector: store canary value on stack after frame setup
        if self._target.stack_protector && self.canary_slot_off > 0 {
            self.emit_stack_chk_prologue(self.canary_slot_off);
        }
        // FTRACE: emit call to mcount/__fentry__ before function body
        let ftrace_call_pos: Option<u64> = match self._target.ftrace_mode {
            crate::cli::FtraceMode::FEntry | crate::cli::FtraceMode::RecordMCount => {
                let pos = self.pos();
                self.emit_jal_to("__fentry__");
                Some(pos)
            }
            crate::cli::FtraceMode::MCount => {
                let pos = self.pos();
                self.emit_jal_to("mcount");
                Some(pos)
            }
            crate::cli::FtraceMode::None => None,
        };
        // -mrecord-mcount: record call site for __mcount_loc section
        if matches!(self._target.ftrace_mode, crate::cli::FtraceMode::RecordMCount) {
            if let Some(call_pos) = ftrace_call_pos {
                self.mcount_loc_entries.push((call_pos, func.name.clone()));
            }
        }
        // 进入变参函数立即把 a0..a7 和 fa0..fa7 写入保存区
        if is_variadic {
            for i in 0..8u8 {
                self.emit_sd(10 + i, 2, self.va_save_base_off + (i as i32) * 8);
            }
            for i in 0..8u8 {
                self.emit_fsd(10 + i, 2, self.va_save_base_off + 64 + (i as i32) * 8);
            }
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
        self.current_return_ty = None;
    }

    /// naked 函数中处理表达式: 仅处理 AsmExpr，其他忽略
    pub(crate) fn gen_naked_asm_expr(&mut self, e: &Expr) {
        match e {
            Expr::AsmExpr {
                asm_str,
                outputs,
                output_names,
                output_constraints,
                inputs,
                input_names,
                input_constraints,
                clobbers,
                volatile,
                span,
                ..
            } => {
                self.emit_inline_asm(
                    asm_str,
                    outputs,
                    output_names,
                    output_constraints,
                    inputs,
                    input_names,
                    input_constraints,
                    clobbers,
                    &span.file,
                    span.line,
                    *volatile,
                );
            }
            // 语句表达式 ({...}) 可能包含 asm: 递归处理
            Expr::StmtExpr { block, .. } => {
                for item in &block.items {
                    if let BlockItem::Stmt(Stmt::Expr { e: inner, .. }) = item {
                        self.gen_naked_asm_expr(inner);
                    }
                }
            }
            _ => {} // 忽略非 asm 表达式
        }
    }

    /// naked 函数中发射单条 asm 文本行 (asm goto 用)
    pub(crate) fn emit_asm_line_naked(
        &mut self,
        line: &str,
        span: &crate::diagnostics::Span,
    ) {
        if self.asm_text_mode {
            self.raw_func_asm_lines
                .push((format!("__naked_{}", self.label_serial), vec![line.to_string()]));
            self.label_serial += 1;
        } else {
            self.encode_and_emit_asm(line, &span.file, span.line);
        }
    }

    pub(crate) fn plan_locals(&mut self, ast: Option<&FuncDecl>) {
        self.locals.clear();
        self.stack_bytes = 0;
        let Some(body) = ast.and_then(|f| f.body.as_ref()) else {
            return;
        };
        // Allocate space for function parameters
        if let Some(func_ast) = ast {
            for param in &func_ast.params {
                if let Some(ref name) = param.name {
                    let size = param.ty.size().max(4) as i32;
                    self.alloc_local(&name.name, size);
                }
            }
        }
        for item in &body.items {
            if let BlockItem::Decl(Decl::Var(v)) = item {
                self.alloc_local(&v.name.name, v.ty.size().max(4) as i32);
            }
        }
    }

}
