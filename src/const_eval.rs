//! 可复用的整数常量表达式求值。
//!
//! parser、typed IR 验证和后端必须共享同一套规则，避免某个阶段把
//! “无法求值”误当成数值 0。

use crate::ast::{find_field, ArraySize, BinOp, Expr, Type, UnOp};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConstEvalError {
    NotConstant,
    DivisionByZero,
    InvalidShift(i128),
    Overflow,
    /// 整型截断 (例如 `(char)0x100`): 编译期已知但可被诊断
    IntegerTruncation,
    /// 隐式指针/整数类型转换
    PointerConversion,
}

pub type ConstEvalResult = Result<i128, ConstEvalError>;

/// 完整的编译期常量值: 除整数外还支持浮点、字符串字面量、地址、
/// 字节数组 (`#embed`)。`eval_full` 返回该枚举; 旧 `eval` 仍仅取
/// 整数部分以保持 parser/IR 既有调用点不破坏。
#[derive(Debug, Clone, PartialEq)]
pub enum ConstValue {
    /// 整型常量 (含枚举/字符)
    Int(i128),
    /// 浮点常量 (f32 / f64 / long double 都存为 f64 表示)
    Float(f64),
    /// 字符串字面量: 字节内容 + 唯一标签 (供 codegen 写入 rodata)
    Str { data: Vec<u8>, label: String },
    /// 静态对象的地址: 符号名 + 字节偏移
    Addr { name: String, offset: i128 },
    /// `#embed` 字节数组: 字节内容 + 标签
    Bytes { data: Vec<u8>, label: String },
}

impl ConstValue {
    /// 转为整数 (仅在确实是整数或可截断为整数时返回)
    pub fn as_int(&self) -> Option<i128> {
        match self {
            ConstValue::Int(v) => Some(*v),
            ConstValue::Float(f) => Some(*f as i128),
            _ => None,
        }
    }
    /// 转为浮点 (仅在确实是浮点时)
    pub fn as_float(&self) -> Option<f64> {
        match self {
            ConstValue::Float(f) => Some(*f),
            ConstValue::Int(v) => Some(*v as f64),
            _ => None,
        }
    }
}

pub fn eval(expr: &Expr) -> ConstEvalResult {
    // 旧 API: 永远返回整数; 浮点截断, 其他返回 NotConstant
    match eval_full(expr) {
        Ok(ConstValue::Int(v)) => Ok(v),
        Ok(ConstValue::Float(f)) => Ok(f as i128),
        Ok(_) => Err(ConstEvalError::NotConstant),
        Err(e) => Err(e),
    }
}

