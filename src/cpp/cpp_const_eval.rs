//! M17.0 辅助: 编译期常量求值 (用于 `if constexpr` 分支判定)
//!
//! 不同于 `crate::const_eval` 处理 C `Expr`, 这里处理 C++ `CppExpr`.
//! 仅支持最常用的字面量 / 二元运算 / 一元运算 / Ident 查局部 const,
//! 不足以判定时返回 None, 调用方回退到"两个分支都保留"或按需报错.
//!
//! 设计原则: 简化而非完美. 大多数 if constexpr 条件的语法子集已足够
//! 真实用例 (enable_if 风格, SFINAE 中的 trait 检测, 平台分支).

use std::collections::HashMap;

use crate::cpp::parser::{BinOpKind, CppExpr, SizeofArg, UnaryOpKind};

/// 求值 CppExpr 到 i128. 失败返回 None.
pub fn eval_cpp_const(e: &CppExpr, locals: &HashMap<String, i128>) -> Option<i128> {
    match e {
        CppExpr::IntLit(v) => Some(*v as i128),
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
                            "int" | "float" | "void*" => Some(4),
                            "long" => {
                                // long 在 64-bit Linux 是 8, 在 Windows 是 4
                                Some(8)
                            }
                            "long long" | "double" | "void**" => Some(8),
                            _ => None,
                        };
                    }
                }
            }
            None
        }
        // sizeof(T) / sizeof expr - 独立语法节点
        CppExpr::Sizeof(arg) => {
            match arg {
                SizeofArg::Expr(_inner) => {
                    // sizeof(expr): 如果内部可求值, 只能得到值的类型大小
                    // 简化: 返回 None, 让调用方处理
                    None
                }
                SizeofArg::Type(_) => {
                    // sizeof(Type): 需要类型信息, 简化返回 None
                    None
                }
            }
        }
        // noexcept(expr) - 编译期始终为 true (1)
        CppExpr::Noexcept(_) => Some(1),
        // 三元运算符: 求值条件后选分支
        CppExpr::Ternary { cond, then, els } => {
            let c = eval_cpp_const(cond, locals)?;
            if c != 0 {
                eval_cpp_const(then, locals)
            } else {
                eval_cpp_const(els, locals)
            }
        }
        // 转型: 如果内部可求值, 尝试按目标类型转换
        CppExpr::Cast(_, inner) | CppExpr::StaticCast { expr: inner, .. } => {
            eval_cpp_const(inner, locals)
        }
        // true/false 标识符
        CppExpr::Ident(name) => {
            match name.as_str() {
                "true" => Some(1),
                "false" => Some(0),
                "nullptr" => Some(0),
                _ => locals.get(name).copied(),
            }
        }
        // 其它表达式：不展开
        _ => None,
    }
}

/// 检查 static_assert 条件，如果为 false 则返回 Some("error message")
pub fn check_static_assert(condition: &CppExpr, locals: &HashMap<String, i128>) -> Option<String> {
    match eval_cpp_const(condition, locals) {
        Some(0) => Some("static assertion failed".to_string()),
        Some(_) => None, // condition is true, OK
        None => None,   // cannot evaluate, defer to semantic analysis
    }
}
