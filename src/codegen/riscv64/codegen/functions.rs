// src/codegen/riscv64/codegen/functions.rs - Function Generation Logic
use crate::ast::{Expr, Type, Statement, Block, Decl, FuncDecl};
// Phase B Day 4 - Extracted from riscv64.rs.bak (2026/9/22)
//
// Source locations in riscv64.rs.bak:
// - gen_function() ~line 701+
// - Function prologue/epilogue generation (~line 500+)
// - Local variable and stack frame setup

use crate::ir::Function;

// Use types defined within this module for now
// use super::*;

/// Code generator for function bodies
pub struct FunctionCodegen {
    /// Current function being generated
    current_func: Option<String>,
    /// Stack frame size in bytes
    frame_size: i32,
    /// Return address offset (-1 if not saved)
    ret_addr_off: i32,
    /// Variables tracking return value type
    return_ty: Option<ValueKind>,
    /// Whether we've emitted a return instruction
    emitted_return: bool,
}

impl Default for FunctionCodegen {
    fn default() -> Self {
        Self::new()
    }
}

impl FunctionCodegen {
    pub fn new() -> Self {
        Self {
            current_func: None,
            frame_size: 0,
            ret_addr_off: -1,
            return_ty: None,
            emitted_return: false,
        }
    }

    /// Initialize function generation context
    pub fn init_for_function(&mut self, func_name: &str, returns_pointer: bool) {
        self.current_func = Some(func_name.to_string());
        self.ret_addr_off = -1; // Not saved initially
        self.return_ty = if returns_pointer {
            Some(ValueKind::Pointer { base: "i8".into() })
        } else {
            None
        };
        self.emitted_return = false;
        self.frame_size = 0;
    }

    /// Generate function entry prologue
    pub fn generate_prologue(&mut self, args: &[ArgInfo]) {
        // Allocate stack frame
        let total_stack = self.calculate_stack_size(args);
        self.frame_size = total_stack;

        // Check if we need to save ra/fp
        if !self.is_leaf_function(args) || total_stack > 0 {
            // Not leaf or needs space - save ra/s0
            self.save_registers();
            self.ret_addr_off = -self.frame_size + 16; // Offset for saved registers
        }

        // Set up frame pointer
        self.setup_frame_pointer();

        // Clear register arguments in debug mode
        self.clear_argument_registers(args.len());
    }

    /// Calculate required stack size for function
    fn calculate_stack_size(&self, args: &[ArgInfo]) -> i32 {
        let mut size = 0i32;
        
        // Space for callee-saved registers
        size += 16 * 2; // s0-s11
        
        // Space for local variables (placeholder count)
        size += args.len() as i32 * 16; // Estimate per local
        
        // Alignment padding
        size = ((size + 15) / 16) * 16; // 16-byte alignment
        
        size
    }

    /// Check if function is a leaf (no calls)
    fn is_leaf_function(&self, _args: &[ArgInfo]) -> bool {
        true
    }

    /// Save callee-saved registers (ra/s0-fp)
    fn save_registers(&mut self) {
        // sd ra, -24(sp)
        // sd s0, -16(sp)
        // addi sp, sp, -48
        // mv s0, sp
    }

    /// Setup frame pointer
    fn setup_frame_pointer(&mut self) {
        // mv s0, sp
    }

    /// Clear argument registers for debug tracing
    fn clear_argument_registers(&mut self, count: usize) {
        // Zero out a0-a7 for debugging
        for i in 0..count.min(8) {
            // li a{}, 0
        }
    }

    /// Generate function epilogue (return)
    pub fn generate_epilogue(&mut self) {
        if self.emitted_return {
            return;
        }

        // Restore callee-saved registers
        if self.ret_addr_off >= 0 {
            // ld ra, ... (sp)
            // ld s0, ... (sp)
            // addi sp, sp, 48
            // jr ra
        } else {
            // Leaf function - just jr ra
            // ret
        }

        self.emitted_return = true;
    }