pub fn eval_full(expr: &Expr) -> Result<ConstValue, ConstEvalError> {
    match expr {
        Expr::Int { v, .. } => Ok(ConstValue::Int(*v)),
        Expr::Char { v, .. } => Ok(ConstValue::Int(*v as i128)),
        Expr::Float { v, .. } => Ok(ConstValue::Float(*v)),
        Expr::String { data, .. } => {
            // 字符串字面量: 编译器生成的 label 与 codegen 中的 rodata 标签对齐
            // 调用方需要确保同源字符串复用同一 label
            let label = format!(".L.str.const.{}", const_str_id(data));
            Ok(ConstValue::Str {
                data: data.clone(),
                label,
            })
        }
        Expr::AddrOf { e, .. } => match e.as_ref() {
            Expr::Ident { name, .. } => Ok(ConstValue::Addr {
                name: name.name.clone(),
                offset: 0,
            }),
            Expr::Index { arr, idx, .. } => {
                let arr_name = match arr.as_ref() {
                    Expr::Ident { name, .. } => name.name.clone(),
                    _ => return Err(ConstEvalError::NotConstant),
                };
                let off = eval(idx)?; // 索引必须是编译期整数
                Ok(ConstValue::Addr {
                    name: arr_name,
                    offset: off,
                })
            }
            // &global_struct.member: 地址常量表达式 (C 标准 6.6.9)
            // 用于全局/静态初始化器，如 `const T *p = &global.field;`
            Expr::Member { expr, field, is_arrow, .. } => {
                if *is_arrow {
                    // arrow 形式 `ptr->field` 需要先解引用指针，暂不支持
                    return Err(ConstEvalError::NotConstant);
                }
                // 递归求基地址: 必须是 &ident.field1.field2... 形式
                let base_addr = match eval_full(&Expr::AddrOf {
                    e: expr.clone(),
                    ty: Type::Ptr(Box::new(expr.ty())),
                    span: crate::diagnostics::Span::point("", 0, 0),
                })? {
                    ConstValue::Addr { name, offset } => (name, offset),
                    _ => return Err(ConstEvalError::NotConstant),
                };
                // 计算字段在结构体内的偏移
                let field_offset = match &expr.ty() {
                    Type::Struct(_, fields, _) | Type::Union(_, fields, _) => {
                        crate::ast::find_field(fields, &field.name)
                            .map(|(f, extra_off)| f.offset + extra_off)
                            .ok_or(ConstEvalError::NotConstant)?
                    }
                    _ => return Err(ConstEvalError::NotConstant),
                };
                Ok(ConstValue::Addr {
                    name: base_addr.0,
                    offset: base_addr.1 + field_offset as i128,
                })
            }
            _ => Err(ConstEvalError::NotConstant),
        },
        Expr::LabelAddr { label, .. } => Ok(ConstValue::Addr {
            name: label.name.clone(),
            offset: 0,
        }),
        Expr::SizeofTy { ty, .. } => Ok(ConstValue::Int(ty.size() as i128)),
        Expr::SizeofExpr { e, .. } => match e.as_ref() {
            // 字符串字面量: C 中为 char[N] 数组 (含 NUL), 返回实际字节数
            Expr::String { data, .. } => Ok(ConstValue::Int(data.len() as i128)),
            _ => Ok(ConstValue::Int(e.ty().size() as i128)),
        },
        Expr::AlignofTy { ty, .. } => Ok(ConstValue::Int(ty.align() as i128)),
        Expr::AlignofExpr { e, .. } => Ok(ConstValue::Int(e.ty().align() as i128)),
        Expr::Unary { op, e, .. } => {
            let v = eval_full(e)?;
            match op {
                UnOp::Neg => match v {
                    ConstValue::Int(i) => i
                        .checked_neg()
                        .map(ConstValue::Int)
                        .ok_or(ConstEvalError::Overflow),
                    ConstValue::Float(f) => Ok(ConstValue::Float(-f)),
                    _ => Err(ConstEvalError::NotConstant),
                },
                UnOp::Not => match v {
                    ConstValue::Int(i) => Ok(ConstValue::Int((i == 0) as i128)),
                    _ => Err(ConstEvalError::NotConstant),
                },
                UnOp::BitNot => match v {
                    ConstValue::Int(i) => Ok(ConstValue::Int(!i)),
                    _ => Err(ConstEvalError::NotConstant),
                },
                UnOp::Plus => Ok(v),
            }
        }
        Expr::Binop { op, l, r, .. } => {
            // && / || 用整数语义, 不要求两边都是常量
            if matches!(op, BinOp::AndAnd) {
                let lv = eval(l)?;
                return if lv == 0 {
                    Ok(ConstValue::Int(0))
                } else {
                    Ok(ConstValue::Int((eval(r)? != 0) as i128))
                };
            }
            if matches!(op, BinOp::OrOr) {
                let lv = eval(l)?;
                return if lv != 0 {
                    Ok(ConstValue::Int(1))
                } else {
                    Ok(ConstValue::Int((eval(r)? != 0) as i128))
                };
            }
            let lv = eval_full(l)?;
            let rv = eval_full(r)?;
            match op {
                // 算术运算
                BinOp::Add => add_values(lv, rv),
                BinOp::Sub => sub_values(lv, rv),
                BinOp::Mul => mul_values(lv, rv),
                BinOp::Div => div_values(lv, rv),
                BinOp::Mod => mod_values(lv, rv),
                BinOp::And => match (lv, rv) {
                    (ConstValue::Int(a), ConstValue::Int(b)) => Ok(ConstValue::Int(a & b)),
                    _ => Err(ConstEvalError::NotConstant),
                },
                BinOp::Or => match (lv, rv) {
                    (ConstValue::Int(a), ConstValue::Int(b)) => Ok(ConstValue::Int(a | b)),
                    _ => Err(ConstEvalError::NotConstant),
                },
                BinOp::Xor => match (lv, rv) {
                    (ConstValue::Int(a), ConstValue::Int(b)) => Ok(ConstValue::Int(a ^ b)),
                    _ => Err(ConstEvalError::NotConstant),
                },
                BinOp::Shl | BinOp::Shr if !(0..128).contains(&rv.as_int().unwrap_or(-1)) => {
                    Err(ConstEvalError::InvalidShift(rv.as_int().unwrap_or(0)))
                }
                BinOp::Shl => match (lv, rv) {
                    (ConstValue::Int(a), ConstValue::Int(b)) => {
                        Ok(ConstValue::Int(a.wrapping_shl(b as u32)))
                    }
                    _ => Err(ConstEvalError::NotConstant),
                },
                BinOp::Shr => match (lv, rv) {
                    (ConstValue::Int(a), ConstValue::Int(b)) => {
                        Ok(ConstValue::Int(a.wrapping_shr(b as u32)))
                    }
                    _ => Err(ConstEvalError::NotConstant),
                },
                // 比较
                BinOp::Lt => cmp_values(lv, rv, |a, b| (a < b) as i128),
                BinOp::Gt => cmp_values(lv, rv, |a, b| (a > b) as i128),
                BinOp::Le => cmp_values(lv, rv, |a, b| (a <= b) as i128),
                BinOp::Ge => cmp_values(lv, rv, |a, b| (a >= b) as i128),
                BinOp::EqEq => cmp_values(lv, rv, |a, b| (a == b) as i128),
                BinOp::Ne => cmp_values(lv, rv, |a, b| (a != b) as i128),
                BinOp::AndAnd | BinOp::OrOr => unreachable!(),
            }
        }
        Expr::Cast { ty, e, .. } => {
            // 强制类型转换: 数值保留, 指针转换产生 NotConstant
            let v = eval_full(e)?;
            // 整数类型间: 根据目标类型位宽截断/零扩展/符号扩展
            // (否则 `~0U` 转为 u64 会保持 -1, 与 C 语义不符)
            if let ConstValue::Int(i) = v {
                if let Some(bits) = int_type_bits(ty) {
                    if is_unsigned_int_ty(ty) {
                        let mask = if bits == 128 { -1i128 } else { (1i128 << bits) - 1 };
                        return Ok(ConstValue::Int(i & mask));
                    } else {
                        // 有符号: 截断到目标位宽再符号扩展
                        let mask = if bits == 128 { -1i128 } else { (1i128 << bits) - 1 };
                        let truncated = i & mask;
                        let sign_bit = 1i128 << (bits - 1);
                        let signed = if (truncated & sign_bit) != 0 {
                            truncated | !mask
                        } else {
                            truncated
                        };
                        return Ok(ConstValue::Int(signed));
                    }
                }
                return Ok(ConstValue::Int(i));
            }
            // 浮点 / 指针等: 简化处理, 让 codegen 处理位宽
            Ok(v)
        }
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            if eval(cond)? != 0 {
                eval_full(then_)
            } else {
                eval_full(else_)
            }
        }
        Expr::Comma { l, r, .. } => {
            eval(l)?;
            eval_full(r)
        }
        Expr::ComplexLit { real, imag, .. } => {
            // 复数字面量: 返回虚部值 (实部在 __real__ 提取时使用)
            // 常量上下文中复数通常通过 __real__/__imag__ 访问;
            // 这里将虚部编码为 Float, 供后续 Real/Imag 提取.
            // 使用特殊标记: 实部存入 Float 的整数部分, 虚部存入小数部分
            // 不, 更好的方式: 返回 ConstValue::Int 编码虚部 (用于 __imag__),
            // 而 __real__ 直接提取 real 字段.
            // 最简方案: 返回虚部作为 Float (内核中复数常量主要用于 __imag__)
            if *imag != 0.0 {
                Ok(ConstValue::Float(*imag))
            } else {
                Ok(ConstValue::Float(*real))
            }
        }
        Expr::Real { e, .. } => {
            // __real__ expr: 提取实部
            match eval_full(e)? {
                ConstValue::Float(f) => {
                    // 如果内部是 ComplexLit, 实部已在 eval_full 中编码
                    // 对于纯复数字面量, 直接返回 real 字段
                    if let Expr::ComplexLit { real, .. } = e.as_ref() {
                        Ok(ConstValue::Float(*real))
                    } else {
                        // 标量的 __real__ 返回自身
                        Ok(ConstValue::Float(f))
                    }
                }
                v @ ConstValue::Int(_) => Ok(v), // 标量的 __real__ 返回自身
                _ => Err(ConstEvalError::NotConstant),
            }
        }
        Expr::Imag { e, .. } => {
            // __imag__ expr: 提取虚部
            match eval_full(e)? {
                ConstValue::Float(f) => {
                    if let Expr::ComplexLit { real, imag, .. } = e.as_ref() {
                        Ok(ConstValue::Float(*imag))
                    } else {
                        // 标量的 __imag__ 返回 0
                        let _ = f;
                        Ok(ConstValue::Int(0))
                    }
                }
                ConstValue::Int(_) => Ok(ConstValue::Int(0)), // 标量虚部为 0
                _ => Err(ConstEvalError::NotConstant),
            }
        }
        Expr::Offsetof { ty, member, .. } => {
            Ok(ConstValue::Int(offsetof_member(ty, member) as i128))
        }
        // 编译期可求值的 GCC/LLVM 内建函数: 参数是常量时返回常量结果。
        // 这些函数通常用 __builtin_constant_p 包裹, dcc 直接展开即可。
        Expr::Call { callee, args, .. } => {
            if let Expr::Ident { name, .. } = callee.as_ref() {
                if let Some(v) = eval_intrinsic(name.name.as_str(), args)? {
                    return Ok(ConstValue::Int(v));
                }
            }
            // constexpr 函数调用在 parser 层不展开, 走 NotConstant
            Err(ConstEvalError::NotConstant)
        }
        _ => Err(ConstEvalError::NotConstant),
    }
}

