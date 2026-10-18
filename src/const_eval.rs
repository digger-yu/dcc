//! 可复用的整数常量表达式求值。
//!
//! parser、typed IR 验证和后端必须共享同一套规则，避免某个阶段把
//! “无法求值”误当成数值 0。

use crate::ast::{
    find_field, ArraySize, BinOp, Block, BlockItem, Decl, Expr, FloatConst, Stmt, Type, UnOp,
};
use crate::tokens::strip_int_suffix;
use std::collections::HashMap;

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
    /// 浮点常量。携带源码语义精度（f32 / f64 / long double），
    /// long double 保留真 80-bit 位型，避免常量折叠把 `0.1L` 拓宽成 f64。
    Float(crate::ast::FloatConst),
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
            ConstValue::Float(f) => Some(f.as_f64() as i128),
            _ => None,
        }
    }
    /// 转为 f64 (仅在确实是浮点或可转浮点的整数时)。
    /// long double 经真 80-bit → f64 正确舍入，调用方即显式降级点。
    pub fn as_float(&self) -> Option<f64> {
        match self {
            ConstValue::Float(f) => Some(f.as_f64()),
            ConstValue::Int(v) => Some(*v as f64),
            _ => None,
        }
    }
}

pub fn eval(expr: &Expr) -> ConstEvalResult {
    // 旧 API: 永远返回整数; 浮点截断, 其他返回 NotConstant
    // fenv_access false for backwards compatibility with old call sites
    match eval_full(expr, false) {
        Ok(ConstValue::Int(v)) => Ok(v),
        Ok(ConstValue::Float(f)) => Ok(f.as_f64() as i128),
        Ok(_) => Err(ConstEvalError::NotConstant),
        Err(e) => Err(e),
    }
}

