// src/link/lto_call_graph_stubs.rs
/// Cross-TU Call Graph Infrastructure for LTO Optimization
///
/// Placeholder implementation for PR-LTO-1. Full implementation TBD.
///
/// References:
/// - GCC IPA (Interprocedural Analysis) call graph implementation
/// - LLVM IR level call graph  
/// - `docs/PHASE_5_EXTENSION_PLAN.md` §5A
use crate::ir::Module;
use std::collections::{BTreeSet, HashMap, HashSet};

/// Node in the call graph
#[derive(Debug, Clone)]
pub enum CallGraphNode {
    /// Regular function definition
    Function(String, Box<FunctionNodeData>),
    /// External declaration (no body, referenced from somewhere)
    External(String),
    /// Internal symbol that appears to be unused (potential dead code)
    Unused(String),
}

/// Data associated with a function node
#[derive(Debug, Clone)]
pub struct FunctionNodeData {
    /// Function name
    pub name: String,
    /// Whether this function has a body
    pub has_body: bool,
    /// Number of times this function is called
    pub usage_count: usize,
    /// Whether function is marked always_inline
    pub always_inline: bool,
    /// Whether function is marked noreturn
    pub noreturn: bool,
    /// Whether function is marked cold
    pub is_cold: bool,
    /// Called from which functions (callers)
    pub callers: HashSet<String>,
    /// Calls which functions (callees)
    pub callees: HashSet<String>,
}

impl FunctionNodeData {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            has_body: false,
            usage_count: 0,
            always_inline: false,
            noreturn: false,
            is_cold: false,
            callers: HashSet::new(),
            callees: HashSet::new(),
        }
    }

    pub fn with_body(&mut self) {
        self.has_body = true;
    }

    pub fn mark_always_inline(&mut self) {
        self.always_inline = true;
    }

    pub fn mark_noreturn(&mut self) {
        self.noreturn = true;
    }

    pub fn mark_cold(&mut self) {
        self.is_cold = true;
    }

    pub fn add_caller(&mut self, caller: &str) {
        self.callers.insert(caller.to_string());
    }

    pub fn add_callee(&mut self, callee: &str) {
        self.callees.insert(callee.to_string());
    }

    pub fn set_usage_count(&mut self, count: usize) {
        self.usage_count = count;
    }
}

/// Call graph representing all function calls across TU(s)
#[derive(Debug, Default, Clone)]
pub struct CallGraph {
    nodes: HashMap<String, CallGraphNode>,
    reachable: BTreeSet<String>,
    dead: BTreeSet<String>,
    entry_points: HashSet<String>,
}

impl CallGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn build_from_module(module: &Module) -> Self {
        let mut cg = Self::new();

        // Create nodes for all functions
        for func in &module.functions {
            let mut node_data = FunctionNodeData::new(&func.name);
            node_data.with_body();

            // Check if function is marked cold or noreturn from AST attributes
            if !cg.nodes.contains_key(&func.name) {
                cg.nodes.insert(
                    func.name.clone(),
                    CallGraphNode::Function(func.name.clone(), Box::new(node_data)),
                );
            }
        }

