//! 可复用的整数常量表达式求值。
//!
//! parser、typed IR 验证和后端必须共享同一套规则，避免某个阶段把
//! “无法求值”误当成数值 0。

use crate::ast::{find_field, BinOp, Expr, Type, UnOp};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConstEvalError {
    NotConstant,
    DivisionByZero,
    InvalidShift(i128),
    Overflow,
}

pub type ConstEvalResult = Result<i128, ConstEvalError>;

pub fn eval(expr: &Expr) -> ConstEvalResult {
    match expr {
        Expr::Int { v, .. } => Ok(*v),
        Expr::Char { v, .. } => Ok(*v as i128),
        Expr::SizeofTy { ty, .. } => Ok(ty.size() as i128),
        Expr::SizeofExpr { e, .. } => Ok(e.ty().size() as i128),
        Expr::AlignofTy { ty, .. } => Ok(ty.align() as i128),
        Expr::AlignofExpr { e, .. } => Ok(e.ty().align() as i128),
        Expr::Unary { op, e, .. } => {
            let v = eval(e)?;
            match op {
                UnOp::Neg => v.checked_neg().ok_or(ConstEvalError::Overflow),
                UnOp::Not => Ok((v == 0) as i128),
                UnOp::BitNot => Ok(!v),
                UnOp::Plus => Ok(v),
            }
        }
        Expr::Binop { op, l, r, .. } => {
            if matches!(op, BinOp::AndAnd) {
                let lv = eval(l)?;
                return if lv == 0 { Ok(0) } else { Ok((eval(r)? != 0) as i128) };
            }
            if matches!(op, BinOp::OrOr) {
                let lv = eval(l)?;
                return if lv != 0 { Ok(1) } else { Ok((eval(r)? != 0) as i128) };
            }
            let lv = eval(l)?;
            let rv = eval(r)?;
            match op {
                BinOp::Add => Ok(lv.wrapping_add(rv)),
                BinOp::Sub => Ok(lv.wrapping_sub(rv)),
                BinOp::Mul => Ok(lv.wrapping_mul(rv)),
                BinOp::Div => {
                    if rv == 0 { Err(ConstEvalError::DivisionByZero) }
                    else { lv.checked_div(rv).ok_or(ConstEvalError::Overflow) }
                }
                BinOp::Mod => {
                    if rv == 0 { Err(ConstEvalError::DivisionByZero) }
                    else { lv.checked_rem(rv).ok_or(ConstEvalError::Overflow) }
                }
                BinOp::And => Ok(lv & rv),
                BinOp::Or => Ok(lv | rv),
                BinOp::Xor => Ok(lv ^ rv),
                BinOp::Shl | BinOp::Shr if !(0..128).contains(&rv) => {
                    Err(ConstEvalError::InvalidShift(rv))
                }
                BinOp::Shl => Ok(lv.wrapping_shl(rv as u32)),
                BinOp::Shr => Ok(lv.wrapping_shr(rv as u32)),
                BinOp::Lt => Ok((lv < rv) as i128),
                BinOp::Gt => Ok((lv > rv) as i128),
                BinOp::Le => Ok((lv <= rv) as i128),
                BinOp::Ge => Ok((lv >= rv) as i128),
                BinOp::EqEq => Ok((lv == rv) as i128),
                BinOp::Ne => Ok((lv != rv) as i128),
                BinOp::AndAnd | BinOp::OrOr => unreachable!(),
            }
        }
        Expr::Cast { e, .. } => eval(e),
        Expr::Cond { cond, then_, else_, .. } => {
            if eval(cond)? != 0 { eval(then_) } else { eval(else_) }
        }
        Expr::Comma { l, r, .. } => {
            eval(l)?;
            eval(r)
        }
        Expr::Offsetof { ty, member, .. } => {
            Ok(offsetof_member(ty, member) as i128)
        }
        _ => Err(ConstEvalError::NotConstant),
    }
}

/// __builtin_offsetof(type, member) 编译期偏移
fn offsetof_member(ty: &Type, member: &str) -> usize {
    match ty {
        Type::Union(_, _) => 0,
        Type::Struct(_, fields) => {
            let parts: Vec<&str> = member.split('.').collect();
            let mut offset = 0usize;
            let mut current_fields = fields.as_ref();
            for (i, part) in parts.iter().enumerate() {
                let name = if part.contains('[') {
                    &part[..part.find('[').unwrap()]
                } else {
                    part
                };
                if let Some((field, inner_off)) = find_field(current_fields, name) {
                    offset += field.offset + inner_off;
                    if i < parts.len() - 1 {
                        if let Type::Struct(_, sub) | Type::Union(_, sub) = &field.ty {
                            current_fields = sub.as_ref();
                        }
                    }
                } else {
                    break;
                }
            }
            offset
        }
        _ => 0,
    }
}

pub fn is_constant(expr: &Expr) -> bool {
    eval(expr).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Type;
    use crate::diagnostics::Span;

    fn int(v: i128) -> Expr {
        Expr::Int { v, span: Span::point("test.c", 1, 1), ty: Type::Int128 }
    }

    fn bin(op: BinOp, l: i128, r: i128) -> Expr {
        Expr::Binop {
            op,
            l: Box::new(int(l)),
            r: Box::new(int(r)),
            span: Span::point("test.c", 1, 1),
            ty: Type::Int128,
        }
    }

    #[test]
    fn evaluates_full_width_mul_div_mod() {
        assert_eq!(eval(&bin(BinOp::Mul, 1i128 << 80, 17)), Ok((1i128 << 80) * 17));
        assert_eq!(eval(&bin(BinOp::Div, (1i128 << 100) + 9, 3)), Ok(((1i128 << 100) + 9) / 3));
        assert_eq!(eval(&bin(BinOp::Mod, (1i128 << 100) + 9, 7)), Ok(((1i128 << 100) + 9) % 7));
    }

    #[test]
    fn division_by_zero_is_not_zero() {
        assert_eq!(eval(&bin(BinOp::Div, 4, 0)), Err(ConstEvalError::DivisionByZero));
    }
}
