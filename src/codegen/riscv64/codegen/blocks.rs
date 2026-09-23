// src/codegen/riscv64/codegen/blocks.rs - Control Flow Block Processing
// Phase B Day 4-5 - Extracted from riscv64.rs.bak (2026/9/22)
//
// Source locations in riscv64.rs.bak:
// - Basic block decomposition logic
// - Branch target resolution and fixups
// - Label management throughout compilation

use std::collections::{HashMap, VecDeque};
use crate::ast::{UnOp, Stmt as AstStmt};
use super::functions::ValueKind;

/// Control flow graph structure
pub struct CFG {
    /// Basic blocks
    pub blocks: Vec<BasicBlock>,
    /// Entry block index
    pub entry: usize,
    /// Blocks that have exits (return/branch)
    pub exit_blocks: Vec<usize>,
}

impl Default for CFG {
    fn default() -> Self {
        Self {
            blocks: Vec::new(),
            entry: 0,
            exit_blocks: Vec::new(),
        }
    }
}

impl CFG {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build CFG from sequence of statements
    pub fn build_from_statements(&self, statements: &[Statement]) -> Self {
        let mut cfg = Self::new();
        
        // Simple linear decomposition (would be enhanced with control flow analysis)
        cfg.blocks.push(BasicBlock {
            instructions: self.decompose_block(statements),
            predecessors: vec![],
            successors: vec![],
            is_entry: true,
            is_exit: false,
        });

        cfg
    }

    /// Decompose a block into instructions
    fn decompose_block(&self, stmts: &[Statement]) -> Vec<Instruction> {
        let mut instrs = Vec::new();
        
        for stmt in stmts {
            match stmt {
                Statement::Label(name) => {
                    instrs.push(Instruction::Label(name.clone()));
                }
                Statement::Return(opt_expr) => {
                    if let Some(e) = opt_expr {
                        instrs.extend(self.expr_to_instrs(e));
                    }
                    instrs.push(Instruction::Ret);
                }
                Statement::Goto(label) => {
                    instrs.push(Instruction::Jmp(LabelRef::Named(label.clone())));
                }
                Statement::If(cond, then, otherwise) => {
                    instrs.extend(self.expr_to_instrs(cond));
                    instrs.push(Instruction::Branch(
                        0u8, // Condition type placeholder
                        then.is_empty().then(|| LabelRef::Fallthrough),
                        otherwise.as_ref().map(|_| LabelRef::NewTemp),
                    ));
                    for stmt in then {
                        instrs.extend(self.stmt_to_instrs(stmt));
                    }
                }
                _ => {}
            }
        }
        
        instrs
    }

    /// Convert expression to instruction sequence
    fn expr_to_instrs(&self, _expr: &Expression) -> Vec<Instruction> {
        vec![]
    }

    /// Convert statement to instruction sequence
    fn stmt_to_instrs(&self, _stmt: &Statement) -> Vec<Instruction> {
        vec![]
    }

    /// Compute dominators for optimization
    pub fn compute_dominators(&mut self) {
        if self.blocks.is_empty() {
            return;
        }

        let n = self.blocks.len();
        let mut dom: Vec<_> = (0..n).map(|_| 0usize).collect();
        
        // Iterative dominance algorithm
        for _ in 0..n {
            for i in 1..n {
                // Simplified - real implementation would compute predecessors
                dom[i] = i;
            }
        }
    }

    /// Generate jump table for switch statement
    pub fn generate_jump_table(&self, cases: &[CaseInfo], default_target: &str) -> JumpTable {
        JumpTable {
            entries: vec![JumpEntry::Default],
            defaults: vec![default_target.to_string()],
        }
    }

    /// Resolve label references
    pub fn resolve_labels<'a>(&'a self, refs: &'a [String]) -> HashMap<String, usize> {
        let mut resolved = HashMap::new();
        
        for ref_name in refs {
            for (i, block) in self.blocks.iter().enumerate() {
                for instr in &block.instructions {
                    if let Instruction::Label(name) = instr {
                        if name == ref_name {
                            resolved.insert(ref_name.clone(), i);
                        }
                    }
                }
            }
        }

