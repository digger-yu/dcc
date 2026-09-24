//!# Copyright (C) 2025 DCC Project
//!# MIT License - See LICENSE file for details

// src/codegen/shared/types.rs - 共享类型系统
// Phase C: 数据驱动代码生成架构重构
//
// 第一性原理：类型提升规则在所有 ISA 后端中都是相同的
// 目标：提取通用类型逻辑，减少重复代码

use crate::ast::Type;

/// Type classification for instruction selection
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeClass {
    Integer,
    FloatingPoint,
    Vector,
    Pointer,
    Struct,
    Union,
    Enum,
    Void,
}

/// Type size and alignment information (ISA-independent)
#[derive(Debug, Clone, Copy)]
pub struct TypeInfo {
    pub size: usize,
    pub align: usize,
    pub type_class: TypeClass,
}

impl TypeInfo {
    /// Create new type info
    pub fn new(size: usize, align: usize, type_class: TypeClass) -> Self {
        Self {
            size,
            align,
            type_class,
        }
    }

    /// Get natural register class for this type
    pub fn reg_class(&self) -> RegisterClass {
        match self.type_class {
            TypeClass::Integer | TypeClass::Pointer => RegisterClass::General,
            TypeClass::FloatingPoint => RegisterClass::Float,
            TypeClass::Vector => RegisterClass::Vector,
            _ => RegisterClass::General, // Default fallback
        }
    }
}

/// Register classes for different operations
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterClass {
    General,      // GPRs for integers, addresses
    Float,        // FPU/FP registers
    Vector,       // SIMD/vector registers
    Special,      // Special purpose registers
}

/// Type promotion rules (C standard)
pub enum PromotionRule {
    /// Int promotion: bool, char, short, int16_t → int (if int can represent all values)
    IntPromotion,
    /// Unsigned int promotion: unsigned char, unsigned short, unsigned int16_t → unsigned int
    UIntPromotion,
    /// Long double promotion: float/short real → long double
    LongDoublePromotion,
    /// No promotion needed
    NoPromotion,
}

/// Type promotion engine - ISA-independent
pub struct TypePromotionEngine;

impl TypePromotionEngine {
    /// Apply C standard type promotions
    pub fn promote_type(ty: &Type) -> Type {
        match ty {
            // Integral promotions
            Type::Char | Type::SChar | Type::UChar | Type::Short | Type::UShort => Type::Int,
            // Float promotion
            Type::Float => Type::Double,
            // No promotion for larger types
            _ => ty.clone(),
        }
    }

    /// Determine if type needs promotion
    pub fn needs_promotion(ty: &Type) -> PromotionRule {
        match ty {
            Type::Char | Type::SChar | Type::UChar | Type::Short | Type::UShort => {
                PromotionRule::IntPromotion
            }
            Type::Float => PromotionRule::LongDoublePromotion,
            _ => PromotionRule::NoPromotion,
        }
    }

    /// Get type class for instruction selection
    pub fn classify_type(ty: &Type) -> TypeClass {
        match ty {
            Type::Char | Type::SChar | Type::UChar | Type::Char8
            | Type::Char16 | Type::Char32
            | Type::Short | Type::UShort
            | Type::Int | Type::UInt | Type::Long | Type::ULong | Type::LongLong
            | Type::ULongLong | Type::Int128 | Type::UInt128 | Type::Bool => {
                TypeClass::Integer
            }
            Type::Float | Type::Double | Type::LongDouble => TypeClass::FloatingPoint,
            Type::Vector(..) => TypeClass::Vector,
            Type::Ptr(..) => TypeClass::Pointer,
            Type::Struct(..) => TypeClass::Struct,
            Type::Union(..) => TypeClass::Union,
            Type::Enum(..) => TypeClass::Enum,
            Type::Void => TypeClass::Void,
            Type::Func(..) => TypeClass::Pointer, // Function decays to pointer
            Type::Array(..) => TypeClass::Pointer, // Array decays to pointer
            Type::Named(_) => TypeClass::Pointer,  // Unknown, default to pointer
            Type::Complex(..) => TypeClass::FloatingPoint,
            Type::Imaginary(..) => TypeClass::FloatingPoint,
            Type::Atomic(inner) => Self::classify_type(&inner.base),
            Type::Fract => TypeClass::FloatingPoint,
            Type::Accum => TypeClass::FloatingPoint,
        }
    }

    /// Get type information (size, alignment, class)
    pub fn get_type_info(ty: &Type) -> TypeInfo {
        let size = ty.size();
        let align = ty.align();
        let type_class = Self::classify_type(ty);
        
        TypeInfo::new(size as usize, align as usize, type_class)
    }

