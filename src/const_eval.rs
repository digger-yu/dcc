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
        // 函数设计符退化为其符号地址（C11 6.3.2.1p4），与上面「数组名退化为首元素
        // 地址」同一口径。内核 `arch/x86/include/asm/irq_stack.h` 的
        // `[__func] "i" (func)`（`call_on_stack` 递进来的函数名）依赖这一步：拿不到
        // 常量时，只立即数约束只能落回寄存器，`call %c[__func]` 就发不出调用目标。
        Expr::Ident {
            name,
            ty: Type::Func(..),
            ..
        } => Ok(ConstValue::Addr {
            name: name.name.clone(),
            offset: 0,
        }),
        // 数组成员表达式的值就是该成员的地址（C11 6.3.2.1p3 数组退化），与上面的
        // 「数组名退化」同一口径。缺这一臂时，`static_calls_table.capable`（extern
        // 全局的结构体成员，类型 `struct lsm_static_call[2]`）作为指针字段的静态
        // 初始化式会落到后端兜底：既不发符号重定位，又按**数组长度**填零 —— 内核
        // `LSM_HOOK_INIT` 的 `.scalls = static_calls_table.NAME` 因此全为 NULL，
        // 且把整个 `security_hook_list` 元素撑到数组长度（§5.33）。
        Expr::Member {
            ty: Type::Array(_, _),
            ..
        } => eval_full(
            &Expr::AddrOf {
                e: Box::new(expr.clone()),
                ty: Type::Ptr(Box::new(crate::ast::QualType::new(expr.ty()))),
                span: crate::diagnostics::Span::point("", 0, 0),
            },
            fenv_access,
        ),
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
                let off = eval(idx)?; // 索引必须是编译期整数
                                      // addend 按元素大小缩放 (与 codegen 的 R_X86_64_64
                                      // addend 计算一致, 否则 &arr[2] 指向 arr[0] 偏移 2 字节)
                let scale = pointee_byte_scale(&arr.ty()).ok_or(ConstEvalError::NotConstant)?;
                // 基址递归求值: 多维下标 `&d[0][3][0]` 的 arr 仍是 Index
                // 而非 Ident。旧写法只认 Ident 基址, 对二维以上取址直接
                // NotConstant, 全局初始化于是落到后端兜底 —— 兜底把
                // `&d[0][3]` 的偏移当结果, 丢最内维 (pr94125: 全局
                // `short *g = &d[0][3][0]` 偏移 48 而非 6, 写全局数组
                // 静默写错位置)。与下方 Member 臂同一递归口径。
                let base = match arr.as_ref() {
                    Expr::Ident { name, .. } => ConstValue::Addr {
                        name: name.name.clone(),
                        offset: 0,
                    },
                    _ => eval_full(
                        &Expr::AddrOf {
                            e: arr.clone(),
                            ty: Type::Ptr(Box::new(crate::ast::QualType::new(arr.ty()))),
                            span: crate::diagnostics::Span::point("", 0, 0),
                        },
                        fenv_access,
                    )?,
                };
                match base {
                    ConstValue::Addr { name, offset } => Ok(ConstValue::Addr {
                        name,
                        offset: offset + off * scale,
                    }),
                    _ => Err(ConstEvalError::NotConstant),
                }
            },
            // `&((T *)expr)[i]` 与 `(T *)&x.m`：`Expr::Cast` 不改变对象地址，
            // 剥掉那一层转型后按内层递归求同一条地址常量链。
            // 内核 `_static_cpu_has` 的 `[cap_byte] "i" (&((const char *)
            // boot_cpu_data.x86_capability)[bit >> 3])` 就是这个形状：不认它，
            // 只能接立即数的约束就落在寄存器上，`.altinstr_aux`/`__jump_table` 条目里
            // 留下一条指向寄存器名的野重定位（objtool 到链接后才报）。
            // 递归时 `e` 必须是 `inner` 而不是原来的 `e`，否则同一个 `Cast` 臂会自旋。
            Expr::Cast { e: inner, .. } => eval_full(
                &Expr::AddrOf {
                    e: inner.clone(),
                    // 只需子层的类型参一下地址链（结果只取符号名+偏移），
                    // 与下面 `Member` 臂递归时同一个构造口径。
                    ty: Type::Ptr(Box::new(crate::ast::QualType::new(inner.ty()))),
                    span: crate::diagnostics::Span::point("", 0, 0),
                },
                fenv_access,
            ),
            // C11 6.5.3.2p3: `&*E` 完全等价于 `E`（`*` 与 `&` 相消，既不取对象
            // 也不求值），所以地址常量的判定与 `E` 自身同一口径。
            // 内核 `drivers/tty/sysrq.c` 的 `sysrq_key_table[]` 就卡在这里：配置关闭时
            // `sysrq_showlocks_op` 定义为 `(*(const struct sysrq_key_op *)((void *)0))`，
            // 数组元素于是成为 `&(*(const struct sysrq_key_op *)((void *)0))`；
            // 不认这条消去律就 E1006 拒掉整张表（值就是空指针常量 0）。
            Expr::Deref { e, .. } => eval_full(e, fenv_access),
            // `&global_struct.member`: 地址常量表达式 (C 标准 6.6.9)
            // 用于全局/静态初始化器，如 `const T *p = &global.field;`
            Expr::Member {
                expr,
                field,
                is_arrow,
                ..
            } => {
                if *is_arrow {
                    // Arrow form: ptr->field is (*ptr).field
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
                    // Evaluate the pointer as an address constant
                    let base_addr = match eval_full(expr, fenv_access)? {
                        ConstValue::Addr { name, offset } => (name, offset),
                        // `((T *)0)->m` / `((T *)const)->m`: 基址是整数常量（空指针
                        // 转换只是地址 0），成员地址就是 该整数 + 字段偏移 —— 整型
                        // 常量表达式。`(unsigned long)&((T*)0)->m / sizeof(void*)`
                        // 是 `"i"` 约束里常见的 offsetof 写法 (analyzer 的
                        // asm-x86-linux-cpuid-paravirt-2.c)，旧实现把基址交给
                        // 后端兜底成 0，地址整体丢失。
                        ConstValue::Int(v) => {
                            return Ok(ConstValue::Int(v + field_offset as i128));
                        },
                        _ => return Err(ConstEvalError::NotConstant),
                    };
                    return Ok(ConstValue::Addr {
                        name: base_addr.0,
                        offset: base_addr.1 + field_offset as i128,
                    });
                }
                // 计算字段在结构体内的偏移
                let field_offset = match &expr.ty() {
                    Type::Struct(_, fields, _, _) | Type::Union(_, fields, _, _) => {
                        crate::ast::find_field(fields, &field.name)
                            .map(|(f, extra_off)| f.offset + extra_off)
                            .ok_or(ConstEvalError::NotConstant)?
                    },
                    _ => return Err(ConstEvalError::NotConstant),
                };
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
                    // `(*(T *)0).m` 与 arrow 臂同源: 基址是整数常量时结果是
                    // 该整数 + 字段偏移 (见上方 arrow 臂的根因说明)。
                    ConstValue::Int(v) => {
                        return Ok(ConstValue::Int(v + field_offset as i128));
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
            // 按 binop_ty 做无符号重解释/规范化; 比较另按操作数类型推出的
            // 公共类型 (C99 6.3.1.8) 定符号性 —— 比较结果类型恒为 int, 不是 binop_ty。
            match op {
                BinOp::AndAnd | BinOp::OrOr => unreachable!(),
                _ => arith_binop_values(*op, binop_ty, l, r, lv, rv, fenv_access),
            }
        },
        Expr::Cast { ty, e, .. } => {
            // 强制类型转换: 数值保留, 指针转换产生 NotConstant
            let v = eval_full(e, fenv_access)?;
            // 目标类型是 `_Bool` 时**不是**「截断到 8 位」而是 `(v != 0)`
            // （C11 6.3.1.2p1）。下面 `int_type_bits` 给 Bool 答 8、
            // `is_unsigned_int_ty` 答 true，于是 `(_Bool)0x200` 会被折成
            // `0x200 & 0xFF = 0` —— 与 §5.30 的 codegen 截断是同一个错。
            // 地址/字符串常量按「非空指针」判真，与 truthy_value 同口径。
            if matches!(ty, Type::Bool) {
                return Ok(ConstValue::Int(if truthy_value(&v)? { 1 } else { 0 }));
            }
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
            // 条件不一定是整数常量：数组名/函数名/字符串字面量在 C 里退化为
            // 非空指针，地址常量作条件恒为真（链接期也跑不掉）。内核
            // `include/linux/netfilter/nf_conntrack_sip.h` 的
            //   `.clen = (__cname) ? sizeof(__cname) - 1 : 0`
            // 就用字符串字面量当条件（`(const char *)"v"` 非空），gcc 折真；
            // 只认整数会让整张 `ct_sip_hdrs[]` 被 E1006 拒掉
            // （`net/netfilter/nf_conntrack_sip.o 错误 1`）。
            let truthy = match eval(cond) {
                Ok(v) => v != 0,
                // 整数口径判不出来时，再看「地址/字符串字面量退化为非空指针」。
                //
                // 两者都不成立必须把错误传出去：把 `Err` 当成「假」会把
                // `d > 0 ? d != 0 : 0` 折成 0，进而让 `while (…)` 整段消失
                // （`opt_float_truth_condition.c` 的 while_d 返回 0）。
                Err(_) => match eval_full(cond, fenv_access) {
                    Ok(
                        ConstValue::Addr { .. }
                        | ConstValue::Str { .. }
                        | ConstValue::Bytes { .. },
                    ) => true,
                    Ok(_) => return Err(ConstEvalError::NotConstant),
                    Err(e) => return Err(e),
                },
            };
            if truthy {
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
        Expr::Offsetof { ty, member, .. } => match eval_offsetof_opt(ty, member) {
            Some(v) => Ok(ConstValue::Int(v as i128)),
            None => Err(ConstEvalError::NotConstant),
        },
        Expr::StmtExpr { block, .. } => eval_stmt_expr_block(block),
        // 编译期可求值的 GCC/LLVM 内建函数: 参数是常量时返回常量结果。
        // 这些函数通常用 __builtin_constant_p 包裹, dcc 直接展开即可。
        Expr::Call { callee, args, .. } => {
            if let Expr::Ident { name, .. } = callee.as_ref() {
                // `__builtin_constant_p(x)` 本身就是个编译期谓词：它问的就是
                // 「x 在常量表达式里能不能求值」，而本函数正是回答这个同题的地方。
                // 以前这里不认它，于是 parser 按「不急切折 0」政策保留下来的那个调用
                // （见 `parser.rs` 与 `__builtin_expect` 同一口径）在常量折叠里变成
                // 「不可求值」，连锁使两条依赖常量条件折叠的路径失效：
                //   - `if (__builtin_constant_p(p)) link_error();` 在 -O0/-O1 仍发出
                //     永不可达的分支，链接期报 undefined reference to `link_error`
                //     （gcc-torture 的链接型用例 20030330-1.c；gcc -O0 反汇编里
                //     `link_error` 出现 0 次）；
                //   - `__builtin_choose_expr (!__builtin_constant_p (y), 3, 4)` 被拒成
                //     E0013「condition is not constant」（pr19449.c，全局初值与函数体
                //     两处）。
                // 只把**标量**常量算作 1：地址常量在 GCC 里也给 1，但把判据放大到地址
                // 会改变 `__builtin_choose_expr` 的选边，而这条路径上没有任何 IPA 上下文
                // （全局/形参在常量表达式里永远不会是常量），所以 `is 常量` 就是它的答案。
                if name.name == "__builtin_constant_p" {
                    let known = match args.first() {
                        Some(a) => matches!(
                            eval_full(a, fenv_access),
                            Ok(ConstValue::Int(_)) | Ok(ConstValue::Float(_))
                        ),
                        None => false,
                    };
                    return Ok(ConstValue::Int(known as i128));
                }
                // `__builtin_expect(x, c)` 的值**就是** `x`，`c` 只是给分支预测的提示。
                //
                // 为什么必须在这里答对：`static inline` 体被整体搬进调用方之后，条件常常
                // 整条只差这一层壳就不定型。现场是 `include/linux/ucopysize.h` 的
                // `check_copy_size()`：
                //     if (unlikely(sz >= 0 && sz < bytes)) { … __bad_copy_from(); … }
                // 内联 + 常量代回之后两个实参都已经折成 `Int 0`，只剩 `__builtin_expect`
                // 这层调用包着，于是 `if` 判不成常量、死块里的调用照发。`__bad_copy_from`
                // /`__bad_copy_to` 在头里只有 `extern void __compiletime_error(...)` 的**声明**
                // （定义根本不存在，gcc 靠「永远不发出引用」来保证），所以每发出一次就是一条
                // 链接期 undefined reference —— 实测 `fs/eventfd.o` 等 116 个 TU 共 232 条。
                if matches!(
                    name.name.as_str(),
                    "__builtin_expect" | "__builtin_expect_with_probability"
                ) {
                    if let Some(a) = args.first() {
                        return eval(a).map(ConstValue::Int);
                    }
                }
                // 浮点常量内建 (__builtin_nan/inf/huge_val 及其 f/l 变体):
                // 结果是编译期浮点常量, 任何参数 (含字符串 tag) 都折叠为同一常量。
                if let Some(v) = eval_float_builtin(name.name.as_str(), args)? {
                    return Ok(v);
                }
                if let Some(v) = eval_math_builtin(name.name.as_str(), args)? {
                    return Ok(v);
                }
                if let Some(v) = eval_intrinsic(name.name.as_str(), args)? {
                    return Ok(ConstValue::Int(v));
                }
            }
            // constexpr 函数调用在 parser 层不展开, 走 NotConstant
            Err(ConstEvalError::NotConstant)
        },
        Expr::Assign {
            l,
            r,
            op: _,
            ty,
            span: _,
        } => {
            // Assignment expression as sub-expression: return the value of RHS
            // after evaluation, so that parent expression can continue constant folding.
            // Side effects (storing to LValue) cannot be done in compile-time constant evaluation,
            // so they will be handled at runtime. We just need to return the value of RHS.
            //
            // 但「赋值表达式的值」是**转换到左值类型之后**的 RHS（C11 6.5.16p3），
            // 不是 RHS 原值。`ty` 是 parser 记下的左值类型（`lhs.ty()`），复合赋值
            // 也已在 parser 展开成 `target = (target op rhs)`，所以这里只要按
            // 左值类型收口即可。左值是 `_Bool` 时归一化为 0/1（6.3.1.2p1）：
            // `v = (lb = 0x200)` 必须得 1，按原值传播就会得到 512（§5.30）。
            //
            // 【位域左值拒绝求值】位域不是独立类型：赋值表达式的值是**写回该位域后**
            // 再读出的值 —— 先截断到 bit_width、再按字段符号性扩展（C11 6.5.16p3），
            // 而 `ty` 只是字段的声明类型（`int`），上面那条「按左值类型收口」收不住它。
            // 这里不做 store，算不出那个值；旧实现直接返回 RHS，于是
            // `(l.m = 1081) == 1081`（`signed int m:11`）被折成常量真，-O1 把 abort
            // 那一臂以外的代码连同那次 store 一起删掉（实测
            // gcc.c-torture/execute/921016-1.c；`while ((x = 0));` 式的条件求值删除
            // 同族）。宁可判 NotConstant，让运行期代码发出来。
            if lvalue_is_bitfield_member(l) {
                return Err(ConstEvalError::NotConstant);
            }
            let v = eval_full(r, fenv_access)?;
            if matches!(ty, Type::Bool) {
                return Ok(ConstValue::Int(if truthy_value(&v)? { 1 } else { 0 }));
            }
            Ok(v)
        },
        _ => Err(ConstEvalError::NotConstant),
    }
}

/// 编译期浮点常量内建求值: __builtin_nan/nanf/nanl、__builtin_inf/inff/infl、
/// __builtin_huge_val/huge_valf/huge_vall。这些在 GCC 里是编译期浮点常量
/// (结果不依赖参数, 字符串 tag 仅用于诊断), 折叠为对应的 NaN / +Infinity
/// 位模式。返回 `ConstValue::Float`; 不匹配时返回 Ok(None) 让外层继续尝试
/// 整数内建或走 NotConstant。
fn eval_float_builtin(name: &str, _args: &[Expr]) -> Result<Option<ConstValue>, ConstEvalError> {
    use crate::float80;
    // 传统 f/l 后缀: 与 GCC 一致.
    // `nans` 系返回**信令** NaN (最低有效尾数位为 1、quiet 位为 0):
    //   f32 sNaN = 0x7F80_0001, f64 sNaN = 0x7FF0_0000_0000_0001。
    // gcc-dg torture/floatn-nan.h 断言 `nans_cst + nans_cst` 置 FE_INVALID,
    // 而 quiet NaN 相加不会 —— 所以这里绝不能与 `nan` 同值。
    // long double 走 float80::from_f64: 它保留 NaN 负载并置整数位,
    // 80-bit quiet 位 (bit 62) 仍为 0, 信令语义不丢。
    let v = match name {
        "__builtin_nan" => Some(FloatConst::F64(f64::NAN)),
        "__builtin_nanf" => Some(FloatConst::F32(f32::NAN)),
        "__builtin_nanl" => Some(FloatConst::LongDouble(float80::from_f64(f64::NAN))),
        "__builtin_nans" => Some(FloatConst::F64(f64::from_bits(0x7FF0_0000_0000_0001))),
        "__builtin_nansf" => Some(FloatConst::F32(f32::from_bits(0x7F80_0001))),
        "__builtin_nansl" => {
            Some(FloatConst::LongDouble(float80::from_f64(f64::from_bits(0x7FF0_0000_0000_0001))))
        },
        "__builtin_inf" | "__builtin_huge_val" => Some(FloatConst::F64(f64::INFINITY)),
        "__builtin_inff" | "__builtin_huge_valf" => Some(FloatConst::F32(f32::INFINITY)),
        "__builtin_infl" | "__builtin_huge_vall" => {
            Some(FloatConst::LongDouble(float80::from_f64(f64::INFINITY)))
        },
        _ => None,
    };
    if let Some(v) = v {
        return Ok(Some(ConstValue::Float(v)));
    }
    // 'q' 后缀 = binary128（GCC 的 __builtin_nanq / __builtin_nansq /
    // __builtin_infq / __builtin_huge_valq），与 f128 变体同义。
    if let Some(stem) = name
        .strip_prefix("__builtin_")
        .and_then(|n| n.strip_suffix('q'))
    {
        let bits = match stem {
            "inf" | "huge_val" => crate::float128::INF_BITS,
            "nans" => crate::float128::SNAN_BITS,
            "nan" => crate::float128::QNAN_BITS,
            _ => return Ok(None),
        };
        return Ok(Some(ConstValue::Float(FloatConst::Float128(bits))));
    }
    // 扩展浮点 (_FloatN/_FloatNx) 宽度后缀: inf/huge_val → +Infinity,
    // nan → quiet NaN, nans → signaling NaN; 后缀决定精度
    // (f16/f32 → f32, f32x/f64 → f64, f64x/f128/f128x → long double).
    // 顺序敏感: 长后缀须先于短前缀匹配。
    let n = match name.strip_prefix("__builtin_") {
        Some(n) => n,
        None => return Ok(None),
    };
    let (stem, suff) = if let Some(r) = n.strip_suffix("f16") {
        (r, "f16")
    } else if let Some(r) = n.strip_suffix("f32x") {
        (r, "f32x")
    } else if let Some(r) = n.strip_suffix("f128x") {
        (r, "f128x")
    } else if let Some(r) = n.strip_suffix("f32") {
        (r, "f32")
    } else if let Some(r) = n.strip_suffix("f64x") {
        (r, "f64x")
    } else if let Some(r) = n.strip_suffix("f128") {
        (r, "f128")
    } else if let Some(r) = n.strip_suffix("f64") {
        (r, "f64")
    } else {
        return Ok(None);
    };
    if stem != "inf" && stem != "huge_val" && stem != "nan" && stem != "nans" {
        return Ok(None);
    }
    let is_inf = stem == "inf" || stem == "huge_val";
    let is_snan = stem == "nans";
    let v = match suff {
        "f16" | "f32" => {
            let x = if is_inf {
                f32::INFINITY
            } else if is_snan {
                f32::from_bits(0x7F80_0001)
            } else {
                f32::NAN
            };
            FloatConst::F32(x)
        },
        "f32x" | "f64" => {
            let x = if is_inf {
                f64::INFINITY
            } else if is_snan {
                f64::from_bits(0x7FF0_0000_0000_0001)
            } else {
                f64::NAN
            };
            FloatConst::F64(x)
        },
        "f128" | "f128x" => {
            // binary128：位模式必须**直接给出**。
            // 旧实现落到下面的 long double 分支（f64 → 80-bit 中转），再由
            // codegen 拓宽回 binary128 —— 中转把信令 NaN 静默成了 quiet NaN
            //（IEEE 的格式转换会 quieting），于是
            // `volatile _Float128 s = __builtin_nansf128("")` 读回的是
            // 0x7fff8000...（qNaN），`s + s` 不再置 FE_INVALID。
            // GCC 的 `__builtin_nansf128("")` = 0x7fff4000...（sNaN）。
            let bits = if is_inf {
                crate::float128::INF_BITS
            } else if is_snan {
                crate::float128::SNAN_BITS
            } else {
                crate::float128::QNAN_BITS
            };
            FloatConst::Float128(bits)
        },
        _ => {
            // f64x：x86-64 上 _Float64x 是 80-bit long double。
            let x = if is_inf {
                f64::INFINITY
            } else if is_snan {
                f64::from_bits(0x7FF0_0000_0000_0001)
            } else {
                f64::NAN
            };
            FloatConst::LongDouble(float80::from_f64(x))
        },
    };
    Ok(Some(ConstValue::Float(v)))
}

/// 编译期折叠 `__builtin_<math>` (acosh/exp2/erfc/pow/hypot/tgamma/… 及其
/// _FloatN 宽度后缀 f16/f32/f32x/f64/f64x/f128/f128x, 以及无后缀的传统 double 版)。
/// 参数均为常量时用 `libm` 求值, 返回对应精度的浮点常量。
///
/// 修复 gcc-dg torture/floatN-builtin.c: 那些测试把一长串 `__builtin_*` 用在
/// **全局** 初始化器 (`volatile TYPE t = FN(__builtin_acosh)(CST(1.0))`)。GCC 在
/// 编译期把常量参数折叠掉; dcc 既无全局动态初始化器、运行期又转发 libm, 常量场景
/// 会报 "global initializer ... not a supported constant expression" (E1006)。
/// 非常量参数仍走运行期 libm 转发, 不受影响。不匹配返回 Ok(None) 交回外层。
fn eval_math_builtin(name: &str, args: &[Expr]) -> Result<Option<ConstValue>, ConstEvalError> {
    use crate::float80;
    let n = match name.strip_prefix("__builtin_") {
        Some(n) => n,
        None => return Ok(None),
    };
    // 精度: 0 = f32, 1 = f64, 2 = long double。长后缀优先。
    let (stem, prec): (&str, u8) = if let Some(r) = n.strip_suffix("f128x") {
        (r, 2)
    } else if let Some(r) = n.strip_suffix("f128") {
        (r, 2)
    } else if let Some(r) = n.strip_suffix("f64x") {
        (r, 2)
    } else if let Some(r) = n.strip_suffix("f32x") {
        (r, 1)
    } else if let Some(r) = n.strip_suffix("f16") {
        (r, 0)
    } else if let Some(r) = n.strip_suffix("f32") {
        (r, 0)
    } else if let Some(r) = n.strip_suffix("f64") {
        (r, 1)
    } else {
        (n, 1)
    };
    // 第 i 个参数求值为 f64 (浮点常量; 整型常量允许提升)。
    let fa = |i: usize| -> Result<f64, ConstEvalError> {
        match eval_full(&args[i], false)? {
            ConstValue::Float(fc) => Ok(fc.as_f64()),
            ConstValue::Int(v) => Ok(v as f64),
            _ => Err(ConstEvalError::NotConstant),
        }
    };
    // 第 i 个参数求值为 i32 (ldexp/scalbn 的整型指数参数)。
    let ia = |i: usize| -> Result<i32, ConstEvalError> {
        match eval_full(&args[i], false)? {
            ConstValue::Int(v) => Ok(v as i32),
            _ => Err(ConstEvalError::NotConstant),
        }
    };
    let argc = args.len();
    let wrap = |x: f64| -> ConstValue {
        match prec {
            0 => ConstValue::Float(FloatConst::F32(x as f32)),
            2 => ConstValue::Float(FloatConst::LongDouble(float80::from_f64(x))),
            _ => ConstValue::Float(FloatConst::F64(x)),
        }
    };
    // 返回整型的内建 (int / long 语义)。
    match stem {
        "ilogb" if argc == 1 => return Ok(Some(ConstValue::Int(libm::ilogb(fa(0)?) as i128))),
        // llround/lround: 半值**远离零**取整 (== f64::round 的语义)。
        "llround" | "lround" if argc == 1 => {
            return Ok(Some(ConstValue::Int(fa(0)?.round() as i64 as i128)))
        },
        // llrint/lrint: 半值**到偶**取整 (当前舍入模式为 nearest-even)。
        "llrint" | "lrint" if argc == 1 => {
            return Ok(Some(ConstValue::Int(const_rint_i64(fa(0)?) as i128)))
        },
        _ => {},
    }
    let r: Option<f64> = match stem {
        "sqrt" if argc == 1 => Some(libm::sqrt(fa(0)?)),
        "fabs" if argc == 1 => Some(libm::fabs(fa(0)?)),
        "copysign" if argc == 2 => Some(libm::copysign(fa(0)?, fa(1)?)),
        "ceil" if argc == 1 => Some(libm::ceil(fa(0)?)),
        "floor" if argc == 1 => Some(libm::floor(fa(0)?)),
        "trunc" if argc == 1 => Some(libm::trunc(fa(0)?)),
        "round" if argc == 1 => Some(libm::round(fa(0)?)),
        "rint" if argc == 1 => Some(libm::rint(fa(0)?)),
        // nearbyint 与 rint 仅差在不置 inexact, 取整值相同 (默认舍入模式)。
        "nearbyint" if argc == 1 => Some(libm::rint(fa(0)?)),
        "exp" if argc == 1 => Some(libm::exp(fa(0)?)),
        "exp2" if argc == 1 => Some(libm::exp2(fa(0)?)),
        "expm1" if argc == 1 => Some(libm::expm1(fa(0)?)),
        "log" if argc == 1 => Some(libm::log(fa(0)?)),
        "log2" if argc == 1 => Some(libm::log2(fa(0)?)),
        "log10" if argc == 1 => Some(libm::log10(fa(0)?)),
        "log1p" if argc == 1 => Some(libm::log1p(fa(0)?)),
        "logb" if argc == 1 => Some(const_logb(fa(0)?)),
        "sin" if argc == 1 => Some(libm::sin(fa(0)?)),
        "cos" if argc == 1 => Some(libm::cos(fa(0)?)),
        "tan" if argc == 1 => Some(libm::tan(fa(0)?)),
        "asin" if argc == 1 => Some(libm::asin(fa(0)?)),
        "acos" if argc == 1 => Some(libm::acos(fa(0)?)),
        "atan" if argc == 1 => Some(libm::atan(fa(0)?)),
        "sinh" if argc == 1 => Some(libm::sinh(fa(0)?)),
        "cosh" if argc == 1 => Some(libm::cosh(fa(0)?)),
        "tanh" if argc == 1 => Some(libm::tanh(fa(0)?)),
        "asinh" if argc == 1 => Some(libm::asinh(fa(0)?)),
        "acosh" if argc == 1 => Some(libm::acosh(fa(0)?)),
        "atanh" if argc == 1 => Some(libm::atanh(fa(0)?)),
        "cbrt" if argc == 1 => Some(libm::cbrt(fa(0)?)),
        "erf" if argc == 1 => Some(libm::erf(fa(0)?)),
        "erfc" if argc == 1 => Some(libm::erfc(fa(0)?)),
        "tgamma" if argc == 1 => Some(libm::tgamma(fa(0)?)),
        "lgamma" if argc == 1 => Some(libm::lgamma(fa(0)?)),
        "pow" if argc == 2 => Some(libm::pow(fa(0)?, fa(1)?)),
        "atan2" if argc == 2 => Some(libm::atan2(fa(0)?, fa(1)?)),
        "fdim" if argc == 2 => Some(libm::fdim(fa(0)?, fa(1)?)),
        "fmax" if argc == 2 => Some(libm::fmax(fa(0)?, fa(1)?)),
        "fmin" if argc == 2 => Some(libm::fmin(fa(0)?, fa(1)?)),
        "fmod" if argc == 2 => Some(libm::fmod(fa(0)?, fa(1)?)),
        "hypot" if argc == 2 => Some(libm::hypot(fa(0)?, fa(1)?)),
        "nextafter" if argc == 2 => Some(libm::nextafter(fa(0)?, fa(1)?)),
        "remainder" if argc == 2 => Some(libm::remainder(fa(0)?, fa(1)?)),
        "ldexp" if argc == 2 => Some(libm::ldexp(fa(0)?, ia(1)?)),
        "scalbn" if argc == 2 => Some(libm::scalbn(fa(0)?, ia(1)?)),
        // scalbln 与 scalbn 仅指数参数宽度不同 (long vs int), 取整值相同。
        "scalbln" if argc == 2 => Some(libm::scalbn(fa(0)?, ia(1)?)),
        "fma" if argc == 3 => Some(libm::fma(fa(0)?, fa(1)?, fa(2)?)),
        _ => None,
    };
    match r {
        Some(x) => Ok(Some(wrap(x))),
        None => Ok(None),
    }
}

/// `logb` 常量折叠: 返回参数的 (无偏) 二进制指数, 以浮点表示。
/// libm 0.2 未导出顶层 `logb`, 这里按 IEEE-754 位型直接算。
fn const_logb(x: f64) -> f64 {
    if x.is_nan() {
        return f64::NAN;
    }
    if x.is_infinite() {
        return f64::INFINITY;
    }
    if x == 0.0 {
        return f64::NEG_INFINITY;
    }
    let bits = x.to_bits();
    let biased = ((bits >> 52) & 0x7FF) as i32;
    if biased != 0 {
        (biased - 1023) as f64
    } else {
        // 次正规数: 值 = 尾数 * 2^-1074, logb = MSB 位号 - 1074。
        let mant = bits & 0xF_FFFF_FFFF_FFFF;
        let msb = 63 - (mant | 1).leading_zeros() as i32;
        (msb - 1074) as f64
    }
}

/// 半值**到偶**取整为 i64 (llrint/lrint 语义; libm 0.2 未导出)。
fn const_rint_i64(x: f64) -> i64 {
    if x.is_nan() || x.is_infinite() {
        return x as i64;
    }
    let f = x.floor();
    let diff = x - f;
    let r = if diff < 0.5 {
        f
    } else if diff > 0.5 {
        f + 1.0
    } else if (f as i64) % 2 == 0 {
        f
    } else {
        f + 1.0
    };
    r as i64
}

/// 这条表达式是否就是一个**字符串字面量**（含结尾 NUL）；是则返回它的字节内容。
///
/// 只认字面量（可裹转型）：`__builtin_strcmp(p, "<")` 里 `p` 是变量时**不能**猜，
/// 那只能让调用方保留运行时调用。`ConstValue::Str` 就是 parser 对字面量的表示，
/// 而 `ConstValue::Bytes` 是 `#embed` 的数组（同样有确定字节），两者都给。
fn literal_string_bytes(e: &Expr) -> Result<Option<Vec<u8>>, ConstEvalError> {
    match e {
        Expr::Cast { e: inner, .. } => literal_string_bytes(inner),
        Expr::String { .. } => match eval_full(e, false)? {
            ConstValue::Str { data, .. } | ConstValue::Bytes { data, .. } => Ok(Some(data)),
            _ => Ok(None),
        },
        _ => Ok(None),
    }
}

/// C 的 `strcmp`/`strncmp` 在对**常量字符串**求值：按无符号字节比，返回第一个不同处
/// 的差（比较到 NUL 就停），与 glibc/gcc 的折叠结果同口径。
fn c_strcmp(a: &[u8], b: &[u8], n: Option<u128>) -> i128 {
    let limit = n.unwrap_or(u128::MAX) as u128;
    for i in 0..limit {
        let idx = i as usize;
        // 超出实际长度就是已经跑进结尾 NUL 之后，按 0 参比（两边一致时自然提前终止）。
        let ca = a.get(idx).copied().unwrap_or(0) as u32;
        let cb = b.get(idx).copied().unwrap_or(0) as u32;
        if ca != cb {
            return (ca as i32 - cb as i32) as i128;
        }
        if ca == 0 {
            return 0;
        }
    }
    0
}

/// 编译期内建函数求值 (如 __builtin_clzll/__builtin_ctzll/__builtin_popcountll，
/// 以及字面量形式的 __builtin_strcmp/__builtin_strncmp)。
/// 仅当参数是常量时返回 Some; 否则返回 None 让外层走 NotConstant。
fn eval_intrinsic(name: &str, args: &[Expr]) -> Result<Option<i128>, ConstEvalError> {
    let argc = args.len();
    let take1 = |args: &[Expr]| -> Result<i128, ConstEvalError> { eval(&args[0]) };
    match name {
        // `__builtin_strcmp` / `__builtin_strncmp` 对**两个字面量**必须折成常量。
        // 现场理由：内核 `kernel/sched/fair.c:537-554` 的 `vruntime_cmp(A, "<", B)`
        // 把「选哪个比较臂」编码成一个字符串字面量参数，整套结构是
        //   `if (!__builtin_strcmp(CMP_STR, "<")) { … } else if … else __BUILD_BUG_vruntime_cmp();`
        // 而 `__BUILD_BUG_vruntime_cmp` 是一个**故意不存在**的函数（注释原话
        // “Unknown operator throws linker error”）。不折那次 strcmp 就留下
        // 永不执行的 else 臂里的引用（实测 `kernel/sched/fair.o` 7 条：
        // `min_vruntime`/`entity_before`/`entity_key`/`vruntime_eligible`/
        // `__min_vruntime_update`/`protect_slice`/`update_deadline`），最终链接
        // 就是 `undefined reference to '__BUILD_BUG_vruntime_cmp'`。
        // gcc -O2 实测基准：strcmp("abc","abd") = -1、strcmp("abc","abc") = 0。
        "__builtin_strcmp" if argc == 2 => {
            match (literal_string_bytes(&args[0])?, literal_string_bytes(&args[1])?) {
                (Some(a), Some(b)) => Ok(Some(c_strcmp(&a, &b, None))),
                _ => Ok(None),
            }
        },
        "__builtin_strncmp" if argc == 3 => {
            let n = eval(&args[2])? as u64;
            match (literal_string_bytes(&args[0])?, literal_string_bytes(&args[1])?) {
                (Some(a), Some(b)) => Ok(Some(c_strcmp(&a, &b, Some(n as u128)))),
                _ => Ok(None),
            }
        },
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
/// 语句表达式 `({ … })` 的常量求值：把它当一段**直线程序**跑一遍。
///
/// 为什么要特意支持 `if`：内核 `__always_inline` 助手被整体搬进调用方后就是这个形状。
/// `include/linux/bitfield.h` 的 `field_multiplier()` 体是
/// ```c
/// if ((field | (field - 1)) & ((field | (field - 1)) + 1))
///         __bad_mask();
/// return field & -field;
/// ```
/// 而 `__bad_mask` / `__field_overflow` / `__bad_udelay` / `____wrong_branch_error` 在头里
/// **只有声明、没有定义**（gcc 靠「永远不发出引用」保证链得上）。旧实现只认「表达式语句 +
/// 带初值的声明」，一遇 `if` 就整体判非常量 ⇒ 外层守卫
/// `if (__builtin_constant_p(v) && (v & ~field_mask(f)))` 折不成常量 ⇒ trap 调用真发进
/// `.text`。实测 localmodconfig 全树因此留下 `__field_overflow` 21 条 / `__bad_mask` 8 条 /
/// `__bad_udelay` 3 条，全在 `LD .tmp_vmlinux1` 报 undefined reference；同配置 gcc 一条没有。
///
/// 每条判据都是「不确定就拒绝」，不做猜测：
///   * 声明：非 static；无初值的声明登记名字但不绑值（读它会因查不到而整体拒绝）；
///   * 表达式语句：形如 `x = E` 且 `x` 是本块自己声明的局部时把 E 的值绑进 env
///     （dcc 的复合赋值在 parser 已脱糖成 `x = x op E`，故无需区分算子），否则求值后丢弃；
///   * `if`：条件必须自己折得出真值，只跑被选中的那支 —— 未选那支里的 trap 调用因此
///     根本不被求值；
///   * 复合语句：进去跑一遍再出来；
///   * `goto`：只允许**向前**跳到本块顶层的标号，并且真的把执行位置改过去（被跳过的
///     赋值/调用因此不生效）。必须认这条：`__always_inline` 的包装器把 callee 体里的
///     `return E;` 发成 `__rv = E; goto __end;` + 末尾 `__end:` + 末表达式 `__rv`，不认
///     `goto` 就等于不认**全部**带提前返回的内联助手；
///   * 后跳（意味着回路）/ `return` / 循环 / `switch` / asm / Duff：拒绝。`return` 尤其
///     关键：`({ … return E … })` 里的 return 返回的是**外层函数**，把 E 当语句表达式的
///     值会造出一个错误的常量。
fn eval_stmt_expr_block(block: &Block) -> Result<ConstValue, ConstEvalError> {
    let items = &block.items;
    if items.is_empty() {
        return Ok(ConstValue::Int(0));
    }
    // Track variable bindings from declarations within the block
    let mut env: HashMap<String, ConstValue> = HashMap::new();
    // 本语句表达式自己声明的局部名：赋值只准落在它们身上（理由见 [`interp_stmt`]）。
    let mut locals: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut steps = 0_usize;
    // 顶层标号表（名字 -> 首次出现的项下标）。`goto` 只往这里查：嵌在子块里的标号
    // 作用域不覆盖本层跳转，查不到即拒绝。
    let mut top_labels: HashMap<String, usize> = HashMap::new();
    for (i, it) in items.iter().enumerate() {
        if let BlockItem::Stmt(Stmt::Label { name, .. }) = it {
            top_labels.entry(name.name.clone()).or_insert(i);
        }
    }
    // 语句表达式的值是**末项表达式语句**，但末项不能从执行序列里剥出来单独算：
    // 内联包装器把 `__end:` 放在末表达式**之前**，跳转要正好落在它身上再取值。
    // 所以这里按位置跑，跑到末项就把它当值返回。
    let value_at = items.len() - 1;
    let mut i = 0_usize;
    while i < items.len() {
        if i == value_at {
            return match &items[i] {
                BlockItem::Stmt(Stmt::Expr { e, .. }) => eval_with_env(e, &env, false),
                other => Err(se_reject("末项不是表达式语句", other)),
            };
        }
        let step = interp_block_item(&items[i], i, &mut env, &mut locals, &top_labels, false, &mut steps)?;
        i = match step {
            Step::Next => i + 1,
            // 只许前跳：后跳意味着回路，不是直线程序。
            Step::Jump(j) if j > i => j,
            Step::Jump(_) => return Err(se_reject("goto 向后跳（回路）", &items[i])),
        };
    }
    Err(se_reject("末项不可达", &items[value_at]))
}

/// [`interp_block_item`] 的执行去向。
enum Step {
    /// 顺序执行下一项
    Next,
    /// 跳到本块顶层的这个下标（内联包装器的 `goto __end;`）
    Jump(usize),
}

/// 「这条表达式被求值一次不产生任何可观察效果」的**白名单**判据，只给
/// [`interp_block_item`] 的「声明初值算不出常量」那一格用。
///
/// 为什么自己写而不复用 `semantics::expr_has_side_effects`：那边是「已知形状里有没有写
/// 动作」的口径（默认假），这里要的是「剩下的形状一律当有效果」（默认真）：内联汇编、
/// 语句表达式、`__builtin_va_arg`（每读一次推进游标）、复合字面量、`cleanup` 属性一个都不
/// 能放。宁可少放行：判不出来就是拒，代价只少折一个常量条件，不改变正确性。
fn init_is_pure_read(e: &Expr) -> bool {
    // volatile 限定类型的读本身就是可观察效果（C11 6.7.3p6）。
    if crate::ast::type_has_volatile(&e.ty()) {
        return false;
    }
    match e {
        Expr::Int { .. }
        | Expr::Char { .. }
        | Expr::Float { .. }
        | Expr::String { .. }
        | Expr::Ident { .. }
        | Expr::LabelAddr { .. }
        | Expr::SizeofTy { .. }
        | Expr::AlignofTy { .. } => true,
        // `sizeof(expr)` / `_Alignof(expr)` 的操作数不被求值（VLA 维度已经由 `ty()` 上的
        // volatile 检查拦不住，这里直接当有效果处处理）。
        Expr::SizeofExpr { .. } | Expr::AlignofExpr { .. } => false,
        Expr::Cast { e, .. }
        | Expr::Unary { e, .. }
        | Expr::AddrOf { e, .. }
        | Expr::Deref { e, .. }
        | Expr::Real { e, .. }
        | Expr::Imag { e, .. }
        | Expr::Member { expr: e, .. } => init_is_pure_read(e),
        Expr::Binop { l, r, .. } | Expr::Index { arr: l, idx: r, .. } => {
            init_is_pure_read(l) && init_is_pure_read(r)
        },
        _ => false,
    }
}

/// [`interp_stmt`] 的步数上限。
///
/// 存在理由：语句表达式可以嵌在被反复求值的折叠点里（`if` 条件、内建谓词、另一个
/// 语句表达式内部），没有上限时一个畸形大块就能把编译期推成平方级 —— 本项目有过
/// 「优化器按 trip 逐趟实体化导致挂起」的同类记录，这里一次性设闸而不是事后补。
/// 超限按非常量拒绝：语义不变，只是不再折叠。
const INTERP_MAX_STEPS: usize = 4096;

/// 报障用：语句表达式求值**为什么**拒绝。`DCC_SE_TRACE=1` 把拒绝点的形状打到 stderr。
/// 与 `crate::opt` 的 `DCC_CPROP_TRACE` 同一纪律：`OnceLock` 只读一次 env（本路径在每个
/// 折叠点都会被叫，直接 getenv 会把巨型 TU 的编译期搅乱），只读不改判据、不进产物。
fn se_trace_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("DCC_SE_TRACE").is_some())
}

/// 统一的「拒绝并留痕」出口：形状只截前 260 字符，否则一个嵌套块就能刷掉整屏。
fn se_reject(what: &str, node: &dyn std::fmt::Debug) -> ConstEvalError {
    if se_trace_on() {
        let s = format!("{node:?}");
        let cut: String = s.chars().take(260).collect();
        eprintln!("[se-fold] 拒绝于 {what}: {cut}");
    }
    ConstEvalError::NotConstant
}

/// 解释块内的一项（声明或语句）。`at` 是本项在顶层序列里的下标，供 `goto` 判前跳。
fn interp_block_item(
    item: &BlockItem,
    at: usize,
    env: &mut HashMap<String, ConstValue>,
    locals: &mut std::collections::HashSet<String>,
    labels: &HashMap<String, usize>,
    fenv_access: bool,
    steps: &mut usize,
) -> Result<Step, ConstEvalError> {
    match item {
        BlockItem::Decl(Decl::Var(v)) => {
            if v.is_static {
                return Err(se_reject("块内 static 声明", item));
            }
            // 无初值的声明**跳过而不拒绝**：`__always_inline` 助手被整体搬进来时，语句
            // 表达式的第一条就是返回值暂存槽 `u64 __inlexp9$rv;`（随后才有 `__rv = E;`）。
            // 这里不绑值是安全的：真正**读**它的那一步在 env 里查不到名字，会退回
            // [`eval_full`] 把 Ident 判成非常量 ⇒ 整体照样拒绝，不存在「拿脏值当常量」。
            // 但名字必须登记为局部：赋值到它身上才算「写在本块内」。
            let name = v.name.name.clone();
            let Some(init) = &v.init else {
                locals.insert(name);
                return Ok(Step::Next);
            };
            match eval_with_env(init, env, fenv_access) {
                Ok(val) => {
                    env.insert(name.clone(), val);
                    locals.insert(name);
                    Ok(Step::Next)
                },
                // 初值算不出来，但它是一次**纯读**（无调用/赋值/自增/内联汇编/语句表达式/
                // volatile）⇒ 这条声明只是往本块一个没人再读的槽里存了个值，丢掉它不改变
                // 可观察行为。安全性靠**不登记这个名字**兜住：后面真读它 ⇒ `eval_with_env`
                // 查不到 ⇒ 退回 `eval_full` 判非常量；真写它 ⇒ 撞「赋值目标不是本块局部」，
                // 两条都还是整块拒绝，所以放行这一格放不进「拿脏值当常量」。
                //
                // 不这样做的代价是实测的：`function_inline` 把被调方形参实体成
                // `unsigned __inl1$_n = n;`（`n` 是**外层**函数的变量，当场算不出值），
                // 于是整条 `({ …; 0; })` 被判非常量 ⇒
                // `pts_feature(&pts, PT_FEAT_DMA_INCOHERENT) && !pt_test_sw_bit_acquire(&pts, …)`
                // 塌不掉 ⇒ `drivers/iommu/generic_pt/fmt/iommu_amdv1.o` 把只有声明、没有
                // 定义的 `__pt_no_sw_bit` 留到链接期（`gcc -O2` 不留）。
                Err(_) if v.cleanup_fn.is_none() && v.register_name.is_none() && init_is_pure_read(init) => {
                    Ok(Step::Next)
                },
                Err(_) => Err(se_reject("声明初值不是常量", item)),
            }
        },
        BlockItem::Decl(_) => Err(se_reject("非变量声明", item)),
        BlockItem::Stmt(s) => interp_stmt(s, at, env, locals, labels, fenv_access, steps),
    }
}

fn interp_stmt(
    s: &Stmt,
    at: usize,
    env: &mut HashMap<String, ConstValue>,
    locals: &mut std::collections::HashSet<String>,
    labels: &HashMap<String, usize>,
    fenv_access: bool,
    steps: &mut usize,
) -> Result<Step, ConstEvalError> {
    *steps += 1;
    if *steps > INTERP_MAX_STEPS {
        return Err(se_reject("超出步数上限", s));
    }
    match s {
        Stmt::Expr { e, .. } => {
            if let Expr::Assign { l, r, ty, .. } = e {
                let val = eval_with_env(r, env, fenv_access)?;
                // 同样按左值类型收口：`_Bool` 局部在语句表达式里绑定的是
                // `(v != 0)`，不是 RHS 原值（§5.30 / C11 6.3.1.2p1）。
                let val = if matches!(ty, Type::Bool) {
                    ConstValue::Int(if truthy_value(&val)? { 1 } else { 0 })
                } else {
                    val
                };
                return match &**l {
                    // 赋值目标必须是**本语句表达式自己声明的局部**。这条不是洁癖而是
                    // 前置条件：`opt::fold_expr` 会把求值成功的语句表达式**整个换成常量**，
                    // 若这里放过写到外层变量上的赋值，那次写就随替换消失了（副作用丢了）。
                    Expr::Ident { name, .. } if locals.contains(&name.name) => {
                        env.insert(name.name.clone(), val);
                        Ok(Step::Next)
                    },
                    other => Err(se_reject("赋值目标不是本块局部", other)),
                };
            }
            eval_with_env(e, env, fenv_access)?;
            Ok(Step::Next)
        },
        Stmt::Block { b, .. } => {
            for it in &b.items {
                match interp_block_item(it, at, env, locals, labels, fenv_access, steps)? {
                    // 子块里的 `goto` 命中本块顶层标号时向上传，由调用方改执行位置。
                    Step::Jump(j) => return Ok(Step::Jump(j)),
                    Step::Next => {},
                }
            }
            Ok(Step::Next)
        },
        Stmt::If {
            cond, then_, else_, ..
        } => {
            let t = truthy_value(&eval_with_env(cond, env, fenv_access)?)?;
            if t {
                interp_stmt(then_, at, env, locals, labels, fenv_access, steps)
            } else {
                match else_ {
                    Some(b) => interp_stmt(b, at, env, locals, labels, fenv_access, steps),
                    None => Ok(Step::Next),
                }
            }
        },
        // 标号本身：继续执行它所包的语句（包装器那里通常是空语句）。
        Stmt::Label { stmt, .. } => interp_stmt(stmt, at, env, locals, labels, fenv_access, steps),
        Stmt::Goto { target, .. } => match labels.get(&target.name) {
            Some(&j) => Ok(Step::Jump(j)),
            None => Err(se_reject("goto 目标不在本块顶层", s)),
        },
        _ => Err(se_reject("不受支持的语句形状", s)),
    }
}

/// 常量值的真假（C 的「非零即真」）。返回 `None` 表示判不出来 —— 调用方必须把它
/// 当「折不动」处理，绝不能当假：当假会选中 `else` 支，当真会留下 trap 调用，
/// 两边都是静默错编。
fn truthy_value(v: &ConstValue) -> Result<bool, ConstEvalError> {
    match v {
        ConstValue::Int(i) => Ok(*i != 0),
        ConstValue::Float(_) => v
            .as_float()
            .map(|f| f != 0.0)
            .ok_or(ConstEvalError::NotConstant),
        // 地址常量/字符串字面量指向真实体，C 里恒真。
        ConstValue::Addr { .. } | ConstValue::Str { .. } | ConstValue::Bytes { .. } => Ok(true),
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
            // `&&` / `||` 必须**短路**，与 [`eval_full`] 的同一条臂一个口径（那里写得很
            // 明白：「不要求两边都是常量」）。原先这里先把两边都求完再判，于是
            //     if (__builtin_constant_p(v) && (v & ~field_mask(field)))
            // 在 `v` 不是常量时因为右边折不动而整条判成非常量；gcc 是右边根本不求值、
            // 直接得 0，死支里的 trap 调用跟着消失。求值器不短路，等于替内核多发一条
            // 永远不可能执行的调用 —— 而那些 trap 函数只有声明没有定义，链接期就爆。
            //
            // 顺带修掉一个真错：旧臂用 `as_int()` 取真假，浮点常量会被**截断成整数**，
            // `0.5 && x` 因此被判成假；现在走 [`truthy_value`]，与非零判定一致。
            if matches!(op, BinOp::AndAnd) {
                let lv = truthy_value(&eval_with_env(l, ctx, fenv_access)?)?;
                return Ok(ConstValue::Int(if lv {
                    truthy_value(&eval_with_env(r, ctx, fenv_access)?)? as i128
                } else {
                    0
                }));
            }
            if matches!(op, BinOp::OrOr) {
                let lv = truthy_value(&eval_with_env(l, ctx, fenv_access)?)?;
                return Ok(ConstValue::Int(if lv {
                    1
                } else {
                    truthy_value(&eval_with_env(r, ctx, fenv_access)?)? as i128
                }));
            }
            let lv = eval_with_env(l, ctx, fenv_access)?;
            let rv = eval_with_env(r, ctx, fenv_access)?;
            // 算术/比较统一走类型感知路径 (与 eval_binop 共用)
            arith_binop_values(*op, ty, l, r, lv, rv, fenv_access)
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
        Expr::Cast { ty, e, .. } => {
            let v = eval_with_env(e, ctx, fenv_access)?;
            // 与 eval_full 的 Cast 分支同一口径：目标 `_Bool` 做真值归一化。
            if matches!(ty, Type::Bool) {
                return Ok(ConstValue::Int(if truthy_value(&v)? { 1 } else { 0 }));
            }
            Ok(v)
        },
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

/// 表达式的**可能取值区间**（闭区间），供 [`truth_by_int_range`] 用。
///
/// 只对「值不依赖运行期状态」的形状扇出，任何一处给不出就整体放弃：区间一旦算宽了，
/// 就会把该折的比较判成「不定」，方向上是保守的；算窄了则会把活代码判成死代码，
/// 所以宁可少折一条。
fn int_range(e: &Expr) -> Option<(i128, i128)> {
    match e {
        Expr::Int { v, ty, .. } => {
            // 无符号字面量在 AST 里存的是按类型截断后的位模式（`0xffffffffffffffffUL`
            // 是 -1），那种形状给不出「数学值」——放弃而不是猜。
            if is_unsigned_int_ty(ty) && *v < 0 {
                return None;
            }
            Some((*v, *v))
        },
        Expr::Char { v, .. } => Some((*v, *v)),
        Expr::Unary {
            op: UnOp::Plus, e, ..
        } => int_range(e),
        Expr::Unary {
            op: UnOp::Neg, e, ..
        } => {
            let (lo, hi) = int_range(e)?;
            Some((hi.checked_neg()?, lo.checked_neg()?))
        },
        // `(X ? a : b)`：值**必是** a 或 b 之一，取并集就不必知道 X。
        // 这一条是本族的支点：`MAX_PHYSMEM_BITS` 就是
        // `(pgtable_l5_enabled() ? 52 : 46)`，X 内联后是带 `asm goto` 的语句表达式，
        // 常量求值器判不出真值，但两臂的值都定得下来。
        Expr::Cond { then_, else_, .. } => {
            let (al, ah) = int_range(then_)?;
            let (bl, bh) = int_range(else_)?;
            Some((al.min(bl), ah.max(bh)))
        },
        Expr::Binop { op, l, r, .. } => {
            let (ll, lh) = int_range(l)?;
            let (rl, rh) = int_range(r)?;
            match op {
                BinOp::Add => Some((ll.checked_add(rl)?, lh.checked_add(rh)?)),
                BinOp::Sub => Some((ll.checked_sub(rh)?, lh.checked_sub(rl)?)),
                BinOp::Mul => {
                    let cands = [
                        ll.checked_mul(rl)?,
                        ll.checked_mul(rh)?,
                        lh.checked_mul(rl)?,
                        lh.checked_mul(rh)?,
                    ];
                    Some((*cands.iter().min()?, *cands.iter().max()?))
                },
                // 移位只接「移位量单值且在 0..128」的形状：否则两端会跨过位宽边界，
                // 那是 UB 区间，按数学意义算出来的界不再覆盖所有实际取值。
                BinOp::Shl if rl == rh && (0..128).contains(&rl) => {
                    Some((ll.checked_shl(rl as u32)?, lh.checked_shl(rl as u32)?))
                },
                _ => None,
            }
        },
        // `(a, b)` 的值就是 b
        Expr::Comma { r, .. } => int_range(r),
        _ => None,
    }
}

/// 比较运算的「区间判据」：两侧都折不出具体值，但**值域**足以定案时返回结果。
///
/// 为什么需要它（`mm/vmstat.o` 2 条 + `fs/proc/task_mmu.o` 1 条
/// `__compiletime_assert_NNN` 的根因）：
/// ```c
/// /* arch/x86/include/asm/sparsemem.h:29 */
/// #define MAX_PHYSMEM_BITS (pgtable_l5_enabled() ? 52 : 46)
/// /* include/linux/swapops.h:36 */
/// #define SWP_PFN_BITS     (MAX_PHYSMEM_BITS - PAGE_SHIFT)
/// BUILD_BUG_ON(SWP_TYPE_SHIFT < SWP_PFN_BITS)   /* 58 < (X ? 40 : 34) 恒假 */
/// ```
/// `pgtable_l5_enabled()` 是 `__always_inline`，内联后长成带 `asm goto` 的语句表达式，
/// [`eval_full`] 判不出真值 ⇒ 整条比较判不出 ⇒ `if (!(!(...)))` 留在产物里 ⇒
/// 链接期 `undefined reference`。GCC 是按值域消掉的：不需要知道 X。
fn cmp_by_int_range(op: BinOp, l: &Expr, r: &Expr) -> Option<bool> {
    if !matches!(
        op,
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::EqEq | BinOp::Ne
    ) {
        return None;
    }
    let (ll, lh) = int_range(l)?;
    let (rl, rh) = int_range(r)?;
    // 无符号且区间含负值：i128 的有序性不再对应该类型的序，放弃。
    if (is_unsigned_int_ty(&l.ty()) || is_unsigned_int_ty(&r.ty())) && (ll < 0 || rl < 0) {
        return None;
    }
    match op {
        BinOp::Lt => {
            if lh < rl {
                Some(true)
            } else if ll >= rh {
                Some(false)
            } else {
                None
            }
        },
        BinOp::Le => {
            if lh <= rl {
                Some(true)
            } else if ll > rh {
                Some(false)
            } else {
                None
            }
        },
        BinOp::Gt => {
            if ll > rh {
                Some(true)
            } else if lh <= rl {
                Some(false)
            } else {
                None
            }
        },
        BinOp::Ge => {
            if ll >= rh {
                Some(true)
            } else if lh < rl {
                Some(false)
            } else {
                None
            }
        },
        BinOp::EqEq => {
            if ll == lh && rl == rh {
                Some(ll == rl)
            } else if lh < rl || rh < ll {
                Some(false)
            } else {
                None
            }
        },
        BinOp::Ne => {
            if ll == lh && rl == rh {
                Some(ll != rl)
            } else if lh < rl || rh < ll {
                Some(true)
            } else {
                None
            }
        },
        _ => None,
    }
}

/// 表达式在 C 的真值意义下是不是**恒定**的非零/零，只看得到值域就能定案时返回。
///
/// 只翻这几形：整数常量、`!`（`BUILD_BUG_ON` 展开成 `if (!(!(cond)))`，两层都得翻得过去）、
/// 逗号（值只看右边）与比较。其它形状一律「不知道」——调用方据此**保留原条件求值**
/// （见 `opt::fold_branches_block`），所以这里回答 false 不会改变任何求值。
pub fn truth_by_int_range(e: &Expr) -> Option<bool> {
    match e {
        Expr::Int { v, .. } => Some(*v != 0),
        Expr::Char { v, .. } => Some(*v != 0),
        Expr::Unary {
            op: UnOp::Not, e: x, ..
        } => truth_by_int_range(x).map(|b| !b),
        Expr::Comma { r, .. } => truth_by_int_range(r),
        Expr::Binop { op, l, r, .. } => cmp_by_int_range(*op, l, r),
        _ => None,
    }
}

/// 左值是不是位域成员（`s.m` / `p->m` / `arr[i].m`；嵌套成员只取最外层字段，
/// 内层基类型已经是那条 Member 的 `ty()`，与 `expr_bitfield_width` 同一判法）。
fn lvalue_is_bitfield_member(e: &Expr) -> bool {
    let Expr::Member {
        expr: inner, field, ..
    } = e
    else {
        return false;
    };
    let inner_ty = inner.ty();
    let struct_ty = match &inner_ty {
        Type::Ptr(p) => &p.base,
        Type::Array(elem, _) => &elem.base,
        other => other,
    };
    match struct_ty {
        Type::Struct(_, fields, _, _) | Type::Union(_, fields, _, false) => find_field(fields, &field.name)
            .map(|(f, _)| f.bit_width.is_some())
            .unwrap_or(false),
        _ => false,
    }
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

/// 类型感知的整数比较: 在【公共类型】里求值 (C11 6.3.1.8)。
///
/// 公共类型由 `ast::Type::usual_arith_type` 给出 —— 与前端类型推导、SSA→AST
/// 重建、codegen 共用同一真源, 其中已含整数提升 (6.3.1.1p2): `UChar`/`UShort` 提升
/// 为 `int` 后自然留在有符号侧, 不需要另列例外。比较结果本身是 `int`, 与公共类型
/// 无关, 故不能像算术臂那样直接取 `binop_ty`。非整数对回退到 f64 比较。
fn cmp_binop_values<F: Fn(i128, i128) -> i128>(
    l: &Expr,
    r: &Expr,
    lv: ConstValue,
    rv: ConstValue,
    f: F,
) -> Result<ConstValue, ConstEvalError> {
    if let (&ConstValue::Int(a), &ConstValue::Int(b)) = (&lv, &rv) {
        let common = l.ty().usual_arith_type(&r.ty());
        // 先按各自类型取回常量真值 (`0U - 1` 的 sub_values 结果是未截断的 -1,
        // 规范化到 UInt 才回到 4294967295), 再截断到公共类型宽度: `-1`(Int)
        // 在 ULong 上下文里是 0xFFFFFFFFFFFFFFFF, 而不是 -1。
        let a = normalize_int_value(normalize_int_value(a, &l.ty()), &common);
        let b = normalize_int_value(normalize_int_value(b, &r.ty()), &common);
        let unsigned_cmp = is_unsigned_int_ty(&common);
        let res = if unsigned_cmp {
            f(unsigned_order(a), unsigned_order(b))
        } else {
            f(a, b)
        };
        return Ok(ConstValue::Int(res));
    }
    cmp_values(lv, rv, |x, y| f(x as i128, y as i128))
}

/// 把「按无符号解释的 i128 位模式」单调嵌入到 i128 的全体序, 使调用方可以复用
/// 同一个 `Fn(i128, i128)` 比较器。
///
/// 旧写法 `f(a as u128 as i128, b as u128 as i128)` 是**往返恒等** —— Rust 的
/// `i128 as u128` 保留位模式、`u128 as i128` 再原样取回, 负数还是那个负数,
/// 于是「重解释为无符号」完全没生效, `-1 > 0xffffffffUL` 被折成 0 (gcc 给 1)。
/// 公共类型宽度 < 128 时上一步截断已保证两值非负, 直接比也对; 但 `UInt128`
/// 的常量在 i128 里本就以补码负数承载, 只有异或最高位这一种映射同时对两类
/// 宽度都保序。
fn unsigned_order(v: i128) -> i128 {
    ((v as u128) ^ (1u128 << 127)) as i128
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

/// 在 f64 域折叠两个浮点常量。任一操作数为 long double 或 binary128 时拒绝折叠：
/// 宿主无 80-bit / 128-bit 运算，f64 折叠会静默改变 `0.1L` / `0.1q` 等的值并
/// 把 113 位尾数截到 53 位，必须留给后端在运行时用 x87 / libgcc TF 例程求值。
/// float 在 C 常规转换中本就提升为 double，故折叠结果统一为 F64。
fn fold_float<F>(
    a: FloatConst,
    b: FloatConst,
    fenv_access: bool,
    f: F,
) -> Result<ConstValue, ConstEvalError>
where
    F: FnOnce(f64, f64) -> f64,
{
    if fenv_access || a.is_long_double() || b.is_long_double() || a.is_float128() || b.is_float128()
    {
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
            if fenv_access
                || a.is_long_double()
                || b.is_long_double()
                || a.is_float128()
                || b.is_float128()
            {
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
    // LongDouble / Float128 不做编译期比较折叠: 宿主无 80-bit / 128-bit
    // 运算, 降级到 f64 会把仅尾数低位不同的两个值判成相等 (与解释器
    // 对该类型的真实比较结果相反)。与 fold_float / mod_values 同口径。
    let wide = |v: &ConstValue| match v {
        ConstValue::Float(c) => c.is_long_double() || c.is_float128(),
        _ => false,
    };
    if wide(&l) || wide(&r) {
        return Err(ConstEvalError::NotConstant);
    }
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
    // 后缀先摘，再按进制解析：十六/二/八进制分支把整串交给 `from_str_radix`，
    // 而 `0x1u` 里的 `u` 不是十六进制数字 —— 旧实现只在十进制分支摘后缀，因此
    // `eval_int_lit("0x11223344u")` 返回 None。parser 的 `__builtin_bswap*` /
    // `__builtin_abs` 直折块又先 `advance()` 再折，折空就把 `;` 当成表达式开头，
    // 报 `E0006 expected expression, got Semi`（§5.58 附带修复）。
    let s = strip_int_suffix(s);
    if let Some(rest) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u128::from_str_radix(rest, 16).ok().map(|v| v as i128)
    } else if let Some(rest) = s.strip_prefix("0b").or_else(|| s.strip_prefix("0B")) {
        u128::from_str_radix(rest, 2).ok().map(|v| v as i128)
    } else if s.starts_with("0") && s.len() > 1 {
        u128::from_str_radix(&s[1..], 8).ok().map(|v| v as i128)
    } else {
        s.parse::<i128>().ok()
    }
}

/// __builtin_offsetof(type, member) 编译期偏移
///
/// 兼容旧调用点的形状：折不动就返回 0（与历史行为逐字相同）。需要区分
/// 「下标不是整型常量表达式」的调用点请用 [`Self::eval_offsetof_opt`]。
pub fn eval_offsetof(ty: &Type, member: &str) -> usize {
    eval_offsetof_opt(ty, member).unwrap_or(0)
}

/// `__builtin_offsetof(type, member)` 的编译期偏移。
///
/// 路径是 `.` 连接的 member-designator，**每一段都能带任意个 `[常量]` 下标**
/// （C11 6.7.9p6；内核大量使用，如 `drivers/tty/serial/serial_core.c:3581` 的
/// `offsetof(struct serial_rs485, padding[1])`）。旧实现把 `[...]` 截掉不看，于是
/// 下标一律算成首元素偏移（上面那条给 12 而不是 16），`_Static_assert` 假失败
/// （实测 W0010，gcc 同文件 0 条）。另一处旧行为是把**顶层联合体**直接返回 0，
/// 而联合体各成员偏移本就是 0，与结构体走同一条字段下降路径即可。
///
/// 只在「下标存在但读不出整型常量」时返回 `None`（那按 C 本来就不是合法的
/// offsetof，上层据此报非常量，而不是悄悄折成一个偏小的数）；找不到字段仍然
/// 按旧行为**就地停下**并返回已累加到的偏移，不给新的错误路径让已有调用点改行为。
pub fn eval_offsetof_opt(ty: &Type, member: &str) -> Option<usize> {
    // 字段表一律拿副本：这一步只在编译期跑几次 offsetof，不值得为借用生命周期
    // 再把函数拆成两段。
    let mut current_fields: Vec<crate::ast::Field> = match ty {
        Type::Struct(_, f, _, _) | Type::Union(_, f, _, _) => f.as_ref().clone(),
        _ => return Some(0),
    };
    let parts: Vec<&str> = member.split('.').collect();
    let mut offset = 0usize;
    for (i, part) in parts.iter().enumerate() {
        let (name, subs) = split_offsetof_part(part);
        let Some((field, inner_off)) = find_field(&current_fields, name).map(|(f, o)| (f.clone(), o))
        else {
            break;
        };
        offset += field.offset + inner_off;
        let mut cur_ty = field.ty;
        for sub in subs {
            let elem = match &cur_ty {
                Type::Array(inner, _) => inner.base.clone(),
                // 不是数组却带下标：这形状旧实现也是直接丢掉下标，保持不前进。
                _ => break,
            };
            let idx = offsetof_subscript_index(sub)?;
            offset += (idx as usize).wrapping_mul(elem.size());
            cur_ty = elem;
        }
        if i + 1 < parts.len() {
            current_fields = match &cur_ty {
                Type::Struct(_, f, _, _) | Type::Union(_, f, _, _) => f.as_ref().clone(),
                _ => break,
            };
        }
    }
    Some(offset)
}

/// 把 `offsetof` 路径中的一段拆成「字段名 + 若干下标」：`padding[1]` →
/// `("padding", ["1"])`，`m[1][2]` → `("m", ["1", "2"])`，下标里还能套括号。
fn split_offsetof_part(part: &str) -> (&str, Vec<&str>) {
    let Some(bp) = part.find('[') else {
        return (part, Vec::new());
    };
    let name = &part[..bp];
    let b = part.as_bytes();
    let mut subs = Vec::new();
    let mut i = bp;
    while i < b.len() {
        if b[i] != b'[' {
            i += 1;
            continue;
        }
        let start = i + 1;
        let mut d = 1_i32;
        let mut j = i + 1;
        while j < b.len() {
            match b[j] {
                b'[' => d += 1,
                b']' => {
                    d -= 1;
                    if d == 0 {
                        break;
                    }
                },
                _ => {},
            }
            j += 1;
        }
        if d != 0 {
            break; // 括号不配对：剩下的原样丢掉，不猜
        }
        subs.push(&part[start..j]);
        i = j + 1;
    }
    (name, subs)
}

/// 下标文本 → 整型值。先试单个字面量（能处理 `0x10`、`2UL` 这类后缀），
/// 不行再交给已有的纯算术折叠（`+ - * ( )`，内联汇编位移那套同一工具），
/// 不另开第三个算术子集。
fn offsetof_subscript_index(s: &str) -> Option<i64> {
    let t = s.trim();
    if let Some(v) = eval_int_lit(t) {
        return i64::try_from(v).ok();
    }
    crate::codegen::x86_64::Codegen::eval_pure_numeric_arith(t)
}

pub fn is_constant(expr: &Expr) -> bool {
    // 函数设计符（`func` 不带 `&`）在 C 里就是一个链接期地址常量：内核
    // `arch/x86/include/asm/irq_stack.h` 的 `call_on_stack(func, ...)` 传的是
    // `[__func] "i" (func)`，GCC 在这里折成符号名后发 `call handle_stack_overflow`。
    // 只看 `eval`（返回 i128）会把它当成非常量，于是只立即数约束硬失败。
    if matches!(
        expr,
        Expr::Ident {
            ty: crate::ast::Type::Func(..),
            ..
        }
    ) {
        return true;
    }
    eval(expr).is_ok()
}

/// `__builtin_constant_p(x)` 专用的判据：比 `is_constant` 多算一类 —— **字符串字面量**。
///
/// 为什么不直接改 `is_constant`：那个函数还喂给内联汇编的立即数约束与 `_Static_assert`，
/// 那边要的确实是「能算出一个数值」；字符串是链接期地址，算不出 `i128`，把它当常量会
/// 让那些消费点在“拿值”的那一步硬失败。而 gcc 对 `__builtin_constant_p("hi")` 给 1
/// （字面量就是一个静态对象地址），实测差这一条直接打坏 gcc-torture 的 `bcp-1.c`。
pub fn is_constant_p_arg(expr: &Expr) -> bool {
    matches!(expr, Expr::String { .. }) || is_constant(expr)
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
    // `&obj` 要的就是 `obj` 这个对象的大小；裸指针/裸变量身上没有「对象大小」可言，
    // 两者必须分开问（`addressed`）。强制转换只是个壳，剥掉重问即可。
    match ptr {
        Expr::AddrOf { e, .. } => object_size_of(e, mode, true),
        Expr::Cast { e, .. } => object_size(e, type_arg),
        other => object_size_of(other, mode, false),
    }
}

/// `addressed` = 传进来的这个表达式就是 `__builtin_object_size(&…)` 里被取地址的**对象**，
/// 而不是一个指针值。
fn object_size_of(expr: &Expr, mode: i128, addressed: bool) -> Option<i128> {
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
            // 【未知必须答未知】指针形参身上没有对象大小可言：gcc 对「未定指针」
            // `p` 的 `__builtin_object_size(p, 0)` 答案是 `(size_t)-1`。
            //
            // 这里以前对**任何**非数组 Ident 都答 `ty.size()`，于是 `const void *p`
            // 得到 8 —— 与 gcc 正好相反。连锁后果很实在：
            // `include/linux/ucopysize.h` 的 `check_copy_size()` 里
            //     if (unlikely(sz >= 0 && sz < bytes)) { … __bad_copy_from(); … }
            // `sz` 恒 >= 0，条件便永远为真，死块里的 `__bad_copy_from()` /
            // `__bad_copy_to()`（头里只有 `extern void __compiletime_error(...)` 的声明，
            // 定义根本不存在，gcc 靠「永远不发出引用」保证）被每个 uaccess 调用点发出，
            // 实测 `kernel/sys.o`、`io_uring/io_uring.o` 等 116 个 TU 共 232 条链接期
            // undefined reference。
            _ if addressed => Some(ty.size() as i128),
            _ => None,
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
            // `arr` 是「被下标的那个数组/指针」，不是对象本身（数组 Ident 走上面的
            // Array 支，与 `addressed` 无关）。
            let parent = object_size_of(arr, mode, false)?;
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
        // `*p` 是「指针值」，退回到「不知道它背后有多大」（gcc 同样答 -1）；
        // 只有显式 `&…` 才是「对象」。
        Expr::Deref { e, .. } => object_size_of(e, mode, false),
        // Transparent wrappers: cast and address-of do not change object size
        Expr::Cast { e, .. } => object_size_of(e, mode, addressed),
        Expr::AddrOf { e, .. } => object_size_of(e, mode, true),
        // Compound literal: size is the type size
        Expr::CompoundLit { ty, .. } => Some(ty.size() as i128),
        // Pointer arithmetic: (ptr + offset) retains base object size
        // Only when the left operand is actually a pointer type
        Expr::Binop {
            op: BinOp::Add, l, ..
        } if matches!(l.ty(), Type::Ptr(_)) => object_size_of(l, mode, false),
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

    /// 带类型的整型字面量（比较的符号性由**两个操作数**的公共类型决定，
    /// 所以构造测例必须能分别指定左右类型，不能共用 `int()` 助手）。
    fn typed(v: i128, ty: Type) -> Expr {
        Expr::Int {
            v,
            span: Span::point("t.c", 1, 1),
            ty,
        }
    }

    /// 折叠一条比较：外层 `Expr::Binop.ty` 故意给 `Int`（C 的比较结果恒为 `int`），
    /// 以此钉住「折叠器不能拿结果类型当符号性依据」这一点。
    fn fold_cmp(op: BinOp, l: (i128, Type), r: (i128, Type)) -> ConstValue {
        let e = Expr::Binop {
            op,
            l: Box::new(typed(l.0, l.1)),
            r: Box::new(typed(r.0, r.1)),
            span: Span::point("t.c", 1, 1),
            ty: Type::Int,
        };
        eval_full(&e, false).expect("比较应可折叠")
    }

    #[test]
    fn cmp_folds_in_usual_arith_type_of_operands() {
        // 同宽混合符号（LP64: `unsigned long` 秩 4 / `long long` 秩 5 但同为 64 位）
        // ⇒ `long long` 表示不了 [2^63, 2^64) ⇒ 公共类型 `unsigned long long` ⇒ -1 变最大值。
        // 旧判据「任一操作数自身无符号」也会选到无符号，但 `a as u128 as i128` 是
        // 往返恒等（负数原样带回），两者任修其一都仍是 0。
        assert_eq!(fold_cmp(BinOp::Gt, (-1, Type::LongLong), (1, Type::ULong)), ConstValue::Int(1));
        assert_eq!(fold_cmp(BinOp::Lt, (-1, Type::Int), (1, Type::ULong)), ConstValue::Int(0));
        assert_eq!(
            fold_cmp(BinOp::Ge, (-1, Type::Int), (0xffffffff, Type::ULong)),
            ConstValue::Int(1)
        );
        assert_eq!(
            fold_cmp(BinOp::Le, (-1, Type::Int), (0xffffffff, Type::ULong)),
            ConstValue::Int(0)
        );

        // 反向探针：有符号侧**确实更宽**时必须停在有符号类型 —— 旧判据在这里翻车
        // （`1U` 自身无符号 ⇒ 走无符号域），当时没暴走纯属往返恒等把两个负数都原样
        // 退回有符号比较。`-1L > 1U` 的公共类型是 `long` ⇒ 0。
        assert_eq!(fold_cmp(BinOp::Gt, (-1, Type::Long), (1, Type::UInt)), ConstValue::Int(0));
        assert_eq!(fold_cmp(BinOp::Gt, (-1, Type::LongLong), (1, Type::UInt)), ConstValue::Int(0));
        assert_eq!(fold_cmp(BinOp::Gt, (1, Type::UInt), (-1, Type::LongLong)), ConstValue::Int(1));

        // 整数提升 (6.3.1.1p2)：`unsigned char`/`unsigned short` 提升为 `int` 后
        // 自然留在有符号侧，不需要另列例外 —— 正如 gcc: `(unsigned char)200 < -1` 为假。
        assert_eq!(fold_cmp(BinOp::Lt, (200, Type::UChar), (-1, Type::Int)), ConstValue::Int(0));
        assert_eq!(fold_cmp(BinOp::Gt, (200, Type::UShort), (-1, Type::Int)), ConstValue::Int(1));

        // 同类型无符号：按值比（顺带钉住「非负值不受映射影响」）。
        assert_eq!(
            fold_cmp(BinOp::Gt, (0xffffffff, Type::UInt), (1, Type::UInt)),
            ConstValue::Int(1)
        );
    }

    #[test]
    fn cmp_folds_left_operand_in_its_own_width_first() {
        // `0U - 1` 的子表达式结果是**未截断的** -1，只有按 `unsigned int` 规范化才回到
        // 4294967295；因此比较要先把两侧各自规范化到自身类型，再截断到公共类型。
        let sub = Expr::Binop {
            op: BinOp::Sub,
            l: Box::new(typed(0, Type::UInt)),
            r: Box::new(typed(1, Type::UInt)),
            span: Span::point("t.c", 1, 1),
            ty: Type::UInt,
        };
        let e = Expr::Binop {
            op: BinOp::Gt,
            l: Box::new(sub),
            r: Box::new(typed(0, Type::Int)),
            span: Span::point("t.c", 1, 1),
            ty: Type::Int,
        };
        assert_eq!(eval_full(&e, false), Ok(ConstValue::Int(1)));
    }

    #[test]
    fn unsigned_order_preserves_full_u128_ordering() {
        // `unsigned_order` 必须是对「按无符号解释的位模式」的单调嵌入：
        // 窄宽度（上一步已截断 ⇒ 非负）与 `UInt128`（在 i128 里以补码负数承载）
        // 共用同一个比较器，只有异或最高位这一种映射对两类宽度同时保序。
        let samples: [i128; 9] = [
            0,
            1,
            -1,
            i128::MIN,
            i128::MIN + 1,
            i128::MAX,
            i128::MAX - 1,
            1i128 << 126,
            -(1i128 << 126),
        ];
        for a in samples {
            for b in samples {
                let u = |v: i128| v as u128;
                assert_eq!(
                    u(a) < u(b),
                    unsigned_order(a) < unsigned_order(b),
                    "保序破坏于 {a} vs {b}"
                );
                assert_eq!(
                    u(a) == u(b),
                    unsigned_order(a) == unsigned_order(b),
                    "相等性破坏于 {a} vs {b}"
                );
            }
        }
    }

    /// `eval_int_lit` 必须在**所有进制**上先摘后缀再解析：旧实现只在十进制
    /// 分支摘，十六进制的 `0x11223344u` 直接返回 None，parser 的
    /// `__builtin_bswap*` 直折因此落空并把 `)` 吃掉了（§5.58 附带修复）。
    #[test]
    fn eval_int_lit_strips_suffix_in_every_radix() {
        assert_eq!(super::eval_int_lit("0x11223344u"), Some(0x11223344));
        assert_eq!(super::eval_int_lit("0xC03B3998U"), Some(0xc03b3998));
        assert_eq!(super::eval_int_lit("0x11223344ul"), Some(0x11223344));
        assert_eq!(super::eval_int_lit("0x11223344ULL"), Some(0x11223344));
        assert_eq!(super::eval_int_lit("0x11223344"), Some(0x11223344));
        assert_eq!(super::eval_int_lit("0b1010u"), Some(10));
        assert_eq!(super::eval_int_lit("0777u"), Some(511));
        assert_eq!(super::eval_int_lit("42u"), Some(42));
        // 十六进制数字里的 b/e/f 不能被当成后缀剥掉。
        assert_eq!(super::eval_int_lit("0xbeeff"), Some(0xbeeff));
        assert_eq!(super::eval_int_lit("0x123wb"), Some(0x123));
    }
}
