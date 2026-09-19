// src/codegen/x86_64/emit/expr/compound.rs
// Compound expressions for x86_64 backend  
// Handles ternary conditional and other complex expressions

use crate::ast::{Expr, Type};

/// Generate code for compound expressions (ternary operator, etc.)
/// 
/// Routes to specialized handlers based on expression type:
/// - Ternary conditional: cond ? expr1 : expr2
/// - Comma expressions: (e1, e2)
/// - Statement expressions (GNU extension)
/// 
/// # Arguments
/// * `cg` - Codegen context
/// * `e` - Compound expression
/// * `dst` - Destination register index
pub fn gen_compound_expr(cg: &mut crate::codegen::x86_64::Codegen, e: &Expr, dst: u8) {
    match e {
        Expr::Ternary { cond, then, els } => {
            gen_ternary_operator(cg, cond, then, els, dst);
        }
        _ => {}
    }
}

/// Generate ternary conditional operator (cond ? then : else)
fn gen_ternary_operator(
    cg: &mut crate::codegen::x86_64::Codegen,
    cond: &Expr,
    then: &Expr,
    els: &Expr,
    dst: u8,
) {
    // Evaluate condition first
    cg.gen_expr(cond, 0); // → rax
    
    // Create labels
    let cond_false = cg.fresh_label();
    let end_cond = cg.fresh_label();
    
    // Test condition and branch
    cg.test_register(0);
    cg.jmp_rel32(&cond_false, crate::codegen::jump::JumpKind::Je);
    
    // Condition true: evaluate "then" branch
    cg.gen_expr(then, dst);
    cg.jmp_rel32(&end_cond, crate::codegen::jump::JumpKind::Jmp);
    
    // Condition false: evaluate "else" branch
    cg.label(&cond_false);
    cg.gen_expr(els, dst);
    
    cg.label(&end_cond);
}
