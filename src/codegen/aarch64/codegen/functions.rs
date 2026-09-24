// AArch64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::ast::{BlockItem, Decl, Expr, FuncDecl, Stmt, Type};
use crate::codegen::aarch64::codegen::{
    aggregate_size, local_stack_size, stmt_has_call, Aarch64Codegen,
};
use crate::ir::Function;

impl Aarch64Codegen {
    pub(crate) fn gen_function(&mut self, func: &Function, ast: Option<&FuncDecl>) {
        // __attribute__((naked)): 不生成 prologue/epilogue
        if ast.map(|f| f.is_naked).unwrap_or(false) {
            self.emitted_return = false;
            if let Some(body) = ast.and_then(|f| f.body.as_ref()) {
                for item in &body.items {
                    match item {
                        BlockItem::Stmt(Stmt::Expr { e, .. }) => {
                            self.gen_naked_asm_expr(e);
                        },
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
                                resolved = resolved.replace(&format!("%l{}", i), &label.name);
                                resolved =
                                    resolved.replace(&format!("%l{}", label_base + i), &label.name);
                                resolved =
                                    resolved.replace(&format!("%l[{}]", label.name), &label.name);
                            }
                            for line in resolved.split('\n') {
                                let line = line.trim();
                                if !line.is_empty() {
                                    self.emit_asm_line_naked(line, span);
                                }
                            }
                        },
                        _ => {}, // naked 函数忽略非 asm 语句
                    }
                }
            }
            if !self.emitted_return {
                // naked 函数应包含自己的返回指令 (如 asm "ret")
                // 不发射额外 epilogue
            }
            return;
        }
        // ftrace: emit call to __fentry__/mcount before prologue
        // The kernel patches these call sites at runtime for tracing.
        let ftrace_call_pos: Option<u64> = match self._target.ftrace_mode {
            crate::cli::FtraceMode::FEntry | crate::cli::FtraceMode::RecordMCount => {
                let pos = self.pos();
                self.emit_bl_to("__fentry__");
                Some(pos)
            },
            crate::cli::FtraceMode::MCount => {
                let pos = self.pos();
                self.emit_bl_to("mcount");
                Some(pos)
            },
            crate::cli::FtraceMode::None => None,
        };
        // -mrecord-mcount: record call site for __mcount_loc section
        if matches!(self._target.ftrace_mode, crate::cli::FtraceMode::RecordMCount) {
            if let Some(call_pos) = ftrace_call_pos {
                self.mcount_loc_entries.push((call_pos, func.name.clone()));
            }
        }
        self.plan_locals(ast);
        self.current_return_ty = Some(func.return_type.clone());
        self.saved_sret_ptr = None;
        // 嵌套 sret: 如果函数返回 >16B 聚合类型, 在帧建立后立即保存 x0 (caller 传入的
        // sret 指针) 到栈局部 <sret_ptr>, 防止后续调用覆盖 x0 导致返回时丢失目标地址.
        let returns_large_agg = aggregate_size(&func.return_type).is_some_and(|s| s > 16);
        // 变参函数需要在 frame 顶部预留寄存器保存区:
        // x0..x7 (64 字节) + v0..v7 (64 字节) = 128 字节.
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
        // MTE (HWASAN 硬件 tag): 为 callee-saved x19 预留槽位, 该槽位计入帧内,
        // 因此只要函数有栈内容 (局部/变参保存区) 就会产生可打 tag 的帧。
        if self.mte_enabled && self.stack_bytes > 0 {
            self.mte_save_off = self.alloc_local("<mte_x19_save>", 8);
        } else {
            self.mte_save_off = 0;
        }
        // Stack protector: allocate canary slot before frame size calculation
        if self._target.stack_protector {
            self.canary_slot_off = self.alloc_local("<canary>", 8);
        } else {
            self.canary_slot_off = 0;
        }
        self.frame_bytes = (self.stack_bytes + 15) & !15;
        // 非叶子函数必须保存 lr(x30): `bl` 会覆写 x30, 否则本函数 `ret`
        // 返回到错误地址(实测 main->callee 嵌套调用死循环)。叶子函数不需帧存
        // lr, 保持帧尺寸不变(不扰动已有产物字节)。ftrace/__fentry__/mcount 与
        // stack-protector 的 __stack_chk_fail 也是调用, 同样需保存 lr。
        let is_leaf = !func.body.iter().any(stmt_has_call)
            && ftrace_call_pos.is_none()
            && !(self._target.stack_protector && self.canary_slot_off > 0);
        self.lr_off = if is_leaf {
            -1
        } else {
            let off = self.frame_bytes;
            self.frame_bytes += 16; // 帧顶留 16B 保 sp 16-对齐, x30 存 [sp, lr_off]
            off
        };
        if self.frame_bytes > 0 {
            if self._target.stack_clash_protection && self.frame_bytes > 4096 {
                self.emit_stack_clash_probes(self.frame_bytes as u32);
            } else {
                self.sub_sp_imm(self.frame_bytes as u32);
            }
        }
        // -ftrivial-auto-var-init=zero: 零初始化整个栈帧
        if self._target.auto_var_init_zero && self.frame_bytes > 0 {
            self.emit_zero_init_frame(self.frame_bytes as usize);
        }
        // 存 lr 必须在帧零初始化之后, 否则 x30 会被清零覆盖。
        if self.lr_off >= 0 {
            self.str_xt_sp(30, self.lr_off);
        }
        // Stack protector: store canary value on stack after frame setup
        if self._target.stack_protector && self.canary_slot_off > 0 {
            self.emit_stack_chk_prologue(self.canary_slot_off);
        }
        // MTE (HWASAN 硬件 tag): prologue tag 栈帧 + 指针 tag 流程。
        //   1) irg  x16, sp          : 随机 tag 写入 x16 (地址不变)
        //   2) addg sp, x16, #0, #0  : sp 携带本帧随机 tag, 由 sp 派生的所有
        //      局部地址 (mov_x_reg_sp_off / [sp,#off]) 自动继承该 tag
        //   3) stg  x16, [sp, #g*16] : 为每个 16B tag granule 写入本帧 tag,
        //      使带 tag 的 sp 访问通过硬件 tag 检查 (STG 本身是 unchecked access)
        //   4) str  x19, [sp, #off]  : 保存入参 x19 (本函数占用 callee-saved 需归还)
        //   5) add  x19, sp, #0      : x19 = 带 tag 的帧基址, 供调用点 untag/retag
        if self.mte_active() {
            self.tag_frame_size = self.frame_bytes;
            self.emit_irg(16, 31); // irg x16, sp
            self.emit_addg(31, 16, 0, 0); // addg sp, x16, #0, #0
            let granules = (self.frame_bytes as u32) / 16;
            for g in 0..granules {
                self.emit_stg(16, 31, g); // stg x16, [sp, #g*16]
            }
            self.str_xt_sp(19, self.mte_save_off); // str x19, [sp, #mte_save_off]
            self.emit_add_imm_x(19, 31, 0); // add x19, sp, #0 (mov x19, sp)
        }
        // 在帧建立后保存 sret 指针 (必须在 sub_sp 之后, 因为此时 sp 才稳定)
        if returns_large_agg {
            let sret_off = self.alloc_local("<sret_ptr>", 8);
            // str x0, [sp, #sret_off]
            self.store_w0(sret_off);
            self.saved_sret_ptr = Some(sret_off);
        }
        // 进入变参函数立即把 x0..x7 和 v0..v7 写入保存区
        if is_variadic {
            for i in 0..8u8 {
                self.str_xt_sp(i, self.va_save_base_off + (i as i32) * 8);
            }
            for i in 0..8u8 {
                self.str_dt_sp(i, self.va_save_base_off + 64 + (i as i32) * 8);
            }
        }
        // Save function parameters to stack (parameters are in x0-x7 for integers, v0-v7 for floats)
        if let Some(func_ast) = ast {
            let mut gp_reg_idx = 0u8; // x0-x7 for integer/pointer params
            let mut fp_reg_idx = 0u8; // v0-v7 for float params
            for param in &func_ast.params {
                if let Some(ref name) = param.name {
                    if let Some(&off) = self.locals.get(&name.name) {
                        let is_float =
                            matches!(param.ty, Type::Float | Type::Double | Type::LongDouble);
                        if is_float && fp_reg_idx < 8 {
                            // Save float parameter from v register to stack
                            match param.ty {
                                Type::Float => {
                                    // str s{fp_reg_idx}, [sp, #off]
                                    let imm = off as u32 & 0xfff;
                                    let insn = imm << 10
                                        | (fp_reg_idx as u32) << 5
                                        | 0b010 << 10
                                        | 2 << 22
                                        | 0x2e000000;
                                    self.emit(&insn.to_le_bytes());
                                },
                                Type::Double | Type::LongDouble => {
                                    // str d{fp_reg_idx}, [sp, #off]
                                    self.str_dt_sp(fp_reg_idx, off);
                                },
                                _ => {},
                            }
                            fp_reg_idx += 1;
                        } else if gp_reg_idx < 8 {
                            // Save integer/pointer parameter from x register to stack
                            match param.ty {
                                Type::Char
                                | Type::UChar
                                | Type::SChar
                                | Type::Short
                                | Type::UShort
                                | Type::Int
                                | Type::UInt
                                | Type::Long
                                | Type::ULong
                                | Type::LongLong
                                | Type::ULongLong
                                | Type::Ptr(_)
                                | Type::Array(_, _) => {
                                    let size = match param.ty {
                                        Type::Char | Type::UChar | Type::SChar => 1,
                                        Type::Short | Type::UShort => 2,
                                        Type::Int | Type::UInt => 4,
                                        _ => 8,
                                    };
                                    self.emit_store_sz(gp_reg_idx, 31, off as u32, size);
                                },
                                _ => {},
                            }
                            gp_reg_idx += 1;
                        }
                    }
                }
            }
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
            },
            // 语句表达式 ({...}) 可能包含 asm: 递归处理
            Expr::StmtExpr { block, .. } => {
                for item in &block.items {
                    if let BlockItem::Stmt(Stmt::Expr { e: inner, .. }) = item {
                        self.gen_naked_asm_expr(inner);
                    }
                }
            },
            _ => {}, // 忽略非 asm 表达式
        }
    }

    /// naked 函数中发射单条 asm 文本行 (asm goto 用)
    pub(crate) fn emit_asm_line_naked(&mut self, line: &str, span: &crate::diagnostics::Span) {
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
        // Allocate space for function parameters and add them to locals map
        if let Some(func_ast) = ast {
            for (i, param) in func_ast.params.iter().enumerate() {
                if let Some(ref name) = param.name {
                    let size = local_stack_size(&param.ty);
                    let off = self.alloc_local(&name.name, size);
                    // Save parameter register to stack in prologue will be done later
                    // For now, just register the parameter in locals map
                    // Parameters are passed in x0-x7 (integers) or v0-v7 (floats)
                    // We'll handle the actual saving in gen_function prologue
                    let _ = (i, off); // Suppress unused variable warning
                }
            }
        }
        for item in &body.items {
            if let BlockItem::Decl(Decl::Var(v)) = item {
                self.alloc_local(&v.name.name, local_stack_size(&v.ty));
            }
        }
        self.alloc_local("<vec_lit>", 16);
    }
}
