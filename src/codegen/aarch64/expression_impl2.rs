// AArch64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::ast::{find_field, BinOp, UnOp};
use crate::codegen::aarch64::codegen::{
    is_complex_ty, is_float_ty, is_vector_ty, trunc_i32, value_type_size, Aarch64Codegen,
};
use crate::ir::{ExprKind, ExprLiteral, TypedExpr, ValueType};

impl Aarch64Codegen {
    pub(crate) fn gen_float_expr(&mut self, expr: &TypedExpr) -> bool {
        match &expr.kind {
            ExprKind::Constant => {
                if let Some(ExprLiteral::Float(v)) = expr.literal {
                    self.load_float_s0(v as f32);
                    return true;
                }
                if let Some(ExprLiteral::Int(v)) = expr.literal {
                    self.mov_w0_imm(trunc_i32(v));
                    // int 转 float: scvtf s0, w0
                    self.emit(&[0x00, 0x00, 0x18, 0x1e]);
                    return true;
                }
            },
            ExprKind::Binary(op) if expr.children.len() >= 2 => {
                self.gen_float_expr(&expr.children[0]);
                self.mov_s1_s0();
                self.gen_float_expr(&expr.children[1]);
                self.float_binop_s0_s1(*op);
                return true;
            },
            ExprKind::Unary(UnOp::Neg) if !expr.children.is_empty() => {
                self.gen_float_expr(&expr.children[0]);
                self.neg_s0();
                return true;
            },
            ExprKind::Cast if !expr.children.is_empty() => {
                let child = &expr.children[0];
                if is_float_ty(&child.ty) && !is_float_ty(&expr.ty) {
                    // Float → Int: delegate to gen_expr which handles it
                    return self.gen_expr(expr);
                }
                // Target is float
                self.gen_expr(child);
                if !is_float_ty(&child.ty) && is_float_ty(&expr.ty) {
                    // Int → Float: gen_expr put integer in w0,
                    // emit SCVTF to convert to float in s0/d0
                    let is_unsigned = matches!(&child.ty, ValueType::Integer { signed: false, .. });
                    match &expr.ty {
                        ValueType::Float { bits: 32 } => {
                            if is_unsigned {
                                // UCVTF S0, W0: 0x1E190000
                                self.emit(&0x1E190000u32.to_le_bytes());
                            } else {
                                // SCVTF S0, W0: 0x1E180000
                                self.emit(&0x1E180000u32.to_le_bytes());
                            }
                        },
                        ValueType::Float { bits: 64 } => {
                            if is_unsigned {
                                // UCVTF D0, W0: 0x9E190000
                                self.emit(&0x9E190000u32.to_le_bytes());
                            } else {
                                // SCVTF D0, W0: 0x9E180000
                                self.emit(&0x9E180000u32.to_le_bytes());
                            }
                        },
                        _ => {},
                    }
                }
                return true;
            },
            // M25: 浮点型 __builtin_va_arg(ap, T) — v0..v7 已在入口保存到 va_save_base+64
            // 按类型分派: double 用 ldr d0 (64-bit), float 用 ldr s0 (32-bit)
            ExprKind::VaArg { ap, ref arg_ty, .. } => {
                match arg_ty {
                    ValueType::Float { bits: 64 } => self.gen_va_arg_double(ap),
                    _ => self.gen_va_arg_float(ap),
                }
                return true;
            },
            // 浮点返回值沿用普通调用路径；AAPCS64 约定结果留在 s0/d0。
            ExprKind::Call if !expr.children.is_empty() => {
                self.gen_call(expr);
                return true;
            },
            // M25: 浮点局部变量从栈加载到 s0 (避免回退 0.0)
            ExprKind::Local(ref name) => {
                if let Some(&off) = self.locals.get(name) {
                    self.ldr_s0_sp(off);
                    return true;
                }
            },
            // Float assignment: evaluate rhs and store to lhs
            ExprKind::Assign if expr.children.len() >= 2 => {
                self.gen_float_expr(&expr.children[1]);
                let lhs = &expr.children[0];
                match &lhs.kind {
                    ExprKind::Local(ref name) => {
                        if let Some(&off) = self.locals.get(name) {
                            self.str_s0_sp(off);
                            return true;
                        }
                    },
                    ExprKind::Member(ref field_name) if !lhs.children.is_empty() => {
                        let child = &lhs.children[0];
                        if let ExprKind::Local(ref name) = child.kind {
                            if let Some(&off) = self.locals.get(name) {
                                // Compute field offset
                                let struct_name = match &child.ty {
                                    ValueType::Aggregate { name, .. } => Some(name.clone()),
                                    _ => None,
                                };
                                let field_off = struct_name
                                    .and_then(|name| {
                                        self.aggregate_fields.get(&name).and_then(|(fields, _)| {
                                            find_field(fields, field_name)
                                                .map(|(f, extra)| f.offset + extra)
                                        })
                                    })
                                    .unwrap_or(0);
                                // Store float to struct field
                                self.str_s0_sp(off + field_off as i32);
                                return true;
                            }
                        }
                    },
                    _ => {},
                }
            },
            // Float member access: load from struct field
            ExprKind::Member(ref field_name) if !expr.children.is_empty() => {
                let child = &expr.children[0];
                if let ExprKind::Local(ref name) = child.kind {
                    if let Some(&off) = self.locals.get(name) {
                        // Compute field offset
                        let struct_name = match &child.ty {
                            ValueType::Aggregate { name, .. } => Some(name.clone()),
                            _ => None,
                        };
                        let field_off = struct_name
                            .and_then(|name| {
                                self.aggregate_fields.get(&name).and_then(|(fields, _)| {
                                    find_field(fields, field_name)
                                        .map(|(f, extra)| f.offset + extra)
                                })
                            })
                            .unwrap_or(0);
                        self.ldr_s0_sp(off + field_off as i32);
                        return true;
                    }
                }
            },
            // M25: 浮点条件表达式: cond ? true_val : false_val
            // 用分支实现, 避免 fcsel 寄存器约束
            ExprKind::Conditional if expr.children.len() >= 3 => {
                let lbl_false = self.fresh_label(".Lflt_cond_f");
                let lbl_end = self.fresh_label(".Lflt_cond_end");
                self.gen_expr(&expr.children[0]);
                self.cmp_w0_zero();
                self.cbz_w0(&lbl_false);
                self.gen_float_expr(&expr.children[1]);
                self.b(&lbl_end);
                self.label(&lbl_false);
                self.gen_float_expr(&expr.children[2]);
                self.label(&lbl_end);
                return true;
            },
            _ => {
                // 已经是 float/vector 走到这里, 不能再回 gen_expr (死循环).
                // 整数/复合字面量进 gen_float_expr 是为了 int→float 转换, 但
                // gen_expr 不会调 gen_float_expr (ty 不是 float), 所以这里
                // expr.ty 必然是 float, 直接返回 false 让上层走 fallback.
                if !is_float_ty(&expr.ty) && !is_vector_ty(&expr.ty) && self.gen_expr(expr) {
                    self.emit(&[0x00, 0x00, 0x18, 0x1e]); // scvtf s0, w0
                    return true;
                }
            },
        }
        // 无法处理的浮点表达式: 加载 0.0 作为 fallback 并报告诊断
        let span = expr.span.clone();
        let mut diag = crate::diagnostics::Diagnostic::err(
            "E0314",
            format!("unsupported float IR expression on aarch64: {:?}", expr.kind),
            span,
        );
        diag.hint = Some("add handling in gen_float_expr".to_string());
        self.diagnostics.push(diag);
        self.load_float_s0(0.0);
        false
    }

