// RISC-V 64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::ast::BinOp;
use crate::codegen::riscv64::codegen::{
    is_complex_ty, is_float_ty, is_vector_ty, trunc_i32, value_type_size, Riscv64Codegen,
};
use crate::ir::{ExprKind, ExprLiteral, TypedExpr, ValueType};

impl Riscv64Codegen {
    pub(crate) fn gen_float_expr(&mut self, expr: &TypedExpr) -> bool {
        match &expr.kind {
            ExprKind::Constant => {
                if let Some(ExprLiteral::Float(v)) = expr.literal {
                    self.load_float_fa0(v as f32);
                    return true;
                }
            },
            ExprKind::Binary(op) if expr.children.len() >= 2 => {
                self.gen_float_expr(&expr.children[0]);
                self.mv_fa1_fa0(); // save left to fa1
                self.gen_float_expr(&expr.children[1]);
                // Now fa0 = right, fa1 = left
                self.float_binop_fa0(*op, &expr.ty);
                return true;
            },
            // M25: 浮点型 __builtin_va_arg(ap, double) — fa0..fa7 已在入口保存到 va_save_base+64
            ExprKind::VaArg { ap, .. } => {
                self.gen_va_arg_float(ap);
                return true;
            },
            // 浮点返回值沿用普通调用路径；RISC-V ABI 约定结果留在 fa0。
            ExprKind::Call if !expr.children.is_empty() => {
                self.gen_call(expr);
                return true;
            },
            // Float local variable: load from stack to fa0
            ExprKind::Local(ref name) => {
                if let Some(&off) = self.locals.get(name) {
                    // Use FLW for 32-bit float, FLD for 64-bit double
                    let bits = match &expr.ty {
                        ValueType::Float { bits } => *bits,
                        _ => 32,
                    };
                    if bits <= 32 {
                        // FLW fa0, off(sp)
                        let imm = off as u32 & 0xfff;
                        let insn = imm << 20 | 2 << 15 | 0b010 << 12 | 10 << 7 | 0x07;
                        self.emit(&insn.to_le_bytes());
                    } else {
                        self.emit_fld(10, 2, off);
                    }
                    return true;
                }
                // Local not found in locals map - might be a function parameter
                // or global. Fall through to gen_expr for handling.
                if !is_float_ty(&expr.ty) && !is_vector_ty(&expr.ty) && self.gen_expr(expr) {
                    self.emit_fcvt_s_w_fa0();
                    return true;
                }
                // For float-typed locals not in map, just load 0.0
                self.load_float_fa0(0.0);
                return true;
            },
            // Float conditional: cond ? true_val : false_val
            ExprKind::Conditional if expr.children.len() >= 3 => {
                let lbl_false = self.fresh_label(".Lflt_cond_f");
                let lbl_end = self.fresh_label(".Lflt_cond_end");
                self.gen_expr(&expr.children[0]);
                self.beqz_a0(&lbl_false);
                self.gen_float_expr(&expr.children[1]);
                self.j(&lbl_end);
                self.label(&lbl_false);
                self.gen_float_expr(&expr.children[2]);
                self.label(&lbl_end);
                return true;
            },
            // Float assignment: evaluate rhs and store to lhs
            ExprKind::Assign if expr.children.len() >= 2 => {
                self.gen_float_expr(&expr.children[1]);
                let lhs = &expr.children[0];
                match &lhs.kind {
                    ExprKind::Local(ref name) => {
                        if let Some(&off) = self.locals.get(name) {
                            let bits = match &lhs.ty {
                                ValueType::Float { bits } => *bits,
                                _ => 32,
                            };
                            if bits <= 32 {
                                // FSW fa0, off(sp)
                                let imm = off as u32 & 0xfff;
                                let insn = imm << 20 | 2 << 15 | 0b010 << 12 | 10 << 7 | 0x27;
                                self.emit(&insn.to_le_bytes());
                            } else {
                                self.emit_fsd(10, 2, off);
                            }
                            return true;
                        }
                    },
                    ExprKind::Member(ref field_name) if !lhs.children.is_empty() => {
                        let child = &lhs.children[0];
                        if let ExprKind::Local(ref name) = child.kind {
                            if let Some(&off) = self.locals.get(name) {
                                let struct_name = match &child.ty {
                                    ValueType::Aggregate { name, .. } => Some(name.clone()),
                                    _ => None,
                                };
                                let field_off = struct_name
                                    .and_then(|name| {
                                        self.aggregate_fields.get(&name).and_then(|(fields, _)| {
                                            crate::ast::find_field(fields, field_name)
                                                .map(|(f, extra)| f.offset + extra)
                                        })
                                    })
                                    .unwrap_or(0);
                                let bits = match &lhs.ty {
                                    ValueType::Float { bits } => *bits,
                                    _ => 32,
                                };
                                if bits <= 32 {
                                    let imm = (off + field_off as i32) as u32 & 0xfff;
                                    let insn = imm << 20 | 2 << 15 | 0b010 << 12 | 10 << 7 | 0x27;
                                    self.emit(&insn.to_le_bytes());
                                } else {
                                    self.emit_fsd(10, 2, off + field_off as i32);
                                }
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
                        let struct_name = match &child.ty {
                            ValueType::Aggregate { name, .. } => Some(name.clone()),
                            _ => None,
                        };
                        let field_off = struct_name
                            .and_then(|name| {
                                self.aggregate_fields.get(&name).and_then(|(fields, _)| {
                                    crate::ast::find_field(fields, field_name)
                                        .map(|(f, extra)| f.offset + extra)
                                })
                            })
                            .unwrap_or(0);
                        let bits = match &expr.ty {
                            ValueType::Float { bits } => *bits,
                            _ => 32,
                        };
                        if bits <= 32 {
                            // FLW fa0, off+field_off(sp)
                            let imm = (off + field_off as i32) as u32 & 0xfff;
                            let insn = imm << 20 | 2 << 15 | 0b010 << 12 | 10 << 7 | 0x07;
                            self.emit(&insn.to_le_bytes());
                        } else {
                            self.emit_fld(10, 2, off + field_off as i32);
                        }
                        return true;
                    }
                }
            },
            // Cast: float→float or int→float
            ExprKind::Cast if !expr.children.is_empty() => {
                let child = &expr.children[0];
                if is_float_ty(&child.ty) && !is_float_ty(&expr.ty) {
                    // Float → Int: delegate to gen_expr
                    return self.gen_expr(expr);
                }
                // Target is float: evaluate child
                self.gen_expr(child);
                if !is_float_ty(&child.ty) && is_float_ty(&expr.ty) {
                    // Int → Float: gen_expr put integer in a0,
                    // emit FCVT.S.W to convert to float in fa0
                    let is_unsigned = matches!(&child.ty, ValueType::Integer { signed: false, .. });
                    if is_unsigned {
                        // FCVT.S.WU fa0, a0
                        self.emit_fcvt_s_wu(10, 10);
                    } else {
                        // FCVT.S.W fa0, a0
                        self.emit_fcvt_s_w_fa0();
                    }
                }
                return true;
            },
            _ => {
                // expr.ty 已是 float/vector 时不能再回 gen_expr (死循环).
                if !is_float_ty(&expr.ty) && !is_vector_ty(&expr.ty) && self.gen_expr(expr) {
                    self.emit_fcvt_s_w_fa0();
                    return true;
                }
            },
        }
        // Unsupported float expression: emit diagnostic and load 0.0
        let span = expr.span.clone();
        let mut diag = crate::diagnostics::Diagnostic::err(
            "E0314",
            format!("unsupported float IR expression on riscv64: {:?}", expr.kind),
            span,
        );
        diag.hint = Some("add handling in gen_float_expr".to_string());
        self.diagnostics.push(diag);
        self.load_float_fa0(0.0);
        false
    }

    pub(crate) fn gen_vector_expr(&mut self, expr: &TypedExpr) -> bool {
        match &expr.kind {
            ExprKind::Binary(op) if expr.children.len() >= 2 => {
                self.gen_vector_expr(&expr.children[0]);
                self.mv_a1_a0();
                self.gen_vector_expr(&expr.children[1]);
                self.binop_a0_a1(
                    *op,
                    matches!(&expr.children[0].ty, ValueType::Integer { signed: true, .. }),
                );
                return true;
            },
            ExprKind::Local(ref name) => {
                if let Some(&off) = self.locals.get(name) {
                    self.load_a0(off);
                } else {
                    self.li_a0(0);
                }
                return true;
            },
            ExprKind::Constant => {
                if let Some(ExprLiteral::Int(v)) = expr.literal {
                    self.li_a0(trunc_i32(v) as i32);
                } else {
                    self.li_a0(0);
                }
                return true;
            },
            ExprKind::Cast if !expr.children.is_empty() => {
                let child = &expr.children[0];
                if is_vector_ty(&child.ty) {
                    self.gen_vector_expr(child);
                } else {
                    self.gen_expr(child);
                }
                return true;
            },
            ExprKind::Assign if expr.children.len() >= 2 => {
                self.gen_vector_expr(&expr.children[1]);
                if let ExprKind::Local(ref name) = expr.children[0].kind {
                    if let Some(&off) = self.locals.get(name) {
                        self.store_a0(off);
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
                self.beqz_a0(&false_label);
                self.gen_vector_expr(&expr.children[1]);
                self.j(&end_label);
                self.label(&false_label);
                self.gen_vector_expr(&expr.children[2]);
                self.label(&end_label);
                return true;
            },
            _ => {
                self.li_a0(0);
            },
        }
        false
    }

    /// Generate code for complex type expressions.
    /// Complex values are stored as (real, imag) pairs:
    /// - Function return: fa0 = real, fa1 = imag
    /// - Local variable: [sp, #off] = real, [sp, #off+inner_size] = imag
    /// - Memory layout: real at lower address, imag at higher address
    pub(crate) fn gen_complex_expr(&mut self, expr: &TypedExpr) -> bool {
        let inner_size = match &expr.ty {
            ValueType::Complex(inner) => value_type_size(inner),
            _ => return false,
        };
        match &expr.kind {
            // Complex local variable: load both parts into fa0, fa1
            ExprKind::Local(ref name) => {
                if let Some(&off) = self.locals.get(name) {
                    // Load real part into fa0
                    if inner_size <= 4 {
                        // FLW fa0, off(sp)
                        let imm = off as u32 & 0xfff;
                        let insn = imm << 20 | 2 << 15 | 0b010 << 12 | 10 << 7 | 0x07;
                        self.emit(&insn.to_le_bytes());
                        // FLW fa1, off+inner_size(sp)
                        let imm2 = (off + inner_size as i32) as u32 & 0xfff;
                        let insn2 = imm2 << 20 | 2 << 15 | 0b010 << 12 | 11 << 7 | 0x07;
                        self.emit(&insn2.to_le_bytes());
                    } else {
                        self.emit_fld(10, 2, off);
                        self.emit_fld(11, 2, off + inner_size as i32);
                    }
                    return true;
                }
                // Unknown local: zero both parts
                self.load_float_fa0(0.0);
                self.mv_fa1_fa0();
                self.load_float_fa0(0.0);
                return true;
            },
            // Complex constant: real = literal, imag = 0
            ExprKind::Constant => {
                if let Some(ExprLiteral::Float(v)) = expr.literal {
                    self.load_float_fa0(v as f32);
                    self.mv_fa1_fa0(); // save real to fa1
                    self.load_float_fa0(0.0); // imag = 0
                                              // Swap: fa0 should be real, fa1 should be imag
                                              // Actually we need: fa0=real, fa1=imag=0
                                              // Current: fa0=0, fa1=real
                                              // Need to swap
                    self.mv_fa2_fa1(); // fa2 = real
                    self.load_float_fa0(0.0); // fa0 = 0 (imag)
                                              // Now swap fa2 and fa0: fa0=real, fa1=0
                                              // Use fa1 as temp
                    self.mv_fa1_fa0(); // fa1 = 0
                                       // Hmm, this is getting complex. Let me simplify:
                                       // Just load real to fa0, zero to fa1
                    self.load_float_fa0(v as f32);
                    // Zero fa1: FSGNJ.S fa1, fa0, fa0 then zero it
                    // Actually just load 0.0 to fa1
                    self.mv_fa1_fa0(); // save real to fa1
                    self.load_float_fa0(0.0); // fa0 = 0
                                              // Now swap fa0 and fa1
                                              // Use FSGNJ to copy: fa2 = fa1 (real), fa1 = fa0 (0), fa0 = fa2 (real)
                    self.emit_fsgnj_s(12, 11, 11); // fa2 = fa1 (real)
                    self.emit_fsgnj_s(11, 10, 10); // fa1 = fa0 (0)
                    self.emit_fsgnj_s(10, 12, 12); // fa0 = fa2 (real)
                    return true;
                }
                if let Some(ExprLiteral::Int(v)) = expr.literal {
                    self.li_a0(trunc_i32(v) as i32);
                    // int to float: FCVT.S fa0, a0
                    self.emit_fcvt_s_w_fa0();
                    self.mv_fa1_fa0(); // save real to fa1
                    self.load_float_fa0(0.0); // fa0 = 0 (imag)
                                              // Swap fa0 and fa1
                    self.emit_fsgnj_s(12, 11, 11); // fa2 = fa1 (real)
                    self.emit_fsgnj_s(11, 10, 10); // fa1 = fa0 (0)
                    self.emit_fsgnj_s(10, 12, 12); // fa0 = fa2 (real)
                    return true;
                }
                self.load_float_fa0(0.0);
                self.mv_fa1_fa0();
                self.load_float_fa0(0.0);
                return true;
            },
            // Complex binary ops: component-wise add/sub
            ExprKind::Binary(op) if expr.children.len() >= 2 => {
                // Evaluate left -> (fa0, fa1)
                self.gen_complex_expr(&expr.children[0]);
                // Save left real/imag to stack temp
                let tmp_real = self.alloc_local("<cxr>", inner_size as i32);
                let tmp_imag = self.alloc_local("<cxi>", inner_size as i32);
                if inner_size <= 4 {
                    // FSW fa0, tmp_real(sp)
                    let imm = tmp_real as u32;
                    let insn = ((imm >> 5) & 0x7f) << 25
                        | 10_u32 << 20
                        | 2_u32 << 15
                        | 0b010 << 12
                        | (imm & 0x1f) << 7
                        | 0x27;
                    self.emit(&insn.to_le_bytes());
                    // FSW fa1, tmp_imag(sp)
                    let imm2 = tmp_imag as u32;
                    let insn2 = ((imm2 >> 5) & 0x7f) << 25
                        | 11_u32 << 20
                        | 2_u32 << 15
                        | 0b010 << 12
                        | (imm2 & 0x1f) << 7
                        | 0x27;
                    self.emit(&insn2.to_le_bytes());
                } else {
                    self.emit_fsd(10, 2, tmp_real);
                    self.emit_fsd(11, 2, tmp_imag);
                }
                // Evaluate right -> (fa0, fa1)
                self.gen_complex_expr(&expr.children[1]);
                // Load left back into fa2, fa3
                if inner_size <= 4 {
                    // FLW fa2, tmp_real(sp)
                    let imm = tmp_real as u32 & 0xfff;
                    let insn = imm << 20 | 2 << 15 | 0b010 << 12 | 12 << 7 | 0x07;
                    self.emit(&insn.to_le_bytes());
                    // FLW fa3, tmp_imag(sp)
                    let imm2 = tmp_imag as u32 & 0xfff;
                    let insn2 = imm2 << 20 | 2 << 15 | 0b010 << 12 | 13 << 7 | 0x07;
                    self.emit(&insn2.to_le_bytes());
                } else {
                    self.emit_fld(12, 2, tmp_real);
                    self.emit_fld(13, 2, tmp_imag);
                }
                // Now: fa0,fa1 = right; fa2,fa3 = left
                // Component-wise operation
                match op {
                    BinOp::Add => {
                        // real: fa0 = fa2 + fa0
                        self.emit_fadd_s(10, 12, 10);
                        // imag: fa1 = fa3 + fa1
                        self.emit_fadd_s(11, 13, 11);
                    },
                    BinOp::Sub => {
                        // real: fa0 = fa2 - fa0
                        self.emit_fsub_s(10, 12, 10);
                        // imag: fa1 = fa3 - fa1
                        self.emit_fsub_s(11, 13, 11);
                    },
                    _ => {
                        // Unsupported complex binop: zero result
                        self.load_float_fa0(0.0);
                        self.mv_fa1_fa0();
                        self.load_float_fa0(0.0);
                    },
                }
                return true;
            },
            // Complex cast: component-wise cast
            ExprKind::Cast if !expr.children.is_empty() => {
                let child = &expr.children[0];
                if is_complex_ty(&child.ty) {
                    // Complex -> Complex: load both parts
                    self.gen_complex_expr(child);
                } else {
                    // Scalar -> Complex: real = scalar, imag = 0
                    self.gen_expr(child);
                    // Convert int/float in a0/fa0 to complex
                    if is_float_ty(&child.ty) {
                        // fa0 already has the value
                    } else {
                        // int in a0 -> float in fa0
                        self.emit_fcvt_s_w_fa0();
                    }
                    self.mv_fa1_fa0(); // save real to fa1
                    self.load_float_fa0(0.0); // fa0 = 0 (imag)
                                              // Swap fa0 and fa1
                    self.emit_fsgnj_s(12, 11, 11);
                    self.emit_fsgnj_s(11, 10, 10);
                    self.emit_fsgnj_s(10, 12, 12);
                }
                return true;
            },
            // Complex call: result in (fa0, fa1)
            ExprKind::Call => {
                self.gen_call(expr);
                return true;
            },
            // Complex conditional: branch-based
            ExprKind::Conditional if expr.children.len() >= 3 => {
                let lbl_false = self.fresh_label(".Lcx_cond_f");
                let lbl_end = self.fresh_label(".Lcx_cond_end");
                self.gen_expr(&expr.children[0]);
                self.beqz_a0(&lbl_false);
                self.gen_complex_expr(&expr.children[1]);
                self.j(&lbl_end);
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
            format!("unsupported complex IR expression on riscv64: {:?}", expr.kind),
            span,
        );
        diag.hint = Some("add handling in gen_complex_expr".to_string());
        self.diagnostics.push(diag);
        self.load_float_fa0(0.0);
        self.mv_fa1_fa0();
        self.load_float_fa0(0.0);
        false
    }
}