pub fn eval_full(expr: &Expr, fenv_access: bool) -> Result<ConstValue, ConstEvalError> {
    match expr {
        Expr::Int { v, .. } => Ok(ConstValue::Int(*v)),
        Expr::Char { v, .. } => Ok(ConstValue::Int(*v)),
        Expr::Float { c, .. } => Ok(ConstValue::Float(*c)),
        // 数组名在常量表达式中退化为首元素地址 (C11 6.3.2.1p3):
        // (arr)->field / arr + 1 等地址常量链的基址
        Expr::Ident {
            name,
            ty: Type::Array(_, _),
            ..
        } => Ok(ConstValue::Addr {
            name: name.name.clone(),
            offset: 0,
        }),
        Expr::String { data, .. } => {
            // 字符串字面量: 编译器生成的 label 与 codegen 中的 rodata 标签对齐
            // 调用方需要确保同源字符串复用同一 label
            let label = format!(".L.str.const.{}", const_str_id(data));
            Ok(ConstValue::Str {
                data: data.clone(),
                label,
            })
        },
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
                                      // addend 按元素大小缩放 (与 codegen 的 R_X86_64_64
                                      // addend 计算一致, 否则 &arr[2] 指向 arr[0] 偏移 2 字节)
                let scale = pointee_byte_scale(&arr.ty()).ok_or(ConstEvalError::NotConstant)?;
                Ok(ConstValue::Addr {
                    name: arr_name,
                    offset: off * scale,
                })
            },
            // &global_struct.member: 地址常量表达式 (C 标准 6.6.9)
            // 用于全局/静态初始化器，如 `const T *p = &global.field;`
            Expr::Member {
                expr,
                field,
                is_arrow,
                ..
            } => {
                if *is_arrow {
                    // Arrow form: ptr->field is (*ptr).field
                    // Evaluate the pointer as an address constant
                    let base_addr = match eval_full(expr, fenv_access)? {
                        ConstValue::Addr { name, offset } => (name, offset),
                        _ => return Err(ConstEvalError::NotConstant),
                    };
                    // Look up field in the pointee struct/union
                    let pointee_ty = expr.ty();
                    let struct_ty = match &pointee_ty {
                        Type::Ptr(inner) => inner.base.clone(),
                        // 数组名 -> 成员: 先按 C11 6.3.2.1 退化为指针
                        // (如 (Upgrade_items)->uaattrid)
                        Type::Array(elem, _) => elem.base.clone(),
                        _ => return Err(ConstEvalError::NotConstant),
                    };
                    let field_offset = match &struct_ty {
                        Type::Struct(_, fields, _, _) | Type::Union(_, fields, _, _) => {
                            crate::ast::find_field(fields, &field.name)
                                .map(|(f, extra_off)| f.offset + extra_off)
                                .ok_or(ConstEvalError::NotConstant)?
                        },
                        _ => return Err(ConstEvalError::NotConstant),
                    };
                    return Ok(ConstValue::Addr {
                        name: base_addr.0,
                        offset: base_addr.1 + field_offset as i128,
                    });
                }
                // 递归求基地址: 必须是 &ident.field1.field2... 形式
                let base_addr = match eval_full(
                    &Expr::AddrOf {
                        e: expr.clone(),
                        ty: Type::Ptr(Box::new(crate::ast::QualType::new(expr.ty()))),
                        span: crate::diagnostics::Span::point("", 0, 0),
                    },
                    fenv_access,
                )? {
                    ConstValue::Addr { name, offset } => (name, offset),
                    _ => return Err(ConstEvalError::NotConstant),
                };
                // 计算字段在结构体内的偏移
                let field_offset = match &expr.ty() {
                    Type::Struct(_, fields, _, _) | Type::Union(_, fields, _, _) => {
                        crate::ast::find_field(fields, &field.name)
                            .map(|(f, extra_off)| f.offset + extra_off)
                            .ok_or(ConstEvalError::NotConstant)?
                    },
                    _ => return Err(ConstEvalError::NotConstant),
                };
                Ok(ConstValue::Addr {
                    name: base_addr.0,
                    offset: base_addr.1 + field_offset as i128,
                })
            },
            _ => Err(ConstEvalError::NotConstant),
        },
        Expr::LabelAddr { label, .. } => Ok(ConstValue::Addr {
            name: label.name.clone(),
            offset: 0,
        }),
        // 含 VLA 维度的 sizeof 不可常量折叠 (C11 6.5.3.4p2 运行时求值):
        // 静态 ty.size() 对 Var 维度用 8 字节 fallback, 折叠结果错误
        // (如 int a[2][N] 折成 16, 真值 N*4*2)。返回 NotConstant 让
        // 调用方 (if 分支折叠/静态断言/case 标签等) 走运行时路径,
        // codegen 的 SizeofExpr 分支读声明时绑定槽。
        Expr::SizeofTy { ty, .. } if ty.has_vla() => Err(ConstEvalError::NotConstant),
        Expr::SizeofTy { ty, .. } => Ok(ConstValue::Int(ty.size() as i128)),
        Expr::SizeofExpr { e, .. } if e.ty().has_vla() => Err(ConstEvalError::NotConstant),
        Expr::SizeofExpr { e, .. } => match e.as_ref() {
            // 字符串字面量: C 中为 char[N] 数组 (含 NUL), 返回实际字节数
            Expr::String { data, .. } => Ok(ConstValue::Int(data.len() as i128)),
            _ => Ok(ConstValue::Int(e.ty().size() as i128)),
        },
        Expr::AlignofTy { ty, .. } => Ok(ConstValue::Int(ty.align() as i128)),
        Expr::AlignofExpr { e, .. } => Ok(ConstValue::Int(e.ty().align() as i128)),
        Expr::Unary { op, e, .. } => {
            let v = eval_full(e, fenv_access)?;
            match op {
                UnOp::Neg => match v {
                    ConstValue::Int(i) => {
                        // 无符号类型: wrapping neg (如 -1U → 0xFFFFFFFF)
                        let is_unsigned = matches!(
                            &e.ty(),
                            Type::Bool
                                | Type::UChar
                                | Type::UShort
                                | Type::UInt
                                | Type::ULong
                                | Type::ULongLong
                                | Type::UInt128
                        );
                        if is_unsigned {
                            if let Some(bits) = int_type_bits(&e.ty()) {
                                if bits < 128 {
                                    let mask = if bits == 64 {
                                        u64::MAX as i128
                                    } else {
                                        (1i128 << bits) - 1
                                    };
                                    Ok(ConstValue::Int((!i + 1) & mask))
                                } else {
                                    Ok(ConstValue::Int(i.wrapping_neg()))
                                }
                            } else {
                                Ok(ConstValue::Int(i.wrapping_neg()))
                            }
                        } else {
                            i.checked_neg()
                                .map(ConstValue::Int)
                                .ok_or(ConstEvalError::Overflow)
                        }
                    },
                    ConstValue::Float(f) => {
                        if fenv_access {
                            return Err(ConstEvalError::NotConstant);
                        }
                        Ok(ConstValue::Float(f.negate()))
                    },
                    _ => Err(ConstEvalError::NotConstant),
                },
                UnOp::Not => match v {
                    ConstValue::Int(i) => Ok(ConstValue::Int((i == 0) as i128)),
                    _ => Err(ConstEvalError::NotConstant),
                },
                UnOp::BitNot => match v {
                    ConstValue::Int(i) => {
                        // 无符号类型: 掩码到类型宽度 (如 ~0U → 0xFFFFFFFF)
                        // 有符号类型: 直接 !i (保留符号扩展, 如 ~0 → -1)
                        let is_unsigned = matches!(
                            &e.ty(),
                            Type::Bool
                                | Type::UChar
                                | Type::UShort
                                | Type::UInt
                                | Type::ULong
                                | Type::ULongLong
                                | Type::UInt128
                        );
                        let result = if is_unsigned {
                            if let Some(bits) = int_type_bits(&e.ty()) {
                                if bits < 128 {
                                    let mask = if bits == 64 {
                                        u64::MAX as i128
                                    } else {
                                        (1i128 << bits) - 1
                                    };
                                    !i & mask
                                } else {
                                    !i
                                }
                            } else {
                                !i
                            }
                        } else {
                            !i
                        };
                        Ok(ConstValue::Int(result))
                    },
                    _ => Err(ConstEvalError::NotConstant),
                },
                UnOp::Plus => {
                    if matches!(v, ConstValue::Float(_)) && fenv_access {
                        return Err(ConstEvalError::NotConstant);
                    }
                    Ok(v)
                },
            }
        },
        Expr::Binop {
            op,
            l,
            r,
            ty: ref binop_ty,
            ..
        } => {
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
            // 指针算术: ptr ± int / int + ptr (数组名已退化为地址常量)。
            // Addr.offset 以字节计, 整数侧须按 pointee 大小缩放
            // (C11 6.5.6p8), 如 (items + 1)->field 的地址常量折叠。
            if matches!(op, BinOp::Add | BinOp::Sub) {
                let l_ptr = matches!(l.ty(), Type::Ptr(_) | Type::Array(_, _));
                let r_ptr = matches!(r.ty(), Type::Ptr(_) | Type::Array(_, _));
                if l_ptr && !r_ptr {
                    if let Ok(ConstValue::Addr { name, offset }) = eval_full(l, fenv_access) {
                        let n = eval_full(r, fenv_access)?
                            .as_int()
                            .ok_or(ConstEvalError::NotConstant)?;
                        let scale =
                            pointee_byte_scale(&l.ty()).ok_or(ConstEvalError::NotConstant)?;
                        let delta = n * scale;
                        return Ok(ConstValue::Addr {
                            name,
                            offset: if matches!(op, BinOp::Add) {
                                offset + delta
                            } else {
                                offset - delta
                            },
                        });
                    }
                } else if r_ptr && !l_ptr {
                    if let Ok(ConstValue::Addr { name, offset }) = eval_full(r, fenv_access) {
                        let n = eval_full(l, fenv_access)?
                            .as_int()
                            .ok_or(ConstEvalError::NotConstant)?;
                        let scale =
                            pointee_byte_scale(&r.ty()).ok_or(ConstEvalError::NotConstant)?;
                        return Ok(ConstValue::Addr {
                            name,
                            offset: offset + n * scale,
                        });
                    }
                }
            }
            let lv = eval_full(l, fenv_access)?;
            let rv = eval_full(r, fenv_access)?;
            // 整数算术/比较统一走类型感知路径 (arith_binop_values):
            // 按 binop_ty 做无符号重解释/规范化, 比较按操作数自身
            // 类型宽度与符号性 (C99 6.3.1.8)。
            match op {
                BinOp::AndAnd | BinOp::OrOr => unreachable!(),
                _ => arith_binop_values(*op, binop_ty, l, r, lv, rv, fenv_access),
            }
        },
        Expr::Cast { ty, e, .. } => {
            // 强制类型转换: 数值保留, 指针转换产生 NotConstant
            let v = eval_full(e, fenv_access)?;
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
                // Integer → float/double/long double cast：在目标精度上
                // 精确 RNE（LongDouble 对 |i| < 2^64 完全精确）。
                if matches!(ty, Type::Float | Type::Double | Type::LongDouble) {
                    return Ok(ConstValue::Float(FloatConst::from_int(ty, i)));
                }
                return Ok(ConstValue::Int(i));
            }
            // Float → integer cast: convert with saturation (match x86 behavior
            // for out-of-range values: clamp to INT_MAX/INT_MIN).
            if let ConstValue::Float(fc) = v {
                let f = fc.as_f64();
                if let Some(bits) = int_type_bits(ty) {
                    let is_signed = !is_unsigned_int_ty(ty);
                    let clamped = if is_signed {
                        let max_val = ((1u128 << (bits - 1)) - 1) as f64;
                        let min_val = -((1i128 << (bits - 1)) as f64);
                        if f.is_nan() {
                            0i128
                        } else if f >= max_val {
                            max_val as i128
                        } else if f <= min_val {
                            min_val as i128
                        } else {
                            f as i128
                        }
                    } else {
                        let max_val = if bits >= 128 {
                            u128::MAX as f64
                        } else {
                            ((1u128 << bits) - 1) as f64
                        };
                        if f.is_nan() || f < 0.0 {
                            0i128
                        } else if f >= max_val {
                            max_val as i128
                        } else {
                            f as i128
                        }
                    };
                    return Ok(ConstValue::Int(clamped));
                }
            }
            // 浮点 / 指针等: 简化处理, 让 codegen 处理位宽
            Ok(v)
        },
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            if eval(cond)? != 0 {
                eval_full(then_, fenv_access)
            } else {
                eval_full(else_, fenv_access)
            }
        },
        Expr::Comma { l, r, .. } => {
            eval(l)?;
            eval_full(r, fenv_access)
        },
        Expr::ComplexLit { real, imag, .. } => {
            // 复数字面量: 返回虚部值 (实部在 __real__ 提取时使用)
            // 常量上下文中复数通常通过 __real__/__imag__ 访问;
            // 这里将虚部编码为 Float, 供后续 Real/Imag 提取.
            // 使用特殊标记: 实部存入 Float 的整数部分, 虚部存入小数部分
            // 不, 更好的方式: 返回 ConstValue::Int 编码虚部 (用于 __imag__),
            // 而 __real__ 直接提取 real 字段.
            // 最简方案: 返回虚部作为 Float (内核中复数常量主要用于 __imag__)
            if !imag.is_zero() {
                Ok(ConstValue::Float(*imag))
            } else {
                Ok(ConstValue::Float(*real))
            }
        },
        Expr::ComplexIntLit { real, imag, .. } => {
            // 整型复数字面量：返回虚部整数（实部由 __real__ 直接取字段）。
            if *imag != 0 {
                Ok(ConstValue::Int(*imag))
            } else {
                Ok(ConstValue::Int(*real))
            }
        },
        Expr::Real { e, .. } => {
            // __real__ expr: 提取实部
            if let Expr::ComplexIntLit { real, .. } = e.as_ref() {
                return Ok(ConstValue::Int(*real));
            }
            match eval_full(e, false)? {
                ConstValue::Float(f) => {
                    // 如果内部是 ComplexLit, 实部已在 eval_full 中编码
                    // 对于纯复数字面量, 直接返回 real 字段
                    if let Expr::ComplexLit { real, .. } = e.as_ref() {
                        Ok(ConstValue::Float(*real))
                    } else {
                        // 标量的 __real__ 返回自身
                        Ok(ConstValue::Float(f))
                    }
                },
                v @ ConstValue::Int(_) => Ok(v), // 标量的 __real__ 返回自身
                _ => Err(ConstEvalError::NotConstant),
            }
        },
        Expr::Imag { e, .. } => {
            // __imag__ expr: 提取虚部
            if let Expr::ComplexIntLit { imag, .. } = e.as_ref() {
                return Ok(ConstValue::Int(*imag));
            }
            match eval_full(e, false)? {
                ConstValue::Float(f) => {
                    if let Expr::ComplexLit { real: _, imag, .. } = e.as_ref() {
                        Ok(ConstValue::Float(*imag))
                    } else {
                        // 标量的 __imag__ 返回 0
                        let _ = f;
                        Ok(ConstValue::Int(0))
                    }
                },
                ConstValue::Int(_) => Ok(ConstValue::Int(0)), // 标量虚部为 0
                _ => Err(ConstEvalError::NotConstant),
            }
        },
        Expr::Offsetof { ty, member, .. } => Ok(ConstValue::Int(eval_offsetof(ty, member) as i128)),
        Expr::StmtExpr { block, .. } => eval_stmt_expr_block(block),
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
        },
        Expr::Assign {
            l: _,
            r,
            op: _,
            ty: _,
            span: _,
        } => {
            // Assignment expression as sub-expression: return the value of RHS
            // after evaluation, so that parent expression can continue constant folding.
            // Side effects (storing to LValue) cannot be done in compile-time constant evaluation,
            // so they will be handled at runtime. We just need to return the value of RHS.
            eval_full(r, fenv_access)
        },
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
                },
                _ => 64,
            };
            // 截断到对应类型宽度, 避免超宽零扩展影响前导零计数
            let mask = if bits >= 128 { u128::MAX } else { (1u128 << bits) - 1 };
            let truncated = (v as u128) & mask;
            let lz = if truncated == 0 {
                bits as u32
            } else {
                128 - truncated.leading_zeros()
            };
            Ok(Some(bits as i128 - lz as i128))
        },
        "__builtin_ctz" | "__builtin_ctzl" | "__builtin_ctzll" if argc == 1 => {
            let v = take1(args)?;
            if v == 0 {
                return Ok(Some(0));
            } // GCC 约定: 输入 0 时结果未定义, 取 0
            Ok(Some((v as u64).trailing_zeros() as i128))
        },
        "__builtin_ffs" | "__builtin_ffsl" | "__builtin_ffsll" if argc == 1 => {
            let v = take1(args)?;
            if v == 0 {
                return Ok(Some(0));
            }
            // 截断到对应类型宽度, 避免超宽零扩展影响
            let bits = match name {
                "__builtin_ffs" => 32,
                "__builtin_ffsl" => {
                    if cfg!(target_pointer_width = "64") {
                        64
                    } else {
                        32
                    }
                },
                _ => 64,
            };
            let mask = if bits >= 128 { u128::MAX } else { (1u128 << bits) - 1 };
            let truncated = (v as u128) & mask;
            if truncated == 0 {
                return Ok(Some(0));
            }
            Ok(Some(truncated.trailing_zeros() as i128 + 1))
        },
        "__builtin_bswap16" if argc == 1 => {
            let v = take1(args)?;
            Ok(Some((v as u16).swap_bytes() as i128))
        },
        "__builtin_bswap32" if argc == 1 => {
            let v = take1(args)?;
            Ok(Some((v as u32).swap_bytes() as i128))
        },
        "__builtin_bswap64" if argc == 1 => {
            let v = take1(args)?;
            Ok(Some((v as u64).swap_bytes() as i128))
        },
        // __builtin_clrsb/clrsbl/clrsbll: 计算符号位冗余位数
        "__builtin_clrsb" | "__builtin_clrsbl" | "__builtin_clrsbll" if argc == 1 => {
            let bits = match name {
                "__builtin_clrsb" => 32,
                "__builtin_clrsbl" => {
                    if cfg!(target_pointer_width = "64") {
                        64
                    } else {
                        32
                    }
                },
                _ => 64,
            };
            let v = take1(args)?;
            let mask = if bits >= 128 { u128::MAX } else { (1u128 << bits) - 1 };
            let truncated = (v as u128) & mask;
            // 符号位
            let sign_bit = (truncated >> (bits - 1)) & 1;
            // 计算与符号位不同的最高位
            let xored = truncated ^ (if sign_bit == 0 { 0 } else { mask });
            if xored == 0 {
                // 所有位都与符号位相同
                Ok(Some((bits - 1) as i128))
            } else {
                let lz = 128 - xored.leading_zeros();
                Ok(Some((bits as u32 - lz) as i128 - 1))
            }
        },
        // __builtin_parity/parityl/parityll
        "__builtin_parity" | "__builtin_parityl" | "__builtin_parityll" if argc == 1 => {
            let bits = match name {
                "__builtin_parity" => 32,
                "__builtin_parityl" => {
                    if cfg!(target_pointer_width = "64") {
                        64
                    } else {
                        32
                    }
                },
                _ => 64,
            };
            let v = take1(args)?;
            let mask = if bits >= 128 { u128::MAX } else { (1u128 << bits) - 1 };
            let truncated = (v as u128) & mask;
            Ok(Some((truncated.count_ones() as i128) & 1))
        },
        // __builtin_popcount/popcountl/popcountll
        "__builtin_popcount" | "__builtin_popcountl" | "__builtin_popcountll" if argc == 1 => {
            let bits = match name {
                "__builtin_popcount" => 32,
                "__builtin_popcountl" => {
                    if cfg!(target_pointer_width = "64") {
                        64
                    } else {
                        32
                    }
                },
                _ => 64,
            };
            let v = take1(args)?;
            let mask = if bits >= 128 { u128::MAX } else { (1u128 << bits) - 1 };
            let truncated = (v as u128) & mask;
            Ok(Some(truncated.count_ones() as i128))
        },
        _ => {
            // Unknown __builtin_* functions are not silently ignored;
            // return a distinct error so callers can differentiate from
            // non-builtin calls that are simply not constant.
            if name.starts_with("__builtin_") {
                return Err(ConstEvalError::NotConstant);
            }
            Ok(None)
        },
    }
}

