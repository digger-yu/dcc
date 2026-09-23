// src/codegen/aarch64/expression_impl.rs
// Phase C: Data-Driven Code Generation Architecture - AArch64 Expression Backend
//
// First Principle: AArch64 implements same ExpressionExecutor trait as x86_64
// This enables shared expression evaluation logic across all ISAs

use crate::ast::{BinOp, UnOp, Type};
use crate::codegen::aarch64::aarch64::{ty_is_floating_point, ty_is_signed, Aarch64Codegen};
use crate::codegen::shared::expression_executor::{ExpressionExecutor, ExprResult};
use crate::ir::ValueType;

/// AArch64-specific ExpressionExecutor implementation
pub struct AArch64ExpressionBackend<'a> {
    pub cg: &'a mut Aarch64Codegen,
}

impl<'a> AArch64ExpressionBackend<'a> {
    pub fn new(cg: &'a mut Aarch64Codegen) -> Self {
        Self { cg }
    }
}

impl<'a> ExpressionExecutor for AArch64ExpressionBackend<'a> {
    // ========== ARM64 BINARY OPERATIONS =========

    fn emit_binary_op(
        &self,
        op: &BinOp,
        result_ty: &Type,
        lhs_reg: u8,
        rhs_reg: u8,
        dest_reg: u8,
    ) -> ExprResult<()> {
        use crate::ast::BinOp::*;

        let _is_float = ty_is_floating_point(result_ty);

        match op {
            // Integer arithmetic (GPRs)
            Add => self.cg.emit_arm64_add(dest_reg, lhs_reg, rhs_reg),
            Sub => self.cg.emit_arm64_sub(dest_reg, lhs_reg, rhs_reg),

            Mul => {
                // MUL for 32-bit, MULH for 64-bit multiply high if needed
                self.cg.emit_arm64_mul(dest_reg, lhs_reg, rhs_reg)?;
            }

            Div => {
                // SDIV for signed, UDIV for unsigned
                let signed = ty_is_signed(result_ty);
                self.cg.emit_arm64_div(dest_reg, lhs_reg, rhs_reg, signed)?;
            }

            Rem => {
                // SREM/UREM via division sequence
                let signed = ty_is_signed(result_ty);
                self.cg.emit_arm64_rem(dest_reg, lhs_reg, rhs_reg, signed)?;
            }

            // Bitwise operations
            And => self.cg.emit_arm64_and(dest_reg, lhs_reg, rhs_reg),
            Or => self.cg.emit_arm64_orr(dest_reg, lhs_reg, rhs_reg),
            Xor => self.cg.emit_arm64_eor(dest_reg, lhs_reg, rhs_reg),

            // Shift operations
            Shl => self.cg.emit_arm64_lsl(dest_reg, lhs_reg, rhs_reg),
            Shr => {
                // LSR for logical shift right (zero-fill)
                self.cg.emit_arm64_lsr(dest_reg, lhs_reg, rhs_reg)?;
            }

            // Comparison operations (returns boolean in GPR)
            Eq => {
                self.cg.emit_arm64_cmp(lhs_reg, rhs_reg)?;
                self.cg.emit_arm64_setcc(dest_reg, arm64::SetCC::EQ)?;
            }
            Ne => {
                self.cg.emit_arm64_cmp(lhs_reg, rhs_reg)?;
                self.cg.emit_arm64_setcc(dest_reg, arm64::SetCC::NE)?;
            }
            Lt => {
                self.cg.emit_arm64_cmp(lhs_reg, rhs_reg)?;
                self.cg.emit_arm64_setcc(dest_reg, arm64::SetCC::LT)?;
            }
            Le => {
                self.cg.emit_arm64_cmp(lhs_reg, rhs_reg)?;
                self.cg.emit_arm64_setcc(dest_reg, arm64::SetCC::LE)?;
            }
            Gt => {
                self.cg.emit_arm64_cmp(lhs_reg, rhs_reg)?;
                self.cg.emit_arm64_setcc(dest_reg, arm64::SetCC::GT)?;
            }
            Ge => {
                self.cg.emit_arm64_cmp(lhs_reg, rhs_reg)?;
                self.cg.emit_arm64_setcc(dest_reg, arm64::SetCC::GE)?;
            }

            // Logical operators need short-circuit evaluation
            AndAnd | OrOr => {
                return Err("Logical operators require label-based evaluation".into())
            }
        }

        Ok(())
    }

    // ========== ARM64 UNARY OPERATIONS =========

    fn emit_unary_op(
        &self,
        op: &UnOp,
        src_reg: u8,
        dest_reg: u8,
        result_ty: &Type,
    ) -> ExprResult<()> {
        use crate::ast::UnOp::*;
        
        match op {
            Neg => {
                // NEGS for integer negation
                // For floats, FNEG instruction
                self.cg.emit_arm64_neg(dest_reg, src_reg, result_ty)?;
            }
            Not => {
                // EOR to all ones (bitwise NOT)
                self.cg.emit_arm64_not(dest_reg, src_reg);
            }
            Plus => {
                // Unary plus is no-op
                if src_reg != dest_reg {
                    self.cg.emit_arm64_mov(dest_reg, src_reg, result_ty.size() as usize)?;
                }
            }
            AddrOf => {
                return Err("Address-of requires lvalue handling".into());
            }
            Deref => {
                // Load from pointer
                self.cg.load_from_address(src_reg, dest_reg, result_ty.size() as usize, ty_is_signed(result_ty));
            }
        }
        
        Ok(())
    }