    pub(crate) fn gen_vector_expr(&mut self, expr: &TypedExpr) -> bool {
        match expr.kind {
            ExprKind::Binary(op) if expr.children.len() >= 2 => {
                self.gen_vector_expr(&expr.children[1]);
                self.mov_v1_v0();
                self.gen_vector_expr(&expr.children[0]);
                self.vector_binop_v0_v1(op);
                return true;
            },
            ExprKind::Local(ref name) => {
                if let Some(&off) = self.locals.get(name) {
                    self.ldr_q0_sp(off);
                } else {
                    self.zero_v0();
                }
                return true;
            },
            ExprKind::Constant => {
                if let Some(ExprLiteral::Int(v)) = expr.literal {
                    self.mov_w0_imm(trunc_i32(v));
                    self.dup_v0_4s_w0();
                } else {
                    self.zero_v0();
                }
                return true;
            },
            ExprKind::Aggregate => {
                self.materialize_vector_aggregate(expr);
                return true;
            },
            ExprKind::Cast if !expr.children.is_empty() => {
                let child = &expr.children[0];
                if is_vector_ty(&child.ty) {
                    self.gen_vector_expr(child);
                } else {
                    self.gen_expr(child);
                    self.dup_v0_4s_w0();
                }
                return true;
            },
            ExprKind::Assign if expr.children.len() >= 2 => {
                self.gen_vector_expr(&expr.children[1]);
                if let ExprKind::Local(ref name) = expr.children[0].kind {
                    if let Some(&off) = self.locals.get(name) {
                        self.str_q0_sp(off);
                    }
                }
                return true;
            },
            ExprKind::Call if !expr.children.is_empty() => {
                self.gen_call(expr);
                return true;
            },
            ExprKind::Conditional if expr.children.len() >= 3 => {
                let false_label = self.fresh_label("Lvec_false");
                let end_label = self.fresh_label("Lvec_end");
                self.gen_expr(&expr.children[0]);
                self.cbz_w0(&false_label);
                self.gen_vector_expr(&expr.children[1]);
                self.b(&end_label);
                self.label(&false_label);
                self.gen_vector_expr(&expr.children[2]);
                self.label(&end_label);
                return true;
            },
            _ => {
                self.zero_v0();
            },
        }
        false
    }