    /// Check if two types are compatible for operations
    pub fn types_compatible(ty1: &Type, ty2: &Type) -> bool {
        if ty1 == ty2 {
            return true;
        }
        
        // Check if one can be promoted to the other
        let promo1 = Self::needs_promotion(ty1);
        let promo2 = Self::needs_promotion(ty2);
        
        match (promo1, promo2) {
            (PromotionRule::IntPromotion, PromotionRule::NoPromotion) => {
                matches!(ty2, Type::Int | Type::UInt)
            }
            (PromotionRule::NoPromotion, PromotionRule::IntPromotion) => {
                matches!(ty1, Type::Int | Type::UInt)
            }
            (PromotionRule::LongDoublePromotion, PromotionRule::NoPromotion) => {
                matches!(ty2, Type::Double | Type::LongDouble)
            }
            (PromotionRule::NoPromotion, PromotionRule::LongDoublePromotion) => {
                matches!(ty1, Type::Double | Type::LongDouble)
            }
            _ => false,
        }
    }
}

/// Type coercion utilities for mixed-mode arithmetic
pub struct TypeCoercionEngine;

impl TypeCoercionEngine {
    /// Determine result type for binary operation using C "usual arithmetic conversions"
    /// 
    /// C Standard (C11 6.3.1.8): If either operand has type `long double`, 
    /// both are converted to `long double`. Otherwise, if either is `double`,
    /// both become `double`, etc.
    pub fn coerce_binary_op(&self, ty1: &Type, ty2: &Type) -> (Type, Type) {
        // Same type - no coercion needed
        if ty1 == ty2 {
            return (ty1.clone(), ty2.clone());
        }
        
        // Apply usual arithmetic conversions
        let coerced_ty = self.usual_arithmetic_conversions(ty1, ty2);
        (coerced_ty.clone(), coerced_ty)
    }
    
    /// Usual arithmetic conversions (C11 6.3.1.8)
    fn usual_arithmetic_conversions(&self, lhs: &Type, rhs: &Type) -> Type {
        use crate::ast::Type;
        
        // First apply integer promotions to both operands
        let promoted_lhs = self.integral_promotion(lhs);
        let promoted_rhs = self.integral_promotion(rhs);
        
        // Check floating point dominance
        if matches!(promoted_lhs, Type::Float | Type::Double | Type::LongDouble) || matches!(promoted_rhs, Type::Float | Type::Double | Type::LongDouble) {
            return self.promote_to_higher_float(&promoted_lhs, &promoted_rhs);
        }
        
        // Integer usual conversions
        self.integer_usual_conversions(&promoted_lhs, &promoted_rhs)
    }
    
    /// Integral promotions (C11 6.3.1.1)
    fn integral_promotion(&self, ty: &Type) -> Type {
        match ty {
            // Types smaller than int promote to int or unsigned int
            Type::Char | Type::SChar | Type::UChar => Type::Int,
            Type::Short | Type::UShort => {
                // If short can't represent all values of int, promote to unsigned int
                // For simplicity, always promote to int (works on most systems)
                Type::Int
            }
            _ => ty.clone(),
        }
    }
    
    /// Promote to higher floating point type
    fn promote_to_higher_float(&self, flt1: &Type, flt2: &Type) -> Type {
        match (flt1, flt2) {
            (Type::LongDouble, _) | (_, Type::LongDouble) => Type::LongDouble,
            (Type::Double, _) | (_, Type::Double) => Type::Double,
            _ => Type::Float, // At least one is float, other promotes to float
        }
    }
    
    /// Integer usual conversions (C11 6.3.1.9)
    fn integer_usual_conversions(&self, lhs: &Type, rhs: &Type) -> Type {
        // Get signedness and rank
        let lhs_signed = Self::is_signed(&lhs);
        let rhs_signed = Self::is_signed(&rhs);
        let lhs_rank = Self::type_rank_integer(lhs);
        let rhs_rank = Self::type_rank_integer(rhs);
        
        // Higher rank dominates
        if lhs_rank > rhs_rank {
            return lhs.clone();
        } else if rhs_rank > lhs_rank {
            return rhs.clone();
        }
        
        // Same rank - handle signedness
        if lhs_signed && rhs_signed {
            // Both signed → left type (or either, they're same size)
            return lhs.clone();
        } else if !lhs_signed && !rhs_signed {
            // Both unsigned → left type
            return lhs.clone();
        } else {
                // Mixed signedness
                if lhs_signed {
                    // lhs is signed, rhs is unsigned, same rank
                    // If signed type can represent all values of unsigned type, result is signed
                    if self.can_signed_represent_unsigned(lhs) {
                        return lhs.clone();
                    } else {
                        // Otherwise, result is unsigned version
                        return rhs.clone();
                    }
                } else {
                    // lhs is unsigned, rhs is signed
                    if self.can_signed_represent_unsigned(rhs) {
                        return rhs.clone();
                    } else {
                        return lhs.clone();
                    }
                }
            }
    }
    