        resolved
    }
}

// ========================================================================
// Basic Block Structure
// ========================================================================

#[derive(Debug, Clone)]
pub struct BasicBlock {
    pub instructions: Vec<Instruction>,
    pub predecessors: Vec<usize>,
    pub successors: Vec<usize>,
    pub is_entry: bool,
    pub is_exit: bool,
}

impl Default for BasicBlock {
    fn default() -> Self {
        Self {
            instructions: Vec::new(),
            predecessors: Vec::new(),
            successors: Vec::new(),
            is_entry: false,
            is_exit: false,
        }
    }
}

// ========================================================================
// Instruction Types
// ========================================================================

#[derive(Debug, Clone)]
pub enum Instruction {
    Nop,
    Add { rd: u8, rs1: u8, rs2: u8 },
    Ld { rd: u8, offset: i32, rs1: u8 },
    St { rs2: u8, offset: i32, rs1: u8 },
    Jmp(LabelRef),
    Branch(u8, Option<LabelRef>, Option<LabelRef>), // cond, true, false targets
    Ret,
    Call(String),
    Label(String),
    Expr(Expression),
    Switch(JumpTable),
}

#[derive(Debug, Clone)]
pub enum LabelRef {
    Named(String),
    NewTemp,
    Fallthrough,
}

// ========================================================================
// Expression Handling
// ========================================================================

#[derive(Debug, Clone)]
pub enum Expression {
    Const(i64),
    Binary(BinaryOp),
    Unary(UnOp, Box<Expression>),
    Load(LValue),
    Call(CallExpr),
}

#[derive(Debug, Clone)]
pub struct BinaryOp {
    pub op: BinOp,
    pub left: Box<Expression>,
    pub right: Box<Expression>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BinOp {
    Eq, Lt, Le, Gt, Ge,
    And, Or, Xor,
    Add, Sub, Mul, Div, Mod,
    Shl, Shr, Sra,
}

#[derive(Debug, Clone)]
pub struct LValue {
    pub base: String,
    pub offset: i32,
    pub ty: ValueKind,
}

#[derive(Debug, Clone)]
pub struct CallExpr {
    pub func: String,
    pub args: Vec<Expression>,
}

// ========================================================================
// Statement Types
// ========================================================================

#[derive(Debug, Clone)]
pub enum Statement {
    Label(String),
    Assign(LValue, Expression),
    If(Expression, Vec<Statement>, Option<Vec<Statement>>),
    While(Expression, Vec<Statement>),
    For(Box<Statement>, Expression, Box<Statement>, Vec<Statement>),
    Switch(Expression, Vec<CaseInfo>),
    Return(Option<Box<Expression>>),
    Goto(String),
    Break,
    Continue,
    InlineAsm(String),
    Expr(Expression),
}

// ========================================================================
// Jump Tables
// ========================================================================

#[derive(Debug, Clone)]
pub struct JumpTable {
    pub entries: Vec<JumpEntry>,
    pub defaults: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum JumpEntry {
    Target(String),
    Default,
    Range { start: i64, end: i64, target: String },
}

// ========================================================================
// Case Information for Switch
// ========================================================================

#[derive(Debug, Clone)]
pub struct CaseInfo {
    pub value: Expression,
    pub body: Vec<Statement>,
    pub is_default: bool,
}

// ========================================================================
// Local Scope Management
// ========================================================================

/// Manages local variable scopes within functions
pub struct ScopeManager {
    /// Current scope depth
    depth: u32,
    /// Variable to offset mapping per scope
    scope_vars: Vec<HashMap<String, i32>>,
    /// Temporary counter
    temp_counter: u32,
}

impl Default for ScopeManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ScopeManager {
    pub fn new() -> Self {
        Self {
            depth: 0,
            scope_vars: vec![HashMap::new()],
            temp_counter: 0,
        }
    }

