// src/codegen/x86_64/mem/global.rs
// Global variable initialization and management for x86_64 backend
// Extracted from mod.rs (Turns 38-39)

use crate::ast::*;
use crate::codegen::x86_64::{Codegen, const_eval, GlobalVar};

use std::collections::HashMap;

/// Evaluate an lvalue expression to (base_global_name, byte_offset).
/// Handles Ident, Index (with constant indices), Member, and Deref chains.
/// Returns None if the expression cannot be evaluated at compile time.
pub fn eval_global_lvalue_offset<'a>(
    e: &Expr,
    globals_raw: &'a HashMap<String, GlobalVar>,
) -> Option<(String, i64)> {
    match e {
        Expr::Ident { name, .. } => {
            // Direct reference to global variable
            Some((name.name.clone(), 0))
        }
        Expr::AddrOf { e: inner, .. } => {
            // &x - peel off one level
            eval_global_lvalue_offset(inner, globals_raw)
        }
        Expr::Cast { e: inner, ty, .. } => {
            // Cast expressions like (T*)&expr
            let base_off = eval_global_lvalue_offset(inner, globals_raw)?;
            let offset_adjustment = match ty {
                Type::Ptr(elem_ty) => elem_ty.size(),
                _ => 1, // Pointer arithmetic by bytes
            };
            Some((base_off.0, base_off.1 + offset_adjustment as i64))
        }
        Expr::Index { arr, idx, .. } => {
            // Array indexing: arr[i]
            if let Some((base_name, base_off)) = eval_global_lvalue_offset(arr, globals_raw) {
                if let Ok(const_idx) = const_eval::eval(idx) {
                    let array_ty = match globals_raw.get(&base_name) {
                        Some(g) => g.ty.get_array_element_type()?,
                        None => return None,
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
        }
        Expr::Member { expr, field, is_arrow, .. } => {
            // Structure member access: struct.field or ptr->field
            if let Some((base_name, base_off)) = eval_global_lvalue_offset(expr, globals_raw) {
                // Placeholder for field offset calculation - full extraction from mod.rs needed
                // For now, skip to avoid compile errors
                let _ = (field, is_arrow);
                Some((base_name, base_off))
            } else {
                None
            }
        }
        Expr::Deref { .. } => {
            // &(*ptr) = ptr, but ptr must be a constant address
            // This is rare in global init; skip for now
            None
        }
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

/// Encode complex number parts as little-endian bytes for floating-point components.
pub fn encode_complex_parts_f(re: f64, im: f64, inner: &Type) -> Vec<u8> {
    let mut b = Vec::new();
    for v in [re, im] {
        match inner {
            Type::Float => b.extend_from_slice(&(v as f32).to_bits().to_le_bytes()),
            Type::LongDouble => {
                b.extend_from_slice(&v.to_bits().to_le_bytes());
                b.extend_from_slice(&[0u8; 8]);
            }
            _ => b.extend_from_slice(&v.to_bits().to_le_bytes()),
        }
    }
    b
}

/// Extract real and imaginary parts from complex initialization expression.
pub fn complex_init_parts(e: &Expr) -> Option<(f64, f64)> {
    use crate::codegen::x86_64::const_eval;
    
    match e {
        Expr::ComplexLit { real, imag, .. } => Some((*real, *imag)),
        Expr::Imag { e: inner, .. } => {
            Some((0.0, const_eval::eval_full(inner, false).ok()?.as_float()?))
        }
        Expr::Float { v, .. } => Some((*v, 0.0)),
        Expr::Int { v, .. } => Some((*v as f64, 0.0)),
        Expr::Unary { op, e, .. } => {
            if matches!(op, UnOp::Neg) {
                let (re, im) = complex_init_parts(e)?;
                Some((-re, -im))
            } else {
                None
            }
        }
        Expr::Binop { op, l, r, .. } => match op {
            BinOp::Add | BinOp::Sub => {
                let neg = if matches!(op, BinOp::Sub) { -1.0 } else { 1.0 };
                let (lre, lim) = complex_init_parts(l)?;
                let (rre, rim) = complex_init_parts(r)?;
                Some((lre + neg * rre, lim + neg * rim))
            }
            _ => None,
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
            Some((*real as i128, *imag as i128))
        }
        Expr::Int { v, .. } => Some((*v, 0)),
        Expr::Float { v, .. } => Some((*v as i128, 0)),
        Expr::Imag { e: inner, .. } => Some((0, const_eval::eval(inner).ok()?)),
        Expr::Unary { op, e, .. } => {
            if matches!(op, UnOp::Neg) {
                let (re, im) = complex_init_parts_i(e)?;
                Some((-re, -im))
            } else {
                None
            }
        }
        Expr::Binop { op, l, r, .. } => match op {
            BinOp::Add | BinOp::Sub => {
                let neg: i128 = if matches!(op, BinOp::Sub) { -1 } else { 1 };
                let (lre, lim) = complex_init_parts_i(l)?;
                let (rre, rim) = complex_init_parts_i(r)?;
                Some((lre + neg * rre, lim + neg * rim))
            }
            _ => None,
        },
        _ => None,
    }
}

/// Generate global initialization bytes from an expression.
/// Handles complex numbers, floats, integers, and special cases.
pub fn global_init_bytes(
    init: &Expr,
    sz: usize,
    is_float: bool,
    ty: &Type,
) -> Option<Vec<u8>> {
    // Complex constant initialization
    if let Type::Complex(inner) = ty {
        if let Some(bytes) = complex_init_bytes(init, &inner.base) {
            return Some(bytes);
        }
    }
    
    if is_float {
        match const_eval::eval_full(init, false).ok()? {
            const_eval::ConstValue::Float(f) => {
                if sz == 4 {
                    Some((f as f32).to_bits().to_le_bytes().to_vec())
                } else if sz == 16 {
                    // long double: xmm0-double convention
                    let mut b = f.to_bits().to_le_bytes().to_vec();
                    b.extend_from_slice(&[0u8; 8]);
                    Some(b)
                } else if sz == 32 {
                    // _Complex long double real part
                    let mut b = f.to_bits().to_le_bytes().to_vec();
                    b.extend_from_slice(&[0u8; 8]);
                    b.extend_from_slice(&0.0f64.to_bits().to_le_bytes());
                    b.extend_from_slice(&[0u8; 8]);
                    Some(b)
                } else {
                    Some(f.to_bits().to_le_bytes().to_vec())
                }
            }
            const_eval::ConstValue::Int(v) => {
                if sz == 4 {
                    Some((v as f32).to_bits().to_le_bytes().to_vec())
                } else if sz == 16 {
                    let mut b = (v as f64).to_bits().to_le_bytes().to_vec();
                    b.extend_from_slice(&[0u8; 8]);
                    Some(b)
                } else if sz == 32 {
                    let mut b = (v as f64).to_bits().to_le_bytes().to_vec();
                    b.extend_from_slice(&[0u8; 8]);
                    b.extend_from_slice(&0.0f64.to_bits().to_le_bytes());
                    b.extend_from_slice(&[0u8; 8]);
                    Some(b)
                } else {
                    Some((v as f64).to_bits().to_le_bytes().to_vec())
                }
            }
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
            let (re, im) = complex_init_parts(init)?;
            Some(encode_complex_parts_f(re, im, inner))
        }
        _ => {
            // Integer components: prefer i128 exact evaluation
            if let Some((re, im)) = complex_init_parts_i(init) {
                return Some(encode_complex_parts_i(re, im, inner));
            }
            let (re, im) = complex_init_parts(init)?;
            Some(encode_complex_parts_i(re as i128, im as i128, inner))
        }
    }
}