    /// Check if signed type can represent all values of unsigned equivalent
    fn can_signed_represent_unsigned(&self, ty: &Type) -> bool {
        match ty {
            Type::UChar => true,      // 0-255 fits in signed char (-128 to 127)? NO!
            Type::UShort => true,     // 0-65535 fits in short? NO!
            Type::UInt => true,       // Depends on system - assume yes for 32-bit int
            Type::ULong => false,     // Likely no on 64-bit systems
            Type::ULongLong => false,
            Type::UInt128 => false,
            _ => true, // Default assumption
        }
    }

    /// Check if an integer type is signed
    fn is_signed(ty: &Type) -> bool {
        match ty {
            Type::Char | Type::SChar | Type::Short | Type::Int
            | Type::Long | Type::LongLong | Type::Int128 => true,
            Type::UChar | Type::UShort | Type::UInt | Type::ULong
            | Type::ULongLong | Type::UInt128 | Type::Bool => false,
            _ => false,
        }
    }
    
    /// Get integer type rank (smaller = lower rank)
    fn type_rank_integer(ty: &Type) -> u8 {
        use crate::ast::Type;
        match ty {
            Type::Bool => 1,
            Type::Char | Type::SChar | Type::UChar => 2,
            Type::Short | Type::UShort => 3,
            Type::Int => 4,
            Type::Long => 5,
            Type::LongLong => 6,
            Type::Int128 => 7,
            Type::ULong | Type::ULongLong | Type::UInt | Type::UShort | Type::UChar => 3,
            _ => 4, // Default to int rank
        }
    }
}

/// Control flow case information for switch statements
/// 
/// This struct represents a single case in a switch statement,
/// containing the case value, body statements, and default flag.
/// 
/// # Fields
/// * `value` - The expression value to compare against (can be constant or expression)
/// * `body` - List of statements to execute when case matches
/// * `is_default` - Whether this is the default case
/// 
/// # Example
/// ```rust,ignore
/// // Corresponds to C code:
/// // switch(x) {
/// //     case 1: do_something(); break;
/// //     default: handle_default();
/// // }
/// let case_1 = CaseInfo {
///     value: Expression::Literal(1),
///     body: vec![Statement::Expr(do_something)],
///     is_default: false,
/// };
/// ```
#[derive(Debug, Clone)]
pub struct CaseInfo {
    pub value: crate::ast::Expr,
    pub body: Vec<crate::ast::Stmt>,
    pub is_default: bool,
}

impl CaseInfo {
    /// Create a new case with a specific value
    pub fn new(value: crate::ast::Expr, body: Vec<crate::ast::Stmt>) -> Self {
        Self { value, body, is_default: false }
    }
    
    /// Create a default case (matches any value)
    pub fn default_case(body: Vec<crate::ast::Stmt>) -> Self {
        Self {
            value: crate::ast::Expr::Ident {
                name: crate::ast::Ident {
                    name: "default".to_string(),
                    span: crate::diagnostics::Span::unknown(),
                },
                span: crate::diagnostics::Span::unknown(),
                ty: crate::ast::Type::Int,
            },
            body,
            is_default: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::QualType;

    #[test]
    fn test_integral_promotion() {
        // Char promotes to int
        assert_eq!(TypePromotionEngine::promote_type(&Type::Char), Type::Int);
        
        // Short promotes to int
        assert_eq!(TypePromotionEngine::promote_type(&Type::Short), Type::Int);
        
        // Float promotes to double
        assert_eq!(TypePromotionEngine::promote_type(&Type::Float), Type::Double);
        
        // int stays int
        assert_eq!(TypePromotionEngine::promote_type(&Type::Int), Type::Int);
    }

    #[test]
    fn test_type_classification() {
        assert_eq!(TypePromotionEngine::classify_type(&Type::Int), TypeClass::Integer);
        assert_eq!(TypePromotionEngine::classify_type(&Type::Float), TypeClass::FloatingPoint);
        assert_eq!(TypePromotionEngine::classify_type(&Type::Ptr(Box::new(QualType::new(Type::Int)))), TypeClass::Pointer);
        assert_eq!(TypePromotionEngine::classify_type(&Type::Void), TypeClass::Void);
    }

    #[test]
    fn test_register_class() {
        let int_info = TypePromotionEngine::get_type_info(&Type::Int);
        assert_eq!(int_info.reg_class(), RegisterClass::General);

        let float_info = TypePromotionEngine::get_type_info(&Type::Double);
        assert_eq!(float_info.reg_class(), RegisterClass::Float);
    }
}