/// 语句表达式 `({ ... })` 编译期求值.
/// 仅当块内所有语句都是常量表达式或声明, 且最后一个语句是
/// 常量表达式时, 才能作为常量返回.
fn eval_stmt_expr_block(block: &Block) -> Result<ConstValue, ConstEvalError> {
    let items = &block.items;
    if items.is_empty() {
        return Ok(ConstValue::Int(0));
    }
    // Track variable bindings from declarations within the block
    let mut env: HashMap<String, ConstValue> = HashMap::new();
    // 求值所有非末尾语句 (必须是可折叠的常量表达式或无副作用声明)
    for item in &items[..items.len() - 1] {
        match item {
            BlockItem::Stmt(Stmt::Expr { e, .. }) => {
                // 中间语句: 必须是可求值的常量表达式 (结果丢弃)
                eval_with_env(e, &env, false)?;
            },
            BlockItem::Decl(Decl::Var(v)) => {
                // Track variable binding: evaluate init and store in env
                if let Some(init) = &v.init {
                    let val = eval_with_env(init, &env, false)?;
                    env.insert(v.name.name.clone(), val);
                }
            },
            _ => return Err(ConstEvalError::NotConstant),
        }
    }
    // 最后一个语句的值即为语句表达式的值
    match items.last().unwrap() {
        BlockItem::Stmt(Stmt::Expr { e, .. }) => eval_with_env(e, &env, false),
        _ => Err(ConstEvalError::NotConstant),
    }
}