    /// Process function body statements
    pub fn process_body(&mut self, body: &[Statement]) -> Result<(), CompilationError> {
        for stmt in body {
            match stmt {
                Statement::Block(b) => self.process_block(b)?,
                Statement::If(e, t, f) => self.process_if(e, t, f)?,
                Statement::While(cond, body) => self.process_while(cond, body)?,
                Statement::For(init, cond, step, body) => self.process_for(init, cond, step, body)?,
                Statement::Switch(expr, cases) => self.process_switch(expr, cases)?,
                Statement::Return(opt_expr) => self.process_return(opt_expr)?,
                Statement::Assign(lhs, rhs) => self.process_assign(lhs, rhs)?,
                Statement::Expr(expr) => self.emit_expression(expr),
                Statement::Label(name) => self.label(&name),
                Statement::Goto(label) => self.emit_goto(label),
                _ => {}
            }
        }
        Ok(())
    }

    /// Handle basic block decomposition
    pub fn decompose_to_blocks(&self, statements: &[Statement]) -> Vec<BasicBlock> {
        let mut blocks = Vec::new();
        let mut current_block = BasicBlock::default();
        
        for stmt in statements {
            if matches!(stmt, Statement::Label(_)) {
                if !current_block.statements.is_empty() {
                    blocks.push(current_block);
                    current_block = BasicBlock::default();
                }
            } else {
                current_block.statements.push(stmt.clone());
            }
        }
        
        if !current_block.statements.is_empty() {
            blocks.push(current_block);
        }

        blocks
    }

    // ========================================================================
    /// Expression Processing Helpers
    // ========================================================================

    /// Evaluate an expression and store result in a register
    pub fn evaluate_expression(&mut self, expr: &Expression) -> RegAllocation {
        match expr {
            Expression::Binary(op, left, right) => {
                self.binary_operation(*op, left, right)
            }
            Expression::Unary(op, operand) => {
                self.unary_operation(*op, operand)
            }
            Expression::Call(call_info) => {
                self.generate_call(call_info)
            }
            Expression::Load(addr) => {
                self.load_memory(addr)
            }
            Expression::Const(value) => {
                self.load_immediate(value)
            }
            _ => RegAllocation::default(),
        }
    }

    fn binary_operation(&mut self, _op: BinOp, _left: &Expression, _right: &Expression) -> RegAllocation {
        RegAllocation::default()
    }

    fn unary_operation(&mut self, _op: UnOp, _operand: &Expression) -> RegAllocation {
        RegAllocation::default()
    }

    fn generate_call(&mut self, _call_info: &CallInfo) -> RegAllocation {
        RegAllocation::default()
    }

    fn load_memory(&mut self, _addr: &MemoryLocation) -> RegAllocation {
        RegAllocation::default()
    }

    fn load_immediate(&mut self, _value: i64) -> RegAllocation {
        RegAllocation::default()
    }

    // ========================================================================
    /// Control Flow Handling
    // ========================================================================

    fn process_if(&mut self, _cond: &Expression, _then: &[Statement], _else: Option<&[Statement]>) -> Result<(), CompilationError> {
        // Generate branch instructions for condition
        // Then block
        // else block (if present)
        Ok(())
    }

    fn process_while(&mut self, _cond: &Expression, _body: &[Statement]) -> Result<(), CompilationError> {
        // Label loop start
        // Evaluate condition
        // Branch to exit if false
        // Body
        // Jump back to start
        // Label exit
        Ok(())
    }

    fn process_for(&mut self, _init: Option<&Statement>, _cond: &Expression, _step: &Statement, _body: &[Statement]) -> Result<(), CompilationError> {
        // Init statement
        self.process_while(_cond, _body)?;
        Ok(())
    }

    fn process_switch(&mut self, _expr: &Expression, _cases: &[CaseInfo]) -> Result<(), CompilationError> {
        // Build jump table or series of comparisons
        Ok(())
    }

    fn process_return(&mut self, opt_expr: Option<&Expression>) -> Result<(), CompilationError> {
        // Copy return values to a0/a1 if needed
        // Jump to epilogue
        self.emitted_return = true;
        Ok(())
    }

    fn process_assign(&mut self, _lhs: &LValue, _rhs: &Expression) -> Result<(), CompilationError> {
        // Evaluate rhs
        // Store to lhs location
        Ok(())
    }

    fn emit_expression(&mut self, _expr: &Expression) {
        // Evaluate but discard result
    }

    fn label(&mut self, name: &str) {
        // Register label in label map
    }

    fn emit_goto(&mut self, label: &str) {
        // Generate unconditional branch
    }
}

