// src/codegen/aarch64/abi/classes.rs
// Type Classification for AAPCS64 Calling Convention
// 
// Classifies parameter and return types according to ARM's AAPCS64 specification.
// Determines which registers or stack locations are used for data passing.

use super::{ArgClass, HfaInfo, ReturnClass};
use crate::ir::ValueType;
use crate::ast::Type;

/// Field information for aggregate types
#[derive(Debug, Clone)]
pub struct FieldInfo {
    pub ty: Type,
    pub offset: usize,
}

/// AAPCS64 类型分类算法
/// 
/// Implements type classification rules from AAPCS64 Section 5 (Parameter Passing):
/// - Scalar In Pregister (SIP): int, ptr, float in X0-X7/V0-V7
/// - Aggregate In Stack (AGS): large structs passed by address on stack  
/// - Harmonic Functional Aggregate (HFA): homogeneous float aggregates in V0-V7
/// 
/// # Arguments
/// * `ty` - The type to classify
/// 
/// # Returns
/// Classification result indicating register vs memory passing
pub fn classify_type(ty: &ValueType) -> ArgClass {
    match ty {
        // Integer types and pointers → GP registers
        ValueType::Integer { .. } | ValueType::Pointer(_) => ArgClass::Gp,
        
        // Floating point → FP registers
        ValueType::Float { .. } => ArgClass::Fp,  // Double is covered here (Float { bits: 64 })
        
        // Complex numbers: check component type
        ValueType::Complex(inner) => classify_type(inner),
        
        // Imaginary floats follow real part
        ValueType::Imaginary(inner) => classify_type(inner),
        
        // SIMD vectors → FP registers
        ValueType::Vector(_, _) => ArgClass::Fp,
        
        // Functions as values → pointers → GP registers
        ValueType::Function { .. } => ArgClass::Gp,
        
        // Aggregates (structs/unions/arrays): check size
        ValueType::Aggregate { size, .. } => {
            if *size <= 16 {
                ArgClass::Gp
            } else {
                ArgClass::Memory
            }
        }
        
        ValueType::Void => ArgClass::Gp,
        _ => ArgClass::Gp,
    }
}

/// Check if type qualifies as HFA (Harmonic Functional Aggregate)
/// 
/// HFA requirements per AAPCS64:
/// - All elements are same scalar float type (float or double only)
/// - Max 8 elements (fits in v0-v7)
/// - Size ≤ 128 bytes
/// 
/// # Arguments
/// * `ty` - Type to check
/// * `fields` - Field information for aggregate types
/// 
/// # Returns
/// Some(HfaInfo) if type is valid HFA, None otherwise
pub fn classify_hfa(
    ty: &ValueType,
    fields: &std::collections::HashMap<String, (Vec<FieldInfo>, bool)>,
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

/// Classify return value according to AAPCS64 Section 5.1
pub fn classify_return_type(
    ty: &ValueType,
    _aggregate_fields: &std::collections::HashMap<String, (Vec<FieldInfo>, bool)>,
) -> ReturnClass {
    match ty {
        ValueType::Integer { .. } => ReturnClass::Scalar,
        ValueType::Pointer(_) => ReturnClass::Scalar,
        ValueType::Float { .. } | ValueType::Double => ReturnClass::Scalar,
        
        ValueType::Vector(elem, lanes) => {
            let size = (*lanes as usize) * value_type_size(elem);
            if size > 128 {
                ReturnClass::LargeAggregate
            } else {
                ReturnClass::VectorOrHfa { hfa_info: None }
            }
        }
        
        ValueType::Complex(inner) => {
            let size = value_type_size(inner) * 2;
            if size > 16 {
                ReturnClass::LargeAggregate
            } else {
                ReturnClass::SmallAggregate
            }
        }
        
        ValueType::Imaginary(inner) => {
            let _ = inner;
            ReturnClass::Scalar
        }
        
        ValueType::Aggregate { size, .. } => {
            if *size <= 16 {
                ReturnClass::SmallAggregate
            } else {
                ReturnClass::LargeAggregate
            }
        }
        
        ValueType::Void => ReturnClass::Scalar,
        _ => ReturnClass::Scalar,
    }
}

fn value_type_size(ty: &ValueType) -> usize {
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

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_scalar_classification() {
        assert_eq!(classify_type(&ValueType::Integer { bits: 32 }), ArgClass::Gp);
        assert_eq!(
            classify_type(&ValueType::Pointer(crate::target::ABI::default())),
            ArgClass::Gp
        );
        assert_eq!(classify_type(&ValueType::Float { bits: 32 }), ArgClass::Fp);
        assert_eq!(classify_type(&ValueType::Float { bits: 64 }), ArgClass::Fp); // Double
    }
    
    #[test]
    fn test_hfa_classification() {
        let hfa = HfaInfo { elem_bits: 64, count: 4, total_size: 32 };
        assert!(hfa.is_valid());
        assert_eq!(hfa.elem_bytes(), 8);
        assert_eq!(hfa.num_slots(), 4);
        
        let invalid_hfa = HfaInfo { elem_bits: 64, count: 9, total_size: 72 };
        assert!(!invalid_hfa.is_valid());
    }
    
    #[test]
    fn test_return_class_sret() {
        let small_struct = ValueType::Aggregate {
            name: "S1".to_string(),
            size: 8,
            is_union: false,
            fields: vec![],
        };
        assert!(
            !classify_return_type(&small_struct, &Default::default())
                .requires_sret()
        );
        
        let large_struct = ValueType::Aggregate {
            name: "S2".to_string(),
            size: 32,
            is_union: false,
            fields: vec![],
        };
        assert!(
            classify_return_type(&large_struct, &Default::default()).requires_sret()
        );
    }
}