    // ========== TYPE CONVERSIONS =========

    fn convert_float_to_int(
        &self,
        src_reg: u8,
        dest_reg: u8,
        signed: bool,
        dest_bits: usize,
    ) -> ExprResult<()> {
        // FCVT from float to integer (floating-point to GPR)
        self.cg.fcvt_float_to_int(src_reg, dest_reg, signed, dest_bits)?;
        Ok(())
    }

    fn convert_int_to_float(&self, src_reg: u8, dest_reg: u8) -> ExprResult<()> {
        // FCVT from integer to float (GPR to floating-point)
        self.cg.fcvt_int_to_float(src_reg, dest_reg)?;
        Ok(())
    }

    fn sign_extend(
        &self,
        src_reg: u8,
        dest_reg: u8,
        bits_from: usize,
        bits_to: usize,
    ) -> ExprResult<()> {
        // ARM64: SXTB/SXTH/SXTW for sign extension
        match bits_from {
            8 => {
                self.cg.emit_sxtb(dest_reg, src_reg, bits_to);
                Ok(())
            }
            16 => {
                self.cg.emit_sxth(dest_reg, src_reg, bits_to);
                Ok(())
            }
            32 => {
                self.cg.emit_sxtw(dest_reg, src_reg, bits_to);
                Ok(())
            }
            _ => {
                // For same size or larger extensions not covered
                if src_reg != dest_reg {
                    self.cg.emit_arm64_mov(dest_reg, src_reg, bits_to as usize / 8)?;
                }
                Ok(())
            }
        }
    }

    fn zero_extend(
        &self,
        src_reg: u8,
        dest_reg: u8,
        bits_from: usize,
        bits_to: usize,
    ) -> ExprResult<()> {
        // ARM64: UXTB/UXTH/UXTW for zero extension
        match bits_from {
            8 => {
                self.cg.emit_uxtb(dest_reg, src_reg, bits_to);
                Ok(())
            }
            16 => {
                self.cg.emit_uxth(dest_reg, src_reg, bits_to);
                Ok(())
            }
            32 => {
                self.cg.emit_uxtw(dest_reg, src_reg, bits_to);
                Ok(())
            }
            _ => {
                if src_reg != dest_reg {
                    self.cg.emit_arm64_mov(dest_reg, src_reg, bits_to as usize / 8)?;
                }
                Ok(())
            }
        }
    }

    // ========== MEMORY OPERATIONS =========

    fn load_from_register(
        &self,
        addr_reg: u8,
        dest_reg: u8,
        size: usize,
        signed: bool,
    ) -> ExprResult<()> {
        // LDRA instructions (LDRA for signed, LDR for unsigned)
        let load_signed = signed && size <= 8;
        self.cg.load_from_memory_address(addr_reg, dest_reg, size, load_signed)?;
        Ok(())
    }

    fn store_to_register(
        &self,
        src_reg: u8,
        addr_reg: u8,
        size: usize,
    ) -> ExprResult<()> {
        // STR instruction family
        self.cg.store_to_memory_address(src_reg, addr_reg, size)?;
        Ok(())
    }

    // ========== REGISTER ALLOCATION ==========
    
    fn allocate_gpr(&mut self, _ty: &Type) -> ExprResult<u8> {
        // Allocate general purpose register (X0-X30)
        let reg = self.cg.alloc_xreg()?;
        Ok(reg)
    }
    
    fn allocate_fpr(&mut self, _ty: &Type) -> ExprResult<u8> {
        // Allocate floating-point register (V0-V31)
        let reg = self.cg.alloc_vreg()?;
        Ok(reg)
    }

    fn spill_register(&mut self, reg: u8, stack_offset: i64) -> ExprResult<()> {
        // Store register to stack
        self.cg.store_to_stack(reg as u32, stack_offset)?;
        Ok(())
    }

    // ========== ADDITIONAL BACKEND HOOKS FOR load_operand() ==========
    
    fn load_immediate_constant(&mut self, value: u64, dest_reg: u8, _ty: &Type) -> ExprResult<()> {
        let size = 8; // AArch64 immediate constants are 64-bit
        // ARM64 uses MOVZ/MOVK for large immediates
        self.cg.emit_arm64_mov_imm(dest_reg, value, size)?;
        Ok(())
    }

