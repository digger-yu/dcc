// src/codegen/x86_64/emit/expr/cast.rs
// Type casting expressions for x86_64 backend
// Handles explicit and implicit type conversions

use crate::ast::{Expr, Type};

/// Generate code for cast expressions.
/// 
/// Routes to specialized handlers based on target type:
/// - Integer casts (signed/unsigned, various widths)
/// - Float/double conversions
/// - Pointer casts
/// - Complex to/from scalar conversions
/// 
/// # Arguments
/// * `cg` - Codegen context
/// * `e` - Cast expression
/// * `dst` - Destination register index
pub fn gen_cast_expr(cg: &mut crate::codegen::x86_64::Codegen, e: &Expr, dst: u8) {
    match e {
        Expr::Cast { expr: operand, ty: target_ty, .. } => {
            let operand_ty = operand.ty();
            
            // Evaluate operand first
            cg.gen_expr(operand, dst);
            
            // Perform appropriate type conversion
            emit_cast_instruction(cg, dst, &operand_ty, target_ty);
        }
        _ => {}
    }
}

/// Emit machine instruction for type casting operation.
fn emit_cast_instruction(
    cg: &mut crate::codegen::x86_64::Codegen,
    reg: u8,
    from_ty: &Type,
    to_ty: &Type,
) {
    // Integer widening/narrowing with sign extension or zero-extension
    
    match (from_ty.size(), to_ty.size()) {
        // Widening: sign-extend or zero-extend
        (1, s) if s >= 2 => {
            // 8-bit to larger width
            let rex_w = 0x48 | if reg >= 8 { 0x44 } else { 0 };
            // Sign-extend: movsx rax, al
            cg.emit(&[rex_w, 0x63, 0xC0 | ((reg & 7) << 3) | (reg & 7)]);
        }
        (2, s) if s >= 4 => {
            // 16-bit to larger width  
            let rex_w = 0x48 | if reg >= 8 { 0x44 } else { 0 };
            // Sign-extend: movsx rax, ax
            cg.emit(&[rex_w, 0x63, 0xC0 | ((reg & 7) << 3) | (reg & 7)]);
        }
        (4, 8) => {
            // 32-bit to 64-bit: sign-extend
            // Already correct in x86_64 when using 32-bit registers (auto sign-extends)
        }
        // Zero-extend for unsigned types
        (1, _) | (2, _) | (4, _) => {
            // Zero-extend smaller to larger
            let rex_w = 0x48 | if reg >= 8 { 0x44 } else { 0 };
            // Zero-extend: movzx rax, byte/word/dword [rax]
            cg.emit(&[rex_w, 0xB6, 0xC0 | ((reg & 7) << 3) | (reg & 7)]);
        }
        
        // Narrowing: truncate (no instruction needed, just use low bits)
        (_, 1) | (_, 2) | (_, 4) => {
            // No instruction needed; compiler should ensure proper handling
            let _ = (reg, from_ty, to_ty);
        }
        
        // Same size: no conversion needed
        _ => {}
    }
    
    // Floating point conversions would go here
    if is_float_conversion(from_ty, to_ty) {
        emit_float_cast(cg, reg, from_ty, to_ty);
    }
}

/// Check if this is a floating point conversion
fn is_float_conversion(from_ty: &Type, to_ty: &Type) -> bool {
    matches!(from_ty, Type::Float | Type::Double | Type::LongDouble)
        || matches!(to_ty, Type::Float | Type::Double | Type::LongDouble)
}

/// Emit floating point conversion instructions.
fn emit_float_cast(
    _cg: &mut crate::codegen::x86_64::Codegen,
    _reg: u8,
    _from_ty: &Type,
    _to_ty: &Type,
) {
    // Placeholder: Extract float conversion logic from gen_expr
    // Typically cvtss2sd, cvtsd2ss, cvttss2si, etc.
    unimplemented!("extract float casting from gen_expr");
}