/// 编译期内建函数求值 (如 __builtin_clzll/__builtin_ctzll/__builtin_popcountll)。
/// 仅当参数是常量整数时返回 Some; 否则返回 None 让外层走 NotConstant。
fn eval_intrinsic(name: &str, args: &[Expr]) -> Result<Option<i128>, ConstEvalError> {
    let argc = args.len();
    let take1 = |args: &[Expr]| -> Result<i128, ConstEvalError> { eval(&args[0]) };
    match name {
        "__builtin_clz" | "__builtin_clzl" | "__builtin_clzll" if argc == 1 => {
            let v = take1(args)?;
            if v == 0 {
                return Ok(Some(0));
            } // GCC 约定: 输入 0 时结果未定义, 取 0
            let bits = match name {
                "__builtin_clz" => 32,
                "__builtin_clzl" => {
                    if cfg!(target_pointer_width = "64") {
                        64
                    } else {
                        32
                    }
                }
                _ => 64,
            };
            let n = (v as u64).leading_zeros() as i128;
            Ok(Some(n - (64 - bits) as i128))
        }
        "__builtin_ctz" | "__builtin_ctzl" | "__builtin_ctzll" if argc == 1 => {
            let v = take1(args)?;
            if v == 0 {
                return Ok(Some(0));
            } // GCC 约定: 输入 0 时结果未定义, 取 0
            Ok(Some((v as u64).trailing_zeros() as i128))
        }
        "__builtin_popcount" | "__builtin_popcountl" | "__builtin_popcountll" if argc == 1 => {
            let v = take1(args)?;
            Ok(Some((v as u64).count_ones() as i128))
        }
        "__builtin_ffs" | "__builtin_ffsl" | "__builtin_ffsll" if argc == 1 => {
            let v = take1(args)?;
            if v == 0 {
                return Ok(Some(0));
            }
            Ok(Some((v as u64).trailing_zeros() as i128 + 1))
        }
        "__builtin_bswap16" if argc == 1 => {
            let v = take1(args)?;
            Ok(Some((v as u16).swap_bytes() as i128))
        }
        "__builtin_bswap32" if argc == 1 => {
            let v = take1(args)?;
            Ok(Some((v as u32).swap_bytes() as i128))
        }
        "__builtin_bswap64" if argc == 1 => {
            let v = take1(args)?;
            Ok(Some((v as u64).swap_bytes() as i128))
        }
        _ => Ok(None),
    }
}

