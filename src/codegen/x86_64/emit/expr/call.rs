// src/codegen/x86_64/emit/expr/call.rs
// Function call expressions for x86_64 backend
// Handles argument passing and return value handling

use crate::ast::{Expr, Type};

/// Generate code for function call expressions.
/// 
/// Routes to specialized handlers based on call type:
/// - Direct calls (known function address)
/// - Indirect calls (function pointer)
/// - Varargs calls (variadic functions)
/// 
/// # Arguments
/// * `cg` - Codegen context
/// * `e` - Call expression
/// * `dst` - Destination register index for return value
pub fn gen_call_expr(cg: &mut crate::codegen::x86_64::Codegen, e: &Expr, dst: u8) {
    match e {
        Expr::Call { callee, args, .. } => {
            let callee_ty = callee.ty();
            
            // Determine if direct or indirect call
            if is_direct_function_call(callee) {
                gen_direct_call(cg, callee, args, dst);
            } else {
                gen_indirect_call(cg, callee, args, dst);
            }
        }
        _ => {}
    }
}

/// Check if callee is a direct function reference
fn is_direct_function_call(_callee: &Expr) -> bool {
    // Would check if callee resolves to known symbol
    true // Placeholder
}

/// Generate direct function call (known function name)
fn gen_direct_call(
    cg: &mut crate::codegen::x86_64::Codegen,
    callee: &Expr,
    args: &[Expr],
    _dst: u8,
) {
    // Get function name
    let func_name = get_function_name(callee);
    
    // Pass arguments according to SysV AMD64 ABI
    pass_sysv_args(cg, args);
    
    // Emit call instruction
    cg.emit_call(&func_name);
    
    // TODO: Handle return values (complex structs, __int128, etc.)
}

/// Generate indirect function call (function pointer)
fn gen_indirect_call(
    cg: &mut crate::codegen::x86_64::Codegen,
    callee: &Expr,
    args: &[Expr],
    _dst: u8,
) {
    // Load function pointer into register
    cg.gen_expr(callee, 0); // Load func ptr into rax
    
    // Pass arguments
    pass_sysv_args(cg, args);
    
    // Emit indirect call
    cg.emit(&[0xFF, 0xD0]); // call rax
}

/// Get function name from callee expression
fn get_function_name(_callee: &Expr) -> String {
    // Would extract function name from AST
    "unknown_function".to_string()
}

/// Pass arguments according to SysV AMD64 calling convention
fn pass_sysv_args(cg: &mut crate::codegen::x86_64::Codegen, args: &[Expr]) {
    use std::collections::HashMap;
    
    // SysV AMD64 register arguments:
    // GP regs: rdi(7), rsi(6), rdx(2), rcx(1), r8(8), r9(9)
    // XMM regs: xmm0-xmm7
    let gp_regs = [7u8, 6, 2, 1, 8, 9];
    let mut gp_idx = 0usize;
    let mut xmm_idx = 0usize;
    
    for (i, arg) in args.iter().enumerate() {
        let arg_ty = arg.ty();
        
        // Determine argument location (register or stack)
        if is_passable_in_registers(arg_ty) {
            // Try register first
            if gp_idx < gp_regs.len() && matches!(arg_ty, 
                Type::Int | Type::Ptr(_) | Type::Long | Type::UInt | Type::ULong)
            {
                // Pass in general-purpose register
                cg.gen_expr(arg, gp_regs[gp_idx]);
                gp_idx += 1;
                continue;
            }
            
            if xmm_idx < 8 && is_sse_passable(arg_ty) {
                // Pass in XMM register
                cg.gen_expr(arg, 0); // Evaluate to rax first
                emit_xmm_transfer(cg, 0, xmm_idx as u8);
                xmm_idx += 1;
                continue;
            }
        }
        
        // Overflow to stack
        evaluate_and_spill_to_stack(cg, arg, i);
    }
}

/// Check if type can be passed in registers per SysV ABI
fn is_passable_in_registers(ty: &Type) -> bool {
    !matches!(ty, Type::Complex(_) | Type::Struct(_, _, _, _) | Type::Union(_, _, _, _))
}

/// Check if type can be passed in SSE registers
fn is_sse_passable(ty: &Type) -> bool {
    matches!(ty, Type::Float | Type::Double | Type::Vector(_, _))
}

/// Transfer value from RAX to XMM register
fn emit_xmm_transfer(cg: &mut crate::codegen::x86_64::Codegen, from_reg: u8, to_xmm: u8) {
    let rex_w = 0x48 | if to_xmm >= 8 { 0x44 } else { 0 };
    let pfx = 0xF2; // MOVSD prefix
    
    // movsd xmm_reg, reg64
    cg.emit(&[pfx, rex_w, 0x10, 0xC0 | ((to_xmm & 7) << 3) | (from_reg & 7)]);
}

/// Evaluate expression and spill to stack slot
fn evaluate_and_spill_to_stack(
    cg: &mut crate::codegen::x86_64::Codegen,
    arg: &Expr,
    arg_index: usize,
) {
    // Evaluate to rax
    cg.gen_expr(arg, 0);
    
    // Store to appropriate stack slot (would calculate offset based on previous args)
    let stack_offset = calculate_stack_offset(arg_index);
    cg.store_rsp_qword(0, stack_offset);
}

/// Calculate stack offset for argument i (based on SysV ordering rules)
fn calculate_stack_offset(i: usize) -> usize {
    // Simplified: just use sequential slots
    i * 8 + 16 // Start after return address and alignment padding
}
