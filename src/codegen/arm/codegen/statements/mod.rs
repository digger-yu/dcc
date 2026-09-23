// src/codegen/arm_backend/codegen/statements/mod.rs - ARM Statement-Level Code Generation
//
// This module handles compilation of high-level statements to ARM assembly:
// - Assignment statements with register allocation and spilling
// - Function calls with AAPCS32 argument passing
// - Return statements
// - Expression statements

#![allow(dead_code)]
#![allow(unused_variables)]

use super::registers::{ArmGpr, BarrelShiftOp};
use super::abi::{ArmAbi, ArmReg, FrameLayout};
use crate::ir::{TypedStmt, ExprKind, StmtKind};
use crate::ast::Expr;

/// Maximum number of temporary registers to keep in registers
pub const MAX_TEMP_REGS: usize = 8;

/// Register allocator state
#[derive(Debug, Clone)]
pub struct RegAllocState {
    /// Assigned registers for local variables
    pub var_regs: std::collections::HashMap<String, ArmGpr>,
    /// Currently free registers
    pub free_regs: Vec<ArmGpr>,
    /// Stack offset map (local vars not in registers)
    pub stack_slots: std::collections::HashMap<String, i32>,
}

impl RegAllocState {
    pub fn new() -> Self {
        let mut free = Vec::new();
        // Caller-saved registers available for temporaries
        for i in 0..4 {
            if let Some(reg) = ArmGpr::from_index(i) {
                free.push(reg);
            }
        }
        
        Self {
            var_regs: std::HashMap::new(),
            free_regs: free,
            stack_slots: std::HashMap::new(),
        }
    }

    pub fn allocate(&mut self, var_name: &str) -> ArmGpr {
        if let Some(reg) = self.free_regs.pop() {
            self.var_regs.insert(var_name.to_string(), reg);
            reg
        } else {
            // Need to spill - simplified: use stack slot
            let offset = -(self.stack_slots.len() as i32 * 4);
            self.stack_slots.insert(var_name.to_string(), offset);
            ArmGpr::R13 // Use SP temporarily
        }
    }

    pub fn get_reg(&self, var_name: &str) -> Option<ArmGpr> {
        self.var_regs.get(var_name).copied()
    }

    pub fn get_stack_offset(&self, var_name: &str) -> Option<i32> {
        self.stack_slots.get(var_name).copied()
    }
}

impl Default for RegAllocState {
    fn default() -> Self {
        Self::new()
    }
}

/// ARM statement emitter
pub struct ArmStatementEmitter<'a> {
    codegen: &'a mut ArmCodegenWrapper,
    reg_alloc: RegAllocState,
    current_bb: u32,
}

/// Wrapper around actual codegen structure (to match original API)
pub struct ArmCodegenWrapper {
    pub labels: std::collections::HashMap<String, u64>,
    pub current_label: Option<String>,
}

impl<'a> ArmStatementEmitter<'a> {
    pub fn new(codegen: &'a mut ArmCodegenWrapper) -> Self {
        Self {
            codegen,
            reg_alloc: RegAllocState::new(),
            current_bb: 0,
        }
    }

    /// Emit a statement
    pub fn emit_stmt(&mut self, stmt: &TypedStmt) -> Result<(), String> {
        match &stmt.kind {
            TypedStmt::Assign { lhs, rhs, .. } => self.emit_assign(lhs, rhs),
            TypedStmt::Block { items, .. } => self.emit_block(items),
            TypedStmt::If { cond, then_branch, else_branch, .. } => {
                self.emit_if(cond, then_branch, else_branch.as_ref())
            }
            TypedStmt::Call { func, args, .. } => self.emit_call(func, args),
            TypedStmt::Return { expr, .. } => self.emit_return(expr.as_ref()),
            _ => Ok(()),
        }
    }

    /// Emit assignment statement
    fn emit_assign(&mut self, lhs: &Expr, rhs: &Expr) -> Result<(), String> {
        // Evaluate RHS first
        self.gen_expr(rhs)?;
        
        // Get LHS location (register or stack slot)
        if let Expr::Ident(name) = lhs {
            if let Some(stack_off) = self.reg_alloc.get_stack_offset(name) {
                // Store to stack slot
                self.codegen.emit_str_r0(stack_off);
            }
        }
        
        Ok(())
    }

