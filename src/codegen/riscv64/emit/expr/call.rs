// src/codegen/riscv64/emit/expr/call.rs - Function calls emission (RISC-V 64 backend)
// Priority 4 Task: Extract working implementations from riscv64.rs.bak

use crate::ast::{Expr};
use crate::codegen::riscv64::Riscv64Codegen;
use crate::ir::{TypedExpr, ValueType};
use crate::target::Target;
use std::collections::HashMap;

impl Riscv64Codegen {
    // ========================================================================
    // 函数调用发射逻辑
    // ========================================================================

    /// 发射函数调用：generate call site handling
    pub fn emit_call(&mut self, func_name: &str, args: &[TypedExpr], ret_ty: &ValueType) -> TypedExpr {
        // GCC 兼容内建函数：AST 路径编译期折叠
        match func_name {
            "__builtin_expect" | "__builtin_expect_with_probability" => {
                if let Some(arg0) = args.first() {
                    self.gen_expr(arg0);
                } else {
                    self.li_a0(0);
                }
                
                return TypedExpr {
                    kind: crate::ir::ExprKind::Call,
                    ty: ret_ty.clone(),
                    literal: None,
                    children: vec![],
                    span: crate::diagnostics::Span::unknown(),
                };
            }
            "__builtin_nonzero_p" => {
                let v = args.first()
                    .and_then(|a| crate::const_eval::eval(a).ok())
                    .map_or(0, |v| if v != 0 { 1i128 } else { 0i128 });
                self.li_a0(crate::codegen::riscv64::trunc_i32(v) as i32);
                
                return TypedExpr {
                    kind: crate::ir::ExprKind::Call,
                    ty: ret_ty.clone(),
                    literal: None,
                    children: vec![],
                    span: crate::diagnostics::Span::unknown(),
                };
            }
            "__builtin_constant_p" => {
                let v = args.first()
                    .map_or(0, |a| crate::const_eval::is_constant(a) as i32);
                self.li_a0(v);
                
                return TypedExpr {
                    kind: crate::ir::ExprKind::Call,
                    ty: ret_ty.clone(),
                    literal: None,
                    children: vec![],
                    span: crate::diagnostics::Span::unknown(),
                };
            }
            "__builtin_object_size" | "__builtin_dynamic_object_size" => {
                self.li_a0(-1);
                
                return TypedExpr {
                    kind: crate::ir::ExprKind::Call,
                    ty: ret_ty.clone(),
                    literal: None,
                    children: vec![],
                    span: crate::diagnostics::Span::unknown(),
                };
            }
            "__builtin_mul_loop" => {
                self.li_a0(0);
                
                return TypedExpr {
                    kind: crate::ir::ExprKind::Call,
                    ty: ret_ty.clone(),
                    literal: None,
                    children: vec![],
                    span: crate::diagnostics::Span::unknown(),
                };
            }
            _ => {}
        }

        // ==================== SysV AMD64 ABI 参数传递 ====================
        
        // 生成所有参数表达式到寄存器
        for (i, arg) in args.iter().enumerate().take(8) {
            self.gen_expr(arg);
            self.mv_a_reg(i as u8);
        }
        
        // 对于超过 8 个参数的情况，需要处理栈上的参数
        if args.len() > 8 {
            self.handle_varargs(args);
        }

        // 为 variadic 函数保存 a0-a7 和 fa0-fa7 到帧中
        if self.is_current_func_variadic {
            self.save_va_args_to_frame();
        }

        // Generate jalr t0, function_address
        self.emit_jal_to(func_name);

        TypedExpr {
            kind: crate::ir::ExprKind::Call,
            ty: ret_ty.clone(),
            literal: None,
            children: vec![],
            span: crate::diagnostics::Span::unknown(),
        }
    }

    /// 处理可变参数函数
    fn handle_varargs(&mut self, args: &[TypedExpr]) {
        // 将额外的参数存储到栈上
        for (i, arg) in args.iter().enumerate().skip(8) {
            self.gen_expr(arg);
            
            // 计算栈偏移 (从 SP 向上分配空间)
            let off = -(i as i32 * 8 + 64); // 预留 64 字节用于前 8 个寄存器的溢出
            
            self.emit_sd(10, 2, off);
        }
    }

