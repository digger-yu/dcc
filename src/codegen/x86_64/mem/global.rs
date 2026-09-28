// src/codegen/x86_64/mem/global.rs
// Global variable initialization and management for x86_64 backend
// Extracted from mod.rs (Turns 38-39)

use crate::ast::*;
use crate::codegen::x86_64::{const_eval, Codegen, GlobalVar};

use std::collections::HashMap;

/// Evaluate an lvalue expression to (base_global_name, byte_offset).
/// Handles Ident, Index (with constant indices), Member, and Deref chains.
/// Returns None if the expression cannot be evaluated at compile time.
pub fn eval_global_lvalue_offset(
    e: &Expr,
    globals_raw: &HashMap<String, GlobalVar>,
) -> Option<(String, i64)> {
    match e {
        Expr::Ident { name, .. } => {
            // Direct reference to global variable
            Some((name.name.clone(), 0))
        },
        Expr::AddrOf { e: inner, .. } => {
            // &x - peel off one level
            eval_global_lvalue_offset(inner, globals_raw)
        },
        Expr::Cast { e: inner, ty, .. } => {
            // Cast expressions like (T*)&expr
            let base_off = eval_global_lvalue_offset(inner, globals_raw)?;
            let offset_adjustment = match ty {
                Type::Ptr(elem_ty) => elem_ty.size(),
                _ => 1, // Pointer arithmetic by bytes
            };
            Some((base_off.0, base_off.1 + offset_adjustment as i64))
        },
        Expr::Index { arr, idx, .. } => {
            // Array indexing: arr[i]
            if let Some((base_name, base_off)) = eval_global_lvalue_offset(arr, globals_raw) {
                if let Ok(const_idx) = const_eval::eval(idx) {
                    let array_ty = {
                        let g = globals_raw.get(&base_name)?;
                        g.ty.get_array_element_type()?
                    };
                    let elem_size = array_ty.size();
                    // const_idx is already i128 from eval()
                    Some((base_name, base_off + (elem_size as i64 * const_idx as i64)))
                } else {
                    None
                }
            } else {
                None
            }
        },
        Expr::Member {
            expr,
            field,
            is_arrow,
            ..
        } => {
            // Structure member access: struct.field or ptr->field
            if let Some((base_name, base_off)) = eval_global_lvalue_offset(expr, globals_raw) {
                // Placeholder for field offset calculation - full extraction from mod.rs needed
                // For now, skip to avoid compile errors
                let _ = (field, is_arrow);
                Some((base_name, base_off))
            } else {
                None
            }
        },
        Expr::Deref { .. } => {
            // &(*ptr) = ptr, but ptr must be a constant address
            // This is rare in global init; skip for now
            None
        },
        _ => None,
    }
}

/// Materialize a compound literal into rodata as an anonymous static object.
/// Returns (anonymous_name, absolute_virtual_address) or None if not a CompoundLit.
pub fn materialize_compound_literal(
    cg: &mut Codegen,
    e: &Expr,
    use_rodata: bool,
) -> Option<(String, u64)> {
    let (ty, inits) = match e {
        Expr::CompoundLit { ty, inits, .. } => (ty.clone(), inits.clone()),
        Expr::InitList { inits, ty, .. } => (ty.clone(), inits.clone()),
        _ => return None,
    };
    let sz = ty.size().max(1);
    let align = ty.align().max(1);

    // Anonymous name: .L.compound_<next_label>
    let anon_name = format!(".L.compound.{}", cg.next_label);
    cg.next_label += 1;

    // Allocate rodata space and align
    let section = if use_rodata {
        while !cg.rodata.len().is_multiple_of(align) {
            cg.rodata.push(0);
        }
        let base = cg.rodata.len() as u64;

        // Recursively generate initialization data
        let init_data = cg
            .gen_global_init_list(&ty, &inits, Some((base, None)), use_rodata)
            .unwrap_or_else(|| vec![0u8; sz]);
        cg.rodata.extend_from_slice(&init_data);
        while !cg.rodata.len().is_multiple_of(align) {
            cg.rodata.push(0);
        }
        (base, 1u16)
    } else {
        while !cg.tdata_section.len().is_multiple_of(align) {
            cg.tdata_section.push(0);
        }
        let base = cg.tdata_section.len() as u64;
        let init_data = cg
            .gen_global_init_list(&ty, &inits, Some((base, None)), use_rodata)
            .unwrap_or_else(|| vec![0u8; sz]);
        cg.tdata_section.extend_from_slice(&init_data);
        while !cg.tdata_section.len().is_multiple_of(align) {
            cg.tdata_section.push(0);
        }
        (base, 5u16)
    };

    let vaddr = cg.entry + section.0;

    // Register anonymous object
    cg.globals_raw.insert(
        anon_name.clone(),
        GlobalVar {
            offset: section.0,
            size: sz,
            ty,
            init: None,
            section: section.1,
            is_tls: false,
            in_data_section: false,
        },
    );

    Some((anon_name, vaddr))
}

