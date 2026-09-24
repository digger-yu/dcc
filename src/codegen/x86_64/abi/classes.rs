// src/codegen/x86_64/abi/classes.rs
// Type classification for SysV AMD64 ABI return value passing
// Determines which registers or stack locations are used for return values

/// ABI class for a single eight-byte chunk of data.
/// Used to classify aggregate types for register passing under SysV AMD64 ABI.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AbiClass {
    /// No data in this eightbyte (padding/filler)
    NoClass,

    /// Integer / pointer / enum / function pointer
    /// Passed in general-purpose registers: rdi, rsi, rdx, rcx, r8, r9
    Integer,

    /// SSE scalar double/single, vector
    /// Passed in XMM registers: xmm0-xmm7
    Sse,

    /// SSE continuation (high 64 bits of 128-bit SSE value)
    /// Shares same XMM register as preceding Sse chunk
    SseUp,

    /// x87 FP (long double)
    /// Must be passed on stack
    X87,

    /// x87 FP continuation
    X87Up,

    /// Complex long double
    ComplexX87,

    /// Memory: requires sret pointer, passed entirely on stack
    /// - Types > 64 bytes
    /// - Complex types (any size)
    /// - Mixed-class aggregates
    /// - x87 floating point types
    Memory,
}

/// Determine ABI classes for type's return value chunks.
///
/// Follows SysV AMD64 ABI Section 3.2 (Return Values):
/// - Integers, pointers fit in rax (64 bits)
/// - SIMD vectors fit in xmm0 (128 bits = 2 eightbytes)
/// - Structs up to 16 bytes can use rax+rdx + xmm0-xmm7
/// - Larger/mixed types require sret hidden pointer argument
///
/// # Arguments
/// * `ty` - Type to classify (struct/union/array/vector/scalar)
///
/// # Returns
/// Vector of AbiClass values, one per eightbyte chunk needed
pub fn abi_classes(ty: &crate::ast::Type) -> Vec<AbiClass> {
    let size = ty.size();
    if size == 0 {
        return Vec::new();
    }

    // Types larger than 64 bytes cannot fit in register set
    // (6 GP + 8 XMM = 64 bytes maximum per SysV ABI)
    // long double and complex types always go through memory
    if size > 64
        || matches!(ty, crate::ast::Type::LongDouble)
        || matches!(ty, crate::ast::Type::Complex(_))
    {
        return vec![AbiClass::Memory];
    }

    let num_eightbytes = size.div_ceil(8);
    let mut classes = vec![AbiClass::NoClass; num_eightbytes];
    classify_field(ty, 0, &mut classes);

    // Post-merger pass 1: NoClass at tail beyond type size stays NoClass
    // These are alignment fillers from ceil division, not actual data chunks
    for (i, c) in classes.iter_mut().enumerate() {
        if *c == AbiClass::NoClass && i * 8 < size {
            *c = AbiClass::Memory;
        }
    }

    // Post-merger pass 2: SSE rule
    // If first eightbyte is SSE, check if remaining data chunks are SSE-compatible
    if num_eightbytes > 1 && classes[0] == AbiClass::Sse {
        let mut all_sse_compatible = true;
        for (i, class) in classes.iter().enumerate().skip(1) {
            if i * 8 >= size {
                // Tail padding: ignore
                continue;
            }
            if *class != AbiClass::Sse && *class != AbiClass::SseUp {
                // Check if this eightbyte actually contains field data
                let lo = i * 8;
                let hi = lo + 8;
                if has_field_data_in_range(ty, lo, hi) {
                    all_sse_compatible = false;
                    break;
                }
            }
        }

        if !all_sse_compatible {
            for c in classes.iter_mut() {
                *c = AbiClass::Memory;
            }
        }
    }

    // Normalize: single Memory element for all-Memory cases
    if classes.len() > 1 && classes.iter().all(|c| *c == AbiClass::Memory) {
        classes.truncate(1);
    }

    // Remove trailing NoClass elements (alignment fillers)
    while classes.last() == Some(&AbiClass::NoClass) {
        classes.pop();
    }

    classes
}

impl AbiClass {
    /// Check if this class requires memory passing (needs sret pointer)
    pub fn is_memory(self) -> bool {
        matches!(self, AbiClass::Memory | AbiClass::X87 | AbiClass::X87Up | AbiClass::ComplexX87)
    }
}

/// Check if type has field data in byte range [lo, hi).
/// Used to distinguish data-containing eightbytes vs pure alignment padding.
fn has_field_data_in_range(ty: &crate::ast::Type, lo: usize, hi: usize) -> bool {
    match ty {
        crate::ast::Type::Struct(_, fields, _, false) => {
            for f in fields.iter() {
                let f_lo = f.offset;
                let f_hi = f.offset + f.ty.size();
                if f_lo < hi && f_hi > lo {
                    return true;
                }
            }
            false
        },
        crate::ast::Type::Union(_, fields, _, false) => {
            for f in fields.iter() {
                if 0 < hi && f.ty.size() > lo {
                    return true;
                }
            }
            false
        },
        crate::ast::Type::Array(elem, crate::ast::ArraySize::Const(n)) => {
            for j in 0..*n {
                let e_lo = j * elem.size();
                let e_hi = e_lo + elem.size();
                if e_lo < hi && e_hi > lo {
                    return true;
                }
            }
            false
        },
        crate::ast::Type::Atomic(inner) => has_field_data_in_range(&inner.base, lo, hi),
        // Scalars/vectors/floats: entire range is data
        _ => true,
    }
}

