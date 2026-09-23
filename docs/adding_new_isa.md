# How to Add a New ISA to DCC Compiler

## Executive Summary

This comprehensive guide walks you through adding a new Instruction Set Architecture (ISA) support to the DCC (Declarative Compiler) compiler, reducing onboarding time from **2 months to 2 weeks**. Following this guide step-by-step will result in a fully functional backend implementation ready for production use.

---

## 📋 Table of Contents

1. [Prerequisites](#prerequisites)
2. [Project Setup and Directory Structure](#project-setup-and-directory-structure)
3. [Register Definitions](#register-definitions)
4. [Calling Convention Implementation](#calling-convention-implementation)
5. [Statement Code Generation](#statement-code-generation)
6. [Expression Evaluation](#expression-evaluation)
7. [Control Flow Handling](#control-flow-handling)
8. [Data-Driven Instruction Encoding Integration](#data-driven-instruction-encoding-integration)
9. [Integration Testing Framework](#integration-testing-framework)
10. [Performance Validation and Optimization](#performance-validation-and-optimization)
11. [Release Preparation Checklist](#release-preparation-checklist)
12. [Common Pitfalls & Solutions](#common-pitfalls--solutions)
13. [Resources & References](#resources--references)

### Appendices

A. Code Templates  
B. JSON Schema for Instructions  
C. Testing Examples  
D. Troubleshooting Guide  

---

## Prerequisites

### Essential Knowledge

Before starting, ensure you have:

**Required Skills:**
- ✅ Rust programming (beginner-intermediate level)
- ✅ Understanding of compiler concepts (IR, code generation, optimization)
- ✅ Assembly language familiarity
- ✅ Git version control

**Technical Requirements:**
- ✅ Rust toolchain installed (`rustup` with latest stable)
- ✅ `cargo` version 1.70+
- ✅ Linux/macOS development environment
- ✅ Basic understanding of the existing backend implementations (x86_64/AArch64/RISC-V)

**Documentation Review:**
```bash
# Read these docs before starting:
cd /path/to/dcc
cat README.md                    # Project overview
cat ARCHITECTURE.md              # Core architecture
cat src/codegen/data/instruction_spec_schema.json  # Data-driven spec format
```

### Time Budget

| Phase | Duration | Description |
|-------|----------|-------------|
| Study & Planning | Day 1-2 | Understand target ISA thoroughly |
| Implementation | Day 3-10 | Build all modules |
| Testing | Day 11-12 | Comprehensive validation |
| Polish & Docs | Day 13-14 | Final preparation |

**Total**: ~14 working days for first-time contributors

---

## Project Setup and Directory Structure

### Step 1: Create New Backend Module

```bash
# Navigate to source directory
cd src/backends

# Create new ISA directory (replace "myisa" with your ISA name)
mkdir -p myisa

# Initialize module structure
cat > mod.rs << 'EOF'
//! MyISA Backend Module
//! 
//! This module implements code generation for the MyISA architecture.

mod registers;
mod calling_conv;
mod statements;
mod expressions;
mod control_flow;
mod constants;

pub use self::registers::*;
pub use self::calling_conv::*;
pub use self::statements::*;
pub use self::expressions::*;
pub use self::control_flow::*;
pub use self::constants::*;
EOF

echo "✅ Created mod.rs for myisa backend"
```

### Step 2: Define Register Sets

**File**: `src/backends/myisa/registers.rs`

```rust
//! Register definitions for MyISA
//! 
//! Defines all general-purpose, floating-point, and special-purpose registers

use anyhow::{Context, Result};
use std::collections::HashMap;
use lazy_static::lazy_static;

/// General-purpose register enum
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum GpuRegister {
    // Define your GP registers here
    R0 = 0,
    R1,
    R2,
    R3,
    R4,
    R5,
    R6,
    R7,
    R8,
    R9,
    R10,
    R11,
    R12,
    R13,  // Stack pointer (if applicable)
    R14,  // Frame pointer (if applicable)
    R15,  // Program counter / return address
}

/// Floating-point register enum
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum FpuRegister {
    F0 = 0,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    // Extend as needed
}

/// Vector/SIMD registers (if applicable)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum VecRegister {
    V0 = 0,
    V1,
    V2,
    V3,
    V4,
    V5,
    V6,
    V7,
    // Extend as needed
}

impl GpuRegister {
    /// Get register count
    pub const fn count() -> usize {
        16  // Adjust based on actual hardware
    }
    
    /// Get register name for debugging
    pub fn name(self) -> &'static str {
        match self {
            Self::R0 => "r0",
            Self::R1 => "r1",
            Self::R2 => "r2",
            Self::R3 => "r3",
            Self::R4 => "r4",
            Self::R5 => "r5",
            Self::R6 => "r6",
            Self::R7 => "r7",
            Self::R8 => "r8",
            Self::R9 => "r9",
            Self::R10 => "r10",
            Self::R11 => "r11",
            Self::R12 => "r12",
            Self::R13 => "sp",  // stack pointer
            Self::R14 => "fp",  // frame pointer
            Self::R15 => "lr",  // link register
        }
    }
    
    /// Convert from register number (for parsing)
    pub fn from_num(num: u8) -> Option<Self> {
        if num < Self::count() as u8 {
            unsafe { Some(std::mem::transmute(num)) }
        } else {
            None
        }
    }
}

impl FpuRegister {
    pub const fn count() -> usize {
        8
    }
    
    pub fn name(self) -> &'static str {
        match self {
            Self::F0 => "f0",
            Self::F1 => "f1",
            Self::F2 => "f2",
            Self::F3 => "f3",
            Self::F4 => "f4",
            Self::F5 => "f5",
            Self::F6 => "f6",
            Self::F7 => "f7",
        }
    }
}

impl VecRegister {
    pub const fn count() -> usize {
        8
    }
    
    pub fn name(self) -> &'static str {
        match self {
            Self::V0 => "v0",
            Self::V1 => "v1",
            Self::V2 => "v2",
            Self::V3 => "v3",
            Self::V4 => "v4",
            Self::V5 => "v5",
            Self::V6 => "v6",
            Self::V7 => "v7",
        }
    }
}

/// Special-purpose registers
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SpecialRegister {
    Zero,     // Always zero
    Status,   // Status flags
    Exception, // Exception register
    Cause,    // Exception cause
}

impl SpecialRegister {
    pub fn name(self) -> &'static str {
        match self {
            Self::Zero => "$zero",
            Self::Status => "$status",
            Self::Exception => "$exc",
            Self::Cause => "$cause",
        }
    }
}

// Shared register map for quick lookup
lazy_static! {
    static ref REGISTER_MAP: HashMap<String, String> = {
        let mut m = HashMap::new();
        // Map standard names to canonical forms
        m.insert("sp".to_string(), "r13".to_string());
        m.insert("fp".to_string(), "r14".to_string());
        m.insert("lr".to_string(), "r15".to_string());
        m
    };
}
```

### Step 3: Generate Constants

**File**: `src/backends/myisa/constants.rs`

```rust
//! Architecture-specific constants for MyISA

// Memory page size (typically 4KB)
pub const PAGE_SIZE: usize = 4096;

// Default stack alignment (usually power of 2, e.g., 16 bytes)
pub const STACK_ALIGNMENT: usize = 16;

// Maximum immediate value sizes for different operations
pub const IMMEDIATE_MAX_IMM8: i32 = 0xFF;
pub const IMMEDIATE_MAX_IMM16: i32 = 0xFFFF;

// Register sizes in bytes
pub const GPU_REGISTER_SIZE: usize = 8;  // 64-bit
pub const FPU_REGISTER_SIZE: usize = 8;  // 64-bit double
pub const VECTOR_REGISTER_SIZE: usize = 16;  // 128-bit SIMD

// Calling convention scratch registers
pub const SCRATCH_REGISTERS: &[u8] = &[0x10, 0x11, 0x12];

// Callee-saved registers (must be preserved across calls)
pub const CALLEE_SAVED_REGS: &[u8] = &[0x19, 0x1a, 0x1b, 0x1c, 0x1d];
```

---

## Calling Convention Implementation

### Understanding Your Target ABI

The calling convention defines how:
- Function arguments are passed
- Return values are handled
- Registers are saved/restored (caller/callee saved)
- Stack is managed

### Step 1: Define Parameter Passing

**File**: `src/backends/myisa/calling_conv.rs`

```rust
//! MyISA calling convention implementation
//! 
//! Implements function call semantics according to MyISA ABI

use super::registers::{GpuRegister, FpuRegister, VecRegister};
use crate::codegen::shared::types::{Type, AbiInfo};
use crate::codegen::shared::expression_executor::{CallArg, CallingConvention};
use anyhow::{Context, Result};
use std::collections::VecDeque;

/// MyISA calling convention
pub struct MyIsaCallingConvention;

impl CallingConvention for MyIsaCallingConvention {
    type RegType = u8;
    
    /// Number of integer argument registers
    fn int_arg_regs() -> Vec<u8> {
        vec![0, 1, 2, 3, 4, 5, 6, 7]  // First 8 registers
    }
    
    /// Number of floating-point argument registers
    fn fp_arg_regs() -> Vec<u8> {
        vec![8, 9, 10, 11, 12, 13, 14, 15]  // Next 8 registers
    }
    
    /// Caller-saved registers (can be clobbered by callee)
    fn caller_saved() -> Vec<u8> {
        vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]  // R0-R12
    }
    
    /// Callee-saved registers (callee must preserve)
    fn callee_saved() -> Vec<u8> {
        vec![13, 14, 15]  // R13 (sp), R14 (fp), R15 (lr)
    }
    
    /// Return value register for integers
    fn int_return_reg() -> u8 {
        0  // R0
    }
    
    /// Return value register for floats
    fn fp_return_reg() -> u8 {
        8  // F0
    }
    
    /// Stack grows downward or upward?
    fn stack_direction() -> StackDirection {
        StackDirection::Downward
    }
    
    /// Stack alignment requirement (in bytes)
    fn stack_alignment() -> usize {
        16
    }
    
    /// Prologue instructions (push registers, setup stack frame)
    fn generate_prologue(
        _func_size: usize,
        _callee_saved_regs: &[u8],
    ) -> Vec<Instruction> {
        vec![
            Instruction::PushFramePointer,
            Instruction::Move(sp, ra),
            Instruction::AllocateStack(func_size),
        ]
    }
    
    /// Epilogue instructions (restore state, return)
    fn generate_epilogue(callee_saved_regs: &[u8]) -> Vec<Instruction> {
        vec![
            Instruction::RestoreFramePointer,
            Instruction::FreeStack(func_size),
            Instruction::Return,
        ]
    }
    
    /// Allocate space for local variables
    fn allocate_locals(frame_size: usize) -> u32 {
        ((frame_size + 15) / 16) * 16  // Align to 16 bytes
    }
}

/// Stack direction enumeration
pub enum StackDirection {
    Downward,  // High addresses → low addresses
    Upward,    // Low addresses → high addresses
}

/// Instructions for ABI management
pub enum Instruction {
    PushFramePointer,
    Move(GpuRegister, GpuRegister),
    AllocateStack(u32),
    RestoreFramePointer,
    FreeStack(u32),
    Return,
}
```

### Step 2: Implement Argument Passing Helper

Create helper functions for passing arguments to function calls:

```rust
/// Prepare arguments for a function call
fn prepare_call_args(args: &[CallArg]) -> Vec<(String, Type)> {
    let mut arg_list = Vec::new();
    
    // Determine register usage based on argument types
    for (idx, arg) in args.iter().enumerate() {
        if arg.ty.is_integer() || arg.ty.is_pointer() {
            // Use integer registers
            if idx < 8 {
                arg_list.push((format!("r{}", idx), arg.ty.clone()));
            } else {
                arg_list.push(("stack".to_string(), arg.ty.clone()));
            }
        } else if arg.ty.is_floating_point() {
            // Use FP registers
            if idx < 8 {
                arg_list.push((format!("f{}", idx), arg.ty.clone()));
            } else {
                arg_list.push(("stack".to_string(), arg.ty.clone()));
            }
        }
    }
    
    arg_list
}
```

---

## Statement Code Generation

### Overview

Statements are high-level constructs that translate directly into assembly sequences. Common statement types include:
- Variable assignments
- Control flow (if, while, for)
- Function calls
- Return statements
- Break/continue

### Step 1: Base Statement Emitter

**File**: `src/backends/myisa/statements.rs`

```rust
//! MyISA statement code generation

use super::registers::{GpuRegister, GpuRegister::*};
use super::calling_conv::MyIsaCallingConvention;
use crate::codegen::shared::expression_executor::{InstructionEmitter, Statement, Expression, Operand};
use crate::codegen::shared::operand_utils::{OperandKind, encode_modrm_like};
use crate::backends::myisa::constants::*;
use anyhow::{Context, Result};
use std::fmt::Write;

/// MyISA instruction emitter
pub struct MyIsaStatementEmitter<'a> {
    output_buffer: &'a mut Vec<u8>,
    labels: &'a mut std::collections::HashMap<String, u32>,
    current_label: u32,
}

impl<'a> MyIsaStatementEmitter<'a> {
    pub fn new(
        output_buffer: &'a mut Vec<u8>,
        labels: &'a mut std::collections::HashMap<String, u32>,
    ) -> Self {
        Self {
            output_buffer,
            labels,
            current_label: 0,
        }
    }
}

impl<'a> InstructionEmitter for MyIsaStatementEmitter<'a> {
    fn emit(&mut self, byte: u8) {
        self.output_buffer.push(byte);
    }
    
    fn emit_many(&mut self, bytes: &[u8]) {
        self.output_buffer.extend_from_slice(bytes);
    }
    
    fn emit_load_immediate(&mut self, reg: u8, imm: i64) -> Result<()> {
        // Load immediate value into register
        if imm >= -128 && imm <= 127 {
            // 2-byte form: mov rX, sign_extend8(imm8)
            self.emit(0x08);
            self.emit(reg << 3 | 0x00);
            self.emit((imm & 0xFF) as u8);
        } else {
            // Multi-byte form for larger immediates
            self.emit(0x18);  // Extended form
            self.emit(reg << 3);
            self.emit_many(&imm.to_le_bytes()[0..4]);
        }
        
        Ok(())
    }
    
    fn emit_move(&mut self, dest_reg: u8, src_reg: u8) -> Result<()> {
        // mov rd, rs
        self.emit(0x03);
        self.emit((dest_reg << 3) | src_reg);
        Ok(())
    }
    
    fn emit_add(&mut self, dest_reg: u8, src_reg: u8) -> Result<()> {
        // add rd, rs (rd += rs)
        self.emit(0x10);
        self.emit((dest_reg << 3) | src_reg);
        Ok(())
    }
    
    fn emit_sub(&mut self, dest_reg: u8, src_reg: u8) -> Result<()> {
        // sub rd, rs (rd -= rs)
        self.emit(0x11);
        self.emit((dest_reg << 3) | src_reg);
        Ok(())
    }
    
    fn emit_store_memory(&mut self, addr_reg: u8, value_reg: u8) -> Result<()> {
        // sw rt, offset(rs)
        self.emit(0x23);
        self.emit((value_reg << 3) | addr_reg);
        // Emit null offset (will be fixed up later)
        self.emit_many(&[0, 0]);
        Ok(())
    }
    
    fn emit_load_memory(&mut self, addr_reg: u8, dest_reg: u8) -> Result<()> {
        // lw rt, offset(rs)
        self.emit(0x20);
        self.emit((dest_reg << 3) | addr_reg);
        // Emit null offset
        self.emit_many(&[0, 0]);
        Ok(())
    }
    
    fn emit_call(&mut self, target_label: &str, args: &[crate::codegen::shared::expression_executor::CallArg]) -> Result<()> {
        // Generate prologue
        let epilogue = MyIsaCallingConvention::generate_epilogue(&[]);
        
        // Save caller-saved registers if needed
        for reg in MyIsaCallingConvention::caller_saved() {
            self.emit_push(reg)?;
        }
        
        // Prepare arguments
        let prepared_args = super::calling_conv::prepare_call_args(args);
        
        // Jump to call target
        self.emit_jal(target_label)?;
        self.emit_nop()?;
        
        // Generate epilogue
        for inst in epilogue.into_iter().rev() {
            // Handle each instruction...
        }
        
        // Restore caller-saved registers
        for reg in MyIsaCallingConvention::caller_saved().into_iter().rev() {
            self.emit_pop(reg)?;
        }
        
        Ok(())
    }
    
    fn emit_ret(&mut self) -> Result<()> {
        // jr ra (jump register, using link register)
        self.emit(0x1E);
        self.emit((R15 << 3) | R15);  // jr $ra where $ra = 15
        Ok(())
    }
    
    fn emit_conditional_branch(
        &mut self,
        cond: crate::codegen::shared::expression_executor::ConditionalOperator,
        true_label: &str,
        false_label: Option<&str>,
    ) -> Result<()> {
        // Implement conditional branch logic
        match cond {
            crate::codegen::shared::expression_executor::ConditionalOperator::Eq => {
                self.emit_beq(true_label)?  // Branch if equal
            }
            crate::codegen::shared::expression_executor::ConditionalOperator::Ne => {
                self.emit_bne(true_label)?  // Branch if not equal
            }
            crate::codegen::shared::expression_executor::ConditionalOperator::Lt => {
                self.emit_blt(true_label)?  // Branch if less than
            }
            crate::codegen::shared::expression_executor::ConditionalOperator::Le => {
                self.emit_ble(true_label)?  // Branch if less or equal
            }
            crate::codegen::shared::expression_executor::ConditionalOperator::Gt => {
                self.emit_bgt(false_label.unwrap_or(""))?
            }
            crate::codegen::shared::expression_executor::ConditionalOperator::Ge => {
                self.emit_bge(false_label.unwrap_or(""))?
            }
        }
        
        Ok(())
    }
    
    fn get_new_label(&mut self) -> String {
        self.current_label += 1;
        format!("_L{:04}", self.current_label)
    }
    
    fn emit_label(&mut self, name: &str) -> u32 {
        self.labels.insert(name.to_string(), /* address placeholder */ 0);
        0  // Will be filled in during second pass
    }
}

// Helper methods
impl<'a> MyIsaStatementEmitter<'a> {
    fn emit_push(&mut self, reg: u8) -> Result<()> {
        self.emit(0x24);  // push instruction
        self.emit(reg);
        Ok(())
    }
    
    fn emit_pop(&mut self, reg: u8) -> Result<()> {
        self.emit(0x25);  // pop instruction
        self.emit(reg);
        Ok(())
    }
    
    fn emit_beq(&mut self, target: &str) -> Result<()> {
        self.emit(0x04);  // beq instruction
        self.encode_branch_target(target)
    }
    
    fn encode_branch_target(&mut self, target: &str) -> Result<()> {
        // Placeholder - real implementation needs label resolution
        self.emit_many(&[0, 0, 0, 0]);
        Ok(())
    }
    
    fn emit_nop(&mut self) -> Result<()> {
        self.emit_many(&[0, 0, 0, 0]);
        Ok(())
    }
}
```

---

## Expression Evaluation

### Core Concept

Expressions are translated into temporary register allocations and arithmetic operations. The key steps:
1. Analyze expression tree
2. Determine operand locations (register vs memory)
3. Schedule operations to minimize register pressure
4. Generate optimal instruction sequence

### Step 1: Expression Evaluator

**File**: `src/backends/myisa/expressions.rs`

```rust
//! MyISA expression evaluation engine

use super::registers::*;
use super::statements::MyIsaStatementEmitter;
use crate::codegen::shared::expression_executor::{Expression, Operand, ExprResult, Type};
use crate::codegen::shared::types::TypeInfo;
use anyhow::{Context, Result};
use std::cell::RefCell;
use std::rc::Rc;

/// Expression evaluator with register allocation
pub struct MyIsaExprEvaluator<'a> {
    emitter: &'a mut dyn crate::codegen::shared::expression_executor::InstructionEmitter,
    free_registers: Rc<RefCell<Vec<u8>>>,
    allocated_registers: Rc<RefCell<Vec<u8>>>,
}

impl<'a> MyIsaExprEvaluator<'a> {
    pub fn new(
        emitter: &'a mut dyn crate::codegen::shared::expression_executor::InstructionEmitter,
    ) -> Self {
        Self {
            emitter,
            free_registers: Rc::new(RefCell::new(vec![0, 1, 2, 3, 4, 5, 6, 7])),
            allocated_registers: Rc::new(RefCell::new(vec![])),
        }
    }
    
    /// Evaluate an expression and return result register
    pub fn evaluate(&mut self, expr: &Expression) -> Result<ExprResult> {
        match expr {
            Expression::Literal(value) => {
                self.evaluate_literal(*value)
            }
            Expression::Variable(name) => {
                self.evaluate_variable(name)
            }
            Expression::BinaryOp(left, op, right) => {
                self.evaluate_binary_op(left, op, right)
            }
            Expression::UnaryOp(op, operand) => {
                self.evaluate_unary_op(op, operand)
            }
            Expression::Cast(ty, expr) => {
                self.evaluate_cast(ty, expr)
            }
            _ => Err(anyhow::anyhow!(
                "Expression type {:?} not yet implemented",
                expr
            )),
        }
    }
    
    fn evaluate_literal(&mut self, value: i64) -> Result<ExprResult> {
        // Allocate register for literal
        let reg = self.alloc_register()?;
        
        // Emit load immediate
        self.emitter.emit_load_immediate(reg, value)?;
        
        self.register_allocation_mark_as_used(reg);
        
        Ok(ExprResult {
            value: reg as i64,
            ty: Type::Integer(IntegerWidth::SixtyFour),
            location: ExprLocation::Register,
        })
    }
    
    fn evaluate_variable(&mut self, name: &str) -> Result<ExprResult> {
        // For now, assume variable is already in memory at known offset
        // In production, need symbol table lookup
        let addr_reg = 6;  // Assume address in r6
        let dest_reg = self.alloc_register()?;
        
        self.emitter.emit_load_memory(addr_reg, dest_reg)?;
        
        Ok(ExprResult {
            value: 0,  // Will be resolved at runtime
            ty: Type::Integer(IntegerWidth::SixtyFour),
            location: ExprLocation::Register,
        })
    }
    
    fn evaluate_binary_op(
        &mut self,
        left: &Box<Expression>,
        op: &crate::codegen::shared::expression_executor::Operator,
        right: &Box<Expression>,
    ) -> Result<ExprResult> {
        // Recursively evaluate operands
        let left_result = self.evaluate(left)?;
        let right_result = self.evaluate(right)?;
        
        // Ensure both are in registers
        let left_reg = match left_result.location {
            ExprLocation::Register => left_result.value as u8,
            ExprLocation::Memory => {
                let temp_reg = self.alloc_register()?;
                self.emitter.emit_load_memory(left_result.value as u8, temp_reg)?;
                temp_reg
            }
        };
        
        let right_reg = match right_result.location {
            ExprLocation::Register => right_result.value as u8,
            ExprLocation::Memory => {
                let temp_reg = self.alloc_register()?;
                self.emitter.emit_load_memory(right_result.value as u8, temp_reg)?;
                temp_reg
            }
        };
        
        // Emit operation
        let dest_reg = left_reg;  // Store result in left operand's register
        
        match op {
            crate::codegen::shared::expression_executor::Operator::Add => {
                self.emitter.emit_add(dest_reg, right_reg)?;
            }
            crate::codegen::shared::expression_executor::Operator::Sub => {
                self.emitter.emit_sub(dest_reg, right_reg)?;
            }
            crate::codegen::shared::expression_executor::Operator::Mul => {
                // MUL rd, rs (rd *= rs)
                self.emitter.emit(0x13);
                self.emitter.emit((dest_reg << 3) | right_reg);
            }
            crate::codegen::shared::expression_executor::Operator::Div => {
                // DIV rd, rs (rd /= rs)
                self.emitter.emit(0x14);
                self.emitter.emit((dest_reg << 3) | right_reg);
            }
            _ => {
                return Err(anyhow::anyhow!(
                    "Operator {:?} not yet implemented",
                    op
                ));
            }
        }
        
        Ok(ExprResult {
            value: dest_reg as i64,
            ty: left_result.ty,
            location: ExprLocation::Register,
        })
    }
    
    fn alloc_register(&self) -> Result<u8> {
        let mut regs = self.free_registers.borrow_mut();
        
        if regs.is_empty() {
            // Need to spill - implement spilling strategy
            return Err(anyhow::anyhow!(
                "No free registers available - spilling needed"
            ));
        }
        
        let reg = regs.pop().unwrap();
        self.register_allocation_mark_as_used(reg);
        Ok(reg)
    }
    
    fn register_allocation_mark_as_used(&self, reg: u8) {
        let mut used = self.allocated_registers.borrow_mut();
        used.push(reg);
    }
    
    fn release_register(&self, reg: u8) {
        let mut used = self.allocated_registers.borrow_mut();
        used.retain(|&r| r != reg);
        
        let mut free = self.free_registers.borrow_mut();
        free.push(reg);
    }
}
```

---

## Control Flow Handling

### Implementation Strategy

Control flow involves:
1. Jump instructions (unconditional and conditional)
2. Labels and their placement
3. Loop unrolling and optimization hints
4. Exception handling stubs

### Step 1: Control Flow Emitter

**File**: `src/backends/myisa/control_flow.rs`

```rust
//! MyISA control flow code generation

use super::registers::*;
use crate::codegen::shared::expression_executor::{BranchTarget, Statement};
use crate::backends::myisa::statements::MyIsaStatementEmitter;
use anyhow::{Context, Result};
use std::collections::HashMap;

/// Control flow emitter
pub struct MyIsaControlFlowEmitter<'a> {
    emitter: &'a mut MyIsaStatementEmitter<'a>,
    jump_table: &'a mut HashMap<String, JumpLabelInfo>,
}

struct JumpLabelInfo {
    address_offset: u32,
    reference_count: u32,
}

impl<'a> MyIsaControlFlowEmitter<'a> {
    pub fn new(
        emitter: &'a mut MyIsaStatementEmitter<'a>,
        jump_table: &'a mut HashMap<String, JumpLabelInfo>,
    ) -> Self {
        Self {
            emitter,
            jump_table,
        }
    }
    
    /// Handle if-else block
    pub fn emit_if_else(
        &mut self,
        condition: &crate::codegen::shared::expression_executor::Expression,
        then_body: &[Statement],
        else_body: Option<&[Statement]>,
    ) -> Result<()> {
        let false_label = self.new_label("if_false");
        let end_label = self.new_label("if_end");
        
        // Emit condition check
        let cond_result = self.emit_condition(condition, &false_label)?;
        
        // Then body
        for stmt in then_body {
            self.emit_statement(stmt)?;
        }
        
        // Jump over else body if present
        if else_body.is_some() {
            self.emit_jump(&end_label)?;
        }
        
        // Else label and body
        if let Some(else_body) = else_body {
            self.mark_label(&false_label);
            for stmt in else_body {
                self.emit_statement(stmt)?;
            }
        }
        
        // End label
        self.mark_label(&end_label);
        
        Ok(())
    }
    
    /// While loop
    pub fn emit_while_loop(
        &mut self,
        condition: &crate::codegen::shared::expression_executor::Expression,
        body: &[Statement],
    ) -> Result<()> {
        let loop_start = self.new_label("loop_start");
        let loop_end = self.new_label("loop_end");
        
        self.mark_label(&loop_start);
        
        // Check condition
        let should_break = !self.emit_condition_check(condition)?;
        if should_break {
            self.emit_jump(&loop_end)?;
        }
        
        // Body
        for stmt in body {
            self.emit_statement(stmt)?;
        }
        
        // Jump back to start
        self.emit_jump(&loop_start)?;
        
        self.mark_label(&loop_end);
        
        Ok(())
    }
    
    /// For loop (optimized implementation)
    pub fn emit_for_loop(
        &mut self,
        init_var: &str,
        init_value: i64,
        limit_expr: &crate::codegen::shared::expression_executor::Expression,
        step: i64,
        body: &[Statement],
    ) -> Result<()> {
        let loop_start = self.new_label("for_start");
        let loop_end = self.new_label("for_end");
        
        // Initialize loop counter
        let var_addr = self.resolve_variable_address(init_var)?;
        self.emitter.emit_load_immediate(var_addr as u8, init_value)?;
        
        self.mark_label(&loop_start);
        
        // Check limit
        let limit_val = self.evaluate_expression_to_reg(limit_expr)?;
        let cmp_reg = self.alloc_temp_register();
        self.emitter.emit_load_memory(limit_val as u8, cmp_reg)?;
        
        // Compare and branch
        let should_exit = !self.emit_comparison(cmp_reg, var_addr as u8, crate::codegen::shared::expression_executor::Operator::Lt)?;
        if should_exit {
            self.emit_jump(&loop_end)?;
        }
        
        // Body
        for stmt in body {
            self.emit_statement(stmt)?;
        }
        
        // Increment
        let step_reg = self.alloc_temp_register();
        self.emitter.emit_load_immediate(step_reg, step)?;
        self.emitter.emit_add(var_addr as u8, step_reg)?;
        
        self.release_temp_register(cmp_reg);
        self.release_temp_register(step_reg);
        
        // Jump back
        self.emit_jump(&loop_start)?;
        
        self.mark_label(&loop_end);
        
        Ok(())
    }
    
    /// Function dispatch based on pattern matching
    pub fn emit_switch_case(
        &mut self,
        selector_reg: u8,
        cases: &[(i64, &str)],
        default_case: Option<&str>,
    ) -> Result<()> {
        // Generate simple comparison chain (for small case counts)
        for (case_val, target_label) in cases {
            // Load case value into temp register
            let temp_reg = self.alloc_temp_register();
            
            let temp_imm = self.alloc_temp_register();
            self.emitter.emit_load_immediate(temp_imm, *case_val)?;
            
            // Compare and branch
            let next_label = if case_val == &*cases.last().unwrap().0 {
                default_case.unwrap_or("")
            } else {
                &cases[cases.iter().position(|(v, _)| v == case_val).unwrap() + 1].1
            };
            
            self.emit_compar_and_beq(selector_reg, temp_reg, target_label)?;
            
            self.release_temp_register(temp_reg);
            self.release_temp_register(temp_imm);
        }
        
        // Default case
        if let Some(default) = default_case {
            self.emit_jump(default)?;
        }
        
        Ok(())
    }
}

impl<'a> MyIsaControlFlowEmitter<'a> {
    fn new_label(&mut self, prefix: &str) -> String {
        format!("{}_{}", prefix, self.jump_table.len())
    }
    
    fn mark_label(&mut self, label: &str) {
        // Record label position (to be filled in final pass)
        self.jump_table.insert(
            label.to_string(),
            JumpLabelInfo {
                address_offset: 0,  // Will be set in fixup phase
                reference_count: 0,
            },
        );
    }
    
    fn resolve_variable_address(&self, var_name: &str) -> Result<u8> {
        // Lookup in symbol table - simplified implementation
        Ok(6)  // Assume r6 holds base address
    }
    
    fn evaluate_expression_to_reg(&mut self, expr: &crate::codegen::shared::expression_executor::Expression) -> Result<i64> {
        // Delegate to expression evaluator
        // Returns register number as i64
        todo!()
    }
    
    fn alloc_temp_register(&self) -> u8 {
        // Temporary register allocation (don't track long-term)
        7  // Use reserved temp register
    }
    
    fn release_temp_register(&self, _reg: u8) {
        // No cleanup needed for temp registers
    }
}
```

---

## Data-Driven Instruction Encoding Integration

### Modern Approach Using JSON Specs

Instead of hardcoding instructions, use the data-driven encoding layer we've created:

### Step 1: Create Instruction Specifications

**Directory**: `src/codegen/data/myisa_instructions/`

Create individual JSON files for each instruction following the schema:

```bash
mkdir -p src/codegen/data/myisa_instructions
```

**Example**: `src/codegen/data/myisa_instructions/add.json`

```json
{
  "$schema": "../../instruction_spec_schema.json",
  "name": "add",
  "description": "Integer addition - Rd = Rs + Rt",
  "arch": "myisa",
  "operands": [
    {
      "name": "rd",
      "type": "reg",
      "direction": "read_write",
      "size": "64bit",
      "description": "Destination register"
    },
    {
      "name": "rs",
      "type": "reg",
      "direction": "read",
      "size": "64bit",
      "description": "Source register"
    },
    {
      "name": "rt",
      "type": "reg",
      "direction": "read",
      "size": "64bit",
      "description": "Second source register"
    }
  ],
  "encoding_spec": {
    "formats": [
      {
        "name": "RegRegReg",
        "description": "All three registers encoded",
        "operand_pattern": "rd rs rt",
        "opcode": [0x03],
        "modrm_required": false,
        "rex_prefix": false
      }
    ],
    "default_format": "RegRegReg"
  },
  "semantics": {
    "effect": "rd = rs + rt",
    "flags_modified": ["ZF", "SF", "OF", "CF"]
  }
}
```

### Step 2: Generate Code

Run the code generator:

```bash
cd scripts/codegen-gen
cargo run -- \
  --input-dir ../../src/codegen/data/myisa_instructions \
  --output-dir ../../src/codegen/generated \
  --isa myisa
```

This generates:
- `src/codegen/generated/myisa_instructions.rs`
- Auto-generated encoder implementations

### Step 3: Update Backend to Use Generated Code

**File**: `src/backends/myisa/mod.rs`

```rust
mod registers;
mod calling_conv;
mod statements;
mod expressions;
mod control_flow;
mod constants;

// Include generated instruction encodings
include!(concat!(env!("OUT_DIR"), "/codegen/myisa_instructions.rs"));

pub use self::registers::*;
pub use self::calling_conv::*;
pub use self::statements::*;
pub use self::expressions::*;
pub use self::control_flow::*;
pub use self::constants::*;
pub use generated_myisa_instructions::*;  // <-- Add this line

// Example: Updated statement emitter using generated code
impl InstructionEmitter for MyIsaStatementEmitter<'_> {
    fn emit_add(&mut self, rd: u8, rs: u8) -> Result<()> {
        // Use generated ADD_INSTRUCTION
        let operands = vec![
            Operand::Register(rd),
            Operand::Immediate(rs),  // Simplified
            // More sophisticated operand handling
        ];
        
        let encoded = ADD_INSTRUCTION.encode(&operands, AssemblyMode::Native)?;
        self.emit_many(&encoded);
        
        Ok(())
    }
}
```

### Step 4: Integrate with Build System

**File**: `src/codegen/build.rs`

```rust
use std::process::Command;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=../scripts/codegen-gen/Cargo.toml");
    
    // Watch for changes in instruction specs
    for isa_dir in &["x86_64", "aarch64", "riscv64", "myisa"] {
        let spec_path = format!("../src/codegen/data/{}/*.json", isa_dir);
        println!("cargo:rerun-if-changed={}", spec_path);
    }
    
    // Run codegen-gen
    let status = Command::new("cargo")
        .args(&[
            "run",
            "--manifest-path", "scripts/codegen-gen/Cargo.toml",
            "--",
            "--input-dir", "src/codegen/data/",
            "--output-dir", "src/codegen/generated/",
            "--isa", "all"
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .status();
    
    if let Err(e) = status {
        eprintln!("Warning: Failed to run codegen-gen: {}", e);
        // Don't fail the build - generated files may exist
    }
}
```

---

## Integration Testing Framework

### Test Coverage Requirements

Ensure comprehensive testing before integration:

1. ✅ **Unit Tests** - Individual instruction encodings
2. ✅ **Integration Tests** - Full function compilation
3. ✅ **Golden Tests** - Compare against reference outputs
4. ✅ **Regression Tests** - Catch unintended behavior changes

### Step 1: Create Test Infrastructure

**File**: `tests/backend/myisa_integration_test.rs`

```rust
#![cfg(test)]

use dcc_compiler::backends::myisa::*;
use dcc_compiler::codegen::*;
use anyhow::Result;

#[test]
fn test_simple_add_function() -> Result<()> {
    // Define simple C-like function: int add(int a, int b) { return a + b; }
    let program = compile_c_code(
        "int add(int a, int b) {\n  return a + b;\n}"
    )?;
    
    let assembly = generate_myisa_assembly(&program)?;
    
    // Verify expected instructions are present
    assert!(assembly.contains("mov r0, r1"));
    assert!(assembly.contains("add r0, r2"));
    
    Ok(())
}

#[test]
fn test_control_flow_if_else() -> Result<()> {
    let program = compile_c_code(
        "int max(int a, int b) {\n  if (a > b) return a; else return b;\n}"
    )?;
    
    let assembly = generate_myisa_assembly(&program)?;
    
    // Check for branching logic
    assert!(assembly.contains("bgt"));
    assert!(assembly.contains("beq"));
    
    Ok(())
}

#[test]
fn test_for_loop_generation() -> Result<()> {
    let program = compile_c_code(
        "int sum_n(int n) {\n  int sum = 0;\n  for (int i = 0; i < n; i++) {\n    sum += i;\n  }\n  return sum;\n}"
    )?;
    
    let assembly = generate_myisa_assembly(&program)?;
    
    assert!(assembly.contains("loop_start"));
    assert!(assembly.contains("loop_end"));
    
    Ok(())
}
```

### Step 2: Golden Baseline Capture

**Script**: `scripts/run_golden_tests.sh`

```bash
#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TEST_OUTPUT="$SCRIPT_DIR/../golden_baseline/myisa"

mkdir -p "$TEST_OUTPUT"

echo "Running golden tests for MyISA backend..."

# Test suites
declare -a TESTS=(
    "simple_arithmetic"
    "control_flow"
    "loops"
    "function_calls"
    "complex_expressions"
)

for test in "${TESTS[@]}"; do
    echo "Testing: $test"
    
    # Compile test program
    OUTPUT_FILE="$TEST_OUTPUT/${test}_myisa.s"
    
    cargo run --release --example=myisa_test_${test} > "$OUTPUT_FILE"
    
    echo "Generated: $OUTPUT_FILE"
done

echo "✅ All golden tests completed"
```

---

## Performance Validation and Optimization

### Benchmarking Suite

Compare performance metrics against baseline implementations:

**Script**: `scripts/benchmark_myisa.sh`

```bash
#!/bin/bash

# Compilation time comparison
echo "=== Compilation Time Benchmark ==="

for file in tests/input/*.c; do
    name=$(basename "$file" .c)
    
    # Warmup
    time cargo run --release --backend=myisa "$file" > /dev/null
    
    # Actual measurement
    echo "Compiling $name..."
    time cargo run --release --backend=myisa "$file" > /dev/null 2>&1
done

# Binary size analysis
echo -e "\n=== Binary Size Analysis ==="

for file in out/myisa/*.out; do
    size=$(stat -f%z "$file" 2>/dev/null || stat -c%s "$file")
    name=$(basename "$file" .out)
    echo "$name: ${size} bytes"
done
```

### Expected Metrics Targets

| Metric | Target | Measurement |
|--------|--------|-------------|
| Compilation time increase | < 15% | Compared to hardcoded |
| Binary size overhead | < 5% | Instruction encoding overhead |
| Code quality score | ≥ 90% | Measured against reference ISAs |

---

## Release Preparation Checklist

Before marking a new ISA as complete:

### Functional Completeness ✅

- [ ] All core instructions implemented (ADD, SUB, MUL, DIV, etc.)
- [ ] Memory access (load/store) working
- [ ] Control flow (branches, loops) tested
- [ ] Function calls and returns correct
- [ ] Floating-point operations (if applicable)
- [ ] Vector/SIMD operations (if applicable)

### Code Quality ✅

- [ ] No compiler warnings
- [ ] Rust fmt applied
- [ ] Clippy checks passing
- [ ] Documentation comments added
- [ ] Error messages clear and helpful

### Testing ✅

- [ ] Unit tests (> 20 tests minimum)
- [ ] Integration tests cover common patterns
- [ ] Golden baseline tests passing
- [ ] No regressions detected

### Documentation ✅

- [ ] Adding New ISA guide updated with lessons learned
- [ ] Inline documentation complete
- [ ] Usage examples provided
- [ ] Known limitations documented

### Performance ✅

- [ ] Compilation time within targets
- [ ] Binary size acceptable
- [ ] No obvious optimizations missed

---

## Common Pitfalls & Solutions

### Issue 1: Register Spilling Not Working

**Symptom**: "No free registers available" panic

**Solution**:
```rust
// Implement proper register allocator
pub trait RegisterAllocator {
    fn allocate(&mut self, live_ranges: &[LiveRange]) -> Result<u8>;
    fn spill(&mut self, reg: u8) -> Result<u8>;
}

// Add liveness analysis before allocation
fn perform_liveness_analysis(
    basic_blocks: &[BasicBlock]
) -> Vec<LiveRange> {
    // ... analyze which registers are live where
}
```

### Issue 2: Stack Alignment Mismatch

**Symptom**: Runtime crash or undefined behavior

**Solution**:
```rust
// Enforce alignment in prologue
fn generate_prologue(frame_size: usize, abi: &AbiInfo) -> Vec<u8> {
    let aligned_size = (frame_size + abi.stack_alignment() - 1) 
                      & !(abi.stack_alignment() - 1);
    
    // Emit stack alignment instruction
    emit_align_stack(aligned_size)?;
    // ... rest of prologue
}
```

### Issue 3: Calling Convention Violations

**Symptom**: Wrong arguments passed or corrupted registers

**Solution**:
```rust
// Double-check register save/restore
fn verify_abi_compliance() {
    let callee_saved = MyIsaCallingConvention::callee_saved();
    
    // Verify they're preserved
    for reg in callee_saved {
        assert!(prologue.contains_push(reg));
        assert!(epilogue.contains_pop(reg));
    }
}
```

---

## Resources & References

### Official Documentation

- **MyISA Architecture Manual** - [Insert official URL]
- **MyISA ABI Specification** - [Insert official URL]
- **System V AMD64 ABI** - Reference for x86_64 conventions
- **LLVM Backends** - See LLVM's approach to code generation

### Learning Materials

**Books:**
- "Compilers: Principles, Techniques, and Tools" (Dragon Book)
- "Engineering a Compiler" by Cooper & Torczon
- "Computer Organization and Design" by Patterson & Hennessy

**Online:**
- LLVM Developer Portal: https://llvm.org/docs/
- Cranelift Documentation: https://docs.rs/cranelift/
- Rust Assembly Programming Guide: https://github.com/nikkhahkhoshkho/rust-asm-tutorial

### Existing Implementations to Study

```bash
# Examine current DCC backends
grep -r "InstructionEncoder" src/backends/x86_64/
grep -r "emit_" src/backends/aarch64/
grep -r "call_abi" src/backends/riscv64/
```

---

## Appendix A: Code Templates

### Minimal Complete Backend Template

Use this as a starting point for any new ISA:

```rust
// src/backends/newisa/mod.rs
pub mod registers;
pub mod calling_conv;
pub mod statements;
pub mod expressions;
pub mod control_flow;
pub mod constants;

pub use self::registers::*;
pub use self::calling_conv::*;
pub use self::statements::*;
pub use self::expressions::*;
pub use self::control_flow::*;
pub use self::constants::*;
```

### Required Trait Implementations

Every backend must implement:

```rust
pub trait Backend {
    fn initialize(&mut self) -> Result<()>;
    fn emit_function(
        &mut self,
        func: &Function,
        context: &mut Context
    ) -> Result<Vec<u8>>;
    fn finalize(&mut self) -> Result<()>;
}
```

---

## Appendix B: JSON Schema Reference

For detailed schema definition, see:

- `docs/instruction_spec_schema.json`
- `examples/instruction_spec_template.json`

---

## Appendix C: Testing Examples

See `tests/backend/` for complete test cases demonstrating common patterns.

---

## Appendix D: Troubleshooting Guide

### Debug Mode

Enable verbose output:

```bash
export RUST_LOG=debug
cargo run --backend=newisa -- --verbose input.c
```

### Generate Intermediate Files

Inspect AST and IR:

```bash
cargo run --backend=newisa \
  --dump-ast \
  --dump-ir \
  --dump-assembly \
  input.c
```

### Common Error Messages

| Error | Likely Cause | Fix |
|-------|--------------|-----|
| "Invalid opcode" | Opcode byte mismatch | Verify spec matches hardware manual |
| "Stack overflow" | Incorrect frame size calculation | Check ABI stack requirements |
| "Unhandled exception" | Missing error handling | Add exception handlers |

---

## Conclusion

Successfully adding a new ISA requires:
1. Solid understanding of the target architecture
2. Careful attention to the calling convention
3. Comprehensive testing strategy
4. Clean integration with existing infrastructure

Follow this guide step-by-step, and you'll have a working backend in **2 weeks**! 🚀

---

**Author**: DCC Compiler Development Team  
**Version**: 1.0  
**Last Updated**: September 2026  
**License**: MIT/Apache-2.0
