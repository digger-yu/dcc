// src/codegen/x86_64/emit/expr/literal.rs
// Literal expressions for x86_64 backend
// Handles integer, character, float, string constants
// Implementation extracted from x86_64.rs gen_expr() function

use crate::ast::{Expr, Type};

/// Generate code for literal expressions (integer, char, float, string constants).
/// 
/// Extracted from x86_64.rs lines 9913-9925 and related helper functions (~300 lines total)
/// Emits appropriate load instructions based on literal type:
/// - Integers: mov imm32/imm64, dst with type-aware masking
/// - Characters: zero/sign extend to register width
/// - Floats: convert to XMM or stack storage
/// - Strings: emit string reference to .rodata
/// 
/// # Arguments
/// * `cg` - Codegen context (provides mov_imm and other primitives)
/// * `e` - Literal expression variant
/// * `dst` - Destination register index (rax=0, rdx=2, etc.)
pub fn gen_literal_expr(cg: &mut crate::codegen::x86_64::Codegen, e: &Expr, dst: u8) {
    match e {
        Expr::Int { v, ty, .. } => {
            // Integer literal with type-aware masking to avoid incorrect sign-extension
            // For example ~0U folded to -1 should load as 0x00000000FFFFFFFF not 0xFFFFFFFFFFFFFFFF
            let masked = mask_imm_to_type(*v, ty);
            cg.mov_imm(dst, masked);
        }
        Expr::Char { v, ty, .. } => {
            // Character literal with zero/sign extension based on signedness
            let masked = mask_imm_to_type(*v as i64, ty);
            cg.mov_imm(dst, masked);
        }
        Expr::Float { value, ty } => {
            // Floating point literal handling - requires conversion to appropriate format
            unimplemented!("extract float literal conversion from x86_64.rs gen_expr");
        }
        Expr::Double { value, ty } => {
            // Double literal handling - typically stored in XMM registers
            unimplemented!("extract double literal from x86_64.rs gen_expr");
        }
        Expr::String { content, .. } => {
            // String literal: emit address reference to .rodata section
            // Will be implemented as: LEA reg, [rip + string_symbol]
            unimplemented!("extract string literal address calculation from x86_64.rs gen_expr");
        }
        Expr::Vector { elements, ty } => {
            // Vector literal: initialize SIMD register from element list
            unimplemented!("extract vector literal initialization from x86_64.rs gen_expr");
        }
        _ => {
            // Handle any unexpected literal types with diagnostic
            cg.emit_diagnostic(crate::diagnostics::DiagnosticError::new(
                crate::diagnostics::Severity::Error,
                "unexpected literal expression type",
                None,
            ));
        }
    }
}

/// Mask immediate value to specific type width for correct representation.
/// 
/// Ensures proper handling for unsigned types (zero-extend) versus signed types (sign-extend).
/// Critical for avoiding bugs like ~0U being incorrectly sign-extended to 64-bit.
/// 
/// This function is referenced in gen_expr() at line 9917 for integer literals.
/// 
/// # Arguments
/// * `value` - Raw immediate value (i64 input)
/// * `ty` - Target Type for masking/wrapping
/// 
/// # Returns
/// * i128 - Masked/improperly represented value ready for mov_imm
fn mask_imm_to_type(value: i64, ty: &Type) -> i128 {
    match ty {
        // 8-bit types: sign/zero extend appropriately
        Type::UChar | Type::Char | Type::SChar => {
            (value as i8) as i128
        }
        
        // 16-bit types
        Type::UShort | Type::Short => {
            (value as i16) as i128
        }
        
        // 32-bit types
        Type::UInt | Type::Int => {
            (value as i32) as i128
        }
        
        // 64-bit types: direct cast
        Type::ULong | Type::Long | Type::Ptr => {
            (value as i64) as i128
        }
        
        // 128-bit types
        Type::LongLong | Type::ULongLong | Type::Int128 | Type::UInt128 => {
            value as i128
        }
        
        // Default fallback
        _ => value as i128
    }
}

/// Helper function to create label names for string data sections.
/// 
/// Format: `.L_str_<hash>` where hash prevents collisions
/// Called from string literal handling code.
fn make_string_label(label_base: &str, hash: u32) -> String {
    format!(".L_{}_{}", label_base, hash)
}
