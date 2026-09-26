// RISC-V 64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::ast::{find_field, BinOp, UnOp};
use crate::codegen::riscv64::codegen::{
    is_aggregate, is_complex_ty, is_float_ty, is_vector_ty, trunc_i32, value_type_size,
    Riscv64Codegen,
};
use crate::ir::{ExprKind, ExprLiteral, TypedExpr, ValueType};

impl Riscv64Codegen {
    pub(crate) fn gen_expr(&mut self, expr: &TypedExpr) -> bool {
        if is_vector_ty(&expr.ty) {
            return self.gen_vector_expr(expr);
        }
        // Handle Assign before float/complex dispatch to avoid
        // infinite recursion (Assign node ty may be float/aggregate).
        if let ExprKind::Assign = &expr.kind {
            if expr.children.len() >= 2
                && !is_float_ty(&expr.children[1].ty)
                && !is_complex_ty(&expr.children[1].ty)
                && !is_vector_ty(&expr.children[1].ty)
            {
                // Check for bitfield write before normal assignment
                let lhs = &expr.children[0];
                if let ExprKind::Member(ref field_name) = lhs.kind {
                    if !lhs.children.is_empty() {
                        let child = &lhs.children[0];
                        let sn = match &child.ty {
                            ValueType::Aggregate { name, .. } => Some(name.clone()),
                            ValueType::Pointer(inner) => {
                                if let ValueType::Aggregate { name, .. } = inner.as_ref() {
                                    Some(name.clone())
                                } else {
                                    None
                                }
                            },
                            _ => None,
                        };
                        let bf = sn.as_ref().and_then(|n| {
                            self.aggregate_fields.get(n).and_then(|(fields, _)| {
                                find_field(fields, field_name).and_then(|(f, extra)| {
                                    f.bit_width.map(|bw| {
                                        (f.offset + extra, bw, f.bit_offset, f.ty.clone())
                                    })
                                })
                            })
                        });
                        if let Some((f_off, bw, b_off, cty)) = bf {
                            if bw < (cty.size() * 8) as u32 {
                                self.gen_bitfield_store_riscv64(
                                    &lhs.children[0],
                                    &expr.children[1],
                                    f_off,
                                    bw,
                                    b_off,
                                    &cty,
                                );
                                return true;
                            }
                        }
                    }
                }
                // Normal (non-bitfield) assignment
                // Evaluate rhs -> a0
                self.gen_expr(&expr.children[1]);
                // Save rhs value to stack
                let save_off = self.alloc_local("<asgn>", 8);
                self.emit_sd(10, 2, save_off); // sd a0, [sp+save_off]
                let lhs = &expr.children[0];
                match &lhs.kind {
                    ExprKind::Local(ref name) => {
                        // Reload rhs
                        self.emit_ld(10, 2, save_off); // a0 = rhs
                        if let Some(&off) = self.locals.get(name) {
                            let sz = value_type_size(&lhs.ty).max(1);
                            self.emit_store_sz(10, 2, off, sz);
                        } else if self.global_rodata.contains_key(name) {
                            self.load_global_addr_a0(name); // a0 = global addr
                            self.emit_ld(11, 2, save_off); // a1 = rhs
                            let sz = value_type_size(&lhs.ty).max(1);
                            self.emit_store_sz(11, 10, 0, sz);
                        }
                    },
                    _ => {
                        self.gen_lvalue_addr(lhs); // a0 = addr
                        self.emit_addi(12, 10, 0); // a2 = addr (save)
                        self.emit_ld(11, 2, save_off); // a1 = rhs
                        let sz = value_type_size(&expr.ty).max(1);
                        if sz <= 8 {
                            self.emit_store_sz(11, 12, 0, sz);
                        } else {
                            // Aggregate: word-by-word copy
                            let mut off: i32 = 0;
                            while (off as usize) + 8 <= sz {
                                self.emit_ld(13, 11, off);
                                self.emit_sd(13, 12, off);
                                off += 8;
                            }
                            if (off as usize) < sz {
                                self.emit_ld(13, 11, off);
                                self.emit_store_sz(13, 12, off, sz - off as usize);
                            }
                        }
                    },
                }
                // Result of assignment is the rhs value
                self.emit_ld(10, 2, save_off); // a0 = rhs
                return true;
            }
        }
        // expr.ty 自身是 float/vector 时走 gen_float_expr.
        // 注意: 不能用 children 检查 (float 子节点会让 gen_expr 反复调
        // gen_float_expr 形成死循环). 例如 (int)sum(double) 这个 Cast
        // children[0]=Double 但 expr.ty=Integer, 应走 gen_expr 主路径,
        // 内层 Call(Double) 再递归调 gen_expr -> gen_float_expr.
        if is_float_ty(&expr.ty) {
            return self.gen_float_expr(expr);
        }
        // Complex type: dispatch to gen_complex_expr
        // Complex values are stored as pairs (real, imag) in consecutive
        // stack slots or register pairs (fa0, fa1).
        if is_complex_ty(&expr.ty) {
            return self.gen_complex_expr(expr);
        }
        match &expr.kind {
            ExprKind::Constant => {
                if let Some(ExprLiteral::Int(v)) = expr.literal {
                    self.li_a0(trunc_i32(v) as i32);
                    return true;
                }
                if let Some(ExprLiteral::Float(v)) = expr.literal {
                    self.load_float_fa0(v.as_f64() as f32);
                    return true;
                }
                self.li_a0(0);
            },
            ExprKind::Local(ref name) => {
                if let Some(&off) = self.locals.get(name) {
                    self.load_a0(off);
                    // 整数提升: char/short → int
                    self.gen_int_promote_ir(&expr.ty);
                    return true;
                }
                if self.global_rodata.contains_key(name) {
                    self.load_global_addr_a0(name);
                    return true;
                }
                self.li_a0(0);
            },
            ExprKind::Binary(op) if expr.children.len() >= 2 => {
                // Float comparison: children are float, result is integer
                if is_float_ty(&expr.children[0].ty)
                    && is_float_ty(&expr.children[1].ty)
                    && matches!(
                        op,
                        BinOp::EqEq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
                    )
                {
                    // Evaluate left float -> fa0
                    self.gen_float_expr(&expr.children[0]);
                    self.mv_fa1_fa0(); // save left to fa1
                                       // Evaluate right float -> fa0
                    self.gen_float_expr(&expr.children[1]);
                    // Now fa0 = right, fa1 = left
                    // Compare and set a0 from result
                    self.float_cmp_a0_fa1_fa0(*op, &expr.children[0].ty);
                    return true;
                }
                // Float binary arithmetic: children are float, result is float
                if is_float_ty(&expr.children[0].ty)
                    && is_float_ty(&expr.children[1].ty)
                    && is_float_ty(&expr.ty)
                {
                    return self.gen_float_expr(expr);
                }
                self.gen_expr(&expr.children[0]);
                self.mv_a1_a0();
                self.gen_expr(&expr.children[1]);
                self.binop_a0_a1(
                    *op,
                    matches!(&expr.children[0].ty, ValueType::Integer { signed: true, .. }),
                );
                return true;
            },
            ExprKind::Unary(op) if !expr.children.is_empty() => {
                self.gen_expr(&expr.children[0]);
                match op {
                    UnOp::Neg => self.neg_a0(),
                    UnOp::Plus => {},
                    UnOp::Not => self.cmp_a0_zero(),
                    UnOp::BitNot => self.not_a0(),
                }
                return true;
            },
            ExprKind::Cast if !expr.children.is_empty() => {
                let child = &expr.children[0];
                if is_float_ty(&child.ty) && !is_float_ty(&expr.ty) {
                    // Float → Int: child eval puts float in fa0,
                    // emit FCVT.W.S/FCVT.WU.S to convert to int in a0
                    self.gen_expr(child);
                    let is_unsigned = matches!(&expr.ty, ValueType::Integer { signed: false, .. });
                    match &child.ty {
                        ValueType::Float { bits: 32 } => {
                            if is_unsigned {
                                // FCVT.WU.S a0, fa0
                                self.emit_fcvt_wu_s(10, 10);
                            } else {
                                // FCVT.W.S a0, fa0
                                self.emit_fcvt_w_s(10, 10);
                            }
                        },
                        ValueType::Float { bits: 64 } => {
                            if is_unsigned {
                                // FCVT.WU.D a0, fa0
                                self.emit_fcvt_wu_d(10, 10);
                            } else {
                                // FCVT.W.D a0, fa0
                                self.emit_fcvt_w_d(10, 10);
                            }
                        },
                        _ => {},
                    }
                } else {
                    self.gen_expr(child);
                }
                return true;
            },
            ExprKind::VaArg { ap, ref arg_ty, .. } => {
                // M25: __builtin_va_arg(ap, T)
                // 聚合类型走 gen_va_arg_struct, 整数/指针走 gen_va_arg_int
                if is_aggregate(arg_ty) {
                    self.gen_va_arg_struct(ap, arg_ty);
                } else {
                    self.gen_va_arg_int(ap);
                }
                return true;
            },
            ExprKind::Call if !expr.children.is_empty() => {
                self.gen_call(expr);
                return true;
            },
            ExprKind::Conditional if expr.children.len() >= 3 => {
                self.gen_expr(&expr.children[0]);
                let lbl_else = self.fresh_label("Lc0");
                let lbl_end = self.fresh_label("Lc1");
                self.beqz_a0(&lbl_else);
                self.gen_expr(&expr.children[1]);
                self.j(&lbl_end);
                self.label(&lbl_else);
                self.gen_expr(&expr.children[2]);
                self.label(&lbl_end);
                return true;
            },
            ExprKind::Address if !expr.children.is_empty() => {
                // Extended: support &member, &deref, &index via gen_lvalue_addr
                match &expr.children[0].kind {
                    ExprKind::Local(ref name) => {
                        if let Some(&off) = self.locals.get(name) {
                            self.mov_a0_sp_off(off);
                            return true;
                        }
                        if self.global_rodata.contains_key(name) {
                            self.load_global_addr_a0(name);
                            return true;
                        }
                    },
                    _ => {
                        self.gen_lvalue_addr(&expr.children[0]);
                        return true;
                    },
                }
                self.li_a0(0);
                return true;
            },
            ExprKind::Dereference if !expr.children.is_empty() => {
                // *ptr: evaluate pointer, load value
                self.gen_expr(&expr.children[0]);
                let sz = value_type_size(&expr.ty).max(1);
                self.emit_load_sz(10, 10, 0, sz);
                return true;
            },
            ExprKind::Member(ref field_name) if !expr.children.is_empty() => {
                let child = &expr.children[0];
                if let ExprKind::Dereference = &child.kind {
                    self.gen_expr(&child.children[0]);
                } else {
                    self.gen_lvalue_addr(child);
                }
                // a0 = base address
                let struct_name = match &child.ty {
                    ValueType::Aggregate { name, .. } => Some(name.clone()),
                    ValueType::Pointer(inner) | ValueType::Array(inner, _) => {
                        if let ValueType::Aggregate { name, .. } = inner.as_ref() {
                            Some(name.clone())
                        } else {
                            None
                        }
                    },
                    _ => None,
                };
                let field_info = struct_name.as_ref().and_then(|n| {
                    self.aggregate_fields.get(n).and_then(|(fields, _)| {
                        find_field(fields, field_name).map(|(f, extra)| {
                            (f.offset + extra, f.bit_width, f.bit_offset, f.ty.clone())
                        })
                    })
                });
                let field_off = field_info.as_ref().map(|(off, _, _, _)| *off).unwrap_or(0);
                // Check if this is a bitfield read
                let bf_info = field_info
                    .as_ref()
                    .and_then(|(_, bw, bo, fty)| bw.map(|w| (w, *bo, fty.clone())));
                if let Some((bit_width, bit_offset, container_ty)) = bf_info {
                    if bit_width < (container_ty.size() * 8) as u32 {
                        // Bitfield extraction
                        if field_off != 0 {
                            self.emit_addi(10, 10, field_off as i32);
                        }
                        // a0 = container address, load container value
                        let csz = container_ty.size();
                        self.emit_load_sz(10, 10, 0, csz);
                        // SRLI a0, a0, bit_offset
                        if bit_offset > 0 {
                            self.emit_srli(10, 10, bit_offset as u8);
                        }
                        // Mask: a0 &= (1 << bit_width) - 1
                        if bit_width < 64 {
                            let mask: u64 = (1u64 << bit_width) - 1;
                            if mask <= 0x7FF {
                                self.emit_andi(10, 10, mask as i32);
                            } else {
                                // Save shifted value, load mask to a1, reload, AND
                                let sv = self.alloc_local("<bf_m>", 8);
                                self.emit_sd(10, 2, sv); // save
                                self.load_const64_a0(mask); // a0 = mask
                                self.emit_addi(11, 10, 0); // a1 = mask
                                self.emit_ld(10, 2, sv); // a0 = shifted
                                self.emit_and(10, 10, 11); // a0 &= mask
                            }
                        }
                        // Sign extend for signed bitfields
                        let is_unsigned =
                            matches!(&expr.ty, ValueType::Integer { signed: false, .. });
                        if !is_unsigned && bit_width < 64 {
                            let sign_bit = 1u64 << (bit_width - 1);
                            let inv_mask: u64 = !((1u64 << bit_width) - 1);
                            let sv2 = self.alloc_local("<bf_v>", 8);
                            self.emit_sd(10, 2, sv2); // save masked value
                                                      // Test sign bit
                            if sign_bit <= 0x7FF {
                                self.emit_andi(10, 10, sign_bit as i32);
                            } else {
                                let sv3 = self.alloc_local("<bf_t>", 8);
                                self.emit_sd(10, 2, sv3);
                                self.load_const64_a0(sign_bit);
                                self.emit_addi(11, 10, 0);
                                self.emit_ld(10, 2, sv3);
                                self.emit_and(10, 10, 11);
                            }
                            let lbl_end = format!(".Lbf_end_{}", self.pos());
                            // BNEZ a0, do_ext (sign bit set -> extend)
                            let lbl_do = format!(".Lbf_do_{}", self.pos());
                            self.emit_bnez(10, &lbl_do);
                            // Sign bit NOT set: reload value, skip extension
                            self.emit_ld(10, 2, sv2); // a0 = masked value
                            self.j(&lbl_end);
                            // Sign bit set: OR with inv_mask
                            self.label(&lbl_do);
                            self.load_const64_a0(inv_mask); // a0 = inv_mask
                            self.emit_addi(11, 10, 0); // a1 = inv_mask
                            self.emit_ld(10, 2, sv2); // a0 = masked value
                            self.emit_or(10, 10, 11); // a0 |= inv_mask
                            self.label(&lbl_end);
                        }
                        return true;
                    }
                }
                // Normal (non-bitfield) field read
                if field_off != 0 {
                    self.emit_addi(10, 10, field_off as i32);
                }
                let sz = value_type_size(&expr.ty).max(1);
                self.emit_load_sz(10, 10, 0, sz);
                return true;
            },
            ExprKind::Index if expr.children.len() >= 2 => {
                let arr = &expr.children[0];
                let idx = &expr.children[1];
                match &arr.ty {
                    ValueType::Array(_, _) => {
                        self.gen_lvalue_addr(arr);
                    },
                    _ => {
                        self.gen_expr(arr);
                    },
                }
                // a0 = base address
                self.emit_addi(12, 10, 0); // a2 = base (save)
                self.gen_expr(idx); // a0 = index
                let elem_sz = value_type_size(&expr.ty).max(1);
                if elem_sz != 1 {
                    self.emit_addi(11, 10, 0); // a1 = index
                    self.li_a0(elem_sz as i32);
                    self.emit_mul(10, 11, 10); // a0 = idx * elem_sz
                }
                // a2 = base, a0 = scaled offset
                self.emit_add(10, 12, 10); // a0 = base + offset
                                           // Load value
                let sz = value_type_size(&expr.ty).max(1);
                self.emit_load_sz(10, 10, 0, sz);
                return true;
            },
            ExprKind::Opaque => {
                // Opaque represents inline asm or label addresses that cannot be
                // represented in the typed IR. These should be handled at the AST
                // level via gen_expr_from_ast. For now, return 0 as a placeholder.
                self.li_a0(0);
                return true;
            },
            ExprKind::Aggregate => {
                // Aggregate expressions (struct/array values) are not yet fully
                // supported in the riscv64 backend. Return 0 as a placeholder.
                self.li_a0(0);
                return true;
            },
            _ => {
                // Unhandled IR expression: emit diagnostic instead of
                // silently producing wrong code (li a0, 0).
                let span = expr.span.clone();
                let mut diag = crate::diagnostics::Diagnostic::err(
                    "E0312",
                    format!("unsupported IR expression on riscv64: {:?}", expr.kind),
                    span,
                );
                diag.hint = Some("add handling in gen_expr or lower in SSA pass".to_string());
                self.diagnostics.push(diag);
                self.li_a0(0);
            },
        }
        false
    }
}