    /// Enter new scope
    pub fn enter_scope(&mut self) {
        self.scope_vars.push(HashMap::new());
        self.depth += 1;
    }

    /// Exit current scope
    pub fn exit_scope(&mut self) {
        self.scope_vars.pop();
        self.depth -= 1;
    }

    /// Declare local variable with stack offset
    pub fn declare_local(&mut self, name: &str, offset: i32) {
        if let Some(current) = self.scope_vars.last_mut() {
            current.insert(name.to_string(), offset);
        }
    }

    /// Get local variable offset by name
    pub fn get_local_offset(&self, name: &str) -> Option<i32> {
        for scope in self.scope_vars.iter().rev() {
            if let Some(&off) = scope.get(name) {
                return Some(off);
            }
        }
        None
    }

    /// Generate temporary variable name
    pub fn gen_temp(&mut self, prefix: &str) -> String {
        self.temp_counter += 1;
        format!("{}_{}", prefix, self.temp_counter)
    }

    /// Check if name is visible in current scope
    pub fn is_visible(&self, name: &str) -> bool {
        self.scope_vars.iter().any(|scope| scope.contains_key(name))
    }
}

// ========================================================================
// Loop Context Management
// ========================================================================

/// Tracks loop nesting and break/continue targets
pub struct LoopContext {
    pub loops: VecDeque<(String, String)>, // (break_label, continue_label)
}

impl Default for LoopContext {
    fn default() -> Self {
        Self::new()
    }
}

impl LoopContext {
    pub fn new() -> Self {
        Self {
            loops: VecDeque::new(),
        }
    }

    /// Enter a new loop context
    pub fn enter_loop(&mut self, break_label: String, continue_label: String) {
        self.loops.push_back((break_label, continue_label));
    }

    /// Exit current loop
    pub fn exit_loop(&mut self) {
        self.loops.pop_back();
    }

    /// Get current loop's break target
    pub fn current_break_label(&self) -> Option<String> {
        self.loops.back().map(|(br, _)| br.clone())
    }

    /// Get current loop's continue target
    pub fn current_continue_label(&self) -> Option<String> {
        self.loops.back().map(|(_, cont)| cont.clone())
    }

    /// Get outermost loop info
    pub fn outermost_loop(&self) -> Option<(&String, &String)> {
        self.loops.front().map(|(a, b)| (a, b))
    }
}

// ========================================================================
// Helpers
// ========================================================================

/// Collect basic block labels
pub fn collect_block_labels(block: &BasicBlock) -> Vec<String> {
    block.instructions.iter()
        .filter_map(|instr| {
            if let Instruction::Label(name) = instr {
                Some(name.clone())
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cfg_creation() {
        let cfg = CFG::build_from_statements(&[]);
        assert_eq!(cfg.blocks.len(), 1);
        assert!(cfg.blocks[0].is_entry);
    }

    #[test]
    fn test_scope_manager() {
        let mut mgr = ScopeManager::new();
        
        mgr.enter_scope();
        mgr.declare_local("var1", -8);
        
        assert!(mgr.is_visible("var1"));
        assert_eq!(mgr.get_local_offset("var1"), Some(-8));
        
        mgr.exit_scope();
        assert!(!mgr.is_visible("var1"));
    }

    #[test]
    fn test_loop_context() {
        let mut ctx = LoopContext::new();
        
        ctx.enter_loop("break_1".to_string(), "continue_1".to_string());
        
        assert_eq!(ctx.current_break_label(), Some("break_1".to_string()));
        assert_eq!(ctx.current_continue_label(), Some("continue_1".to_string()));
        
        ctx.exit_loop();
        assert!(ctx.current_break_label().is_none());
    }

    #[test]
    fn test_collect_labels() {
        let block = BasicBlock {
            instructions: vec![
                Instruction::Label("L1".to_string()),
                Instruction::Nop,
                Instruction::Label("L2".to_string()),
            ],
            ..Default::default()
        };

        let labels = collect_block_labels(&block);
        assert_eq!(labels, vec!["L1", "L2"]);
    }
}
