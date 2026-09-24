// AArch64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::ast::{Decl, Field, FuncDecl, Program, Type};
use crate::ir::ValueType;
use std::collections::HashMap;
use std::sync::Arc;

pub(crate) fn find_func<'a>(prog: &'a Program, name: &str) -> Option<&'a FuncDecl> {
    prog.decls.iter().find_map(|d| match d {
        Decl::Func(f) if f.name.name == name => Some(f),
        _ => None,
    })
}

/// 文件作用域 `static` 变量判定 (与 find_func 对函数做同样的事)。
/// 只看顶层 Decl::Var; 函数内 static local 由 Global::is_static_local 单独判。
pub(crate) fn var_is_static(prog: &Program, name: &str) -> bool {
    prog.decls.iter().any(|d| match d {
        Decl::Var(v) => v.name.name == name && v.is_static,
        _ => false,
    })
}

/// 递归判定一条 IR 表达式子树里是否含有调用 (Call)。
pub(crate) fn expr_has_call(e: &crate::ir::TypedExpr) -> bool {
    if e.kind == crate::ir::ExprKind::Call {
        return true;
    }
    e.children.iter().any(expr_has_call)
}

/// 递归判定一条 IR 语句子树里是否含有调用。
pub(crate) fn stmt_has_call(s: &crate::ir::TypedStmt) -> bool {
    s.expressions.iter().any(expr_has_call) || s.nested.iter().any(stmt_has_call)
}

pub(crate) fn classify_hfa(
    ty: &ValueType,
    fields: &HashMap<String, (Arc<Vec<Field>>, bool)>,
) -> Option<HfaInfo> {
    let ValueType::Aggregate {
        name,
        size,
        is_union: false,
    } = ty
    else {
        return None;
    };
    if *size == 0 || *size > 128 {
        return None;
    }
    let (flds, is_union) = fields.get(name)?;
    // AAPCS64: HFA 最多 8 个同类浮点元素 (v0-v7)
    if *is_union || flds.is_empty() || flds.len() > 8 {
        return None;
    }
    let first_bits = match &flds[0].ty {
        Type::Float => 32u16,
        Type::Double => 64u16,
        _ => return None,
    };
    for f in flds.iter() {
        match (&f.ty, first_bits) {
            (Type::Float, 32) | (Type::Double, 64) => {}
            _ => return None,
        }
    }
    Some(HfaInfo {
        elem_bits: first_bits,
        count: flds.len() as u8,
        total_size: *size,
    })
}

/// AAPCS64 同构浮点聚合 (HFA) 描述
#[derive(Clone, Copy, Debug)]
pub(crate) struct HfaInfo {
    pub(crate) elem_bits: u16,
    pub(crate) count: u8,
    pub(crate) total_size: usize,
}

/// 局部变量栈槽大小: 向量固定 16B, 其余类型至少 4B (AArch64 32-bit
/// 子寄存器写入不保证清零高位, 不足 4B 的槽也分配 4B)。
pub(crate) fn local_stack_size(ty: &Type) -> i32 {
    if matches!(ty, Type::Vector(_, _)) {
        16
    } else {
        ty.size().max(4) as i32
    }
}

pub(crate) fn trunc_i32(v: i128) -> u32 {
    (v as i32) as u32
}

pub(crate) fn is_float_ty(ty: &ValueType) -> bool {
    matches!(ty, ValueType::Float { .. })
}

pub(crate) fn is_vector_ty(ty: &ValueType) -> bool {
    matches!(ty, ValueType::Vector(_, _))
}

pub(crate) fn is_complex_ty(ty: &ValueType) -> bool {
    matches!(ty, ValueType::Complex(_))
}

// M7 修复: 聚合类型判定 (struct/union/array of known size)
pub(crate) fn is_aggregate(ty: &ValueType) -> bool {
    matches!(
        ty,
        ValueType::Aggregate { .. } | ValueType::Array(_, Some(_)) | ValueType::Complex(_)
    )
}

// M7 修复: 聚合类型字节数 (非聚合返回 None)
pub(crate) fn aggregate_size(ty: &ValueType) -> Option<usize> {
    match ty {
        ValueType::Aggregate { size, .. } => Some(*size),
        ValueType::Array(inner, Some(n)) => Some(value_type_size(inner) * n),
        ValueType::Complex(inner) => Some(value_type_size(inner) * 2),
        _ => None,
    }
}

pub(crate) fn value_type_size(ty: &ValueType) -> usize {
    match ty {
        ValueType::Void => 0,
        ValueType::Integer { bits, .. } => (*bits as usize).div_ceil(8),
        ValueType::Float { bits } => (*bits as usize).div_ceil(8),
        ValueType::Pointer(_) => 8,
        ValueType::Array(inner, Some(n)) => value_type_size(inner) * n,
        ValueType::Array(inner, None) => value_type_size(inner),
        ValueType::Aggregate { size, .. } => *size,
        ValueType::Function { .. } => 8,
        ValueType::Vector(inner, lanes) => value_type_size(inner) * lanes,
        ValueType::Complex(inner) => value_type_size(inner) * 2,
        ValueType::Imaginary(inner) => value_type_size(inner),
    }
}

pub(crate) fn align_rodata(buf: &mut Vec<u8>, align: u64) -> u64 {
    let off = align_up_u64(buf.len() as u64, align);
    buf.resize(off as usize, 0);
    off
}

pub(crate) fn align_up_u64(v: u64, align: u64) -> u64 {
    (v + align - 1) & !(align - 1)
}
