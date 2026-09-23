// src/codegen/riscv64/emit/expr/unary.rs - Unary operators emission (RISC-V 64 backend)
// Priority 4 Task: Extract working implementations from riscv64.rs.bak

use crate::ast::{Expr, Type, UnOp};
use crate::codegen::riscv64::Riscv64Codegen;
use crate::ir::TypedExpr;
use crate::target::Target;
use std::collections::HashMap;

impl Riscv64Codegen {
    // ========================================================================
    // 一元表达式发射逻辑
    // ========================================================================

    /// 发射一元运算：operand -> result
    pub fn emit_unary(&mut self, unop: UnOp, operand: &TypedExpr) -> TypedExpr {
        match unop {
            UnOp::AddrOf => self.emit_address_of(operand),
            UnOp::Deref => self.emit_dereference(operand),
            UnOp::PreInc => self.emit_pre_increment(operand),
            UnOp::PreDec => self.emit_pre_decrement(operand),
            UnOp::PostInc => self.emit_post_increment(operand),
            UnOp::PostDec => self.emit_post_decrement(operand),
            UnOp::NotBitwise => self.emit_bitwise_not(operand),
            UnOp::NotLogical => self.emit_logical_not(operand),
            UnOp::Cast(cast_ty) => self.emit_type_cast(operand, cast_ty),
        }
    }

    /// 发射地址取址 (&): generate LEA instruction
    fn emit_address_of(&mut self, operand: &TypedExpr) -> TypedExpr {
        match &operand.kind {
            crate::ir::ExprKind::Local(name) => {
                // 局部变量：加载栈偏移地址
                if let Some(&off) = self.locals.get(name) {
                    self.li_a0(off as i32); // Load offset directly as address
                    // Adjust for stack pointer
                    self.add_sp_fp();
                } else if let Some(&offset) = self.global_rodata.get(name) {
                    // 全局变量：计算绝对地址
                    let addr = self.entry + crate::codegen::riscv64::align_up_u64(self.code.len() as u64, 8) + offset;
                    self.li_a0(addr as i32);
                } else {
                    self.li_a0(0);
                }
            }
            crate::ir::ExprKind::Member(field_name) => {
                // 结构体成员：计算成员地址
                if !operand.children.is_empty() {
                    let base = &operand.children[0];
                    self.gen_expr(base); // a0 = base address
                    
                    // 获取字段偏移
                    if let ValueType::Aggregate { name, .. } = &base.ty {
                        if let Some(fields) = crate::codegen::riscv64::find_aggregate_fields(name) {
                            if let Some((field_info, extra)) = crate::ast::find_field(&fields, field_name) {
                                let offset = field_info.offset + extra;
                                if offset != 0 {
                                    self.emit_addi(10, 10, offset as i32);
                                }
                            }
                        }
                    }
                } else {
                    self.li_a0(0);
                }
            }
            crate::ir::ExprKind::Index(idx_expr) => {
                // 数组索引：计算元素地址
                if operand.children.len() >= 2 {
                    let base_expr = &operand.children[0];
                    let index_expr = &operand.children[1];
                    
                    self.gen_expr(base_expr); // a0 = base
                    
                    self.gen_expr(index_expr); // a1 = index
                    let elem_size = crate::ir::value_type_size(&operand.ty);
                    self.emit_slli(11, 11, elem_size.trailing_zeros() as i32); // index * size
                    self.emit_add(10, 10, 11); // a0 += index * size
                } else {
                    self.li_a0(0);
                }
            }
            _ => {
                // 默认情况：直接加载为地址
                self.gen_expr(operand);
            }
        }

        TypedExpr {
            kind: crate::ir::ExprKind::Unary(unop),
            ty: crate::ir::ValueType::Pointer(Box::new(operand.ty.clone())),
            literal: None,
            children: vec![operand.clone()],
            span: operand.span.clone(),
        }
    }