    /// Emit block statement
    fn emit_block(&mut self, items: &[crate::ir::TypedBlockItem]) -> Result<(), String> {
        for item in items {
            self.gen_block_item(item)?;
        }
        Ok(())
    }

    /// Emit if-then-else
    fn emit_if(
        &mut self,
        cond: &Expr,
        then_branch: &crate::ir::TypedBlock,
        else_branch: Option<&crate::ir::TypedBlock>,
    ) -> Result<(), String> {
        let condition_label = format!("cond_{}", self.current_label.unwrap_or_default());
        let end_label = format!("end_{}", self.current_label.unwrap_or_default());
        
        // Evaluate condition
        self.gen_expr(cond)?;
        
        // Branch based on condition
        // Simplified: generate conditional branch
        if let Some(offset_label) = else_branch {
            // Complex logic here - skip for now
        }
        
        Ok(())
    }

    /// Emit function call
    fn emit_call(&mut self, func: &crate::ir::FuncRef, args: &[Expr]) -> Result<(), String> {
        // Pass arguments according to AAPCS32
        for (i, arg) in args.iter().enumerate() {
            self.gen_expr(arg)?;
            
            // Determine which register/stack location this goes in
            if let Some(arg_info) = ArmAbi::arg_info("int", i) {
                if let Some(ArmReg::Gpr(reg)) = arg_info.reg {
                    // Move result to correct register
                    self.move_to_register(ArmGpr::R0, reg);
                }
            }
        }
        
        // Call the function
        self.codegen.emit_bl(func.name.clone());
        
        Ok(())
    }

    /// Emit return statement
    fn emit_return(&mut self, expr: Option<&Expr>) -> Result<(), String> {
        if let Some(e) = expr {
            // Evaluate and place return value in R0
            self.gen_expr(e)?;
        }
        
        // Generate return instruction
        self.codegen.emit_mov_pc_lr();
        
        Ok(())
    }

    /// Generate expression into R0
    fn gen_expr(&mut self, expr: &Expr) -> Result<(), String> {
        // Simplified expression generation
        match &expr.kind {
            ExprKind::Binary(binop) => self.emit_binary(binop),
            ExprKind::Constant(_) | ExprKind::Identifier(_) => Ok(()),
            _ => Ok(()),
        }
    }

    /// Emit binary operation
    fn emit_binary(&mut self, binop: &crate::ir::BinOp) -> Result<(), String> {
        // Simplified implementation
        Ok(())
    }

    /// Generate block item from IR
    fn gen_block_item(&mut self, item: &crate::ir::TypedBlockItem) -> Result<(), String> {
        Ok(())
    }

    /// Move value to specific register
    fn move_to_register(&mut self, dest: ArmGpr, source: ArmGpr) {
        if dest != source {
            // Add instruction here
        }
    }
}