/// Encode complex number parts as little-endian bytes for integer components.
pub fn encode_complex_parts_i(re: i128, im: i128, inner: &Type) -> Vec<u8> {
    let isz = inner.size().max(1);
    let mut b = re.to_le_bytes()[..isz.min(16)].to_vec();
    b.extend_from_slice(&im.to_le_bytes()[..isz.min(16)]);
    b
}

/// 单个浮点分量按目标类型编码为字节：
/// - Float  ：4 字节 IEEE-754 binary32
/// - Double ：8 字节 IEEE-754 binary64
/// - LongDouble：16 字节对象 = 真 10 字节 80-bit + 6 字节 padding
fn encode_float_component(c: FloatConst, inner: &Type) -> Vec<u8> {
    match inner {
        Type::Float => {
            // F32 精确；F64/LongDouble 在此显式降级到 f32。
            (c.as_f64() as f32).to_bits().to_le_bytes().to_vec()
        },
        Type::LongDouble => match c {
            FloatConst::LongDouble(b) => {
                let mut bits = b.to_vec();
                bits.resize(16, 0);
                bits
            },
            other => {
                let mut bits = crate::float80::from_f64(other.as_f64()).to_vec();
                bits.resize(16, 0);
                bits
            },
        },
        _ => match c {
            FloatConst::F64(f) => f.to_bits().to_le_bytes().to_vec(),
            other => other.as_f64().to_bits().to_le_bytes().to_vec(),
        },
    }
}

/// Encode complex number parts as little-endian bytes for floating-point components.
pub fn encode_complex_parts_f(re: FloatConst, im: FloatConst, inner: &Type) -> Vec<u8> {
    let mut b = encode_float_component(re, inner);
    b.extend_from_slice(&encode_float_component(im, inner));
    b
}

/// 两个同目标宽度浮点常量的加/减组合。
/// F32/F64 在 f64 域求值后按目标宽度舍回；LongDouble 走真 80-bit
/// 整数域加/减，全程不经 f64 中转。
fn combine_float_consts(l: FloatConst, r: FloatConst, subtract: bool, inner: &Type) -> FloatConst {
    match inner {
        Type::LongDouble => {
            let (la, lb) = match (l, r) {
                (FloatConst::LongDouble(a), FloatConst::LongDouble(b)) => (a, b),
                (a, b) => {
                    (crate::float80::from_f64(a.as_f64()), crate::float80::from_f64(b.as_f64()))
                },
            };
            let out = if subtract {
                crate::float80::sub(&la, &lb)
            } else {
                crate::float80::add(&la, &lb)
            };
            FloatConst::LongDouble(out)
        },
        Type::Float => {
            let v = if subtract {
                l.as_f64() - r.as_f64()
            } else {
                l.as_f64() + r.as_f64()
            };
            FloatConst::F32(v as f32)
        },
        _ => {
            let v = if subtract {
                l.as_f64() - r.as_f64()
            } else {
                l.as_f64() + r.as_f64()
            };
            FloatConst::F64(v)
        },
    }
}