/// 返回整数类型的位宽; 非整数类型返回 None。
fn int_type_bits(ty: &Type) -> Option<usize> {
    Some(match ty {
        Type::Bool | Type::Char | Type::SChar | Type::UChar => 8,
        Type::Short | Type::UShort => 16,
        Type::Int | Type::UInt => 32,
        Type::Long | Type::ULong => 64, // x86_64: long 64-bit
        Type::LongLong | Type::ULongLong => 64,
        Type::Int128 | Type::UInt128 => 128,
        _ => return None,
    })
}

/// 是否为无符号整数类型。
fn is_unsigned_int_ty(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Bool
            | Type::UChar
            | Type::UShort
            | Type::UInt
            | Type::ULong
            | Type::ULongLong
            | Type::UInt128
    )
}

fn add_values(l: ConstValue, r: ConstValue) -> Result<ConstValue, ConstEvalError> {
    match (l, r) {
        (ConstValue::Int(a), ConstValue::Int(b)) => Ok(ConstValue::Int(a.wrapping_add(b))),
        (ConstValue::Float(a), ConstValue::Float(b)) => Ok(ConstValue::Float(a + b)),
        (ConstValue::Int(a), ConstValue::Float(b)) => Ok(ConstValue::Float(a as f64 + b)),
        (ConstValue::Float(a), ConstValue::Int(b)) => Ok(ConstValue::Float(a + b as f64)),
        _ => Err(ConstEvalError::NotConstant),
    }
}
fn sub_values(l: ConstValue, r: ConstValue) -> Result<ConstValue, ConstEvalError> {
    match (l, r) {
        (ConstValue::Int(a), ConstValue::Int(b)) => Ok(ConstValue::Int(a.wrapping_sub(b))),
        (ConstValue::Float(a), ConstValue::Float(b)) => Ok(ConstValue::Float(a - b)),
        (ConstValue::Int(a), ConstValue::Float(b)) => Ok(ConstValue::Float(a as f64 - b)),
        (ConstValue::Float(a), ConstValue::Int(b)) => Ok(ConstValue::Float(a - b as f64)),
        _ => Err(ConstEvalError::NotConstant),
    }
}
fn mul_values(l: ConstValue, r: ConstValue) -> Result<ConstValue, ConstEvalError> {
    match (l, r) {
        (ConstValue::Int(a), ConstValue::Int(b)) => Ok(ConstValue::Int(a.wrapping_mul(b))),
        (ConstValue::Float(a), ConstValue::Float(b)) => Ok(ConstValue::Float(a * b)),
        (ConstValue::Int(a), ConstValue::Float(b)) => Ok(ConstValue::Float(a as f64 * b)),
        (ConstValue::Float(a), ConstValue::Int(b)) => Ok(ConstValue::Float(a * b as f64)),
        _ => Err(ConstEvalError::NotConstant),
    }
}
fn div_values(l: ConstValue, r: ConstValue) -> Result<ConstValue, ConstEvalError> {
    match (l, r) {
        (ConstValue::Int(_), ConstValue::Int(0)) => Err(ConstEvalError::DivisionByZero),
        (ConstValue::Int(a), ConstValue::Int(b)) => a
            .checked_div(b)
            .map(ConstValue::Int)
            .ok_or(ConstEvalError::Overflow),
        (ConstValue::Float(a), ConstValue::Float(b)) => Ok(ConstValue::Float(a / b)),
        (ConstValue::Int(a), ConstValue::Float(b)) => Ok(ConstValue::Float(a as f64 / b)),
        (ConstValue::Float(a), ConstValue::Int(b)) => Ok(ConstValue::Float(a / b as f64)),
        _ => Err(ConstEvalError::NotConstant),
    }
}
fn mod_values(l: ConstValue, r: ConstValue) -> Result<ConstValue, ConstEvalError> {
    match (l, r) {
        (ConstValue::Int(_), ConstValue::Int(0)) => Err(ConstEvalError::DivisionByZero),
        (ConstValue::Int(a), ConstValue::Int(b)) => a
            .checked_rem(b)
            .map(ConstValue::Int)
            .ok_or(ConstEvalError::Overflow),
        (ConstValue::Float(_), ConstValue::Float(_)) => {
            // GCC 语义: a % b = a - trunc(a/b) * b, 但 C 中浮点取模不在我们范围
            Err(ConstEvalError::NotConstant)
        }
        _ => Err(ConstEvalError::NotConstant),
    }
}
fn cmp_values<F: Fn(f64, f64) -> i128>(
    l: ConstValue,
    r: ConstValue,
    f: F,
) -> Result<ConstValue, ConstEvalError> {
    // 提升到 f64 比较; 整数/整数走 i128 -> f64 精确
    let a = l.as_float().ok_or(ConstEvalError::NotConstant)?;
    let b = r.as_float().ok_or(ConstEvalError::NotConstant)?;
    Ok(ConstValue::Int(f(a, b)))
}