/// Like eval_full but with a variable environment for looking up
/// identifiers bound within statement expressions.
fn eval_with_env(
    expr: &Expr,
    ctx: &HashMap<String, ConstValue>,
    fenv_access: bool,
) -> Result<ConstValue, ConstEvalError> {
    // Check if this is a simple identifier lookup in the ctx
    if let Expr::Ident { name, .. } = expr {
        if let Some(val) = ctx.get(&name.name) {
            return Ok(val.clone());
        }
    }
    // For binary/unary ops, recursively evaluate children with env
    // then apply the same operation logic as eval_full
    match expr {
        Expr::Binop { op, l, r, ty, .. } => {
            let lv = eval_with_env(l, ctx, fenv_access)?;
            let rv = eval_with_env(r, ctx, fenv_access)?;
            match op {
                BinOp::AndAnd => Ok(ConstValue::Int(if lv.as_int().unwrap_or(0) == 0 {
                    0
                } else if rv.as_int().unwrap_or(0) != 0 {
                    1
                } else {
                    0
                })),
                BinOp::OrOr => Ok(ConstValue::Int(
                    (lv.as_int().unwrap_or(0) != 0 || rv.as_int().unwrap_or(0) != 0) as i128,
                )),
                // 算术/比较统一走类型感知路径 (与 eval_binop 共用)
                _ => arith_binop_values(*op, ty, l, r, lv, rv, fenv_access),
            }
        },
        Expr::Unary { op, e, .. } => {
            let v = eval_with_env(e, ctx, fenv_access)?;
            match op {
                UnOp::Neg => match v {
                    ConstValue::Int(i) => {
                        let is_unsigned = matches!(
                            &e.ty(),
                            Type::Bool
                                | Type::UChar
                                | Type::UShort
                                | Type::UInt
                                | Type::ULong
                                | Type::ULongLong
                                | Type::UInt128
                        );
                        if is_unsigned {
                            if let Some(bits) = int_type_bits(&e.ty()) {
                                if bits < 128 {
                                    let mask = if bits == 64 {
                                        u64::MAX as i128
                                    } else {
                                        (1i128 << bits) - 1
                                    };
                                    Ok(ConstValue::Int((!i + 1) & mask))
                                } else {
                                    Ok(ConstValue::Int(i.wrapping_neg()))
                                }
                            } else {
                                Ok(ConstValue::Int(i.wrapping_neg()))
                            }
                        } else {
                            i.checked_neg()
                                .map(ConstValue::Int)
                                .ok_or(ConstEvalError::Overflow)
                        }
                    },
                    ConstValue::Float(f) => {
                        if fenv_access {
                            return Err(ConstEvalError::NotConstant);
                        }
                        Ok(ConstValue::Float(f.negate()))
                    },
                    _ => Err(ConstEvalError::NotConstant),
                },
                UnOp::Not => match v {
                    ConstValue::Int(i) => Ok(ConstValue::Int((i == 0) as i128)),
                    _ => Err(ConstEvalError::NotConstant),
                },
                UnOp::BitNot => match v {
                    ConstValue::Int(i) => {
                        let is_unsigned = matches!(
                            &e.ty(),
                            Type::Bool
                                | Type::UChar
                                | Type::UShort
                                | Type::UInt
                                | Type::ULong
                                | Type::ULongLong
                                | Type::UInt128
                        );
                        let result = if is_unsigned {
                            if let Some(bits) = int_type_bits(&e.ty()) {
                                if bits < 128 {
                                    let mask = if bits == 64 {
                                        u64::MAX as i128
                                    } else {
                                        (1i128 << bits) - 1
                                    };
                                    !i & mask
                                } else {
                                    !i
                                }
                            } else {
                                !i
                            }
                        } else {
                            !i
                        };
                        Ok(ConstValue::Int(result))
                    },
                    _ => Err(ConstEvalError::NotConstant),
                },
                UnOp::Plus => {
                    if matches!(v, ConstValue::Float(_)) && fenv_access {
                        return Err(ConstEvalError::NotConstant);
                    }
                    Ok(v)
                },
            }
        },
        Expr::Cast { e, .. } => eval_with_env(e, ctx, fenv_access),
        _ => eval_full(expr, fenv_access),
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

/// 按类型规范化整数值: 有符号类型截断到位宽后符号扩展, 无符号
/// 类型截断到位宽 (零扩展, 结果非负)。使 i128 承载的常量值与其
/// C 类型宽度语义一致 (如 UInt 上下文中的 -1 → 4294967295)。
fn normalize_int_value(v: i128, ty: &Type) -> i128 {
    if let Some(bits) = int_type_bits(ty) {
        if bits < 128 {
            let mask = if bits == 64 {
                u64::MAX as i128
            } else {
                (1i128 << bits) - 1
            };
            let truncated = v & mask;
            if !is_unsigned_int_ty(ty) && (truncated & (1i128 << (bits - 1))) != 0 {
                truncated | !mask
            } else {
                truncated
            }
        } else {
            v
        }
    } else {
        v
    }
}

/// 类型感知的双目运算求值 (eval_binop 与 eval_with_env 共用)。
/// binop_ty 为常用算术转换后的结果类型:
/// - Div/Mod: 无符号结果类型时, 操作数先重解释为无符号再运算
///   (C99 6.3.1.8)。纯 i128 有符号除法会把 -18U / -__INT_MAX__
///   折叠为 -1, 正确值为 1 (pr94412)。
/// - And/Or/Xor/Shl/Shr: 结果规范化到类型宽度 (有符号符号扩展,
///   无符号掩码), 避免未截断的 i128 中间值泄漏到后续折叠
///   (如有符号 (-1 << 3) 应为 -8 而非 0xFFFFFFF8)。
/// - 比较: 按各操作数自身类型规范化; 任一操作数整型提升后为
///   无符号 (宽度 >= int) 时按无符号比较。
fn arith_binop_values(
    op: BinOp,
    binop_ty: &Type,
    l: &Expr,
    r: &Expr,
    lv: ConstValue,
    rv: ConstValue,
    fenv_access: bool,
) -> Result<ConstValue, ConstEvalError> {
    let is_signed = !is_unsigned_int_ty(binop_ty);
    match op {
        BinOp::Add => add_values(lv, rv, fenv_access),
        BinOp::Sub => sub_values(lv, rv, fenv_access),
        BinOp::Mul => mul_values(lv, rv, fenv_access),
        BinOp::Div | BinOp::Mod => {
            if let (ConstValue::Int(a), ConstValue::Int(b)) = (&lv, &rv) {
                let (a, b) = (*a, *b);
                let a = normalize_int_value(a, binop_ty);
                let b = normalize_int_value(b, binop_ty);
                if b == 0 {
                    return Err(ConstEvalError::DivisionByZero);
                }
                let val = if op == BinOp::Div {
                    if is_signed {
                        a.checked_div(b)
                    } else {
                        (a as u128).checked_div(b as u128).map(|v| v as i128)
                    }
                } else if is_signed {
                    a.checked_rem(b)
                } else {
                    (a as u128).checked_rem(b as u128).map(|v| v as i128)
                };
                val.map(ConstValue::Int).ok_or(ConstEvalError::Overflow)
            } else if op == BinOp::Div {
                div_values(lv, rv, fenv_access)
            } else {
                mod_values(lv, rv, fenv_access)
            }
        },
        BinOp::And => match (lv, rv) {
            (ConstValue::Int(a), ConstValue::Int(b)) => Ok(ConstValue::Int(normalize_int_value(
                normalize_int_value(a, binop_ty) & normalize_int_value(b, binop_ty),
                binop_ty,
            ))),
            _ => Err(ConstEvalError::NotConstant),
        },
        BinOp::Or => match (lv, rv) {
            (ConstValue::Int(a), ConstValue::Int(b)) => Ok(ConstValue::Int(normalize_int_value(
                normalize_int_value(a, binop_ty) | normalize_int_value(b, binop_ty),
                binop_ty,
            ))),
            _ => Err(ConstEvalError::NotConstant),
        },
        BinOp::Xor => match (lv, rv) {
            (ConstValue::Int(a), ConstValue::Int(b)) => Ok(ConstValue::Int(normalize_int_value(
                normalize_int_value(a, binop_ty) ^ normalize_int_value(b, binop_ty),
                binop_ty,
            ))),
            _ => Err(ConstEvalError::NotConstant),
        },
        BinOp::Shl | BinOp::Shr if !(0..128).contains(&rv.as_int().unwrap_or(-1)) => {
            Err(ConstEvalError::InvalidShift(rv.as_int().unwrap_or(0)))
        },
        BinOp::Shl => match (lv, rv) {
            (ConstValue::Int(a), ConstValue::Int(b)) => {
                let a = normalize_int_value(a, binop_ty);
                Ok(ConstValue::Int(normalize_int_value(a.wrapping_shl(b as u32), binop_ty)))
            },
            _ => Err(ConstEvalError::NotConstant),
        },
        BinOp::Shr => match (lv, rv) {
            (ConstValue::Int(a), ConstValue::Int(b)) => {
                // 有符号: 规范化 (符号扩展) 后算术右移; 无符号:
                // 规范化 (掩码) 后逻辑右移。
                let a = normalize_int_value(a, binop_ty);
                Ok(ConstValue::Int(normalize_int_value(a >> b, binop_ty)))
            },
            _ => Err(ConstEvalError::NotConstant),
        },
        BinOp::Lt => cmp_binop_values(l, r, lv, rv, |a, b| (a < b) as i128),
        BinOp::Gt => cmp_binop_values(l, r, lv, rv, |a, b| (a > b) as i128),
        BinOp::Le => cmp_binop_values(l, r, lv, rv, |a, b| (a <= b) as i128),
        BinOp::Ge => cmp_binop_values(l, r, lv, rv, |a, b| (a >= b) as i128),
        BinOp::EqEq => cmp_binop_values(l, r, lv, rv, |a, b| (a == b) as i128),
        BinOp::Ne => cmp_binop_values(l, r, lv, rv, |a, b| (a != b) as i128),
        _ => Err(ConstEvalError::NotConstant),
    }
}

/// 类型感知的整数比较: 按各操作数自身类型规范化 (掩码/符号扩展)
/// 后比较。任一操作数整型提升后为无符号 (无符号且宽度 >= int,
/// C99 6.3.1.1/6.3.1.8) 时, 位模式重解释为无符号再比较:
/// (0U-1) > 0 应为真。UChar/UShort 等提升为 int, 不触发无符号
/// 比较。非整数对回退到 f64 比较。
fn cmp_binop_values<F: Fn(i128, i128) -> i128>(
    l: &Expr,
    r: &Expr,
    lv: ConstValue,
    rv: ConstValue,
    f: F,
) -> Result<ConstValue, ConstEvalError> {
    if let (&ConstValue::Int(a), &ConstValue::Int(b)) = (&lv, &rv) {
        let unsigned_cmp =
            matches!(&l.ty(), Type::UInt | Type::ULong | Type::ULongLong | Type::UInt128)
                || matches!(&r.ty(), Type::UInt | Type::ULong | Type::ULongLong | Type::UInt128);
        let a = normalize_int_value(a, &l.ty());
        let b = normalize_int_value(b, &r.ty());
        let res = if unsigned_cmp {
            f(a as u128 as i128, b as u128 as i128)
        } else {
            f(a, b)
        };
        return Ok(ConstValue::Int(res));
    }
    cmp_values(lv, rv, |x, y| f(x as i128, y as i128))
}

/// 指针算术的字节缩放: ptr±n 中 n 乘 pointee 大小。
/// 数组类型取元素大小, 指针取 pointee 大小, 其余为 None。
fn pointee_byte_scale(ty: &Type) -> Option<i128> {
    match ty {
        Type::Ptr(inner) => Some(inner.base.size().max(1) as i128),
        Type::Array(elem, _) => Some(elem.base.size().max(1) as i128),
        _ => None,
    }
}

/// 在 f64 域折叠两个浮点常量。任一操作数为 long double 时拒绝折叠：
/// 宿主无 80-bit 运算，f64 折叠会静默改变 `0.1L` 等的值，必须留给
/// 后端在运行时用 x87 求值。float 在 C 常规转换中本就提升为 double，
/// 故折叠结果统一为 F64。
fn fold_float<F>(
    a: FloatConst,
    b: FloatConst,
    fenv_access: bool,
    f: F,
) -> Result<ConstValue, ConstEvalError>
where
    F: FnOnce(f64, f64) -> f64,
{
    if fenv_access || a.is_long_double() || b.is_long_double() {
        return Err(ConstEvalError::NotConstant);
    }
    Ok(ConstValue::Float(FloatConst::F64(f(a.as_f64(), b.as_f64()))))
}

fn add_values(
    l: ConstValue,
    r: ConstValue,
    fenv_access: bool,
) -> Result<ConstValue, ConstEvalError> {
    match (l, r) {
        (ConstValue::Int(a), ConstValue::Int(b)) => Ok(ConstValue::Int(a.wrapping_add(b))),
        (ConstValue::Float(a), ConstValue::Float(b)) => fold_float(a, b, fenv_access, |x, y| x + y),
        (ConstValue::Int(a), ConstValue::Float(b)) => {
            fold_float(FloatConst::F64(crate::float80::i128_to_f64(a)), b, fenv_access, |x, y| {
                x + y
            })
        },
        (ConstValue::Float(a), ConstValue::Int(b)) => {
            fold_float(a, FloatConst::F64(crate::float80::i128_to_f64(b)), fenv_access, |x, y| {
                x + y
            })
        },
        _ => Err(ConstEvalError::NotConstant),
    }
}
fn sub_values(
    l: ConstValue,
    r: ConstValue,
    fenv_access: bool,
) -> Result<ConstValue, ConstEvalError> {
    match (l, r) {
        (ConstValue::Int(a), ConstValue::Int(b)) => Ok(ConstValue::Int(a.wrapping_sub(b))),
        (ConstValue::Float(a), ConstValue::Float(b)) => fold_float(a, b, fenv_access, |x, y| x - y),
        (ConstValue::Int(a), ConstValue::Float(b)) => {
            fold_float(FloatConst::F64(crate::float80::i128_to_f64(a)), b, fenv_access, |x, y| {
                x - y
            })
        },
        (ConstValue::Float(a), ConstValue::Int(b)) => {
            fold_float(a, FloatConst::F64(crate::float80::i128_to_f64(b)), fenv_access, |x, y| {
                x - y
            })
        },
        _ => Err(ConstEvalError::NotConstant),
    }
}
fn mul_values(
    l: ConstValue,
    r: ConstValue,
    fenv_access: bool,
) -> Result<ConstValue, ConstEvalError> {
    match (l, r) {
        (ConstValue::Int(a), ConstValue::Int(b)) => Ok(ConstValue::Int(a.wrapping_mul(b))),
        (ConstValue::Float(a), ConstValue::Float(b)) => fold_float(a, b, fenv_access, |x, y| x * y),
        (ConstValue::Int(a), ConstValue::Float(b)) => {
            fold_float(FloatConst::F64(crate::float80::i128_to_f64(a)), b, fenv_access, |x, y| {
                x * y
            })
        },
        (ConstValue::Float(a), ConstValue::Int(b)) => {
            fold_float(a, FloatConst::F64(crate::float80::i128_to_f64(b)), fenv_access, |x, y| {
                x * y
            })
        },
        _ => Err(ConstEvalError::NotConstant),
    }
}
fn div_values(
    l: ConstValue,
    r: ConstValue,
    fenv_access: bool,
) -> Result<ConstValue, ConstEvalError> {
    match (l, r) {
        (ConstValue::Int(_), ConstValue::Int(0)) => Err(ConstEvalError::DivisionByZero),
        (ConstValue::Int(a), ConstValue::Int(b)) => a
            .checked_div(b)
            .map(ConstValue::Int)
            .ok_or(ConstEvalError::Overflow),
        (ConstValue::Float(a), ConstValue::Float(b)) => fold_float(a, b, fenv_access, |x, y| x / y),
        (ConstValue::Int(a), ConstValue::Float(b)) => {
            fold_float(FloatConst::F64(crate::float80::i128_to_f64(a)), b, fenv_access, |x, y| {
                x / y
            })
        },
        (ConstValue::Float(a), ConstValue::Int(b)) => {
            fold_float(a, FloatConst::F64(crate::float80::i128_to_f64(b)), fenv_access, |x, y| {
                x / y
            })
        },
        _ => Err(ConstEvalError::NotConstant),
    }
}
fn mod_values(
    l: ConstValue,
    r: ConstValue,
    fenv_access: bool,
) -> Result<ConstValue, ConstEvalError> {
    match (l, r) {
        (ConstValue::Int(_), ConstValue::Int(0)) => Err(ConstEvalError::DivisionByZero),
        (ConstValue::Int(a), ConstValue::Int(b)) => a
            .checked_rem(b)
            .map(ConstValue::Int)
            .ok_or(ConstEvalError::Overflow),
        (ConstValue::Float(a), ConstValue::Float(b)) => {
            if fenv_access || a.is_long_double() || b.is_long_double() {
                return Err(ConstEvalError::NotConstant);
            }
            // C fmod semantics: a - trunc(a/b) * b
            // Rust f64 % f64 matches C fmod()
            let (af, bf) = (a.as_f64(), b.as_f64());
            if bf == 0.0 {
                return Err(ConstEvalError::DivisionByZero);
            }
            Ok(ConstValue::Float(FloatConst::F64(af % bf)))
        },
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
pub fn const_str_id(data: &[u8]) -> u64 {
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
        let t = strip_int_suffix(s);
        t.parse::<i128>().ok()
    }
}

/// __builtin_offsetof(type, member) 编译期偏移
pub fn eval_offsetof(ty: &Type, member: &str) -> usize {
    match ty {
        Type::Union(_, _, _, _) => 0,
        Type::Struct(_, fields, _, _) => {
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
                        if let Type::Struct(_, sub, _, _) | Type::Union(_, sub, _, _) = &field.ty {
                            current_fields = sub.as_ref();
                        }
                    }
                } else {
                    break;
                }
            }
            offset
        },
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
            Expr::Char { v, .. } => {
                if *v >= 0 && *v <= 0x10ffff {
                    if let Some(c) = char::from_u32(*v as u32) {
                        write!(f, "'{}'", c)
                    } else {
                        write!(f, "'\\x{:x}'", *v)
                    }
                } else {
                    write!(f, "'\\x{:x}'", *v)
                }
            },
            Expr::Float { c, .. } => write!(f, "{}", c.as_f64()),
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
            },
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
            },
            Expr::Unary { op, e, .. } => {
                let sym = match op {
                    UnOp::Neg => "-",
                    UnOp::Not => "!",
                    UnOp::BitNot => "~",
                    UnOp::Plus => "+",
                };
                write!(f, "{}{}", sym, ExprDebug(e))
            },
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
            },
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
            },
            Expr::Cond {
                cond, then_, else_, ..
            } => {
                write!(f, "({} ? {} : {})", ExprDebug(cond), ExprDebug(then_), ExprDebug(else_))
            },
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
                        },
                    }
                }
                write!(f, "}}")
            },
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
                        },
                    }
                }
                write!(f, "}}")
            },
            Expr::Offsetof { ty, member, .. } => write!(f, "offsetof({}, {})", ty, member),
            _ => write!(f, "<expr>"),
        }
    }
}

