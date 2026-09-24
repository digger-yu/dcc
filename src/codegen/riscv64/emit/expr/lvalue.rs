// RISC-V 64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::ast::{find_field};
use crate::codegen::riscv64::codegen::{Riscv64Codegen, value_type_size};
use crate::ir::{ExprKind, TypedExpr, ValueType};

impl Riscv64Codegen {
    /// Compute the address of an lvalue expression into a0 (x10).
    pub(crate) fn gen_lvalue_addr(&mut self, expr: &TypedExpr) {
        match &expr.kind {
            ExprKind::Local(ref name) => {
                if let Some(&off) = self.locals.get(name) {
                    self.mov_a0_sp_off(off);
                } else if self.global_rodata.contains_key(name) {
                    self.load_global_addr_a0(name);
                } else {
                    self.li_a0(0);
                }
            }
            ExprKind::Member(ref field_name) => {
                let child = &expr.children[0];
                if let ExprKind::Dereference = &child.kind {
                    self.gen_expr(&child.children[0]);
                } else {
                    self.gen_lvalue_addr(child);
                }
                let struct_name = match &child.ty {
                    ValueType::Aggregate { name, .. } => Some(name.clone()),
                    ValueType::Pointer(inner) | ValueType::Array(inner, _) => {
                        if let ValueType::Aggregate { name, .. } = inner.as_ref() {
                            Some(name.clone())
                        } else { None }
                    }
                    _ => None,
                };
                let off = struct_name.and_then(|n| {
                    self.aggregate_fields.get(&n).and_then(|(fields, _)| {
                        find_field(fields, field_name).map(|(f, extra)| f.offset + extra)
                    })
                }).unwrap_or(0);
                if off != 0 {
                    self.emit_addi(10, 10, off as i32);
                }
            }
            ExprKind::Index if expr.children.len() >= 2 => {
                let arr = &expr.children[0];
                match &arr.ty {
                    ValueType::Array(_, _) => { self.gen_lvalue_addr(arr); }
                    _ => { self.gen_expr(arr); }
                }
                self.emit_addi(12, 10, 0); // a2 = base
                self.gen_expr(&expr.children[1]); // a0 = index
                let elem_sz = value_type_size(&expr.ty).max(1);
                if elem_sz != 1 {
                    self.emit_addi(11, 10, 0); // a1 = index
                    self.li_a0(elem_sz as i32);
                    self.emit_mul(10, 11, 10); // a0 = idx * elem_sz
                }
                self.emit_add(10, 12, 10); // a0 = base + offset
            }
            ExprKind::Dereference if !expr.children.is_empty() => {
                self.gen_expr(&expr.children[0]);
            }
            ExprKind::Cast if !expr.children.is_empty() => {
                self.gen_lvalue_addr(&expr.children[0]);
            }
            ExprKind::Address if !expr.children.is_empty() => {
                self.gen_lvalue_addr(&expr.children[0]);
            }
            _ => {
                self.li_a0(0);
            }
        }
    }

}
