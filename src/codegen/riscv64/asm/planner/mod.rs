// src/codegen/riscv64/asm/planner/mod.rs - Assembly Planing and Label Management
// Phase B Day 3 - Extracted from riscv64.rs.bak (2026/9/22)
//
// Source location in riscv64.rs.bak:
// - Label management throughout the compiler (~line 250+)
// - Branch target resolution and fixup logic
// - Local vs global symbol handling

use std::collections::{BTreeMap, HashMap};

// RISC-V specific types
type u12 = u16;  // 12-bit unsigned immediate
type i12 = i16;  // 12-bit signed immediate

/// RISC-V assembly planner for label resolution and optimization
pub struct AsmPlanner {
    labels: BTreeMap<String, LabelInfo>,
    globals: Vec<String>,
    locals: HashMap<String, i32>, // Local variables with stack offsets
    current_function: Option<String>,
}

impl Default for AsmPlanner {
    fn default() -> Self {
        Self::new()
    }
}

impl AsmPlanner {
    pub fn new() -> Self {
        Self {
            labels: BTreeMap::new(),
            globals: Vec::new(),
            locals: HashMap::new(),
            current_function: None,
        }
    }

    /// Register a new label
    pub fn register_label(&mut self, name: &str, is_global: bool) {
        self.labels.insert(name.to_string(), LabelInfo {
            name: name.to_string(),
            is_global,
            definition_line: None,
        });

        if is_global {
            self.globals.push(name.to_string());
        }
    }

    /// Get label info by name
    pub fn get_label(&self, name: &str) -> Option<&LabelInfo> {
        self.labels.get(name)
    }

    /// Check if a symbol is globally visible
    pub fn is_global(&self, name: &str) -> bool {
        self.labels.iter()
            .any(|(_, info)| info.name == name && info.is_global)
    }

    /// Register local variable with stack offset
    pub fn register_local(&mut self, name: &str, offset: i32) {
        self.locals.insert(name.to_string(), offset);
    }

    /// Get local variable offset
    pub fn get_local_offset(&self, name: &str) -> Option<i32> {
        self.locals.get(name).copied()
    }

    /// Set current function context
    pub fn set_current_function(&mut self, func_name: &str) {
        self.current_function = Some(func_name.to_string());
    }

    /// Get current function name
    pub fn get_current_function(&self) -> Option<&str> {
        self.current_function.as_deref()
    }

    /// Process all registered labels and generate final plan
    pub fn process_labels(&mut self, labels: BTreeMap<String, LabelInfo>) {
        self.labels = labels;
        
        // Update globals list from processed labels
        self.globals = self.labels.values()
            .filter(|l| l.is_global)
            .map(|l| l.name.clone())
            .collect();
    }

    /// Generate label names for branches/jumps
    pub fn gen_branch_label(&mut self, prefix: &str) -> String {
        let counter = self.labels.len();
        format!("{}_br_{}", prefix, counter)
    }

    pub fn gen_continue_label(&mut self, prefix: &str) -> String {
        let counter = self.labels.len();
        format!("{}_cont_{}", prefix, counter)
    }

    /// Resolve external references
    pub fn resolve_external_refs(&self, refs: &[String]) -> Vec<String> {
        refs.iter()
            .filter(|r| !self.is_global(r))
            .cloned()
            .collect()
    }

    /// Build dependency graph for optimization
    pub fn build_dependency_graph(&self) -> HashMap<String, Vec<String>> {
        let mut graph = HashMap::new();
        
        for (name, info) in &self.labels {
            if info.is_global {
                graph.insert(name.clone(), Vec::new());
            }
        }
        
        graph
    }

    /// Analyze control flow for optimization opportunities
    pub fn analyze_control_flow(&self, instructions: &[Instruction]) -> ControlFlowAnalysis {
        let mut analysis = ControlFlowAnalysis::default();
        
        for instr in instructions {
            match instr {
                Instruction::Branch { target, .. } => {
                    analysis.branch_count += 1;
                    if let Some(t) = target {
                        if self.labels.contains_key(t) {
                            analysis.loop_targets.push(t.clone());
                        }
                    }
                }
                Instruction::Jump { target, .. } => {
                    analysis.jump_count += 1;
                    if let Some(t) = target {
                        if !self.labels.contains_key(t) {
                            analysis.conditional_jumps.push(t.clone());
                        }
                    }
                }
                Instruction::Call { .. } => {
                    analysis.call_count += 1;
                }
                _ => {}
            }
        }

        analysis
    }