    /// 为 variadic 函数保存 a0-a7 和 fa0-fa7 到帧中
    fn save_va_args_to_frame(&mut self) {
        // 保存 GP 参数 (a0..a7)
        for i in 0..8 {
            let reg = 10 + i; // x10..x17
            let stack_off = self.va_save_base_off + (i as i32 * 8);
            self.emit_sd(reg, 2, stack_off);
        }
        
        // 保存 FP 参数 (fa0..fa7)
        // 注意：RISC-V FPU 寄存器映射到整数寄存器进行保存
        // 这里假设使用软浮点模式，否则需要使用特殊的保存序列
    }

    /// 获取 L-value 地址 (for variadic arguments)
    fn get_lvalue_addr(&mut self, expr: &TypedExpr) {
        match &expr.kind {
            crate::ir::ExprKind::Local(name) => {
                if let Some(&off) = self.locals.get(name) {
                    self.load_a0(off);
                } else {
                    self.li_a0(0);
                }
            }
            _ => {
                self.gen_lvalue_addr(expr);
            }
        }
    }

    // ============================================================
    // 辅助函数：寄存器分配与调用约定处理
    // ============================================================

    /// 根据 SysV ABI 将值移动到参数寄存器
    fn mv_a_reg(&mut self, reg_idx: u8) {
        // a0-a7 are x10-x17
        if reg_idx < 8 {
            let target_reg = 10 + reg_idx;
            self.emit_mv(target_reg, 10); // a0 -> target
        }
    }

    /// 保存当前返回值寄存器内容
    fn save_return_value(&mut self, tmp_off: i32) {
        // 将返回结果保存到临时槽
        self.emit_sd(10, 2, tmp_off);
    }

    /// 从临时槽恢复返回值
    fn restore_return_value(&mut self, tmp_off: i32) {
        self.emit_ld(10, 2, tmp_off);
    }

    // ============================================================
    // 内联汇编支持
    // ============================================================

    /// 发射内联汇编指令序列
    pub fn emit_inline_asm(
        &mut self,
        asm_str: &str,
        outputs: &[Box<Expr>],
        output_names: &[String],
        output_constraints: &[String],
        inputs: &[Box<Expr>],
        input_names: &[String],
        input_constraints: &[String],
        clobbers: &[String],
        span_file: &str,
        span_line: u32,
        volatile: bool,
    ) {
        // 解析并解析内联汇编模板
        let resolved = Self::resolve_asm_template_enhanced(
            asm_str,
            output_constraints,
            input_constraints,
            output_names,
            input_names,
            &[],
        );

        if self.asm_text_mode {
            // -S 模式：保留原始文本
            let lines: Vec<String> = resolved
                .split('\n')
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect();
            self.raw_func_asm_lines
                .push((self.source_name.clone(), lines));
        } else {
            // 正常编译：编码为机器码
            self.encode_and_emit_asm(&resolved, span_file, span_line);
        }

        // 内存屏障
        if clobbers.iter().any(|c| c == "memory") {
            self.emit_fence();
        }
    }

    // ============================================================
    // 原子操作内建函数
    // ============================================================