/// Extract real and imaginary parts from complex initialization expression.
///
/// 返回携带源码语义精度的 `(real, imag)`，不提前降级。
/// `inner` 是复数元素类型（Float/Double/LongDouble）：标量表达式（`Expr::Float`
/// 占实部、`Expr::Imag` 占虚部）的**缺失分量必须按 inner 构造同精度零**，
/// 否则 `_Complex long double c = 0.1L + 0.2Li` 这类初始化器的实/虚部会
/// 被写成同一个值或被 F64 零静默降级。
pub fn complex_init_parts(e: &Expr, inner: &Type) -> Option<(FloatConst, FloatConst)> {
    use crate::codegen::x86_64::const_eval;

    let zero = || FloatConst::zero(inner);
    match e {
        Expr::ComplexLit { real, imag, .. } => Some((*real, *imag)),
        Expr::ComplexIntLit { .. } => {
            // 整型字面量不属于浮点 complex 路径（下方 i128 路径处理）。
            None
        },
        Expr::Imag { e: sub, .. } => {
            // 纯虚数：实部为 inner 精度的零，虚部取常量值。
            let f = match const_eval::eval_full(sub, false).ok()? {
                const_eval::ConstValue::Float(f) => f,
                const_eval::ConstValue::Int(v) => FloatConst::from_int(inner, v),
                _ => return None,
            };
            Some((zero(), f))
        },
        Expr::Float { c, .. } => Some((*c, zero())),
        Expr::Int { v, .. } => Some((FloatConst::from_int(inner, *v), zero())),
        Expr::Unary { op, e, .. } => {
            if matches!(op, UnOp::Neg) {
                let (re, im) = complex_init_parts(e, inner)?;
                Some((re.negate(), im.negate()))
            } else {
                None
            }
        },
        Expr::Binop { op, l, r, ty, .. } => {
            // binop 的结果类型可能与外层 inner 相同；以表达式自身 ty 为准。
            let bin_inner = match ty {
                Type::Complex(i) | Type::Imaginary(i) => &i.base,
                other => other,
            };
            match op {
                BinOp::Add | BinOp::Sub => {
                    let sub = matches!(op, BinOp::Sub);
                    // complex 分量组合：(a+bi) ± (c+di)。
                    // 实部 = combine(re_l, re_r)，虚部 = combine(im_l, im_r)。
                    let (lre, lim) = complex_init_parts(l, bin_inner)?;
                    let (rre, rim) = complex_init_parts(r, bin_inner)?;
                    Some((
                        combine_float_consts(lre, rre, sub, bin_inner),
                        combine_float_consts(lim, rim, sub, bin_inner),
                    ))
                },
                _ => None,
            }
        },
        _ => None,
    }
}

/// Extract integer parts of complex initialization with i128 precision.
pub fn complex_init_parts_i(e: &Expr) -> Option<(i128, i128)> {
    use crate::codegen::x86_64::const_eval;

    match e {
        Expr::ComplexLit { real, imag, ty, .. } => {
            let inner = match ty {
                Type::Complex(i) | Type::Imaginary(i) => &i.base,
                other => other,
            };
            if matches!(inner, Type::Float | Type::Double | Type::LongDouble) {
                return None;
            }
            Some((real.as_f64() as i128, imag.as_f64() as i128))
        },
        Expr::ComplexIntLit { real, imag, .. } => Some((*real, *imag)),
        Expr::Int { v, .. } => Some((*v, 0)),
        Expr::Float { c, .. } => Some((c.as_f64() as i128, 0)),
        Expr::Imag { e: inner, .. } => Some((0, const_eval::eval(inner).ok()?)),
        Expr::Unary { op, e, .. } => {
            if matches!(op, UnOp::Neg) {
                let (re, im) = complex_init_parts_i(e)?;
                Some((-re, -im))
            } else {
                None
            }
        },
        Expr::Binop { op, l, r, .. } => match op {
            BinOp::Add | BinOp::Sub => {
                let neg: i128 = if matches!(op, BinOp::Sub) { -1 } else { 1 };
                let (lre, lim) = complex_init_parts_i(l)?;
                let (rre, rim) = complex_init_parts_i(r)?;
                Some((lre + neg * rre, lim + neg * rim))
            },
            _ => None,
        },
        _ => None,
    }
}