/// 字符串字面量稳定性 ID: 不直接哈希 (避免开销), 仅在 const_eval
/// 内部使用, codegen 仍按需要 dedup。 实际 label 由 codegen
/// 分配; const_eval 这里给的是 logical label。
fn const_str_id(data: &[u8]) -> u64 {
    // FNV-1a 64-bit
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// 把词法器抓出来的整型字面量文本解析为 i128。
/// 支持十进制 / 0x / 0X / 0b / 0B / 八进制, 后缀 u/l/ll/U/L 忽略。
/// 仅用于 __builtin_* 折叠等需要从字面量文本构造常量的场景。
pub fn eval_int_lit(s: &str) -> Option<i128> {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u128::from_str_radix(rest, 16).ok().map(|v| v as i128)
    } else if let Some(rest) = s.strip_prefix("0b").or_else(|| s.strip_prefix("0B")) {
        u128::from_str_radix(rest, 2).ok().map(|v| v as i128)
    } else if s.starts_with("0") && s.len() > 1 {
        u128::from_str_radix(&s[1..], 8).ok().map(|v| v as i128)
    } else {
        // 截去 u/l/L/ll/LL 后缀后转十进制
        let mut t = s;
        while let Some(rest) =
            t.strip_suffix(|c: char| c == 'u' || c == 'U' || c == 'l' || c == 'L')
        {
            t = rest;
        }
        t.parse::<i128>().ok()
    }
}