    /// Generate code for complex type expressions.
    /// Complex values are stored as (real, imag) pairs:
    /// - Function return: v0 = real, v1 = imag
    /// - Local variable: [sp, #off] = real, [sp, #off+inner_size] = imag
    /// - Memory layout: real at lower address, imag at higher address
    pub(crate) fn gen_complex_expr(&mut self, expr: &TypedExpr) -> bool {
        let inner_size = match &expr.ty {
            ValueType::Complex(inner) => value_type_size(inner),
            _ => return false,
        };
        match &expr.kind {
            // Complex local variable: load both parts into v0, v1
            ExprKind::Local(ref name) => {
                if let Some(&off) = self.locals.get(name) {
                    // Load real part into v0
                    self.ldr_s0_sp(off);
                    // Load imag part into v1
                    self.ldr_s1_sp(off + inner_size as i32);
                    return true;
                }
                // Unknown local: zero both parts
                self.zero_v0();
                self.zero_v1();
                return true;
            },
            // Complex constant: real = literal, imag = 0
            ExprKind::Constant => {
                if let Some(ExprLiteral::Float(v)) = expr.literal {
                    self.load_float_s0(v as f32);
                    self.zero_v1();
                    return true;
                }
                if let Some(ExprLiteral::Int(v)) = expr.literal {
                    self.mov_w0_imm(trunc_i32(v));
                    // int to float: scvtf s0, w0
                    self.emit(&[0x00, 0x00, 0x18, 0x1e]);
                    self.zero_v1();
                    return true;
                }
                self.zero_v0();
                self.zero_v1();
                return true;
            },
            // Complex binary ops: component-wise add/sub
            ExprKind::Binary(op) if expr.children.len() >= 2 => {
                // Evaluate left -> (v0, v1)
                self.gen_complex_expr(&expr.children[0]);
                // Save left real to stack temp
                let tmp_real = self.alloc_local("<cxr>", inner_size as i32);
                let tmp_imag = self.alloc_local("<cxi>", inner_size as i32);
                self.str_s0_sp(tmp_real);
                self.str_s1_sp(tmp_imag);
                // Evaluate right -> (v0, v1)
                self.gen_complex_expr(&expr.children[1]);
                // Load left back into v2, v3 (use s2, s3 as temp)
                self.ldr_s2_sp(tmp_real);
                self.ldr_s3_sp(tmp_imag);
                // Now: v0,v1 = right; v2,v3 = left
                // Component-wise operation
                match op {
                    BinOp::Add => {
                        // real: v0 = v2 + v0
                        self.emit_fadd_s0_v2_v0();
                        // imag: v1 = v3 + v1
                        self.emit_fadd_s1_v3_v1();
                    },
                    BinOp::Sub => {
                        // real: v0 = v2 - v0
                        self.emit_fsub_s0_v2_v0();
                        // imag: v1 = v3 - v1
                        self.emit_fsub_s1_v3_v1();
                    },
                    _ => {
                        // Unsupported complex binop: zero result
                        self.zero_v0();
                        self.zero_v1();
                    },
                }
                return true;
            },
            // Complex cast: component-wise cast
            ExprKind::Cast if !expr.children.is_empty() => {
                let child = &expr.children[0];
                if is_complex_ty(&child.ty) {
                    // Complex -> Complex: load both parts, possibly convert
                    self.gen_complex_expr(child);
                    // TODO: handle different inner types (float->double, etc.)
                } else {
                    // Scalar -> Complex: real = scalar, imag = 0
                    self.gen_expr(child);
                    // Convert int/float in w0/s0 to complex
                    if is_float_ty(&child.ty) {
                        // s0 already has the value, copy to v0
                        // (gen_expr for float puts result in s0)
                    } else {
                        // int in w0 -> float in s0
                        self.emit(&[0x00, 0x00, 0x18, 0x1e]); // scvtf s0, w0
                    }
                    self.zero_v1();
                }
                return true;
            },
            // Complex call: result in (v0, v1)
            ExprKind::Call => {
                self.gen_call(expr);
                // gen_call should leave result in v0, v1 for complex returns
                return true;
            },
            // Complex conditional: branch-based
            ExprKind::Conditional if expr.children.len() >= 3 => {
                let lbl_false = self.fresh_label(".Lcx_cond_f");
                let lbl_end = self.fresh_label(".Lcx_cond_end");
                self.gen_expr(&expr.children[0]);
                self.cmp_w0_zero();
                self.cbz_w0(&lbl_false);
                self.gen_complex_expr(&expr.children[1]);
                self.b(&lbl_end);
                self.label(&lbl_false);
                self.gen_complex_expr(&expr.children[2]);
                self.label(&lbl_end);
                return true;
            },
            _ => {},
        }
        // Fallback: zero both parts
        let span = expr.span.clone();
        let mut diag = crate::diagnostics::Diagnostic::err(
            "E0315",
            format!("unsupported complex IR expression on aarch64: {:?}", expr.kind),
            span,
        );
        diag.hint = Some("add handling in gen_complex_expr".to_string());
        self.diagnostics.push(diag);
        self.zero_v0();
        self.zero_v1();
        false
    }

