// src/profile/kcov_merge.rs - KCOV cross-function inline merging
//
// Implements cross-function guard array merging for KCOV coverage:
// - Analyzes call graph during LTO phase
// - Merges guard arrays from inlined callees into global index
// - Generates __sanitizer_cov_weak_hook_* calls for inlined functions
//
// This enables more accurate coverage tracking when functions are inlined
// across translation units.

use std::collections::{HashMap, HashSet};

/// Call graph representation for KCOV analysis
#[derive(Debug, Clone, Default)]
pub struct CallGraph {
    /// Function name -> set of callees
    pub callees: HashMap<String, HashSet<String>>,
    /// Function name -> set of callers
    pub callers: HashMap<String, HashSet<String>>,
    /// Functions that are inlined
    pub inlined: HashSet<String>,
}

impl CallGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a call edge from caller to callee
    pub fn add_edge(&mut self, caller: &str, callee: &str) {
        self.callees.entry(caller.to_string()).or_default().insert(callee.to_string());
        self.callers.entry(callee.to_string()).or_default().insert(caller.to_string());
    }

    /// Mark a function as inlined
    pub fn mark_inlined(&mut self, func: &str) {
        self.inlined.insert(func.to_string());
    }

    /// Check if a function is inlined
    pub fn is_inlined(&self, func: &str) -> bool {
        self.inlined.contains(func)
    }

    /// Get all callers of a function
    pub fn get_callers(&self, func: &str) -> Option<&HashSet<String>> {
        self.callers.get(func)
    }

    /// Get all callees of a function
    pub fn get_callees(&self, func: &str) -> Option<&HashSet<String>> {
        self.callees.get(func)
    }
}

/// Global guard index for merged coverage
#[derive(Debug, Clone, Default)]
pub struct GlobalGuardIndex {
    /// Function name -> (guard_array_name, guard_count)
    pub func_guards: HashMap<String, (String, u32)>,
    /// Global guard counter
    pub next_global_id: u32,
    /// Merged guard arrays: array_name -> size
    pub merged_arrays: HashMap<String, u32>,
}

impl GlobalGuardIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocate guards for a function, merging if inlined
    pub fn allocate_guards(&mut self, func: &str, requested_guards: u32, is_inlined: bool) -> (String, u32) {
        if is_inlined {
            // For inlined functions, merge into caller's guard array
            // Find the first caller and use their guard array
            let first_caller = self.func_guards.keys().next().cloned();
            if let Some(caller) = first_caller {
                if let Some((array_name, count)) = self.func_guards.get_mut(&caller) {
                    let base_id = *count;
                    *count += requested_guards;
                    return (array_name.clone(), base_id);
                }
            }
        }

        // Allocate new guard array for this function
        let array_name = format!("__dcc_cov_guards_global_{}", self.next_global_id);
        self.next_global_id += 1;
        self.func_guards.insert(func.to_string(), (array_name.clone(), requested_guards));
        self.merged_arrays.insert(array_name.clone(), requested_guards);
        (array_name, 0)
    }

    /// Get guard array info for a function
    pub fn get_guards(&self, func: &str) -> Option<&(String, u32)> {
        self.func_guards.get(func)
    }
}

/// Generate weak hook calls for inlined functions
pub fn generate_weak_hooks(call_graph: &CallGraph, func: &str) -> Vec<String> {
    let mut hooks = Vec::new();
    
    // If this function is inlined, generate weak hooks for its callees
    if call_graph.is_inlined(func) {
        if let Some(callees) = call_graph.get_callees(func) {
            for callee in callees {
                // Generate weak hook call
                hooks.push(format!("__sanitizer_cov_weak_hook_{}", callee));
            }
        }
    }
    
    hooks
}

/// Merge KCOV guards across translation units
pub fn merge_kcov_guards(
    call_graph: &CallGraph,
    func_guard_counts: &HashMap<String, u32>,
) -> GlobalGuardIndex {
    let mut index = GlobalGuardIndex::new();
    
    for (func, &count) in func_guard_counts {
        let is_inlined = call_graph.is_inlined(func);
        index.allocate_guards(func, count, is_inlined);
    }
    
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_call_graph_basic() {
        let mut cg = CallGraph::new();
        cg.add_edge("main", "foo");
        cg.add_edge("foo", "bar");
        
        assert!(cg.get_callees("main").unwrap().contains("foo"));
        assert!(cg.get_callers("foo").unwrap().contains("main"));
        assert!(cg.get_callees("foo").unwrap().contains("bar"));
    }

    #[test]
    fn test_inlined_functions() {
        let mut cg = CallGraph::new();
        cg.add_edge("main", "foo");
        cg.mark_inlined("foo");
        
        assert!(cg.is_inlined("foo"));
        assert!(!cg.is_inlined("main"));
    }

    #[test]
    fn test_global_guard_allocation() {
        let mut index = GlobalGuardIndex::new();
        
        // Allocate guards for non-inlined function
        let (arr1, base1) = index.allocate_guards("main", 4, false);
        assert_eq!(base1, 0);
        assert!(arr1.contains("__dcc_cov_guards_global_"));
        
        // Allocate guards for another function
        let (arr2, base2) = index.allocate_guards("foo", 3, false);
        assert_eq!(base2, 0);
        assert_ne!(arr1, arr2);
    }

    #[test]
    fn test_weak_hook_generation() {
        let mut cg = CallGraph::new();
        cg.add_edge("main", "foo");
        cg.add_edge("main", "bar");
        cg.mark_inlined("main");
        
        let hooks = generate_weak_hooks(&cg, "main");
        assert_eq!(hooks.len(), 2);
        assert!(hooks.contains(&"__sanitizer_cov_weak_hook_foo".to_string()));
        assert!(hooks.contains(&"__sanitizer_cov_weak_hook_bar".to_string()));
    }

    #[test]
    fn test_kcov_merge_integration() {
        let mut cg = CallGraph::new();
        cg.add_edge("main", "foo");
        cg.mark_inlined("foo");
        
        let mut func_counts = HashMap::new();
        func_counts.insert("main".to_string(), 4);
        func_counts.insert("foo".to_string(), 2);
        
        let index = merge_kcov_guards(&cg, &func_counts);
        
        // Both functions should have guard allocations
        assert!(index.get_guards("main").is_some());
        assert!(index.get_guards("foo").is_some());
    }
}
