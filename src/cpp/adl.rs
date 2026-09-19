// ADL (Argument Dependent Lookup) Implementation for DCC Compiler
// 
// This module implements Argument Dependent Lookup as per C++ standard [basic.lookup.argdep]
// When resolving a function call `f(a, b)`, search namespaces associated with types of a and b.

use crate::cpp::sema::{CxxType, ClassId, SymbolId};

/// Result of ADL lookup
#[derive(Debug, Clone, Default)]
pub struct AdlResult {
    /// Symbols found via ADL
    pub symbols: Vec<SymbolId>,
    /// Namespaces that were searched
    pub namespaces_searched: Vec<String>,
}

impl AdlResult {
    pub fn new() -> Self {
        Self {
            symbols: Vec::new(),
            namespaces_searched: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.symbols.is_empty()
    }

    pub fn push(&mut self, symbol: SymbolId, namespace: String) {
        self.symbols.push(symbol);
        if !self.namespaces_searched.contains(&namespace) {
            self.namespaces_searched.push(namespace);
        }
    }
}

/// Extract all namespaces from a type for ADL purposes
/// Returns list of unique namespace paths (e.g., ["std", "std::pmr"])
pub fn collect_adl_namespaces_for_type(ty: &CxxType) -> Vec<String> {
    match ty {
        // Fundamental types have no namespace
        CxxType::Void | CxxType::Bool | CxxType::Integer { .. } | CxxType::Float { .. } => {
            Vec::new()
        }

        // Class type - extract its namespace
        CxxType::Class(class_id) => extract_class_namespace(*class_id),

        // Enum type
        CxxType::Enum(_) => Vec::new(),

        // Dependent type - parse namespace from name string
        CxxType::Dependent(name) => {
            if let Some(pos) = name.rfind("::") {
                let ns = &name[..pos];
                if !ns.is_empty() {
                    return vec![ns.to_string()];
                }
            }
            Vec::new()
        }

        // Pointer/Reference/Array - recurse into inner type
        CxxType::Pointer(inner)
        | CxxType::LValueRef(inner)
        | CxxType::RValueRef(inner)
        | CxxType::Array(inner, _) => collect_adl_namespaces_for_type(inner),

        // Alias and complex types - skip for now
        CxxType::Alias(_) | CxxType::Function { .. } | CxxType::MemberPointer { .. } => {
            Vec::new()
        }
    }
}

/// Extract namespace path from class ID
fn extract_class_namespace(class_id: ClassId) -> Vec<String> {
    // TODO: This needs access to the ClassTable to get the full namespace path
    // For now, return empty - will be enhanced when ClassTable is integrated
    Vec::new()
}

/// Collect namespaces from multiple argument types
pub fn collect_adl_namespaces(args: &[(&str, &CxxType)]) -> Vec<String> {
    let mut namespaces = Vec::new();
    
    for (_, arg_ty) in args {
        let arg_ns = collect_adl_namespaces_for_type(arg_ty);
        namespaces.extend(arg_ns);
    }

    // Remove duplicates while preserving order
    namespaces.sort();
    namespaces.dedup();
    
    namespaces
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpp::sema::CxxType;

    #[test]
    fn test_fundamental_no_namespace() {
        assert!(collect_adl_namespaces_for_type(&CxxType::Void).is_empty());
        assert!(collect_adl_namespaces_for_type(&CxxType::Integer { bits: 32, signed: true }).is_empty());
        assert!(collect_adl_namespaces_for_type(&CxxType::Float { bits: 64 }).is_empty());
    }

    #[test]
    fn test_pointer_recurse() {
        let int_ptr = CxxType::Pointer(Box::new(CxxType::Integer { bits: 32, signed: true }));
        assert!(collect_adl_namespaces_for_type(&int_ptr).is_empty());
    }

    #[test]
    fn test_dependent_with_namespace() {
        let dep = CxxType::Dependent("std::vector<int>".to_string());
        let ns = collect_adl_namespaces_for_type(&dep);
        assert!(!ns.is_empty());
        assert!(ns[0].contains("std"));
    }
}