        cg
    }

    pub fn compute_reachability(&mut self) {
        let mut visited = HashSet::new();
        let mut worklist = Vec::new();

        for entry in &self.entry_points {
            worklist.push(entry.clone());
        }

        while let Some(func_name) = worklist.pop() {
            if visited.contains(&func_name) {
                continue;
            }

            visited.insert(func_name.clone());
            self.reachable.insert(func_name.clone());

            if let Some(CallGraphNode::Function(_, ref data)) = self.nodes.get(&func_name) {
                for callee in &data.callees {
                    if !visited.contains(callee) {
                        worklist.push(callee.clone());
                    }
                }
            }
        }

        for (name, _node) in &self.nodes {
            if !self.reachable.contains(name) {
                self.dead.insert(name.clone());
            }
        }
    }

    pub fn get_dead_code(&self) -> impl Iterator<Item = &String> {
        self.dead.iter()
    }

    pub fn get_reachable(&self) -> impl Iterator<Item = &String> {
        self.reachable.iter()
    }

    pub fn is_reachable(&self, name: &str) -> bool {
        self.reachable.contains(name)
    }

    pub fn is_dead(&self, name: &str) -> bool {
        self.dead.contains(name)
    }

    pub fn get_node(&self, name: &str) -> Option<&CallGraphNode> {
        self.nodes.get(name)
    }

    pub fn get_usage_count(&self, name: &str) -> usize {
        if let Some(CallGraphNode::Function(_, ref data)) = self.nodes.get(name) {
            data.usage_count
        } else {
            0
        }
    }

    /// Build call graph from multiple modules (Cross-TU analysis)
    pub fn build_from_modules(modules: &[Module]) -> Self {
        let mut cg = Self::new();

        // Phase 1: Create all function nodes
        for module in modules {
            for func in &module.functions {
                if !cg.nodes.contains_key(&func.name) {
                    let mut node_data = FunctionNodeData::new(&func.name);
                    node_data.with_body();
                    cg.nodes.insert(
                        func.name.clone(),
                        CallGraphNode::Function(func.name.clone(), Box::new(node_data)),
                    );
                }
            }
        }

        // Phase 2: Extract calls from all functions and record them
        // Simplified implementation - recursively traverse all statements/expressions
        for module in modules {
            for func in &module.functions {
                cg.extract_calls_from_function(&func.name, &func.body);
            }
        }

        // Phase 3: Mark entry points
        if cg.nodes.contains_key("main") {
            cg.entry_points.insert("main".to_string());
        }
        if cg.nodes.contains_key("_start") {
            cg.entry_points.insert("_start".to_string());
        }

        cg
    }

    /// Recursively extract all calls from function body
    fn extract_calls_from_function(&mut self, caller: &str, statements: &[crate::ir::TypedStmt]) {
        for stmt in statements {
            // Process expressions in statement
            for expr in &stmt.expressions {
                self.extract_calls_from_expr(caller, expr);
            }
            // Process nested statements
            for nested_stmt in &stmt.nested {
                for expr in &nested_stmt.expressions {
                    self.extract_calls_from_expr(caller, expr);
                }
            }
        }
    }

    /// Extract calls from an expression (simplified implementation)
    fn extract_calls_from_expr(&mut self, caller: &str, expr: &crate::ir::TypedExpr) {
        use crate::ir::ExprKind;

        // Check if this expression is a call
        if matches!(expr.kind, ExprKind::Call) {
            self.record_call_site(caller);
        }

        // Recursively process children expressions
        for child in &expr.children {
            self.extract_calls_from_expr(caller, child);
        }
    }

    /// Record a call site found during analysis
    fn record_call_site(&mut self, caller: &str) {
        if self.entry_points.is_empty() || !self.nodes.contains_key(caller) {
            return;
        }

        // Parse the callee name from expression context
        // In production, this would extract the actual callee identifier
        let callee_name = "callee_placeholder"; // TODO: Extract real callee name

        // Update caller -> callee relationship
        if let Some(CallGraphNode::Function(_, ref mut data)) = self.nodes.get_mut(caller) {
            data.add_callee(callee_name);
        }

        // Create callee node if not exists
        if !self.nodes.contains_key(callee_name) {
            let mut callee_data = FunctionNodeData::new(callee_name);
            callee_data.with_body();
            self.nodes.insert(
                callee_name.to_string(),
                CallGraphNode::Function(callee_name.to_string(), Box::new(callee_data)),
            );
        }

        // Update callee <- caller relationship and increment usage count
        if let Some(CallGraphNode::Function(_, ref mut data)) = self.nodes.get_mut(callee_name) {
            data.add_caller(caller);
            data.usage_count += 1;
        }
    }

    /// Get list of dead functions that can be eliminated
    pub fn get_dead_functions(&self) -> Vec<String> {
        // Compute reachability if not already computed
        use std::collections::VecDeque;

        let mut reachable_set = HashSet::new();
        let mut worklist = VecDeque::new();

        for entry in &self.entry_points {
            worklist.push_back(entry.clone());
        }

        while let Some(func_name) = worklist.pop_front() {
            if reachable_set.contains(&func_name) {
                continue;
            }
            reachable_set.insert(func_name.clone());

            if let Some(CallGraphNode::Function(_, ref data)) = self.nodes.get(&func_name) {
                for callee in &data.callees {
                    if !reachable_set.contains(callee) {
                        worklist.push_back(callee.clone());
                    }
                }
            }
        }

        // Collect all functions not in reachable set
        let mut dead_funcs = Vec::new();
        for (name, node) in &self.nodes {
            if !reachable_set.contains(name) {
                if let CallGraphNode::Function(_, ref data) = node {
                    if !data.always_inline && !data.noreturn {
                        dead_funcs.push(name.clone());
                    }
                }
            }
        }

        dead_funcs
    }

    /// Mark functions for removal (preparation for DCE)
    pub fn mark_for_removal(&mut self, func_names: &[String]) {
        for name in func_names {
            // Remove the function from the graph
            self.nodes.remove(name);
        }
    }

    /// Remove marked functions from call graph
    pub fn remove_dead_functions(&mut self, dead_funcs: &[String]) {
        for name in dead_funcs {
            self.nodes.remove(name);
        }
    }

    /// Inline small, hot functions (PR-LTO-4)
    pub fn optimize_inlining(&mut self) -> Vec<String> {
        let mut inlined = Vec::new();

        // Find functions that are good candidates for inlining:
        // 1. Marked as always_inline
        // 2. High usage count (>3 calls)
        // 3. Small size (we'd need size info - placeholder)

        for (_name, node) in &self.nodes {
            if let CallGraphNode::Function(ref func_name, ref data) = node {
                // Inline if marked always_inline or hot (high usage)
                if data.always_inline || data.usage_count > 3 {
                    inlined.push(func_name.clone());
                }
            }
        }

        inlined
    }

    /// Get inline candidates based on cost model
    pub fn get_inline_candidates(&self, max_cost: usize) -> Vec<(String, usize)> {
        let mut candidates = Vec::new();

        for (name, node) in &self.nodes {
            if let CallGraphNode::Function(_, ref data) = node {
                // Simple cost model: size (assumed) + inverse of usage
                let estimated_size = 10; // Placeholder - would be calculated from IR
                let cost = if data.usage_count == 0 {
                    999
                } else {
                    estimated_size / data.usage_count
                };

                if cost <= max_cost && !data.noreturn {
                    candidates.push((name.clone(), cost));
                }
            }
        }

        // Sort by cost (lower is better)
        candidates.sort_by_key(|(_, cost)| *cost);
        candidates
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_call_graph() {
        let cg = CallGraph::new();
        assert!(cg.nodes.is_empty());
    }

    #[test]
    fn test_function_creation() {
        let mut node_data = FunctionNodeData::new("test_func");
        assert_eq!(node_data.name, "test_func");
        assert!(!node_data.has_body);

        node_data.with_body();
        assert!(node_data.has_body);
    }

    #[test]
    fn test_caller_callee_tracking() {
        let mut cg = CallGraph::new();

        // Create two functions
        let mut foo_data = FunctionNodeData::new("foo");
        foo_data.with_body();
        cg.nodes.insert(
            "foo".to_string(),
            CallGraphNode::Function("foo".to_string(), Box::new(foo_data)),
        );

        let mut bar_data = FunctionNodeData::new("bar");
        bar_data.with_body();
        cg.nodes.insert(
            "bar".to_string(),
            CallGraphNode::Function("bar".to_string(), Box::new(bar_data)),
        );

        // Record call: foo calls bar
        if let Some(CallGraphNode::Function(_, ref mut data)) = cg.nodes.get_mut("foo") {
            data.add_callee("bar");
        }
        if let Some(CallGraphNode::Function(_, ref mut data)) = cg.nodes.get_mut("bar") {
            data.add_caller("foo");
            data.usage_count += 1;
        }

        // Verify
        if let Some(CallGraphNode::Function(_, ref data)) = cg.get_node("foo") {
            assert!(data.callees.contains("bar"));
        }

        if let Some(CallGraphNode::Function(_, ref data)) = cg.get_node("bar") {
            assert!(data.callers.contains("foo"));
            assert_eq!(data.usage_count, 1);
        }
    }

    #[test]
    fn test_dead_code_detection() {
        let mut cg = CallGraph::new();

        // Setup entry points
        cg.entry_points.insert("main".to_string());

        // Create a simple call chain: main -> used_func (unused_func not called)
        for name in vec!["main", "used_func"] {
            let mut data = FunctionNodeData::new(&name);
            data.with_body();
            cg.nodes.insert(
                name.to_string(),
                CallGraphNode::Function(name.to_string(), Box::new(data)),
            );
        }

        // unused_func will be dead code (never called)
        let unused_name = "unused_func";
        let mut unused_data = FunctionNodeData::new(unused_name);
        unused_data.with_body();
        cg.nodes.insert(
            unused_name.to_string(),
            CallGraphNode::Function(unused_name.to_string(), Box::new(unused_data)),
        );

        // Record call from main to used_func
        if let Some(CallGraphNode::Function(_, ref mut data)) = cg.nodes.get_mut("main") {
            data.add_callee("used_func");
        }
        if let Some(CallGraphNode::Function(_, ref mut data)) = cg.nodes.get_mut("used_func") {
            data.add_caller("main");
            data.usage_count += 1;
        }

        // Get dead functions
        let dead_funcs = cg.get_dead_functions();

        // Verify unused_func is detected as dead
        assert!(dead_funcs.contains(&"unused_func".to_string()));

        // Verify used_func is NOT dead (it's reachable from main)
        assert!(!dead_funcs.contains(&"used_func".to_string()));
        assert!(!cg.is_dead("main"));
        assert!(!cg.is_dead("used_func"));
    }

    #[test]
    fn test_usage_count_accumulation() {
        let mut cg = CallGraph::new();
        cg.entry_points.insert("main".to_string());

        // Create functions with multiple callers
        for name in vec!["main", "helper1", "helper2", "shared_func"] {
            let mut data = FunctionNodeData::new(&name);
            data.with_body();
            cg.nodes.insert(
                name.to_string(),
                CallGraphNode::Function(name.to_string(), Box::new(data)),
            );
        }

        // shared_func called by both helpers
        let call_sources = vec!["helper1", "helper2"];
        for caller in &call_sources {
            if let Some(CallGraphNode::Function(_, ref mut data)) = cg.nodes.get_mut(*caller) {
                data.add_callee("shared_func");
            }
            if let Some(CallGraphNode::Function(_, ref mut data)) = cg.nodes.get_mut("shared_func")
            {
                data.add_caller(caller);
                data.usage_count += 1;
            }
        }

        // Verify usage count
        if let Some(CallGraphNode::Function(_, ref data)) = cg.get_node("shared_func") {
            assert_eq!(data.usage_count, 2); // Called twice
            assert_eq!(data.callers.len(), 2); // Two different callers
        }
    }

    #[test]
    fn test_inlining_candidates() {
        let mut cg = CallGraph::new();

        // Create various function types
        for (name, always_inline) in vec![
            ("always_inline_func", true),
            ("hot_func", false),
            ("cold_func", false),
        ] {
            let mut data = FunctionNodeData::new(name);
            data.with_body();
            if always_inline {
                data.mark_always_inline();
            }
            data.usage_count = if name == "hot_func" { 5 } else { 1 };
            cg.nodes.insert(
                name.to_string(),
                CallGraphNode::Function(name.to_string(), Box::new(data)),
            );
        }

        // Get inline candidates
        let candidates = cg.get_inline_candidates(10);

        // Should include always_inline and hot functions
        let candidate_names: Vec<&String> = candidates.iter().map(|(name, _)| name).collect();
        assert!(candidate_names.contains(&&"always_inline_func".to_string()));
        assert!(candidate_names.contains(&&"hot_func".to_string()));
    }
}
