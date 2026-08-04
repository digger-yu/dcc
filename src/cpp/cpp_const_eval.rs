//! M17.0 辅助: 编译期常量求值 (用于 `if constexpr` 分支判定)
//!
//! 不同于 `crate::const_eval` 处理 C `Expr`, 这里处理 C++ `CppExpr`.
//! 仅支持最常用的字面量 / 二元运算 / 一元运算 / Ident 查局部 const,
//! 不足以判定时返回 None, 调用方回退到"两个分支都保留"或按需报错.
//!
//! 设计原则: 简化而非完美. 大多数 if constexpr 条件的语法子集已足够
//! 真实用例 (enable_if 风格, SFINAE 中的 trait 检测, 平台分支).

use std::collections::HashMap;

use crate::cpp::parser::{BinOpKind, CppExpr, UnaryOpKind};

/// 求值 CppExpr 到 i128. 失败返回 None.
pub fn eval_cpp_const(e: &CppExpr, locals: &HashMap<String, i128>) -> Option<i128> {
    match e {
        CppExpr::IntLit(v) => Some(*v as i128),
        CppExpr::Ident(name) => locals.get(name).copied(),
        CppExpr::UnaryOp(op, inner) => {
            let v = eval_cpp_const(inner, locals)?;
            let res = match op {
                UnaryOpKind::Neg => v.wrapping_neg(),
                UnaryOpKind::Plus => v,
                UnaryOpKind::LNot => {
                    if v == 0 {
                        1
                    } else {
                        0
                    }
                }
                UnaryOpKind::Not => !v,
                _ => return None,
            };
            Some(res)
        }
        CppExpr::BinOp(op, lhs, rhs) => {
            let l = eval_cpp_const(lhs, locals)?;
            let r = eval_cpp_const(rhs, locals)?;
            let res = match op {
                BinOpKind::Add => l + r,
                BinOpKind::Sub => l - r,
                BinOpKind::Mul => l * r,
                BinOpKind::Div => {
                    if r != 0 {
                        l / r
                    } else {
                        return None;
                    }
                }
                BinOpKind::Mod => {
                    if r != 0 {
                        l % r
                    } else {
                        return None;
                    }
                }
                BinOpKind::And => l & r,
                BinOpKind::Or => l | r,
                BinOpKind::Xor => l ^ r,
                BinOpKind::Shl => l << r,
                BinOpKind::Shr => l >> r,
                BinOpKind::EqEq => {
                    if l == r {
                        1
                    } else {
                        0
                    }
                }
                BinOpKind::Ne => {
                    if l != r {
                        1
                    } else {
                        0
                    }
                }
                BinOpKind::Lt => {
                    if l < r {
                        1
                    } else {
                        0
                    }
                }
                BinOpKind::Le => {
                    if l <= r {
                        1
                    } else {
                        0
                    }
                }
                BinOpKind::Gt => {
                    if l > r {
                        1
                    } else {
                        0
                    }
                }
                BinOpKind::Ge => {
                    if l >= r {
                        1
                    } else {
                        0
                    }
                }
                BinOpKind::LAnd => {
                    if l != 0 && r != 0 {
                        1
                    } else {
                        0
                    }
                }
                BinOpKind::LOr => {
                    if l != 0 || r != 0 {
                        1
                    } else {
                        0
                    }
                }
                _ => return None,
            };
            Some(res)
        }
        // sizeof(T) 在 C++ 是 constexpr. 简化: 已知类型映射到字节数
        CppExpr::Call(f, args) => {
            if let CppExpr::Ident(name) = &**f {
                if name == "sizeof" && args.len() == 1 {
                    if let CppExpr::Ident(t) = &args[0] {
                        return match t.as_str() {
                            "char" | "bool" | "_Bool" => Some(1),
                            "short" => Some(2),
                            "int" | "long" | "float" | "void*" => Some(4),
                            "long long" | "double" | "void**" => Some(8),
                            _ => None,
                        };
                    }
                }
            }
            None
        }
        // 其它表达式: 不展开
        _ => None,
    }
}
