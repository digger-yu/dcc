use super::*;

use crate::ast::{Expr, Type};
use crate::diagnostics::Span;
use crate::ssa::expr_rebuild::inst_to_expr_resolved;
use std::collections::HashMap;

fn un(op: crate::ast::UnOp, inner_ty: Type) -> InstKind {
    let sp = Span::point("t.c", 1, 1);
    InstKind::Un {
        op,
        e: Operand::Var(SsaVar {
            base: "b".into(),
            version: 1,
            ty: inner_ty,
            span: sp,
        }),
    }
}

fn rebuilt(kind: &InstKind, dest_ty: Option<&Type>) -> Expr {
    let sp = Span::point("t.c", 1, 1);
    let f = Function::new("f".into(), Type::Int, sp.clone());
    inst_to_expr_resolved(kind, &HashMap::new(), &sp, &f, dest_ty)
}

#[test]
fn float_negation_keeps_double_and_int_keeps_int() {
    let k = un(crate::ast::UnOp::Neg, Type::Double);
    assert!(
        matches!(&rebuilt(&k, Some(&Type::Double)), Expr::Unary { ty, .. } if matches!(ty, Type::Double)),
        "double 取负的结果类型不得退化成 Int：{:?}",
        rebuilt(&k, Some(&Type::Double))
    );
    let ki = un(crate::ast::UnOp::Neg, Type::Int);
    assert!(
        matches!(&rebuilt(&ki, Some(&Type::Int)), Expr::Unary { ty, .. } if matches!(ty, Type::Int)),
        "整数取负仍得 Int"
    );
    // `!x` 在 C 里结果永远是 int，不得因为操作数是 double 就把结果定成 double。
    let kn = un(crate::ast::UnOp::Not, Type::Double);
    assert!(
        matches!(&rebuilt(&kn, Some(&Type::Int)), Expr::Unary { ty, .. } if matches!(ty, Type::Int)),
        "逻辑非必须回到 int：{:?}",
        rebuilt(&kn, Some(&Type::Int))
    );
    // dest 缺失时按操作数类型推（不能默认成 Int，否则浮点又丢一次）。
    assert!(
        matches!(&rebuilt(&k, None), Expr::Unary { ty, .. } if matches!(ty, Type::Double)),
        "无 dest 时按操作数类型推"
    );
}