    /// 发射取消引用 (*): generate LOAD from address
    fn emit_dereference(&mut self, operand: &TypedExpr) -> TypedExpr {
        match &operand.ty {
            crate::ir::ValueType::Pointer(inner_ty) => {
                // 加载指针指向的值
                self.gen_expr(operand); // a0 = address
                
                // 根据类型大小选择合适的加载指令
                let size = inner_ty.size().max(1);
                self.emit_load_sz(10, 10, 0, size);
                
                // 整数提升处理
                if matches!(inner_ty.as_ref(), 
                    crate::ir::ValueType::Char(_) | 
                    crate::ir::ValueType::Int(_)
                ) {
                    self.gen_int_promote_ir(inner_ty);
                }
            }
            crate::ir::ValueType::Float { bits: 64 } => {
                // 浮点指针加载
                self.gen_expr(operand); // a0 = address
                self.emit_fld_fa0(10, 0); // fload double precision
            }
            _ => {
                // 默认处理：零值
                self.li_a0(0);
            }
        }

        TypedExpr {
            kind: crate::ir::ExprKind::Unary(unop),
            ty: inner_ty.clone(),
            literal: None,
            children: vec![operand.clone()],
            span: operand.span.clone(),
        }
    }

    /// 发射前置增量 (++x): load-modify-store sequence
    fn emit_pre_increment(&mut self, operand: &TypedExpr) -> TypedExpr {
        let one = TypedExpr {
            kind: crate::ir::ExprKind::Constant,
            ty: crate::ir::ValueType::Int(32),
            literal: Some(crate::ir::ExprLiteral::Int(1)),
            children: Vec::new(),
            span: operand.span.clone(),
        };

        // ++x: x = x + 1, 返回新值
        self.gen_expr(operand); // Load current value
        
        let val_reg = 11; // a1 = current value
        self.emit_mv(val_reg, 10);
        
        self.gen_expr_from_ast_opt(&one); // a0 = 1
        self.emit_add(10, val_reg, 10); // a0 += 1
        
        // Store back to memory (L-value required)
        self.store_to_lvalue(operand, 10);
        
        TypedExpr {
            kind: crate::ir::ExprKind::Unary(unop),
            ty: operand.ty.clone(),
            literal: None,
            children: vec![operand.clone()],
            span: operand.span.clone(),
        }
    }

    /// 发射前置减量 (--x): load-modify-store sequence  
    fn emit_pre_decrement(&mut self, operand: &TypedExpr) -> TypedExpr {
        let minus_one = TypedExpr {
            kind: crate::ir::ExprKind::Constant,
            ty: crate::ir::ValueType::Int(32),
            literal: Some(crate::ir::ExprLiteral::Int(-1)),
            children: Vec::new(),
            span: operand.span.clone(),
        };

        // --x: x = x - 1, 返回新值
        self.gen_expr(operand); // Load current value
        
        let val_reg = 11; // a1 = current value
        self.emit_mv(val_reg, 10);
        
        self.gen_expr_from_ast_opt(&minus_one); // a0 = -1
        self.emit_add(10, val_reg, 10); // a0 -= 1
        
        // Store back to memory
        self.store_to_lvalue(operand, 10);
        
        TypedExpr {
            kind: crate::ir::ExprKind::Unary(unop),
            ty: operand.ty.clone(),
            literal: None,
            children: vec![operand.clone()],
            span: operand.span.clone(),
        }
    }