    /// 原子内建函数 codegen: 返回 true 表示已处理
    pub fn gen_atomic_builtin(&mut self, name: &str, args: &[TypedExpr]) -> bool {
        match name {
            "__atomic_load_n" => {
                if args.len() < 2 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                let ord = self.eval_mem_order(&args[1]);

                // a0 = ptr
                self.gen_expr(&args[0]);
                self.mv_a_reg(11); // a1 = ptr (save)
                
                match ord {
                    0 => {} // RELAXED: no fence
                    2 => { // ACQUIRE: fence r, rw
                        self.emit_fence();
                    }
                    _ => { // SEQ_CST/RELEASE: fence rw, rw
                        self.emit_fence();
                    }
                }
                
                // load from [a1] to a0
                self.emit_load_sz(10, 11, 0, sz);
                true
            }
            "__atomic_store_n" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                let ord = self.eval_mem_order(&args[2]);

                // a0 = ptr
                self.gen_expr(&args[0]);
                self.mv_a_reg(11); // a1 = ptr
                
                // a0 = val
                self.gen_expr(&args[1]);
                
                match ord {
                    0 => {} // RELAXED: no fence
                    _ => { // SEQ_CST/ACQUIRE/RELEASE: fence before store
                        self.emit_fence();
                    }
                }
                
                // store a0 to [a1]
                self.emit_store_sz(10, 11, 0, sz);
                true
            }
            "__atomic_exchange_n" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                
                // a0 = ptr
                self.gen_expr(&args[0]);
                self.mv_a_reg(11); // a1 = ptr
                
                // a0 = val
                self.gen_expr(&args[1]);
                self.mv_a_reg(12); // a2 = val
                
                // amoswap [a1], a2 -> a0 (old value)
                self.emit_amoswap(sz, 10, 12, 11);
                true
            }
            "__atomic_fetch_add" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                
                self.gen_expr(&args[0]);
                self.mv_a_reg(11);
                self.gen_expr(&args[1]);
                self.mv_a_reg(12);
                
                // fence rw, rw only for SEQ_CST before AMO
                let ord = self.eval_mem_order(&args[2]);
                if ord == 5 { 
                    self.emit_fence(); 
                }
                