/// 剥掉「末端是字符串字面量」的强制转换链：`(u8*)" "` 与 `" "` 在静态
/// 初始化槽里指向同一地址。只有末端确实是 `Expr::String` 时才剥，
/// 其余转换原样返回，以免影响整型截断等语义。
/// 静态数据发射器只认 `Expr::String` 形态，缺了这一步 `(T*)"x"` 初始化
/// 的指针槽会被 0 填充（sqlite3.c trimFunc 的 `azOne = {(u8*)" "}`）。
pub fn unwrap_string_cast(e: &Expr) -> &Expr {
    match e {
        Expr::Cast { e: inner, .. } => {
            let stripped = unwrap_string_cast(inner);
            if matches!(stripped, Expr::String { .. }) {
                stripped
            } else {
                e
            }
        },
        _ => e,
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
            },
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
        },
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
        },
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
                (true, Type::Ptr(inner)) => match &inner.base {
                    Type::Struct(_, f, _, _) | Type::Union(_, f, _, _) => lookup(f.as_ref()),
                    _ => None,
                },
                (false, Type::Struct(_, f, _, _) | Type::Union(_, f, _, _)) => lookup(f.as_ref()),
                _ => None,
            }
        },
        Expr::Deref { e, .. } => object_size_of(e, mode),
        // Transparent wrappers: cast and address-of do not change object size
        Expr::Cast { e, .. } => object_size_of(e, mode),
        Expr::AddrOf { e, .. } => object_size_of(e, mode),
        // Compound literal: size is the type size
        Expr::CompoundLit { ty, .. } => Some(ty.size() as i128),
        // Pointer arithmetic: (ptr + offset) retains base object size
        // Only when the left operand is actually a pointer type
        Expr::Binop {
            op: BinOp::Add, l, ..
        } if matches!(l.ty(), Type::Ptr(_)) => object_size_of(l, mode),
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
            ty: Type::Ptr(Box::new(Type::Int.into())),
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
            ty: Type::Array(Box::new(Type::Char.into()), ArraySize::Const(16)),
        };
        assert_eq!(object_size(&buf, 0), Some(16));
        assert_eq!(object_size(&buf, 1), Some(1));
    }

    fn float(v: f64, ty: Type) -> Expr {
        let c = match ty {
            Type::LongDouble => FloatConst::LongDouble(crate::float80::from_f64(v)),
            Type::Float => FloatConst::F32(v as f32),
            _ => FloatConst::F64(v),
        };
        Expr::Float {
            c,
            span: Span::point("t.c", 1, 1),
            ty,
        }
    }

    #[test]
    fn float_arithmetic_eval() {
        // 浮点 + 浮点
        let a = bin_f(BinOp::Add, 1.5, 2.25, Type::Double, Type::Double);
        assert_eq!(eval_full(&a, false), Ok(ConstValue::Float(FloatConst::F64(3.75))));
        // 浮点 * 浮点
        let m = bin_f(BinOp::Mul, 2.0, 3.0, Type::Double, Type::Double);
        assert_eq!(eval_full(&m, false), Ok(ConstValue::Float(FloatConst::F64(6.0))));
        // 浮点 / 0 返回有限 NaN/inf, 不报错 (符合 IEEE 754)
        let d = bin_f(BinOp::Div, 1.0, 0.0, Type::Double, Type::Double);
        assert!(matches!(eval_full(&d, false), Ok(ConstValue::Float(_))));
    }

    #[test]
    fn float_comparison_eval() {
        let a = bin_f(BinOp::Lt, 1.0, 2.0, Type::Double, Type::Double);
        assert_eq!(eval_full(&a, false), Ok(ConstValue::Int(1)));
        let b = bin_f(BinOp::Ge, 2.0, 2.0, Type::Double, Type::Double);
        assert_eq!(eval_full(&b, false), Ok(ConstValue::Int(1)));
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
        assert_eq!(eval_full(&e, false), Ok(ConstValue::Float(FloatConst::F64(3.5))));
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
            ty: Type::Ptr(Box::new(Type::Char.into())),
        };
        match eval_full(&s, false) {
            Ok(ConstValue::Str { data, label }) => {
                assert_eq!(data, b"hello\0");
                assert!(label.starts_with(".L.str.const."));
            },
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
            ty: Type::Array(Box::new(Type::Int.into()), ArraySize::Const(8)),
        };
        let addr = Expr::AddrOf {
            e: Box::new(id),
            span: Span::point("t.c", 1, 1),
            ty: Type::Ptr(Box::new(Type::Int.into())),
        };
        match eval_full(&addr, false) {
            Ok(ConstValue::Addr { name, offset }) => {
                assert_eq!(name, "arr");
                assert_eq!(offset, 0);
            },
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
            ty: Type::Array(Box::new(Type::Char.into()), ArraySize::Const(16)),
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
            ty: Type::Ptr(Box::new(Type::Char.into())),
        };
        match eval_full(&addr, false) {
            Ok(ConstValue::Addr { name, offset }) => {
                assert_eq!(name, "buf");
                assert_eq!(offset, 4);
            },
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
        match eval_full(&e, false) {
            Ok(ConstValue::Addr { name, offset }) => {
                assert_eq!(name, "my_label");
                assert_eq!(offset, 0);
            },
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
            real: FloatConst::F64(3.0),
            imag: FloatConst::F64(4.0),
            span: Span::point("t.c", 1, 1),
            ty: Type::Complex(Box::new(Type::Double.into())),
        };
        let r = Expr::Real {
            e: Box::new(c),
            span: Span::point("t.c", 1, 1),
            ty: Type::Double,
        };
        assert_eq!(eval_full(&r, false), Ok(ConstValue::Float(FloatConst::F64(3.0))));
    }

    #[test]
    fn complex_lit_imag_extract() {
        // __imag__ (3.0 + 4.0i) == 4.0
        let c = Expr::ComplexLit {
            real: FloatConst::F64(3.0),
            imag: FloatConst::F64(4.0),
            span: Span::point("t.c", 1, 1),
            ty: Type::Complex(Box::new(Type::Double.into())),
        };
        let im = Expr::Imag {
            e: Box::new(c),
            span: Span::point("t.c", 1, 1),
            ty: Type::Double,
        };
        assert_eq!(eval_full(&im, false), Ok(ConstValue::Float(FloatConst::F64(4.0))));
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
        assert_eq!(eval_full(&r, false), Ok(ConstValue::Float(FloatConst::F64(5.0))));
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
        assert_eq!(eval_full(&im, false), Ok(ConstValue::Int(0)));
    }

    #[test]
    fn stmt_expr_single_expr() {
        // ({ 42; }) == 42
        let block = Block {
            items: vec![BlockItem::Stmt(Stmt::Expr {
                e: int(42),
                span: Span::point("t.c", 1, 1),
            })],
            span: Span::point("t.c", 1, 1),
        };
        let se = Expr::StmtExpr {
            block: Box::new(block),
            span: Span::point("t.c", 1, 1),
            ty: Type::Int,
        };
        assert_eq!(eval_full(&se, false), Ok(ConstValue::Int(42)));
    }

    #[test]
    fn stmt_expr_multi_stmt() {
        // ({ 1 + 2; 3 * 4; }) == 12 (最后一个表达式的值)
        let block = Block {
            items: vec![
                BlockItem::Stmt(Stmt::Expr {
                    e: bin(BinOp::Add, 1, 2),
                    span: Span::point("t.c", 1, 1),
                }),
                BlockItem::Stmt(Stmt::Expr {
                    e: bin(BinOp::Mul, 3, 4),
                    span: Span::point("t.c", 1, 1),
                }),
            ],
            span: Span::point("t.c", 1, 1),
        };
        let se = Expr::StmtExpr {
            block: Box::new(block),
            span: Span::point("t.c", 1, 1),
            ty: Type::Int,
        };
        assert_eq!(eval_full(&se, false), Ok(ConstValue::Int(12)));
    }

    #[test]
    fn stmt_expr_empty() {
        // ({}) == 0 (GCC 扩展: 空语句表达式结果为 0)
        let block = Block {
            items: vec![],
            span: Span::point("t.c", 1, 1),
        };
        let se = Expr::StmtExpr {
            block: Box::new(block),
            span: Span::point("t.c", 1, 1),
            ty: Type::Void,
        };
        assert_eq!(eval_full(&se, false), Ok(ConstValue::Int(0)));
    }
}
