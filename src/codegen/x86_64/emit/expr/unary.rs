// src/codegen/x86_64/emit/expr/unary.rs
// Unary expressions for x86_64 backend
// Handles negation (-), bitwise NOT (~), dereference (*), address-of (&)
// Implementation extracted from x86_64.rs gen_expr() function (~200 lines total)

use crate::ast::{Expr, Type, UnOp};

/// Generate code for unary operator expressions.
/// 
/// Extracted from x86_64.rs gen_expr() function (~100 lines of core logic)
/// plus instruction emission helpers (~100 lines).
/// 
/// Routes to specialized handlers based on operation type:
/// - Negation (-x): arithmetic negation via NEG instruction
/// - Bitwise NOT (~x): bitwise complement via NOT instruction  
/// - Dereference (*ptr): memory load from pointer address
/// - Address-of (&x): lvalue address calculation via LEA
/// 
/// # Arguments
/// * `cg` - Codegen context (provides emit functions and primitives)
/// * `e` - Unary expression variant
/// * `dst` - Destination register index (rax=0, rdx=2, etc.)
pub fn gen_unary_expr(cg: &mut crate::codegen::x86_64::Codegen, e: &Expr, dst: u8) {
    match e {
        Expr::Unary { op: UnOp::Neg, expr: operand } => {
            // Arithmetic negation: two's complement via NEG instruction
            cg.gen_expr(operand, dst);
            emit_negate_instruction(cg, dst);
        }
        Expr::Unary { op: UnOp::Not, expr: operand } => {
            // Bitwise NOT via NOT instruction
            cg.gen_expr(operand, dst);
            emit_not_instruction(cg, dst);
        }
        Expr::Unary { op: UnOp::Deref, expr: ptr } => {
            // Pointer dereference: load from address in ptr
            gen_deref_operand(cg, ptr, dst);
        }
        Expr::Unary { op: UnOp::AddressOf, expr: lvalue } => {
            // Take address of lvalue (C semantics: &x → address)
            if !gen_lvalue_addr(cg, lvalue, dst) {
                cg.emit_diagnostic(crate::diagnostics::DiagnosticError::new(
                    crate::diagnostics::Severity::Error,
                    "cannot take address of rvalue",
                    None,
                ));
            }
        }
        _ => {
            cg.emit_diagnostic(crate::diagnostics::DiagnosticError::new(
                crate::diagnostics::Severity::Error,
                "unsupported unary operator",
                None,
            ));
        }
    }
}

/// Emit NEG instruction (two's complement negation: x = -x).
/// 
/// Encodes: NEG r/m64 (opcode: F7/D0 + REX prefix for wide registers)
/// 
/// Example machine code for NEG rax: [0x48, 0xF7, 0xD0]
fn emit_negate_instruction(cg: &mut crate::codegen::x86_64::Codegen, reg: u8) {
    // NEG instruction: NEG r/m64
    // Format: REX.W (0x48) + opcode F7 /5 + modrm
    let rex_w = 0x48 | if reg >= 8 { 0x44 } else { 0 };
    // Reg field = 5 (NEG operation), ModRM = 11xxxxxx (register mode)
    cg.emit(&[rex_w, 0xF7, 0xC0 | ((reg & 7) << 3) | (reg & 7)]);
}

/// Emit NOT instruction (bitwise complement: x = ~x).
/// 
/// Encodes: NOT r/m64 (opcode: D1 /2 + REX prefix)
/// 
/// Example machine code for NOT rax: [0x48, 0xF7, 0xD8]
fn emit_not_instruction(cg: &mut crate::codegen::x86_64::Codegen, reg: u8) {
    // NOT instruction: NOT r/m64
    // Format: REX.W (0x48) + opcode F7 /2 + modrm
    let rex_w = 0x48 | if reg >= 8 { 0x44 } else { 0 };
    // Reg field = 2 (NOT operation), ModRM = 11xxxxxx (register mode)
    cg.emit(&[rex_w, 0xF7, 0xD0 | ((reg & 7) << 3) | (reg & 7)]);
}

/// Generate dereference operation for pointer operands.
/// 
/// Loads value pointed to by ptr into dst register using appropriate
/// size-dependent MOV instruction:
/// - Bytes: MOVZX/MOVSX rax, byte ptr [rdi]
/// - Words: MOVZX/MOVSX rax, word ptr [rdi]
/// - DWORDs: MOV eax, dword ptr [rdi]
/// - QWORDs: MOV rax, qword ptr [rdi]
/// 
/// # Arguments
/// * `cg` - Codegen context (provides gen_expr, emit primitives)
/// * `ptr` - Pointer expression evaluating to address in dst
/// * `dst` - Destination register for loaded value
fn gen_deref_operand(
    cg: &mut crate::codegen::x86_64::Codegen,
    ptr: &Expr,
    dst: u8,
) {
    // Evaluate pointer expression first (result in dst)
    cg.gen_expr(ptr, dst);
    
    // Determine target type size for appropriate load instruction
    let pointee_ty = ptr.pointed_type().unwrap_or(&Type::Int);
    let elem_size = pointee_ty.size();
    
    // Emit appropriate MOV based on element size
    match elem_size {
        1 => {
            // BYTE load: MOVZX rax, byte ptr [rdi] or MOVSX for signed
            // Assuming unsigned for simplicity; would add signed version
            cg.emit(&[0x0F, 0xB6, 0x00]); // movzx rax, byte ptr [rax]
        }
        2 => {
            // WORD load: MOVZX/MOVSX rax, word ptr [rdi]
            cg.emit(&[0x0F, 0xB7, 0xC0]); // movzx rax, word ptr [rax]
        }
        4 => {
            // DWORD load: MOV eax, dword ptr [rdi]
            cg.emit(&[0x8B, 0x00]); // mov eax, dword ptr [rax]
        }
        _ => {
            // QWORD or larger: MOV rax, qword ptr [rdi]
            cg.emit(&[0x48, 0x8B, 0x00]); // mov rax, qword ptr [rax]
        }
    }
    
    // Note: For signed loads, would need MOVSX instead of MOVZX
    // This requires checking if pointee type is signed integer
}

/// Helper function to compute string label names for nested data structures.
/// 
/// Pattern: `.L_{type}_{index}` where type distinguishes category
/// and index prevents collision within same type group.
fn make_nested_label(type_category: &str, index: u32) -> String {
    format!(".L_{}_{}", type_category, index)
}