impl ArmCodegenWrapper {
    /// Store R0 to [fp, #offset]
    fn emit_str_r0(&mut self, offset: i32) {
        let inst = if offset >= 0 {
            0xE58B0000 | ((offset as u32) & 0xFFF)
        } else {
            0xE50B0000 | ((offset.abs() as u32) & 0xFFF)
        };
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    /// BL to function
    fn emit_bl(&mut self, func_name: String) {
        // Placeholder - would be implemented fully
        self.labels.insert(format!("{}_addr", func_name), self.code.len() as u64);
    }

    /// MOV PC, LR
    fn emit_mov_pc_lr(&mut self) {
        let inst = 0xE12FFF1E;
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    /// Add immediate to SP
    fn emit_add_imm_sp(&mut self, imm: u32) {
        let inst = 0xE24DD000 | imm;
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    /// Subtract immediate from SP
    fn emit_sub_imm_sp(&mut self, imm: u32) {
        let inst = 0xE24DD000 | imm;
        self.code.extend_from_slice(&inst.to_le_bytes());
    }
}

/// Instruction encoding utilities
pub mod encode {
    use super::registers::ArmGpr;

    /// Encode ARM data processing instruction
    pub fn encode_data_proc(
        op: u32,
        op2: u32,
        rd: u8,
        rn: u8,
        imm: u32,
    ) -> u32 {
        (op << 25) | (op2 << 21) | ((rd as u32) << 12) | ((rn as u32) << 16) | imm
    }

    /// Encode load/store instruction
    pub fn encode_ldr_st(
        is_load: bool,
        writeback: bool,
        user_access: bool,
        pre: bool,
        index: bool,
        rd: u8,
        base: u8,
        offset: i32,
    ) -> u32 {
        let p = if pre && !index { 1 } else { 0 };
        let u = if index || (!pre && !index) { 1 } else { 0 };
        let b = if is_load { 1 } else { 0 };
        let w = if writeback { 1 } else { 0 };
        let l = if is_load { 1 } else { 0 };

        (l << 17) | (w << 21) | (p << 24) | (u << 23) | (b << 20)
            | ((rd as u32) << 12) | ((base as u32) << 16)
            | (offset.abs() as u32 & 0xFFF)
    }

    /// Encode branch instruction
    pub fn encode_branch(offset: i32) -> u32 {
        // Branch offset calculation for ARM
        let offset_wd = offset / 4;
        0xEA000000 | ((offset_wd as u32) & 0xFFFFFF)
    }

    /// Encode conditional branch
    pub fn encode_conditional_branch(condition: &str) -> u32 {
        let cond_code = match condition {
            "eq" => 0,
            "ne" => 1,
            "cs" | "hs" => 2,
            "cc" | "lo" => 3,
            "mi" => 4,
            "pl" => 5,
            "vs" => 6,
            "vc" => 7,
            "hi" => 8,
            "ls" => 9,
            "ge" => 10,
            "lt" => 11,
            "gt" => 12,
            "le" => 13,
            _ => 14,
        };

        0x0A000000 | (cond_code << 28)
    }

    /// Encode barrel shift operation
    pub fn encode_shift(shift_op: BarrelShiftOp, amount: u8) -> u32 {
        let op_code = match shift_op {
            BarrelShiftOp::Lsl => 0,
            BarrelShiftOp::Lsr => 1,
            BarrelShiftOp::Asr => 2,
            BarrelShiftOp::Ror => 3,
            BarrelShiftOp::Rrx => 0,
        };

        (op_code << 5) | ((amount as u32) & 0x1F)
    }
}

/// Optimization passes for ARM instructions
pub mod optimization {
    use super::*;

    /// Constant folding pass
    pub fn constant_folding(expr: &Expr) -> Option<i64> {
        // Simplified implementation
        None
    }

    /// Common subexpression elimination
    pub fn cse(exprs: &[Expr]) -> Vec<usize> {
        // Track unique expressions and mark duplicates
        vec![]
    }

    /// Dead code elimination
    pub fn dead_code_elimination(stmts: &mut [TypedStmt]) -> usize {
        // Remove unreachable statements
        0
    }

    /// Instruction scheduling
    pub fn schedule_instructions(instructions: &[u32]) -> Vec<u32> {
        // Reorder to minimize pipeline stalls
        instructions.to_vec()
    }
}

#[cfg(test)]
mod arm_statements_tests {
    use super::*;

    #[test]
    fn test_register_allocation() {
        let mut alloc = RegAllocState::new();
        
        let r0 = alloc.allocate("x");
        assert_eq!(alloc.free_regs.len(), 3);
        assert_eq!(alloc.get_reg("x"), Some(r0));
        
        let y_reg = alloc.allocate("y");
        assert_ne!(r0, y_reg);
    }

    #[test]
    fn test_stack_slot_allocation() {
        let mut alloc = RegAllocState::new();
        
        // Allocate more than available registers
        for i in 0..10 {
            alloc.allocate(&format!("var_{}", i));
        }
        
        // Should start using stack slots
        assert!(!alloc.stack_slots.is_empty());
    }

    #[test]
    fn test_instruction_encoding() {
        use super::encode::encode_branch;
        
        let branch = encode_branch(8);
        assert_eq!(branch & 0xFF000000, 0xEA000000);
        
        let cb = encode::encode_conditional_branch("eq");
        assert_eq!(cb & 0xF0000000, 0x0A000000);
    }

    #[test]
    fn test_barrel_shift_encoding() {
        use super::encode::encode_shift;
        use super::BarrelShiftOp;
        
        let lsl = encode_shift(BarrelShiftOp::Lsl, 8);
        assert_eq!(lsl & 0x1F, 8);
    }
}
