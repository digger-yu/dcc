//! Expression executor for all ISA backends
//! Provides common expression evaluation logic across architectures

use crate::ast::{BinOp, UnOp};
use std::fmt;

/// Result type for expression operations
pub type ExprResult<T> = Result<T, String>;

/// Register class identifier
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RegisterClass {
    GPR,  // General Purpose Register
    FPR,  // Floating Point Register
    XMM,  // SIMD Register
}

/// Register value tracking
#[derive(Debug, Clone)]
pub struct RegValue {
    pub reg: u8,
    pub reg_class: RegisterClass,
}

/// Expression executor trait - defines interface for ISA-specific expression evaluation
/// 
/// This trait must be implemented by each backend (x86_64, AArch64, RISC-V, etc.)
pub trait ExpressionExecutor: Sized {
    // ========== BINARY OPERATIONS ==========
    
    fn emit_binary_op(
        &self,
        op: &BinOp,
        result_ty: &Type,
        lhs_reg: u8,
        rhs_reg: u8,
        dest_reg: u8,
    ) -> ExprResult<()>;

    // ========== UNARY OPERATIONS ==========
    
    fn emit_unary_op(
        &self,
        op: &UnOp,
        src_reg: u8,
        dest_reg: u8,
        result_ty: &Type,
    ) -> ExprResult<()>;

    // ========== TYPE CONVERSIONS ==========
    
    fn convert_float_to_int(
        &self,
        src_reg: u8,
        dest_reg: u8,
        signed: bool,
        dest_bits: usize,
    ) -> ExprResult<()>;

    fn convert_int_to_float(&self, src_reg: u8, dest_reg: u8) -> ExprResult<()>;

    fn sign_extend(
        &self,
        src_reg: u8,
        dest_reg: u8,
        bits_from: usize,
        bits_to: usize,
    ) -> ExprResult<()>;

    fn zero_extend(
        &self,
        src_reg: u8,
        dest_reg: u8,
        bits_from: usize,
        bits_to: usize,
    ) -> ExprResult<()>;

    // ========== MEMORY OPERATIONS ==========
    
    fn load_from_register(
        &self,
        addr_reg: u8,
        dest_reg: u8,
        size: usize,
        signed: bool,
    ) -> ExprResult<()>;

    fn store_to_register(
        &self,
        src_reg: u8,
        addr_reg: u8,
        size: usize,
    ) -> ExprResult<()>;

    // ========== REGISTER ALLOCATION ==========
    
    fn allocate_gpr(&mut self, ty: &Type) -> ExprResult<u8>;

    fn allocate_fpr(&mut self, ty: &Type) -> ExprResult<u8>;

    fn spill_register(&mut self, reg: u8, stack_offset: i64) -> ExprResult<()>;

    // ========== ADDITIONAL BACKEND HOOKS ==========
    
    fn load_immediate_constant(&mut self, value: u64, dest_reg: u8, ty: &Type) -> ExprResult<()>;

    fn load_from_symbol_address(&mut self, symbol: &str, dest_reg: u8, ty: &Type) -> ExprResult<()>;

    fn shift_left(&mut self, src_reg: u8, dest_reg: u8, count: u8) -> ExprResult<()>;

    fn imul_imm(&mut self, dest_reg: u8, src_reg: u8, imm: u64) -> ExprResult<()>;

    fn add_regs(&mut self, src1: u8, src2: u8, dest: u8) -> ExprResult<()>;

    fn sub_regs(&mut self, src1: u8, src2: u8, dest: u8) -> ExprResult<()>;

    fn add_imm(&mut self, base_reg: u8, offset: i64, dest_reg: u8) -> ExprResult<()>;

    fn mov_reg_to_reg(&mut self, src_reg: u8, dest_reg: u8, size_bytes: usize) -> ExprResult<()>;

    fn store_address(&mut self, dest_reg: u8, symbol: &str) -> ExprResult<()>;

    fn emit_call(
        &mut self,
        func: &crate::ast::Expr,
        args: &[Box<crate::ast::Expr>],
        result_reg: u8,
    ) -> ExprResult<()>;

    fn branch_if_zero(&mut self, cond_reg: u8, target: &str) -> ExprResult<()>;

    fn jump(&mut self, target: &str) -> ExprResult<()>;

    fn define_label(&mut self, label: &str) -> ExprResult<()>;

    fn clear_register(&mut self, reg: u8, ty: &Type) -> ExprResult<()>;
}