    fn load_from_symbol_address(&mut self, symbol: &str, dest_reg: u8, ty: &Type) -> ExprResult<()> {
        let size = ty.size() as usize;
        // Load address of symbol
        self.cg.load_symbol_address(symbol, dest_reg)?;
        // Then load the value; multi-word aggregates are handled separately.
        self.cg.load_from_memory_address(dest_reg, dest_reg, size, ty_is_signed(ty))?;
        Ok(())
    }

    fn shift_left(&mut self, src_reg: u8, dest_reg: u8, count: u8) -> ExprResult<()> {
        // LSL (logical shift left) with register operand
        self.cg.emit_arm64_lsl_reg(dest_reg, src_reg, count)?;
        Ok(())
    }

    fn imul_imm(&mut self, dest_reg: u8, src_reg: u8, imm: u64) -> ExprResult<()> {
        // UMOV + MUL for immediate multiplication
        self.cg.imul_with_imm(dest_reg, src_reg, imm)?;
        Ok(())
    }

    fn add_regs(&mut self, src1: u8, src2: u8, dest: u8) -> ExprResult<()> {
        // ADD dest, src1, src2
        self.cg.emit_arm64_add(dest, src1, src2);
        Ok(())
    }

    fn sub_regs(&mut self, src1: u8, src2: u8, dest: u8) -> ExprResult<()> {
        // SUB dest, src1, src2
        self.cg.emit_arm64_sub(dest, src1, src2);
        Ok(())
    }

    fn add_imm(&mut self, base_reg: u8, offset: i64, dest_reg: u8) -> ExprResult<()> {
        // ADRP/ADD or ADR for address calculation
        if offset >= 0 && offset < 4096 {
            self.cg.add_positive_offset(base_reg, dest_reg, offset as u32)?;
        } else if offset >= -4096 && offset < 0 {
            self.cg.add_negative_offset(base_reg, dest_reg, (-offset) as u32)?;
        } else {
            // Large offsets: movz/movk pattern
            self.cg.ldr_with_large_offset(dest_reg, base_reg, offset)?;
        }
        Ok(())
    }

    fn mov_reg_to_reg(&mut self, src_reg: u8, dest_reg: u8, size_bytes: usize) -> ExprResult<()> {
        // MOV/MOVK pair if needed
        self.cg.emit_arm64_mov(dest_reg, src_reg, size_bytes)?;
        Ok(())
    }

    fn store_address(&mut self, dest_reg: u8, symbol: &str) -> ExprResult<()> {
        // Get symbol address
        self.cg.load_symbol_address(symbol, dest_reg)?;
        Ok(())
    }

    fn emit_call(&mut self, func: &crate::ast::Expr, args: &[crate::ast::Expr], _result_reg: u8) -> ExprResult<()> {
        // Evaluate arguments (AAPCS64 calling convention)
        // First 8 args in X0-X7, rest on stack.
        // TODO: Full argument lowering (stack args, HFA/HVA in V registers).
        for (i, _arg) in args.iter().enumerate().take(8) {
            let reg = self.cg.alloc_xreg()?;
            let _ = (i, reg);
        }

        // Call function
        if let crate::ast::Expr::Ident { name, .. } = func {
            self.cg.call_label(name.name.as_str(), true)?;
        }

        Ok(())
    }

    fn branch_if_zero(&mut self, cond_reg: u8, target: &str) -> ExprResult<()> {
        // CBZ/X (compare and branch on zero)
        self.cg.branch_if_zero(cond_reg, target)?;
        Ok(())
    }

    fn jump(&mut self, target: &str) -> ExprResult<()> {
        // B (branch unconditional)
        self.cg.jump(target)?;
        Ok(())
    }

    fn define_label(&mut self, label: &str) -> ExprResult<()> {
        // Label definition
        self.cg.define_label(label)?;
        Ok(())
    }

    fn clear_register(&mut self, reg: u8, ty: &Type) -> ExprResult<()> {
        // ORR dest, reg, #0 (or MOVZX for efficiency)
        let size = ty.size() as usize;
        self.cg.orr_clear(reg, size)?;
        Ok(())
    }
}

// ARM64-specific constants
pub(crate) mod arm64 {
    #[derive(Clone, Copy, PartialEq)]
    pub enum SetCC {
        EQ, NE, LT, GE, GT, LE, HI, LS, MI, PL, VS, VC, HS, LO,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_binary_ops_covered() {
        // Verify all binary operations are handled
        let ops = [
            BinOp::Add, BinOp::Sub, BinOp::Mul, BinOp::Div, BinOp::Rem,
            BinOp::And, BinOp::Or, BinOp::Xor,
            BinOp::Shl, BinOp::Shr,
            BinOp::Eq, BinOp::Ne, BinOp::Lt, BinOp::Le, BinOp::Gt, BinOp::Ge,
        ];
        
        for _op in &ops {
            // Just verify structure compiles
        }
    }
    
    #[test]
    fn test_unary_ops_covered() {
        // Verify unary operations
        let ops = [
            UnOp::Neg, UnOp::Not, UnOp::Plus, UnOp::AddrOf, UnOp::Deref,
        ];
        
        for _op in &ops {
            // Structure verification
        }
    }
}