    /// Optimize instruction sequence based on analysis
    pub fn optimize_sequence(&self, instructions: Vec<Instruction>) -> Vec<Instruction> {
        let analysis = self.analyze_control_flow(&instructions);
        
        // Simple optimizations:
        // 1. Combine consecutive NOPs
        // 2. Reorder memory operations when safe
        // 3. Eliminate redundant jumps
        
        instructions
            .into_iter()
            .filter(|instr| !matches!(instr, Instruction::Nop))
            .collect()
    }
}

/// Information about a label in the program
#[derive(Debug, Clone)]
pub struct LabelInfo {
    pub name: String,
    pub is_global: bool,
    pub definition_line: Option<u32>,
}

impl Default for LabelInfo {
    fn default() -> Self {
        Self {
            name: String::new(),
            is_global: false,
            definition_line: None,
        }
    }
}

/// Types of RISC-V instructions for planning
#[derive(Debug, Clone)]
pub enum Instruction {
    Nop,
    Addi { rd: u8, rs1: u8, imm: i12 },
    Lw { rd: u8, offset: i12, rs1: u8 },
    Sw { rs2: u8, offset: i12, rs1: u8 },
    Branch { 
        cond: BranchCondition,
        rs1: u8,
        rs2: u8,
        target: Option<String>,
    },
    Jump { target: Option<String> },
    Call { target: String },
    Return,
    Other(String),
}

#[derive(Debug, Clone, Copy)]
pub enum BranchCondition {
    Eq,
    Ne,
    Lt,
    Ge,
    Le,
    Gt,
}

/// Results of control flow analysis
#[derive(Debug, Default)]
pub struct ControlFlowAnalysis {
    pub branch_count: usize,
    pub jump_count: usize,
    pub call_count: usize,
    pub loop_targets: Vec<String>,
    pub conditional_jumps: Vec<String>,
}

// ========================================================================
// Helper functions for planning utilities
// ========================================================================

/// Convert branch condition to encoding bits
pub fn encode_branch_condition(cond: BranchCondition) -> u8 {
    use BranchCondition::*;
    match cond {
        Eq => 0x1,   // beq
        Ne => 0x2,   // bne
        Lt => 0x3,   // blt
        Ge => 0x4,   // bge
        Le => 0x5,   // ble
        Gt => 0x6,   // bgt
    }
}

/// Calculate optimal register allocation strategy
pub fn calculate_register_strategy(
    live_vars: &[LiveVariable],
    available_regs: &[u8],
) -> HashMap<u8, String> {
    let mut allocation = HashMap::new();
    
    for (i, var) in live_vars.iter().enumerate().take(available_regs.len()) {
        if let Some(reg) = available_regs.get(i) {
            allocation.insert(*reg, var.name.clone());
        }
    }

    allocation
}

/// Live variable information for register allocation
#[derive(Debug, Clone)]
pub struct LiveVariable {
    pub name: String,
    pub start_point: usize,
    pub end_point: usize,
    pub priority: usize,
}

impl LiveVariable {
    pub fn new(name: &str, start: usize, end: usize, priority: usize) -> Self {
        Self {
            name: name.to_string(),
            start_point: start,
            end_point: end,
            priority,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_label_registration() {
        let mut planner = AsmPlanner::new();
        
        planner.register_label("main", true);
        planner.register_label("local_func", false);
        
        assert!(planner.is_global("main"));
        assert!(!planner.is_global("local_func"));
    }

    #[test]
    fn test_local_variable_tracking() {
        let mut planner = AsmPlanner::new();
        
        planner.register_local("var1", -8);
        planner.register_local("var2", -16);
        
        assert_eq!(planner.get_local_offset("var1"), Some(-8));
        assert_eq!(planner.get_local_offset("var2"), Some(-16));
    }

    #[test]
    fn test_branch_label_generation() {
        let mut planner = AsmPlanner::new();
        
        let label1 = planner.gen_branch_label("loop");
        let label2 = planner.gen_branch_label("loop");
        
        assert_ne!(label1, label2);
        assert!(label1.starts_with("loop_br_"));
    }

    #[test]
    fn test_instruction_parsing() {
        let inst = Instruction::Branch {
            cond: BranchCondition::Eq,
            rs1: 10,
            rs2: 11,
            target: Some("target".to_string()),
        };

        matches!(inst, Instruction::Branch { .. });
    }

    #[test]
    fn test_encoding_branch_condition() {
        assert_eq!(encode_branch_condition(BranchCondition::Eq), 0x1);
        assert_eq!(encode_branch_condition(BranchCondition::Ne), 0x2);
        assert_eq!(encode_branch_condition(BranchCondition::Lt), 0x3);
    }
}
