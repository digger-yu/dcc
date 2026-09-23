// src/codegen/aarch64/emit/expr/call.rs
// Function Call Evaluator
//
// Handles function calls including argument passing, return value extraction,
// variadic functions, and built-in functions.
// Implements AAPCS64 calling convention for AArch64.
//
// Extracted from: src/codegen/aarch64/original_monolith.rs (~2039-2200+)

/// Evaluate function call expression
pub fn eval_call(callee_name: &str, arguments: &[TypedExpr]) -> bool {
    todo!("Extract call evaluation from gen_call in original_monolith.rs")
}

#[derive(Debug, Clone)]
pub struct TypedExpr {
    pub kind: crate::ir::ExprKind,
    pub ty: crate::ir::ValueType,
    pub children: Vec<TypedExpr>,
    pub span: crate::diagnostics::Span,
    pub literal: Option<crate::ir::ExprLiteral>,
}