    pub(crate) fn gen_vector_operand(&mut self, expr: &TypedExpr) {
        self.gen_vector_expr(expr);
    }

    pub(crate) fn materialize_vector_aggregate(&mut self, expr: &TypedExpr) {
        // Optimization: if all elements are the same constant, use DUP
        let all_same_const = expr.children.len() >= 2 && {
            let first_val = expr.children[0].literal.and_then(|l| match l {
                ExprLiteral::Int(v) => Some(v),
                _ => None,
            });
            first_val.is_some()
                && expr.children.iter().all(|c| {
                    c.literal.and_then(|l| match l {
                        ExprLiteral::Int(v) => Some(v),
                        _ => None,
                    }) == first_val
                })
        };
        if all_same_const {
            // All elements are the same integer constant: use DUP
            let val = expr.children[0].literal.unwrap();
            if let ExprLiteral::Int(v) = val {
                self.mov_w0_imm(trunc_i32(v));
                self.dup_v0_4s_w0();
                return;
            }
        }
        let off = self
            .locals
            .get("<vec_lit>")
            .copied()
            .unwrap_or_else(|| self.alloc_local("<vec_lit>", 16));
        self.zero_v0();
        self.str_q0_sp(off);
        for (i, child) in expr.children.iter().take(4).enumerate() {
            self.gen_expr(child);
            self.str_wt_sp(0, off + (i as i32) * 4);
        }
        self.ldr_q0_sp(off);
    }
}
