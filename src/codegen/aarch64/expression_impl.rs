// AArch64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::ast::{find_field, BinOp, UnOp};
use crate::codegen::aarch64::codegen::{
    is_aggregate, is_complex_ty, is_float_ty, is_vector_ty, trunc_i32, value_type_size,
    Aarch64Codegen,
};
use crate::codegen::aarch64::insn::Insn;
use crate::ir::{ExprKind, ExprLiteral, TypedExpr, ValueType};

impl Aarch64Codegen {
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
                            ValueType::Pointer(inner) | ValueType::Array(inner, _) => {
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
                                self.gen_bitfield_store_aarch64(
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
                // Evaluate rhs -> x0
                self.gen_expr(&expr.children[1]);
                // Save rhs value to stack temp (safe across recursive calls)
                let save_off = self.alloc_local("<asgn>", 8);
                self.emit_str_x(0, 31, save_off as u32, 8);
                let lhs = &expr.children[0];
                match &lhs.kind {
                    ExprKind::Local(ref name) => {
                        // Reload rhs value
                        self.emit_ldr_x(0, 31, save_off as u32, 8);
                        if let Some(&off) = self.locals.get(name) {
                            let sz = value_type_size(&lhs.ty).max(1);
                            match sz {
                                1 => {
                                    self.emit_str_x(0, 31, 0, 1); /* placeholder */
                                    self.store_x_at_sp(off, 0, 1);
                                },
                                2 => {
                                    self.emit_str_x(0, 31, 0, 1);
                                    self.store_x_at_sp(off, 0, 2);
                                },
                                4 => self.store_w0(off),
                                _ => {
                                    // 8 bytes or aggregate
                                    self.emit_str_x(0, 31, off as u32, 8);
                                    if sz > 8 {
                                        // Aggregate: save src addr, copy word-by-word
                                        self.emit_ldr_x(14, 31, save_off as u32, 8);
                                        self.mov_x0_sp_off(off); // x0 = dst addr
                                        self.aggregate_store_loop(14, 0, 8, sz);
                                    }
                                },
                            }
                        } else if self.global_rodata.contains_key(name) {
                            self.load_global_addr_w0(name); // x0 = global addr
                            self.emit_ldr_x(14, 31, save_off as u32, 8); // x14 = rhs
                            let sz = value_type_size(&lhs.ty).max(1);
                            self.emit_store_sz(14, 0, 0, sz);
                        }
                    },
                    _ => {
                        self.gen_lvalue_addr(lhs); // x0 = addr
                        self.emit_mov_x_reg(12, 0); // x12 = addr
                        self.emit_ldr_x(14, 31, save_off as u32, 8); // x14 = rhs
                        let sz = value_type_size(&expr.ty).max(1);
                        if sz <= 8 {
                            self.emit_store_sz(14, 12, 0, sz);
                        } else {
                            self.aggregate_store_loop(14, 12, 0, sz);
                        }
                    },
                }
                // Result of assignment is the rhs value
                self.emit_ldr_x(0, 31, save_off as u32, 8);
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
        // stack slots or register pairs (v0, v1).
        if is_complex_ty(&expr.ty) {
            return self.gen_complex_expr(expr);
        }
        match &expr.kind {
            ExprKind::Constant => {
                if let Some(ExprLiteral::Int(v)) = expr.literal {
                    self.mov_w0_imm(trunc_i32(v));
                    return true;
                }
                if let Some(ExprLiteral::Float(v)) = expr.literal {
                    self.load_float_s0(v.as_f64() as f32);
                    return true;
                }
                self.mov_w0_imm(0);
            },
            ExprKind::Local(ref name) => {
                if let Some(&off) = self.locals.get(name) {
                    self.load_w0(off);
                    // 整数提升: char/short → int
                    self.gen_int_promote_ir(&expr.ty);
                    return true;
                }
                if self.global_rodata.contains_key(name) {
                    self.load_global_addr_w0(name);
                    return true;
                }
                self.mov_w0_imm(0);
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
                    // Evaluate left float -> s0
                    self.gen_float_expr(&expr.children[0]);
                    self.mov_s1_s0(); // save left to s1
                                      // Evaluate right float -> s0
                    self.gen_float_expr(&expr.children[1]);
                    // Now s0 = right, s1 = left
                    // fcmp s1, s0 and set w0 from condition
                    self.fcmp_s0_s1();
                    self.fset_w0_from_fcmp(*op);
                    return true;
                }
                // Float binary arithmetic: children are float, result is float
                // This case is reached when expr.ty is float but gen_expr was
                // called (e.g., from a cast context). Dispatch to gen_float_expr.
                if is_float_ty(&expr.children[0].ty)
                    && is_float_ty(&expr.children[1].ty)
                    && is_float_ty(&expr.ty)
                {
                    return self.gen_float_expr(expr);
                }
                self.gen_expr(&expr.children[0]);
                self.mov_w14_w0();
                self.gen_expr(&expr.children[1]);
                self.mov_w1_w0();
                self.mov_w0_w14();
                self.binop_w0_w1(
                    *op,
                    matches!(&expr.children[0].ty, ValueType::Integer { signed: true, .. }),
                );
                return true;
            },
            ExprKind::Unary(op) if !expr.children.is_empty() => {
                self.gen_expr(&expr.children[0]);
                match op {
                    UnOp::Neg => self.neg_w0(),
                    UnOp::Plus => {},
                    UnOp::Not => self.cmp_w0_zero(),
                    UnOp::BitNot => self.mvn_w0(),
                }
                return true;
            },
            ExprKind::Cast if !expr.children.is_empty() => {
                let child = &expr.children[0];
                if is_vector_ty(&child.ty) {
                    self.gen_vector_expr(child);
                    self.extract_vector_lane_w0(0);
                } else if is_float_ty(&child.ty) && !is_float_ty(&expr.ty) {
                    // Float → Int conversion: child eval puts result in s0,
                    // emit FCVTZS/FCVTZU to convert to integer in w0
                    self.gen_expr(child);
                    let is_unsigned = matches!(&expr.ty, ValueType::Integer { signed: false, .. });
                    match &child.ty {
                        ValueType::Float { bits: 32 } => {
                            if is_unsigned {
                                // FCVTZU W0, S0: 0x1E190000
                                self.emit_insn(&Insn::raw(0x1E190000));
                            } else {
                                // FCVTZS W0, S0: 0x1E180000
                                self.emit_insn(&Insn::raw(0x1E180000));
                            }
                        },
                        ValueType::Float { bits: 64 } => {
                            if is_unsigned {
                                // FCVTZU W0, D0: 0x9E190000
                                self.emit_insn(&Insn::raw(0x9E190000));
                            } else {
                                // FCVTZS W0, D0: 0x9E180000
                                self.emit_insn(&Insn::raw(0x9E180000));
                            }
                        },
                        _ => {},
                    }
                    self.truncate_w0_to(&expr.ty);
                } else {
                    self.gen_expr(child);
                    self.truncate_w0_to(&expr.ty);
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
            ExprKind::Call => {
                self.gen_call(expr);
                return true;
            },
            ExprKind::Conditional if expr.children.len() >= 3 => {
                // csel 无分支三元: cond ? true_val : false_val
                self.gen_expr(&expr.children[0]);
                self.mov_w15_w0();
                self.gen_expr(&expr.children[1]);
                self.mov_w14_w0();
                self.gen_expr(&expr.children[2]);
                self.mov_w1_w0();
                self.mov_w0_w14();
                self.cmp_w15_zero();
                self.csel_w0_w0_w1_ne();
                return true;
            },
            ExprKind::Index if expr.children.len() >= 2 && is_vector_ty(&expr.children[0].ty) => {
                self.gen_vector_expr(&expr.children[0]);
                let idx = expr.children[1]
                    .literal
                    .and_then(|lit| match lit {
                        ExprLiteral::Int(v) => Some(v as usize),
                        ExprLiteral::Float(_) => None,
                    })
                    .unwrap_or(0);
                self.extract_vector_lane_w0(idx);
                return true;
            },
            ExprKind::Address if !expr.children.is_empty() => {
                // Extended: support &member, &deref, &index via gen_lvalue_addr
                match &expr.children[0].kind {
                    ExprKind::Local(ref name) => {
                        if let Some(&off) = self.locals.get(name) {
                            self.mov_x0_sp_off(off);
                            return true;
                        }
                        if self.global_rodata.contains_key(name) {
                            self.load_global_addr_w0(name);
                            return true;
                        }
                    },
                    _ => {
                        self.gen_lvalue_addr(&expr.children[0]);
                        return true;
                    },
                }
                self.mov_w0_imm(0);
                return true;
            },
            ExprKind::Dereference if !expr.children.is_empty() => {
                // *ptr: evaluate pointer, load value
                self.gen_expr(&expr.children[0]);
                // x0 = pointer address
                let sz = value_type_size(&expr.ty).max(1);
                let signed = matches!(&expr.ty, ValueType::Integer { signed: true, .. });
                if sz <= 8 {
                    self.emit_load_sz(0, 0, 0, sz, signed);
                } else {
                    // Aggregate dereference: load first word
                    self.emit_ldr_x(0, 0, 0, 8);
                }
                return true;
            },
            ExprKind::Member(ref field_name) if !expr.children.is_empty() => {
                // Compute base address
                let child = &expr.children[0];
                if let ExprKind::Dereference = &child.kind {
                    // ptr->field: evaluate inner pointer
                    self.gen_expr(&child.children[0]);
                } else {
                    // s.field: compute address of struct
                    self.gen_lvalue_addr(child);
                }
                // x0 = base address
                // Look up field info
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
                let field_info = struct_name.as_ref().and_then(|name| {
                    self.aggregate_fields.get(name).and_then(|(fields, _)| {
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
                            self.emit_mov_x_reg(12, 0);
                            self.emit_add_imm_x(0, 12, field_off as u32);
                        }
                        // x0 = container address, load container value
                        let csz = container_ty.size();
                        self.emit_ldr_x(0, 0, 0, csz); // zero-extend load
                                                       // LSR x0, x0, bit_offset
                        if bit_offset > 0 {
                            self.emit_lsr_x(0, 0, bit_offset as u8);
                        }
                        // AND x0, x0, mask
                        if bit_width < 64 {
                            let mask: u64 = (1u64 << bit_width) - 1;
                            self.mov_x_reg_imm(12, mask);
                            self.emit_and_x_reg(0, 0, 12);
                        }
                        // Sign extend for signed bitfields
                        let is_unsigned =
                            matches!(&expr.ty, ValueType::Integer { signed: false, .. });
                        if !is_unsigned && bit_width < 64 {
                            let sign_bit = 1u64 << (bit_width - 1);
                            self.mov_x_reg_imm(12, sign_bit);
                            self.emit_ands_x_reg(12, 0, 12); // TST x0, sign_bit
                                                             // B.NE +2 (skip next ORR, 8 bytes = 2 insns)
                            self.emit_insn(&Insn::raw(0x54000041));
                            let inv_mask: u64 = !((1u64 << bit_width) - 1);
                            self.mov_x_reg_imm(12, inv_mask);
                            self.emit_or_x_reg(0, 0, 12); // ORR x0, x0, x12
                        }
                        return true;
                    }
                }
                // Normal (non-bitfield) field read
                if field_off != 0 {
                    self.emit_mov_x_reg(12, 0); // x12 = base
                    self.emit_add_imm_x(0, 12, field_off as u32); // x0 = base + off
                }
                // Now x0 = address of field, load value
                let sz = value_type_size(&expr.ty).max(1);
                let signed = matches!(&expr.ty, ValueType::Integer { signed: true, .. });
                if sz <= 8 {
                    self.emit_load_sz(0, 0, 0, sz, signed);
                } else {
                    self.emit_ldr_x(0, 0, 0, 8);
                }
                return true;
            },
            ExprKind::Index if expr.children.len() >= 2 => {
                let arr = &expr.children[0];
                let idx = &expr.children[1];
                // Array base: compute address; Pointer base: evaluate value
                match &arr.ty {
                    ValueType::Array(_, _) => {
                        self.gen_lvalue_addr(arr);
                    },
                    _ => {
                        self.gen_expr(arr);
                    },
                }
                // x0 = base address
                self.emit_mov_x_reg(12, 0); // x12 = base
                self.gen_expr(idx); // x0 = index
                                    // Scale index by element size
                let elem_sz = value_type_size(&expr.ty).max(1);
                if elem_sz != 1 {
                    self.emit_mov_x_reg(9, 0); // x9 = index
                                               // Use LSL for power-of-2 sizes
                    match elem_sz {
                        2 => {
                            self.emit_insn(&Insn::raw(0xd3000109));
                        }, // lsl x9, x9, #1
                        4 => {
                            self.emit_insn(&Insn::raw(0xd3000209));
                        }, // lsl x9, x9, #2
                        8 => {
                            self.emit_insn(&Insn::raw(0xd3000309));
                        }, // lsl x9, x9, #3
                        16 => {
                            self.emit_insn(&Insn::raw(0xd3000409));
                        }, // lsl x9, x9, #4
                        _ => {
                            self.mov_x_reg_imm(10, elem_sz as u64);
                            self.emit_mul_x(9, 9, 10); // x9 = idx * elem_sz
                        },
                    }
                } else {
                    self.emit_mov_x_reg(9, 0); // x9 = index (no scaling)
                }
                // x12 = base, x9 = scaled offset
                self.emit_add_x_reg(0, 12, 9); // x0 = base + offset
                                               // Load value
                let sz = value_type_size(&expr.ty).max(1);
                let signed = matches!(&expr.ty, ValueType::Integer { signed: true, .. });
                if sz <= 8 {
                    self.emit_load_sz(0, 0, 0, sz, signed);
                } else {
                    self.emit_ldr_x(0, 0, 0, 8);
                }
                return true;
            },
            ExprKind::Opaque => {
                // Opaque represents inline asm or label addresses that cannot be
                // represented in the typed IR. These should be handled at the AST
                // level via gen_expr_from_ast. For now, return 0 as a placeholder.
                self.mov_w0_imm(0);
                return true;
            },
            ExprKind::Aggregate => {
                // Aggregate expressions (struct/array values) are not yet fully
                // supported in the aarch64 backend. Return 0 as a placeholder.
                self.mov_w0_imm(0);
                return true;
            },
            _ => {
                // Unhandled IR expression: emit diagnostic instead of
                // silently producing wrong code (mov w0, #0).
                let span = expr.span.clone();
                let mut diag = crate::diagnostics::Diagnostic::err(
                    "E0312",
                    format!("unsupported IR expression on aarch64: {:?}", expr.kind),
                    span,
                );
                diag.hint = Some("add handling in gen_expr or lower in SSA pass".to_string());
                self.diagnostics.push(diag);
                self.mov_w0_imm(0);
            },
        }
        false
    }

    /// Compute the address of an lvalue expression into x0.
    /// Supports: Local, Member, Dereference, Index, Cast, Address.
    pub(crate) fn gen_lvalue_addr(&mut self, expr: &TypedExpr) {
        match &expr.kind {
            ExprKind::Local(ref name) => {
                if let Some(&off) = self.locals.get(name) {
                    self.mov_x0_sp_off(off);
                } else if self.global_rodata.contains_key(name) {
                    self.load_global_addr_w0(name);
                } else {
                    self.mov_w0_imm(0);
                }
            },
            ExprKind::Member(ref field_name) => {
                let child = &expr.children[0];
                if let ExprKind::Dereference = &child.kind {
                    // ptr->field: evaluate pointer
                    self.gen_expr(&child.children[0]);
                } else {
                    // s.field: compute address of struct
                    self.gen_lvalue_addr(child);
                }
                // x0 = base address, add field offset
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
                let off = struct_name
                    .and_then(|n| {
                        self.aggregate_fields.get(&n).and_then(|(fields, _)| {
                            find_field(fields, field_name).map(|(f, extra)| f.offset + extra)
                        })
                    })
                    .unwrap_or(0);
                if off != 0 {
                    self.emit_mov_x_reg(12, 0);
                    self.emit_add_imm_x(0, 12, off as u32);
                }
            },
            ExprKind::Index if expr.children.len() >= 2 => {
                let arr = &expr.children[0];
                match &arr.ty {
                    ValueType::Array(_, _) => {
                        self.gen_lvalue_addr(arr);
                    },
                    _ => {
                        self.gen_expr(arr);
                    },
                }
                // x0 = base address
                self.emit_mov_x_reg(12, 0); // x12 = base
                self.gen_expr(&expr.children[1]); // x0 = index
                let elem_sz = value_type_size(&expr.ty).max(1);
                self.emit_mov_x_reg(9, 0); // x9 = index
                if elem_sz != 1 {
                    match elem_sz {
                        2 => {
                            self.emit_insn(&Insn::raw(0xd3000109));
                        },
                        4 => {
                            self.emit_insn(&Insn::raw(0xd3000209));
                        },
                        8 => {
                            self.emit_insn(&Insn::raw(0xd3000309));
                        },
                        16 => {
                            self.emit_insn(&Insn::raw(0xd3000409));
                        },
                        _ => {
                            self.mov_x_reg_imm(10, elem_sz as u64);
                            self.emit_mul_x(9, 9, 10);
                        },
                    }
                }
                self.emit_add_x_reg(0, 12, 9); // x0 = base + scaled_idx
            },
            ExprKind::Dereference if !expr.children.is_empty() => {
                self.gen_expr(&expr.children[0]);
                // x0 = pointer value = address
            },
            ExprKind::Cast if !expr.children.is_empty() => {
                self.gen_lvalue_addr(&expr.children[0]);
            },
            ExprKind::Address if !expr.children.is_empty() => {
                self.gen_lvalue_addr(&expr.children[0]);
            },
            _ => {
                self.mov_w0_imm(0);
            },
        }
    }
}