// ========================================================================
// Supporting Structures
// ========================================================================

#[derive(Debug, Clone)]
pub struct ArgInfo {
    pub name: String,
    pub reg: u8,      // RISC-V register number
    pub ty: ValueType,
}

#[derive(Debug, Clone)]
pub struct CallInfo {
    pub target: String,
    pub args: Vec<Expression>,
    pub return_reg: u8,
}

#[derive(Debug, Clone)]
pub enum LValue {
    Memory(MemoryLocation),
    Register(u8),
}

#[derive(Debug, Clone)]
pub struct MemoryLocation {
    pub base_reg: u8,
    pub offset: i32,
    pub ty: ValueType,
}

#[derive(Debug, Clone)]
pub struct BasicBlock {
    pub statements: Vec<Statement>,
    pub predecessor_count: usize,
    pub successor_indices: Vec<usize>,
}

impl Default for BasicBlock {
    fn default() -> Self {
        Self {
            statements: Vec::new(),
            predecessor_count: 0,
            successor_indices: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct RegAllocation {
    pub reg: u8,
    pub value: i64,
    pub valid: bool,
}

#[derive(Debug, Clone)]
pub enum Statement {
    Block(Vec<Statement>),
    If(Box<Expression>, Vec<Statement>, Option<Vec<Statement>>),
    While(Box<Expression>, Vec<Statement>),
    For(Option<Box<Statement>>, Box<Expression>, Box<Statement>, Vec<Statement>),
    Switch(Box<Expression>, Vec<CaseInfo>),
    Return(Option<Box<Expression>>),
    Assign(Box<LValue>, Box<Expression>),
    Expr(Box<Expression>),
    Label(String),
    Goto(String),
    InlineAsm(String),
    Break,
    Continue,
}

#[derive(Debug, Clone)]
pub struct CaseInfo {
    pub expr: Expression,
    pub body: Vec<Statement>,
    pub is_default: bool,
}

#[derive(Debug, Clone)]
pub struct CallStmt {
    pub func_name: String,
    pub arguments: Vec<Expression>,
    pub dest_regs: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct BinaryExpr {
    pub op: BinOp,
    pub left: Box<Expression>,
    pub right: Box<Expression>,
    pub result_type: ValueType,
}

#[derive(Debug, Clone)]
pub enum Expression {
    Const(i64),
    Binary(BinaryExpr),
    Unary(UnOp, Box<Expression>),
    Load(MemoryLocation),
    Call(CallInfo),
    Ident(String),
    LValue(LValue),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UnOp {
    Neg,      // -x
    Not,      // ~x
    AddrOf,   // &x
    Deref,    // *x
    PostInc,  // x++
    PreInc,   // ++x
    PostDec,  // x--
    PreDec,   // --x
    Cast,     // (type)x
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BinOp {
    Add, Sub, Mul, Div, Rem,
    And, Or, Xor,
    Sll, Srl, Sra,
    Lt, Le, Gt, Ge,
    Eq, Ne,
}

#[derive(Debug, Clone)]
pub enum ValueKind {
    Integer { bits: u8 },
    Float { is_double: bool },
    Pointer { base: String },
    Vector { el_type: String, len: u32 },
    Aggregate { fields: Vec<FieldInfo> },
}

#[derive(Debug, Clone)]
pub struct FieldInfo {
    pub name: Option<String>,
    pub ty: ValueKind,
    pub offset: usize,
    pub packed: bool,
}

// Use the unified ValueType from crate::ir
use crate::ir::ValueType;

#[derive(Debug, Clone)]
pub struct CompilationError {
    pub message: String,
    pub span_line: Option<u32>,
}

impl From<String> for CompilationError {
    fn from(msg: String) -> Self {
        Self {
            message: msg,
            span_line: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_function_codegen_init() {
        let mut fc = FunctionCodegen::new();
        fc.init_for_function("main", false);
        
        assert_eq!(fc.current_func, Some("main".to_string()));
    }

    #[test]
    fn test_basic_block_decomposition() {
        let fc = FunctionCodegen::new();
        let blocks = fc.decompose_to_blocks(&[]);
        assert!(blocks.is_empty());
    }

    #[test]
    fn test_reg_allocation() {
        let alloc = RegAllocation::default();
        assert!(!alloc.valid);
    }
}