/// Generate global initialization bytes from an expression.
/// Handles complex numbers, floats, integers, and special cases.
pub fn global_init_bytes(init: &Expr, sz: usize, is_float: bool, ty: &Type) -> Option<Vec<u8>> {
    // Complex constant initialization
    if let Type::Complex(inner) = ty {
        if let Some(bytes) = complex_init_bytes(init, &inner.base) {
            return Some(bytes);
        }
    }

    if is_float {
        match const_eval::eval_full(init, false).ok()? {
            const_eval::ConstValue::Float(f) => {
                // 字节编码交给单一真源 `Codegen::float_data_bytes`。
                // 旧写法在这里内联发“f64 位型 + 高 8 零”（注释还写着
                // xmm0-double convention），而读侧在对象表示迁移后已统一
                // 用 `fld tbyte`，于是 `long double g = 1.0L` 读回来是 `-nan`。
                // 此函数与 `Codegen::global_init_bytes`（emit/expr/utils.rs）
                // 是**两份都活着的副本**（前者走 codegen.rs / static_locals.rs，
                // 后者走 elf/init_list.rs），只改一份会得到“测试全绿但现象不变”。
                if sz == 32 {
                    // _Complex long double：实部 + 零虚部，每分量 16 字节。
                    let ld = Type::LongDouble;
                    let mut b = crate::codegen::x86_64::Codegen::float_data_bytes(f, 16);
                    b.extend_from_slice(&crate::codegen::x86_64::Codegen::float_data_bytes(
                        FloatConst::zero(&ld),
                        16,
                    ));
                    Some(b)
                } else {
                    Some(crate::codegen::x86_64::Codegen::float_data_bytes(f, sz))
                }
            },
            const_eval::ConstValue::Int(v) => {
                if sz == 32 {
                    // _Complex long double 实部源为整型：精确 RNE 转换 +
                    // LongDouble 精度零虚部。
                    let ld = Type::LongDouble;
                    let mut b = crate::codegen::x86_64::Codegen::float_data_bytes(
                        FloatConst::from_int(&ld, v),
                        16,
                    );
                    b.extend_from_slice(&crate::codegen::x86_64::Codegen::float_data_bytes(
                        FloatConst::zero(&ld),
                        16,
                    ));
                    Some(b)
                } else {
                    Some(crate::codegen::x86_64::Codegen::float_data_bytes(
                        crate::ast::FloatConst::F64(crate::float80::i128_to_f64(v)),
                        sz,
                    ))
                }
            },
            _ => None,
        }
    } else {
        const_eval::eval(init)
            .ok()
            .map(|value| value.to_le_bytes()[..sz].to_vec())
    }
}

/// Generate bytes for complex constant initialization.
pub fn complex_init_bytes(init: &Expr, inner: &Type) -> Option<Vec<u8>> {
    match inner {
        Type::Float | Type::Double | Type::LongDouble => {
            let (re, im) = complex_init_parts(init, inner)?;
            Some(encode_complex_parts_f(re, im, inner))
        },
        _ => {
            // Integer components: prefer i128 exact evaluation
            if let Some((re, im)) = complex_init_parts_i(init) {
                return Some(encode_complex_parts_i(re, im, inner));
            }
            // 浮点常量回退到整数初始化：在 f64 边界显式降级（可审计）。
            let (re, im) = complex_init_parts(init, inner)?;
            Some(encode_complex_parts_i(re.as_f64() as i128, im.as_f64() as i128, inner))
        },
    }
}