    /// 发射后置增量 (x++): load with increment after
    fn emit_post_increment(&mut self, operand: &TypedExpr) -> TypedExpr {
        // x++: return old value, then x = x + 1
        let temp_off = self.alloc_local("<post_inc>", 8);
        
        // Save current value
        self.gen_expr(operand);
        self.emit_sd(10, 2, temp_off); // Save original value
        
        // Calculate new value
        let one = TypedExpr {
            kind: crate::ir::ExprKind::Constant,
            ty: crate::ir::ValueType::Int(32),
            literal: Some(crate::ir::ExprLiteral::Int(1)),
            children: Vec::new(),
            span: operand.span.clone(),
        };
        
        let orig_reg = 11; // a1 = old value
        self.emit_mv(orig_reg, 10);
        
        self.gen_expr_from_ast_opt(&one);
        self.emit_add(10, orig_reg, 10);
        
        // Store back
        self.store_to_lvalue(operand, 10);
        
        // Return original value
        self.emit_ld(10, 2, temp_off);
        
        TypedExpr {
            kind: crate::ir::ExprKind::Unary(UnOp::PostInc),
            ty: operand.ty.clone(),
            literal: None,
            children: vec![operand.clone()],
            span: operand.span.clone(),
        }
    }

    /// 发射后置减量 (x--): load with decrement after
    fn emit_post_decrement(&mut self, operand: &TypedExpr) -> TypedExpr {
        let temp_off = self.alloc_local("<post_dec>", 8);
        
        // Save current value
        self.gen_expr(operand);
        self.emit_sd(10, 2, temp_off); // Save original value
        
        // Calculate new value
        let minus_one = TypedExpr {
            kind: crate::ir::ExprKind::Constant,
            ty: crate::ir::ValueType::Int(32),
            literal: Some(crate::ir::ExprLiteral::Int(-1)),
            children: Vec::new(),
            span: operand.span.clone(),
        };
        
        let orig_reg = 11; // a1 = old value
        self.emit_mv(orig_reg, 10);
        
        self.gen_expr_from_ast_opt(&minus_one);
        self.emit_add(10, orig_reg, 10);
        
        // Store back
        self.store_to_lvalue(operand, 10);
        
        // Return original value
        self.emit_ld(10, 2, temp_off);
        
        TypedExpr {
            kind: crate::ir::ExprKind::Unary(UnOp::PostDec),
            ty: operand.ty.clone(),
            literal: None,
            children: vec![operand.clone()],
            span: operand.span.clone(),
        }
    }

    /// 发射逻辑 NOT (!): zero-check comparison
    fn emit_logical_not(&mut self, operand: &TypedExpr) -> TypedExpr {
        let l_nonzero = self.fresh_label("Lnonzero");
        let l_zero = self.fresh_label("Lzero");
        
        self.gen_expr(operand);
        
        // 如果非零，则结果为 false (0)
        self.emit_bnez(10, &l_nonzero);
        
        // 否则结果为 true (1)
        self.li_a0(1);
        let l_done = self.fresh_label("Ldone");
        self.j(&l_done);
        
        self.label(&l_nonzero);
        self.li_a0(0);
        
        self.label(&l_done);
        
        TypedExpr {
            kind: crate::ir::ExprKind::Unary(UnOp::NotLogical),
            ty: crate::ir::ValueType::Int(32), // 逻辑结果总是 int
            literal: None,
            children: vec![operand.clone()],
            span: operand.span.clone(),
        }
    }

    /// 发射按位 NOT (~): XOR with all-ones
    fn emit_bitwise_not(&mut self, operand: &TypedExpr) -> TypedExpr {
        self.gen_expr(operand); // a0 = operand
        
        // 按位取反：XOR 全 1 (对于不同类型宽度不同)
        match &operand.ty {
            crate::ir::ValueType::Int(bits) if *bits == 8 => {
                // 字节操作
                self.emit_xori(10, 10, 0xFF);
            }
            crate::ir::ValueType::Int(bits) if *bits == 16 => {
                // 半字操作
                self.emit_xori(10, 10, 0xFFFF);
            }
            crate::ir::ValueType::Int(64) => {
                // 双字：先设置全 1，然后 XNOR（或两次 XOR）
                self.load_all_ones_64();
                self.emit_xor(10, 10, 11);
            }
            _ => {
                // 默认 32 位
                self.emit_xori(10, 10, 0xFFFFFFFF);
            }
        }

        TypedExpr {
            kind: crate::ir::ExprKind::Unary(UnOp::NotBitwise),
            ty: operand.ty.clone(),
            literal: None,
            children: vec![operand.clone()],
            span: operand.span.clone(),
        }
    }