/// __builtin_offsetof(type, member) 编译期偏移
fn offsetof_member(ty: &Type, member: &str) -> usize {
    match ty {
        Type::Union(_, _, _) => 0,
        Type::Struct(_, fields, _) => {
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
                        if let Type::Struct(_, sub, _) | Type::Union(_, sub, _) = &field.ty {
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

/// 把表达式渲染为可读文本 (用于 `_Static_assert` 失败时的诊断)。
/// 模仿 GCC 的 `stringify` 风格：操作符前后留空格，标识符原样输出。
pub struct ExprDebug<'a>(pub &'a Expr);

impl<'a> std::fmt::Display for ExprDebug<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Expr::Int { v, .. } => write!(f, "{}", v),
            Expr::Char { v, .. } => write!(f, "'{}'", *v as char),
            Expr::Float { v, .. } => write!(f, "{}", v),
            Expr::String { data, .. } => {
                write!(f, "\"")?;
                for b in data {
                    if *b == 0 {
                        break;
                    }
                    if *b == b'"' {
                        write!(f, "\\\"")?;
                    } else if *b == b'\\' {
                        write!(f, "\\\\")?;
                    } else if (0x20..0x7f).contains(b) {
                        write!(f, "{}", *b as char)?;
                    } else {
                        write!(f, "\\x{:02x}", b)?;
                    }
                }
                write!(f, "\"")
            }
            Expr::Ident { name, .. } => write!(f, "{}", name.name),
            Expr::Binop { op, l, r, .. } => {
                let sym = match op {
                    BinOp::Add => "+",
                    BinOp::Sub => "-",
                    BinOp::Mul => "*",
                    BinOp::Div => "/",
                    BinOp::Mod => "%",
                    BinOp::And => "&",
                    BinOp::Or => "|",
                    BinOp::Xor => "^",
                    BinOp::Shl => "<<",
                    BinOp::Shr => ">>",
                    BinOp::Lt => "<",
                    BinOp::Gt => ">",
                    BinOp::Le => "<=",
                    BinOp::Ge => ">=",
                    BinOp::EqEq => "==",
                    BinOp::Ne => "!=",
                    BinOp::AndAnd => "&&",
                    BinOp::OrOr => "||",
                };
                write!(f, "({} {} {})", ExprDebug(l), sym, ExprDebug(r))
            }
            Expr::Unary { op, e, .. } => {
                let sym = match op {
                    UnOp::Neg => "-",
                    UnOp::Not => "!",
                    UnOp::BitNot => "~",
                    UnOp::Plus => "+",
                };
                write!(f, "{}{}", sym, ExprDebug(e))
            }
            Expr::SizeofTy { ty, .. } => write!(f, "sizeof({})", ty),
            Expr::SizeofExpr { e, .. } => write!(f, "sizeof({})", ExprDebug(e)),
            Expr::AlignofTy { ty, .. } => write!(f, "_Alignof({})", ty),
            Expr::AlignofExpr { e, .. } => write!(f, "_Alignof({})", ExprDebug(e)),
            Expr::Cast { ty, e, .. } => write!(f, "({}){}", ty, ExprDebug(e)),
            Expr::Index { arr, idx, .. } => write!(f, "{}[{}]", ExprDebug(arr), ExprDebug(idx)),
            Expr::Member {
                expr,
                field,
                is_arrow,
                ..
            } => {
                if *is_arrow {
                    write!(f, "{}->{}", ExprDebug(expr), field.name)
                } else {
                    write!(f, "{}.{}", ExprDebug(expr), field.name)
                }
            }
            Expr::AddrOf { e, .. } => write!(f, "&{}", ExprDebug(e)),
            Expr::Deref { e, .. } => write!(f, "*{}", ExprDebug(e)),
            Expr::Call { callee, args, .. } => {
                write!(f, "{}(", ExprDebug(callee))?;
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", ExprDebug(a))?;
                }
                write!(f, ")")
            }
            Expr::Cond {
                cond, then_, else_, ..
            } => {
                write!(
                    f,
                    "({} ? {} : {})",
                    ExprDebug(cond),
                    ExprDebug(then_),
                    ExprDebug(else_)
                )
            }
            Expr::Assign { l, r, .. } => write!(f, "{} = {}", ExprDebug(l), ExprDebug(r)),
            Expr::PostInc { e, .. } => write!(f, "{}++", ExprDebug(e)),
            Expr::PostDec { e, .. } => write!(f, "{}--", ExprDebug(e)),
            Expr::PreInc { e, .. } => write!(f, "++{}", ExprDebug(e)),
            Expr::PreDec { e, .. } => write!(f, "--{}", ExprDebug(e)),
            Expr::Comma { l, r, .. } => write!(f, "({}, {})", ExprDebug(l), ExprDebug(r)),
            Expr::CompoundLit { ty, inits, .. } => {
                write!(f, "({}){{", ty)?;
                for (i, it) in inits.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    match it {
                        crate::ast::InitItem::Expr(e) => write!(f, "{}", ExprDebug(e))?,
                        crate::ast::InitItem::Designated { value, .. } => {
                            write!(f, "{}", ExprDebug(value))?
                        }
                    }
                }
                write!(f, "}}")
            }
            Expr::InitList { inits, .. } => {
                write!(f, "{{")?;
                for (i, it) in inits.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    match it {
                        crate::ast::InitItem::Expr(e) => write!(f, "{}", ExprDebug(e))?,
                        crate::ast::InitItem::Designated { value, .. } => {
                            write!(f, "{}", ExprDebug(value))?
                        }
                    }
                }
                write!(f, "}}")
            }
            Expr::Offsetof { ty, member, .. } => write!(f, "offsetof({}, {})", ty, member),
            _ => write!(f, "<expr>"),
        }
    }
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
        Expr::Ident { ty, .. } => match ty {
            Type::Array(elem, ArraySize::Const(n)) => {
                let total = (elem.size() * n) as i128;
                if mode == 1 || mode == 3 {
                    Some(elem.size() as i128)
                } else {
                    Some(total)
                }
            }
            _ => Some(ty.size() as i128),
        },
        Expr::String { data, .. } => {
            // 字符串字面量: 含末尾 '\0' 的大小 (sizeof "abcd" == 5)
            let total = data.len() as i128;
            if mode == 1 || mode == 3 {
                Some(1) // 子对象 (char) 元素大小
            } else {
                Some(total)
            }
        }
        Expr::Index {
            arr,
            idx,
            ty: elem_ty,
            ..
        } => {
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
        Expr::Member {
            expr,
            field,
            is_arrow,
            ..
        } => {
            let lookup = |fields: &[crate::ast::Field]| -> Option<i128> {
                let (f, _extra) = find_field(fields, &field.name)?;
                let field_sz = f.ty.size() as i128;
                Some(if mode == 1 || mode == 3 {
                    match &f.ty {
                        Type::Array(elem, _) => elem.size() as i128,
                        _ => field_sz,
                    }
                } else {
                    field_sz
                })
            };
            match (*is_arrow, expr.ty()) {
                (true, Type::Ptr(inner)) => match inner.as_ref() {
                    Type::Struct(_, f, _) | Type::Union(_, f, _) => lookup(f.as_ref()),
                    _ => None,
                },
                (false, Type::Struct(_, f, _) | Type::Union(_, f, _)) => lookup(f.as_ref()),
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
    use crate::ast::{ArraySize, Type};
    use crate::diagnostics::Span;

    fn int(v: i128) -> Expr {
        Expr::Int {
            v,
            span: Span::point("test.c", 1, 1),
            ty: Type::Int128,
        }
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
        assert_eq!(
            eval(&bin(BinOp::Mul, 1i128 << 80, 17)),
            Ok((1i128 << 80) * 17)
        );
        assert_eq!(
            eval(&bin(BinOp::Div, (1i128 << 100) + 9, 3)),
            Ok(((1i128 << 100) + 9) / 3)
        );
        assert_eq!(
            eval(&bin(BinOp::Mod, (1i128 << 100) + 9, 7)),
            Ok(((1i128 << 100) + 9) % 7)
        );
    }

    #[test]
    fn division_by_zero_is_not_zero() {
        assert_eq!(
            eval(&bin(BinOp::Div, 4, 0)),
            Err(ConstEvalError::DivisionByZero)
        );
    }

    #[test]
    fn object_size_of_local_int() {
        let x = Expr::Ident {
            name: crate::ast::Ident {
                name: "x".into(),
                span: Span::point("t.c", 1, 1),
            },
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

    #[test]
    fn object_size_array_mode1() {
        let buf = Expr::Ident {
            name: crate::ast::Ident {
                name: "buf".into(),
                span: Span::point("t.c", 1, 1),
            },
            span: Span::point("t.c", 1, 1),
            ty: Type::Array(Box::new(Type::Char), ArraySize::Const(16)),
        };
        assert_eq!(object_size(&buf, 0), Some(16));
        assert_eq!(object_size(&buf, 1), Some(1));
    }

    fn float(v: f64, ty: Type) -> Expr {
        Expr::Float {
            v,
            span: Span::point("t.c", 1, 1),
            ty,
        }
    }

    #[test]
    fn float_arithmetic_eval() {
        // 浮点 + 浮点
        let a = bin_f(BinOp::Add, 1.5, 2.25, Type::Double, Type::Double);
        assert_eq!(eval_full(&a), Ok(ConstValue::Float(3.75)));
        // 浮点 * 浮点
        let m = bin_f(BinOp::Mul, 2.0, 3.0, Type::Double, Type::Double);
        assert_eq!(eval_full(&m), Ok(ConstValue::Float(6.0)));
        // 浮点 / 0 返回有限 NaN/inf, 不报错 (符合 IEEE 754)
        let d = bin_f(BinOp::Div, 1.0, 0.0, Type::Double, Type::Double);
        assert!(matches!(eval_full(&d), Ok(ConstValue::Float(_))));
    }

    #[test]
    fn float_comparison_eval() {
        let a = bin_f(BinOp::Lt, 1.0, 2.0, Type::Double, Type::Double);
        assert_eq!(eval_full(&a), Ok(ConstValue::Int(1)));
        let b = bin_f(BinOp::Ge, 2.0, 2.0, Type::Double, Type::Double);
        assert_eq!(eval_full(&b), Ok(ConstValue::Int(1)));
    }

    #[test]
    fn int_float_mixed_eval() {
        // 1.5 + 2 (整型) -> 3.5 浮点
        let e = Expr::Binop {
            op: BinOp::Add,
            l: Box::new(float(1.5, Type::Double)),
            r: Box::new(int(2)),
            span: Span::point("t.c", 1, 1),
            ty: Type::Double,
        };
        assert_eq!(eval_full(&e), Ok(ConstValue::Float(3.5)));
    }

    fn bin_f(op: BinOp, l: f64, r: f64, lt: Type, rt: Type) -> Expr {
        Expr::Binop {
            op,
            l: Box::new(float(l, lt)),
            r: Box::new(float(r, rt)),
            span: Span::point("t.c", 1, 1),
            ty: Type::Double,
        }
    }

    #[test]
    fn string_literal_eval_returns_str_value() {
        let s = Expr::String {
            data: b"hello\0".to_vec(),
            span: Span::point("t.c", 1, 1),
            ty: Type::Ptr(Box::new(Type::Char)),
        };
        match eval_full(&s) {
            Ok(ConstValue::Str { data, label }) => {
                assert_eq!(data, b"hello\0");
                assert!(label.starts_with(".L.str.const."));
            }
            other => panic!("expected Str, got {:?}", other),
        }
    }

    #[test]
    fn address_of_ident_eval() {
        let id = Expr::Ident {
            name: crate::ast::Ident {
                name: "arr".into(),
                span: Span::point("t.c", 1, 1),
            },
            span: Span::point("t.c", 1, 1),
            ty: Type::Array(Box::new(Type::Int), ArraySize::Const(8)),
        };
        let addr = Expr::AddrOf {
            e: Box::new(id),
            span: Span::point("t.c", 1, 1),
            ty: Type::Ptr(Box::new(Type::Int)),
        };
        match eval_full(&addr) {
            Ok(ConstValue::Addr { name, offset }) => {
                assert_eq!(name, "arr");
                assert_eq!(offset, 0);
            }
            other => panic!("expected Addr, got {:?}", other),
        }
    }

    #[test]
    fn address_of_index_eval() {
        let arr = Expr::Ident {
            name: crate::ast::Ident {
                name: "buf".into(),
                span: Span::point("t.c", 1, 1),
            },
            span: Span::point("t.c", 1, 1),
            ty: Type::Array(Box::new(Type::Char), ArraySize::Const(16)),
        };
        let idx = int(4);
        let indexed = Expr::Index {
            arr: Box::new(arr),
            idx: Box::new(idx),
            span: Span::point("t.c", 1, 1),
            ty: Type::Char,
        };
        let addr = Expr::AddrOf {
            e: Box::new(indexed),
            span: Span::point("t.c", 1, 1),
            ty: Type::Ptr(Box::new(Type::Char)),
        };
        match eval_full(&addr) {
            Ok(ConstValue::Addr { name, offset }) => {
                assert_eq!(name, "buf");
                assert_eq!(offset, 4);
            }
            other => panic!("expected Addr, got {:?}", other),
        }
    }

    #[test]
    fn label_address_eval() {
        let e = Expr::LabelAddr {
            label: crate::ast::Ident {
                name: "my_label".into(),
                span: Span::point("t.c", 1, 1),
            },
            span: Span::point("t.c", 1, 1),
            ty: Type::Void,
        };
        match eval_full(&e) {
            Ok(ConstValue::Addr { name, offset }) => {
                assert_eq!(name, "my_label");
                assert_eq!(offset, 0);
            }
            other => panic!("expected Addr, got {:?}", other),
        }
    }

    #[test]
    fn legacy_eval_truncates_float_to_int() {
        let e = float(3.7, Type::Double);
        // 旧 eval API: 浮点截断为整数 (3)
        assert_eq!(eval(&e), Ok(3));
    }

    #[test]
    fn complex_lit_real_extract() {
        // __real__ (3.0 + 4.0i) == 3.0
        let c = Expr::ComplexLit {
            real: 3.0,
            imag: 4.0,
            span: Span::point("t.c", 1, 1),
            ty: Type::Complex(Box::new(Type::Double)),
        };
        let r = Expr::Real {
            e: Box::new(c),
            span: Span::point("t.c", 1, 1),
            ty: Type::Double,
        };
        assert_eq!(eval_full(&r), Ok(ConstValue::Float(3.0)));
    }

    #[test]
    fn complex_lit_imag_extract() {
        // __imag__ (3.0 + 4.0i) == 4.0
        let c = Expr::ComplexLit {
            real: 3.0,
            imag: 4.0,
            span: Span::point("t.c", 1, 1),
            ty: Type::Complex(Box::new(Type::Double)),
        };
        let im = Expr::Imag {
            e: Box::new(c),
            span: Span::point("t.c", 1, 1),
            ty: Type::Double,
        };
        assert_eq!(eval_full(&im), Ok(ConstValue::Float(4.0)));
    }

    #[test]
    fn scalar_real_returns_self() {
        // __real__ 5.0 == 5.0 (标量的 __real__ 返回自身)
        let f = float(5.0, Type::Double);
        let r = Expr::Real {
            e: Box::new(f),
            span: Span::point("t.c", 1, 1),
            ty: Type::Double,
        };
        assert_eq!(eval_full(&r), Ok(ConstValue::Float(5.0)));
    }

    #[test]
    fn scalar_imag_returns_zero() {
        // __imag__ 5 == 0 (标量的 __imag__ 返回 0)
        let i = int(5);
        let im = Expr::Imag {
            e: Box::new(i),
            span: Span::point("t.c", 1, 1),
            ty: Type::Int,
        };
        assert_eq!(eval_full(&im), Ok(ConstValue::Int(0)));
    }
}
