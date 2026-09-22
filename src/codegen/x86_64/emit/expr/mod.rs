// src/codegen/x86_64/emit/expr/mod.rs
// Expression code generation for x86_64 backend
// Extracted from gen_expr() in x86_64.rs (~4.4k lines total)

pub mod lvalue;

pub mod cast;
pub mod call;
pub mod unary;
pub mod literal;
pub mod init;
pub mod compound;

use crate::ast::{Expr, Type};

/// Main expression dispatcher - extracts from gen_expr() in x86_64.rs
/// 
/// Routes expressions to appropriate handlers based on Expr enum variant.
/// This is the primary entry point that replaces direct calls to gen_expr()
/// after migration is complete.
/// 
/// # Arguments
/// * `cg` - Codegen context (x86_64::Codegen)
/// * `e` - Expression to generate code for  
/// * `dst` - Destination register index (0-7, maps to rax-r7)
/// 
/// # Expression Types Handled
/// - Literals: Int, Char, Float, String, Vector
/// - Unary: Neg, Not, Deref, AddressOf
/// - Binary: Add, Sub, Mul, Div, Mod, comparisons
/// - Logical: AndAnd, OrOr (short-circuit)
/// - Cast: Type casting operations
/// - Call: Function calls
/// - L-value: Ident, Member, Index, Deref
/// - Compound: Assign, CompoundLit, Sizeof, Alignof
pub fn gen_expr(cg: &mut crate::codegen::x86_64::Codegen, e: &Expr, dst: u8) {
    // Handle special vector lvalue case first
    if matches!(
        e,
        Expr::Member { .. } | Expr::Index { .. } | Expr::Deref { .. }
    ) && matches!(e.ty(), Type::Vector(_, _))
    {
        cg.gen_vector_load_into_slot(e, 0);
        return;
    }
    
    // Handle special complex lvalue case
    if matches!(
        e,
        Expr::Member { .. } | Expr::Index { .. } | Expr::Deref { .. }
    ) && matches!(e.ty(), Type::Complex(_))
        && cg.gen_complex_lvalue_rvalue(e)
    {
        return;
    }
    
    // Dispatch to specific handlers based on expression type
    match e {
        Expr::Int { v, ty, .. } => {
            cg.gen_literal_expr(e, dst);
        }
        Expr::Char { v, ty, .. } => {
            cg.gen_literal_expr(e, dst);
        }
        Expr::Ident { name, ty, .. } => {
            lvalue::gen_lvalue_addr(cg, e, dst);
        }
        Expr::Binop { op, l, r, .. } => {
            binary::gen_binop_expr(cg, *op, l.as_ref(), r.as_ref(), dst);
        }
        Expr::Unary { op, e: operand, .. } => {
            unary::gen_unary_expr(cg, e, dst);
        }
        Expr::Cast { e: operand, type_name, .. } => {
            cast::gen_cast_expr(cg, operand.as_ref(), type_name, dst);
        }
        Expr::Call { f, args, .. } => {
            call::gen_call_expr(cg, e, dst);
        }
        Expr::Assign { l, r, .. } => {
            init::gen_init_expr(cg, e, dst);
        }
        Expr::CompoundLit { ty: _, fields } => {
            compound::gen_compound_expr(cg, e, dst);
        }
        // Additional expression types as they are extracted...
        _ => unimplemented!("Expression type not yet migrated: {:?}", std::mem::discriminant(e)),
    }
}