/// Classify fields into parent's classes array.
/// Each field classified to temporary array, then merged into parent.
pub(crate) fn classify_field(ty: &crate::ast::Type, base: usize, classes: &mut [AbiClass]) {
    let size = ty.size();
    if size == 0 {
        return;
    }

    match ty {
        crate::ast::Type::Struct(_, fields, _, false) => {
            for field in fields.iter() {
                classify_field(&field.ty, base + field.offset, classes);
            }
        },
        crate::ast::Type::Union(_, fields, _, false) => {
            // Unions share base address, classify each field then merge
            for field in fields.iter() {
                let f_sz = field.ty.size();
                if f_sz == 0 {
                    continue;
                }
                let n = f_sz.div_ceil(8).min(classes.len() - base / 8);
                let mut f_classes = vec![AbiClass::NoClass; n];
                classify_field(&field.ty, 0, &mut f_classes);

                let start = base / 8;
                for i in 0..n {
                    if start + i < classes.len() {
                        classes[start + i] = merge_classes(classes[start + i], f_classes[i]);
                    }
                }
            }
        },
        crate::ast::Type::Array(elem, crate::ast::ArraySize::Const(n)) => {
            for j in 0..*n {
                classify_field(&elem.base, base + j * elem.size(), classes);
            }
        },
        crate::ast::Type::Atomic(inner) => classify_field(&inner.base, base, classes),
        crate::ast::Type::Vector(_, _) => {
            // SIMD vectors: first eightbyte Sse, rest SseUp (vector continuation)
            let start = base / 8;
            let end = (base + size.saturating_sub(1)) / 8;
            for i in start..=end {
                if i < classes.len() {
                    let cls = if i == start { AbiClass::Sse } else { AbiClass::SseUp };
                    classes[i] = merge_classes(classes[i], cls);
                }
            }
        },
        _ => {
            // Leaf type: classify directly into classes
            let start = base / 8;
            let end = (base + size.saturating_sub(1)) / 8;
            let cls = leaf_abi_class(ty);
            for i in start..=end {
                if i < classes.len() {
                    classes[i] = merge_classes(classes[i], cls);
                }
            }
        },
    }
}

/// SysV ABI class merging table.
/// Determines final class when two eightbyte classifications overlap.
pub(crate) fn merge_classes(a: AbiClass, b: AbiClass) -> AbiClass {
    use AbiClass::*;

    if a == b {
        return a;
    }

    // NO_CLASS is initial value, take other side when merging
    if a == NoClass {
        return b;
    }
    if b == NoClass {
        return a;
    }

    // Any Memory results in Memory
    if a == Memory || b == Memory {
        return Memory;
    }

    match (a, b) {
        // Mixed integer/SSE/X87 → Memory (cannot split across reg groups)
        (Integer, Sse) | (Sse, Integer) => Memory,
        (Integer, X87) | (X87, Integer) => Memory,
        (Integer, X87Up) | (X87Up, Integer) => Memory,
        (Integer, ComplexX87) | (ComplexX87, Integer) => Memory,
        (Sse, X87) | (X87, Sse) => Memory,
        (Sse, X87Up) | (X87Up, Sse) => Memory,
        (Sse, ComplexX87) | (ComplexX87, Sse) => Memory,
        (X87, X87Up) | (X87Up, X87) => Memory,
        (X87, ComplexX87) | (ComplexX87, X87) => Memory,
        (X87Up, ComplexX87) | (ComplexX87, X87Up) => Memory,
        // SSEUP only merges with SSE to produce SSEUP (vector continuation)
        (Sse, SseUp) | (SseUp, Sse) => SseUp,
        // Default to Memory for unrecognized combinations
        _ => Memory,
    }
}

/// Leaf type ABI classification.
pub(crate) fn leaf_abi_class(ty: &crate::ast::Type) -> AbiClass {
    match ty {
        crate::ast::Type::Float | crate::ast::Type::Double => AbiClass::Sse,
        crate::ast::Type::LongDouble => AbiClass::Memory,

        // _Complex follows real component type (SysV ABI 3.2.3):
        // float/double → SSE, long double → Memory,
        // integer (_Complex int/long) → INTEGER.
        // Previous bug: integer _Complex incorrectly marked SSE, args returned
        // via wrong register group (complex-6).
        crate::ast::Type::Complex(inner) => leaf_abi_class(&inner.base),

        // _Imaginary float/double: like SSE
        crate::ast::Type::Imaginary(inner)
            if matches!(&inner.base, crate::ast::Type::Float | crate::ast::Type::Double) =>
        {
            AbiClass::Sse
        },

        // SIMD vectors: first eightbyte Sse, rest SseUp
        crate::ast::Type::Vector(_, _) => AbiClass::Sse,

        // Integers, pointers, enums, etc.
        _ => AbiClass::Integer,
    }
}
