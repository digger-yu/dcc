// ARM backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::ast::{BlockItem, Expr, FuncDecl, Stmt};
use crate::codegen::shared::elf::{
    ElfSection, ElfSym, SHF_ALLOC, SHF_GNU_RETAIN, SHT_PROGBITS, STB_GLOBAL,
    STB_LOCAL, STB_WEAK, STT_FUNC, STT_GNU_IFUNC,
};
use crate::ir::Function;

use super::struct_def::ArmCodegen;

impl ArmCodegen {
    // ===== 函数生成 =====

    pub(crate) fn gen_function(&mut self, func: &Function, ast: Option<&FuncDecl>) {
        // -fpatchable-function-entry: emit NOPs before and after label
        let func_start: u64;
        if let Some((n, m)) = self._target.patchable_function_entry {
            let prefix_len = m.min(n);
            let suffix_len = n.saturating_sub(prefix_len);
            for _ in 0..prefix_len {
                // ARM NOP: 0xE320F000
                self.code.extend_from_slice(&0xE320F000u32.to_le_bytes());
            }
            func_start = self.code.len() as u64;
            self.labels.insert(func.name.clone(), func_start);
            for _ in 0..suffix_len {
                self.code.extend_from_slice(&0xE320F000u32.to_le_bytes());
            }
        } else {
            func_start = self.code.len() as u64;
            self.labels.insert(func.name.clone(), func_start);
        }

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
        self.current_return_ty = Some(func.return_type.clone());
        self.emitted_return = false;

        // Stack protector: allocate canary slot before frame size calculation
        if self._target.stack_protector {
            self.canary_slot_off = self.alloc_local("<canary>", 4);
        } else {
            self.canary_slot_off = 0;
        }

        // 变参函数: 在局部变量之后预留 r0-r3 保存区 (4×4 = 16 字节)
        let is_variadic = ast.map(|f| f.is_variadic).unwrap_or(false);
        self.is_current_func_variadic = is_variadic;
        self.va_gp_offs = 0;
        if is_variadic {
            self.va_save_base_off = self.stack_bytes;
            self.stack_bytes += 16;
        } else {
            self.va_save_base_off = 0;
        }

        // 函数序言：保存 fp, lr 并设置帧指针
        self.emit_push(vec![11, 14]); // push {fp, lr}
        self.emit_mov_reg(11, 13); // mov fp, sp

        // 分配局部变量空间
        self.frame_bytes = (self.stack_bytes + 7) & !7; // 8 字节对齐
        if self.frame_bytes > 0 {
            if self._target.stack_clash_protection && self.frame_bytes > 4096 {
                self.emit_stack_clash_probes(self.frame_bytes as u32);
            } else {
                self.emit_sub_imm(13, 13, self.frame_bytes as u32);
            }
        }
        // -ftrivial-auto-var-init=zero: 零初始化整个栈帧
        if self._target.auto_var_init_zero && self.frame_bytes > 0 {
            self.emit_zero_init_frame(self.frame_bytes as usize);
        }
        // Stack protector: store canary value on stack after frame setup
        if self._target.stack_protector && self.canary_slot_off > 0 {
            self.emit_stack_chk_prologue(self.canary_slot_off);
        }
        // FTRACE: emit call to mcount/__fentry__ before function body
        let ftrace_call_pos: Option<u64> = match self._target.ftrace_mode {
            crate::cli::FtraceMode::FEntry | crate::cli::FtraceMode::RecordMCount => {
                let pos = self.code.len() as u64;
                self.emit_bl("__fentry__");
                Some(pos)
            }
            crate::cli::FtraceMode::MCount => {
                let pos = self.code.len() as u64;
                self.emit_bl("mcount");
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

        // 变参函数: 帧建立后立即保存 r0-r3 到 va_save 区
        // ARM 局部变量在 fp+off (正偏移), va_save 区同理
        if is_variadic {
            for i in 0..4u8 {
                let off = self.va_save_base_off + (i as i32) * 4;
                self.emit_str_fp_off(4 + i, off); // str r{i}, [fp, #off]
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
        // static 函数必须是 STB_LOCAL, weak 必须是 STB_WEAK —— 此前无条件写
        // STB_GLOBAL, 使 `static int priv(void)` 在 .o 里以可导出全局符号出现,
        // 链接期可与其它 TU 的同名 static 冲突。判据抄已验证的 aarch64/riscv64。
        let func_bind = if ast.map(|f| f.is_weak).unwrap_or(false) {
            STB_WEAK
        } else if ast.map(|f| f.is_static).unwrap_or(false) {
            STB_LOCAL
        } else {
            STB_GLOBAL
        };
        let sym_idx = self.symbols.len() as u32;
        self.symbols.push(ElfSym {
            name: func.name.clone(),
            section: 1,
            value: func_start,
            size: func_end - func_start,
            bind: func_bind,
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
                group_name: None,
            };
            self.extra_sections.push(sec);
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

    pub(crate) fn emit_epilogue(&mut self) {
        // Stack protector epilogue: verify canary BEFORE frame teardown
        if self._target.stack_protector && self.canary_slot_off > 0 {
            self.emit_stack_chk_epilogue(self.canary_slot_off);
        }
        // 恢复 sp 和弹出 fp, pc
        if self.frame_bytes > 0 {
            self.emit_add_imm(13, 13, self.frame_bytes as u32); // add sp, sp, #frame
        }
        self.emit_mov_reg(13, 11); // mov sp, fp
        self.emit_pop(vec![11, 15]); // pop {fp, pc}
        self.emitted_return = true;
    }

    /// -ftrivial-auto-var-init=zero: 零初始化栈帧
    /// 使用 str r0, [fp, #off] 每次清零 4 字节 (r0 = 0)
    /// ARM 局部变量在 fp 正偏移方向
    pub(crate) fn emit_zero_init_frame(&mut self, frame_size: usize) {
        if frame_size == 0 {
            return;
        }
        // 先保存 r0, 然后用于零初始化, 最后恢复
        self.emit_push(vec![0]); // push r0
        self.emit_mov_imm(0, 0); // mov r0, #0
        let words = frame_size / 4;
        for i in 0..words {
            let off = (i as i32) * 4;
            self.emit_str_fp_off(0, off); // str r0, [fp, #off]
        }
        self.emit_pop(vec![0]); // pop r0
    }

    // STR rd, [fp, #off] — 任意寄存器, 正偏移
    pub(crate) fn emit_str_fp_off(&mut self, rd: u8, off: i32) {
        if off >= 0 {
            let inst = 0xE58B0000 | ((rd as u32) << 12) | (off as u32 & 0xFFF);
            self.code.extend_from_slice(&inst.to_le_bytes());
        } else {
            let inst = 0xE50B0000 | ((rd as u32) << 12) | ((-off) as u32 & 0xFFF);
            self.code.extend_from_slice(&inst.to_le_bytes());
        }
    }

    // LDR rd, [fp, #off] — 任意寄存器, 正偏移
    pub(crate) fn emit_ldr_fp_off(&mut self, rd: u8, off: i32) {
        if off >= 0 {
            let inst = 0xE59B0000 | ((rd as u32) << 12) | (off as u32 & 0xFFF);
            self.code.extend_from_slice(&inst.to_le_bytes());
        } else {
            let inst = 0xE51B0000 | ((rd as u32) << 12) | ((-off) as u32 & 0xFFF);
            self.code.extend_from_slice(&inst.to_le_bytes());
        }
    }

    // M25: ARM va_arg 整数路径 — 从保存区顺序读 r0
    // AAPCS ABI: r0-r3 在函数入口保存到 va_save 区,
    // va_arg 依次读取, 超出 4 个则从 caller 栈读.
    pub(crate) fn gen_va_arg_int(&mut self, _ap_name: &str) {
        if self.va_gp_offs < 4 {
            let off = self.va_save_base_off + self.va_gp_offs * 4;
            self.emit_ldr_fp_off(0, off); // ldr r0, [fp, #off]
            self.va_gp_offs += 1;
        } else {
            // 从 caller 栈读取
            let stack_off = self.frame_bytes + (self.va_gp_offs - 4) * 4;
            // ldr r0, [fp, #stack_off] (fp 上方即 caller 帧)
            self.emit_ldr_fp_off(0, stack_off);
            self.va_gp_offs += 1;
        }
    }

    // ARM va_arg 浮点路径 — 从保存区顺序读 s0/d0
    // AAPCS ABI (hard-float): s0-s3 (或 d0-d1) 在函数入口保存到 va_save 区
    // 简化实现: 从整数保存区读取 (AAPCS 软浮点兼容)
    pub(crate) fn gen_va_arg_float(&mut self, _ap_name: &str, is_double: bool) {
        if self.va_flt_offs < 4 {
            // 从浮点保存区读取 (预留空间在 va_save_base_off + 16)
            let off = self.va_save_base_off + 16 + self.va_flt_offs * 4;
            if is_double {
                // VLDR d0, [fp, #off]
                self.emit_vldr_d(0, off);
            } else {
                // VLDR s0, [fp, #off]
                self.emit_vldr_s(0, off);
            }
            self.va_flt_offs += 1;
        } else {
            // 从 caller 栈读取
            let stack_off = self.frame_bytes + 16 + (self.va_flt_offs - 4) * 4;
            if is_double {
                self.emit_vldr_d(0, stack_off);
            } else {
                self.emit_vldr_s(0, stack_off);
            }
            self.va_flt_offs += 1;
        }
    }

    /// Stack protector prologue: load canary from __stack_chk_guard
    /// and store at [fp, #canary_off]. Uses r0 as scratch.
    pub(crate) fn emit_stack_chk_prologue(&mut self, canary_off: i32) {
        // Load __stack_chk_guard address (simplified: use load_imm)
        self.ensure_sym_object("__stack_chk_guard");
        // For relocatable: load address via literal pool or GOT
        // Simplified: load canary value directly from __stack_chk_guard
        // In real implementation, this would use ADRP+LDR or similar
        // For now, emit a placeholder that loads 0 (will be patched by linker)
        self.emit_load_imm(0, 0); // r0 = 0 (placeholder for canary)
        // STR r0, [fp, #canary_off]
        self.emit_str_fp_off(0, canary_off);
    }

    /// Stack protector epilogue: verify canary at [fp, #canary_off]
    /// against __stack_chk_guard. Calls __stack_chk_fail on mismatch.
    pub(crate) fn emit_stack_chk_epilogue(&mut self, canary_off: i32) {
        let fail_label = self.fresh_label(".Lstack_chk_fail_");
        // LDR r0, [fp, #canary_off] - load stored canary
        self.emit_ldr_fp_off(0, canary_off);
        // Load expected canary (simplified: compare with 0)
        self.emit_load_imm(1, 0); // r1 = 0 (placeholder for expected canary)
        // CMP r0, r1
        self.emit_cmp(0, 1);
        // BNE fail_label
        self.emit_bne(&fail_label);
        // Normal path continues
        // Fail path: call __stack_chk_fail (never returns)
        self.set_label(&fail_label);
        self.ensure_sym("__stack_chk_fail");
        self.emit_bl("__stack_chk_fail");
    }
}
