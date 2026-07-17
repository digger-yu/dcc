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

/// 编译期 `__builtin_object_size(ptr, type)` 求值。
/// 未知时返回 `None`，调用方应回退为 `(size_t)-1`。
pub fn object_size(ptr: &Expr, type_arg: i128) -> Option<i128> {
    let mode = type_arg.clamp(0, 3);
    let base = match ptr {
        Expr::AddrOf { e, .. } | Expr::Cast { e, .. } => e.as_ref(),
        other => other,
    };
    object_size_of(base, mode)
}

fn object_size_of(expr: &Expr, mode: i128) -> Option<i128> {
    match expr {
        Expr::Ident { ty, .. } => Some(ty.size() as i128),
        Expr::Index { arr, idx, ty: elem_ty, .. } => {
            let parent = object_size_of(arr, mode)?;
            if let Ok(i) = eval(idx) {
                let off = i * elem_ty.size() as i128;
                let remain = parent.saturating_sub(off);
                Some(remain.max(elem_ty.size() as i128))
            } else if mode == 1 || mode == 3 {
                None
            } else {
                Some(parent)
            }
        }
        Expr::Member { expr, field, is_arrow, .. } => {
            let lookup = |container: i128, fields: &[crate::ast::Field]| -> Option<i128> {
                let (f, extra) = find_field(fields, &field.name)?;
                let field_sz = f.ty.size() as i128;
                Some(if mode == 1 || mode == 3 {
                    field_sz
                } else {
                    let off = (f.offset + extra) as i128;
                    container.saturating_sub(off).max(field_sz)
                })
            };
            match (*is_arrow, expr.ty()) {
                (true, Type::Ptr(inner)) => match inner.as_ref() {
                    Type::Struct(_, f) | Type::Union(_, f) => {
                        lookup(inner.size() as i128, f.as_ref())
                    }
                    _ => None,
                },
                (false, Type::Struct(_, f) | Type::Union(_, f)) => {
                    lookup(expr.ty().size() as i128, f.as_ref())
                }
                _ => None,
            }
        }
        Expr::Deref { e, .. } => object_size_of(e, mode),
        _ => None,
    }
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

    #[test]
    fn object_size_of_local_int() {
        let x = Expr::Ident {
            name: crate::ast::Ident { name: "x".into(), span: Span::point("t.c", 1, 1) },
            span: Span::point("t.c", 1, 1),
            ty: Type::Int,
        };
        let addr = Expr::AddrOf {
            e: Box::new(x),
            span: Span::point("t.c", 1, 1),
            ty: Type::Ptr(Box::new(Type::Int)),
        };
        assert_eq!(object_size(&addr, 0), Some(4));
    }
}