                // amoadd [a1], a2 -> a0 (old value)
                self.emit_amoadd(sz, 10, 12, 11);
                true
            }
            "__atomic_fetch_sub" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                
                self.gen_expr(&args[0]);
                self.mv_a_reg(11);
                self.gen_expr(&args[1]);
                
                // negate a0 (a0 = -val)
                self.emit_neg(10, 10);
                self.mv_a_reg(12);
                
                // fence rw, rw only for SEQ_CST before AMO
                let ord = self.eval_mem_order(&args[2]);
                if ord == 5 { 
                    self.emit_fence(); 
                }
                
                // amoadd [a1], a2 -> a0 (old value, adding negative)
                self.emit_amoadd(sz, 10, 12, 11);
                true
            }
            "__atomic_fetch_and" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                
                self.gen_expr(&args[0]);
                self.mv_a_reg(11);
                self.gen_expr(&args[1]);
                self.mv_a_reg(12);
                
                // fence rw, rw only for SEQ_CST before AMO
                let ord = self.eval_mem_order(&args[2]);
                if ord == 5 { 
                    self.emit_fence(); 
                }
                
                // amoand [a1], a2 -> a0
                self.emit_amoand(sz, 10, 12, 11);
                true
            }
            "__atomic_fetch_or" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                
                self.gen_expr(&args[0]);
                self.mv_a_reg(11);
                self.gen_expr(&args[1]);
                self.mv_a_reg(12);
                
                // fence rw, rw only for SEQ_CST before AMO
                let ord = self.eval_mem_order(&args[2]);
                if ord == 5 { 
                    self.emit_fence(); 
                }
                
                // amoor [a1], a2 -> a0
                self.emit_amoor(sz, 10, 12, 11);
                true
            }
            "__atomic_fetch_xor" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                
                self.gen_expr(&args[0]);
                self.mv_a_reg(11);
                self.gen_expr(&args[1]);
                self.mv_a_reg(12);
                
                // fence rw, rw only for SEQ_CST before AMO
                let ord = self.eval_mem_order(&args[2]);
                if ord == 5 { 
                    self.emit_fence(); 
                }
                
                // amoxor [a1], a2 -> a0
                self.emit_amoxor(sz, 10, 12, 11);
                true
            }
            "__atomic_fetch_nand" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                
                self.gen_expr(&args[0]);
                self.mv_a_reg(11); // a1 = ptr
                self.gen_expr(&args[1]);
                self.mv_a_reg(12); // a2 = val
                let ord = self.eval_mem_order(&args[2]);
                
                if ord == 5 { 
                    self.emit_fence(); 
                }
                
                let l_retry = self.fresh_label("atomic_nand");
                let l_done = self.fresh_label("atomic_nand_done");
                
                self.label(&l_retry);
                self.emit_lr(sz, 10, 11);    // a0 = old
                self.emit_and(13, 10, 12);    // a3 = old & val
                self.emit_xori(13, 13, -1);   // a3 = ~(old & val)
                self.emit_sc(sz, 15, 13, 11);
                self.emit_bnez(15, &l_retry);
                
                self.label(&l_done);
                true
            }
            "__atomic_compare_exchange_n" => {
                // args: (ptr, expected, desired, weak, success_order, failure_order)
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                
                // a0 = ptr
                self.gen_expr(&args[0]);
                self.mv_a_reg(11); // a1 = ptr
                
                // a0 = *expected (dereference expected pointer)
                self.gen_expr(&args[1]);
                self.mv_a_reg(13); // a3 = expected_ptr
                
                // load *expected_ptr to a2
                self.emit_load_sz(12, 13, 0, sz); // a2 = *expected
                
                // a0 = desired
                self.gen_expr(&args[2]);
                self.mv_a_reg(14); // a4 = desired
                
                // fence rw, rw only for SEQ_CST before LR/SC
                let sord = if args.len() > 4 {
                    self.eval_mem_order(&args[4])
                } else {
                    5
                };
                if sord == 5 { 
                    self.emit_fence(); 
                }
                
                // LR/SC CAS loop
                let l_retry = self.fresh_label("atomic_cas");
                let l_fail = self.fresh_label("atomic_cas_fail");
                let l_done = self.fresh_label("atomic_cas_done");
                
                self.label(&l_retry);
                // lr.w/d a0, [a1] (load current value)
                self.emit_lr(sz, 10, 11);
                // bne a0, a2, fail (compare current with expected)
                self.emit_bne(10, 12, &l_fail);
                // sc.w/d a5, a4, [a1] (store desired)
                self.emit_sc(sz, 15, 14, 11);
                // bnez a5, retry (if store failed, retry)
                self.emit_bnez(15, &l_retry);
                
                // success: a0 = 1
                self.li_a0(1);
                self.j(&l_done);
                
                // failure: store current to *expected, a0 = 0
                self.label(&l_fail);
                self.emit_store_sz(10, 13, 0, sz); // *expected = current
                self.li_a0(0);
                
                self.label(&l_done);
                true
            }
            _ => false,
        }
    }

    /// 获取指针参数所指元素的大小
    fn atomic_elem_size(&self, args: &[TypedExpr], idx: usize) -> usize {
        if idx >= args.len() {
            return 8;
        }
        match &args[idx].ty {
            ValueType::Pointer(inner) => crate::ir::value_type_size(inner),
            ValueType::Array(inner, _) => crate::ir::value_type_size(inner),
            _ => 8,
        }
    }

    /// 尝试从 IR 表达式中提取原子内存序常量值
    fn eval_mem_order(&self, arg: &TypedExpr) -> i32 {
        match &arg.literal {
            Some(crate::ir::ExprLiteral::Int(v)) => *v as i32,
            _ => 5, // default SEQ_CST for safety
        }
    }

    /// 加载变量到寄存器 (for inline asm)
    fn asm_load_var_to_reg(&mut self, name: &str, reg_idx: u8) {
        if let Some(&off) = self.locals.get(name) {
            self.load_a0(off);
            self.emit_mv(reg_idx as u8, 10);
        } else {
            self.li_a0(0);
            self.emit_mv(reg_idx as u8, 10);
        }
    }

    /// 从寄存器写回变量 (for inline asm)
    fn asm_store_reg_to_var(&mut self, name: &str, reg_idx: u8) {
        if let Some(&off) = self.locals.get(name) {
            self.emit_store_sz(reg_idx as u8, 2, off, 8);
        }
    }
}