    /// 发射类型转换 (explicit casts)
    fn emit_type_cast(&mut self, operand: &TypedExpr, cast_ty: Box<crate::ast::Type>) -> TypedExpr {
        let from_ty = &operand.ty;
        let to_ty = &cast_ty.to_value_type();

        match (from_ty, to_ty) {
            // Integer to integer conversion
            (crate::ir::ValueType::Int(from_bits), crate::ir::ValueType::Int(to_bits)) => {
                self.gen_expr(operand);
                
                if *from_bits > *to_bits {
                    // 截断：高位丢弃
                    // 例如：long -> int, int -> short
                } else if *from_bits < *to_bits {
                    // 扩展：有符号符号扩展到目标宽度
                    if matches!(from_ty, crate::ir::ValueType::Char(Type::SChar)) {
                        self.sign_extend_to_reg(10);
                    }
                }
            }
            
            // Integer to float
            (crate::ir::ValueType::Int(_), crate::ir::ValueType::Float { bits: 32 }) => {
                self.gen_expr(operand);
                self.emit_fcvt_s_w_fa0(); // 整数到单精度浮点
            }
            (crate::ir::ValueType::Int(_), crate::ir::ValueType::Float { bits: 64 }) => {
                self.gen_expr(operand);
                self.emit_fcvt_d_w_fa0(); // 整数到双精度浮点
            }
            
            // Float to integer
            (crate::ir::ValueType::Float { bits: 32 }, crate::ir::ValueType::Int(_)) => {
                self.gen_expr(operand);
                self.emit_fcvt_w_s_a0(); // 单精度浮点到整数
            }
            (crate::ir::ValueType::Float { bits: 64 }, crate::ir::ValueType::Int(_)) => {
                self.gen_expr(operand);
                self.emit_fcvt_w_d_a0(); // 双精度浮点到整数
            }
            
            // Pointer conversions
            (crate::ir::ValueType::Pointer(_), _) => {
                self.gen_expr(operand);
            }
            (_, crate::ir::ValueType::Pointer(_)) => {
                self.gen_expr(operand);
            }
            
            _ => {
                // 默认：直接传递
                self.gen_expr(operand);
            }
        }

        TypedExpr {
            kind: crate::ir::ExprKind::Cast,
            ty: to_ty.clone(),
            literal: None,
            children: vec![operand.clone()],
            span: operand.span.clone(),
        }
    }

    /// 将值存储到 L-value
    fn store_to_lvalue(&mut self, lvalue: &TypedExpr, value_reg: u8) {
        match &lvalue.kind {
            crate::ir::ExprKind::Local(name) => {
                if let Some(&off) = self.locals.get(name) {
                    self.emit_store_sz(value_reg, 2, off, 8);
                }
            }
            _ => {
                // 通用 L-value 存储
                self.gen_lvalue_addr(lvalue); // a0 = address
                self.emit_addi(12, 10, 0); // a2 = address (save)
                self.emit_store_sz(value_reg, 12, 0, 8);
            }
        }
    }

    /// 生成所有 1 的 64 位常量
    fn load_all_ones_64(&mut self) {
        // li a1, -1 产生全 1 模式
        self.li_a0(-1_i32);
        self.emit_mv(11, 10);
    }

    /// 从 AST 解析的表达式中评估常量 (辅助函数)
    fn gen_expr_from_ast_opt(&mut self, expr: &Option<Box<crate::ast::Expr>>) {
        if let Some(e) = expr {
            self.gen_expr_from_ast(e);
        } else {
            self.li_a0(0);
        }
    }

    /// 添加 SP 和 FP 以计算有效地址
    fn add_sp_fp(&mut self) {
        // 假设 fp 寄存器是 x19 (s0)
        self.emit_add(10, 2, 19); // a0 += sp + fp
    }
}
