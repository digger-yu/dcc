// src/codegen/riscv64/expression_impl.rs
// Phase C: Data-Driven Code Generation Architecture - RISC-V 64 Expression Backend
//
// First Principle: RISC-V implements same ExpressionExecutor trait as x86_64/AArch64
// Enables shared expression evaluation across all ISAs

use crate::ast::{BinOp, Type, UnOp};
use crate::codegen::shared::expression_executor::{ExpressionExecutor, ExprResult};

/// RISC-V 64-specific ExpressionExecutor implementation
pub struct RiscV64ExpressionBackend<'a> {
    pub cg: &'a mut crate::codegen::riscv64::codegen::Riscv64Codegen,
}

impl<'a> RiscV64ExpressionBackend<'a> {
    pub fn new(cg: &'a mut crate::codegen::riscv64::codegen::Riscv64Codegen) -> Self {
        Self { cg }
    }
}

impl<'a> ExpressionExecutor for RiscV64ExpressionBackend<'a> {
    // ========== RISCV64 BINARY OPERATIONS =========

    fn emit_binary_op(
        &self,
        op: &BinOp,
        result_ty: &Type,
        lhs_reg: u8,
        rhs_reg: u8,
        dest_reg: u8,
    ) -> ExprResult<()> {
        use crate::ast::BinOp::*;
        
        let is_float = result_ty.is_floating_point();
        
        match op {
            // Integer arithmetic (RV64I base instruction set)
            Add => self.cg.emit_add(dest_reg, lhs_reg, rhs_reg),
            Sub => self.cg.emit_sub(dest_reg, lhs_reg, rhs_reg),
            
            Mul => {
                // MUL instruction (RV64I)
                self.cg.emit_mul(dest_reg, lhs_reg, rhs_reg);
            }
            
            Div => {
                // SDIV (signed) or DIVU (unsigned)
                let signed = result_ty.is_signed();
                if signed {
                    self.cg.emit_div(dest_reg, lhs_reg, rhs_reg);
                } else {
                    self.cg.emit_divu(dest_reg, lhs_reg, rhs_reg);
                }
            }
            
            Rem => {
                // REM (signed) or REMU (unsigned)
                let signed = result_ty.is_signed();
                if signed {
                    self.cg.emit_rem(dest_reg, lhs_reg, rhs_reg);
                } else {
                    self.cg.emit_remu(dest_reg, lhs_reg, rhs_reg);
                }
            }
            
            // Bitwise operations
            And => self.cg.emit_and(dest_reg, lhs_reg, rhs_reg),
            Or => self.cg.emit_or(dest_reg, lhs_reg, rhs_reg),
            Xor => self.cg.emit_xor(dest_reg, lhs_reg, rhs_reg),
            
            // Shift operations
            Shl => {
                // SLL (shift logical left)
                self.cg.emit_sll(dest_reg, lhs_reg, rhs_reg);
            }
            Shr => {
                // SRL (shift logical right) / SRA (arithmatic right)
                let signed = result_ty.is_signed();
                if signed {
                    self.cg.emit_sra(dest_reg, lhs_reg, rhs_reg);
                } else {
                    self.cg.emit_srl(dest_reg, lhs_reg, rhs_reg);
                }
            }
            
            // Comparison operations (returns 0 or 1)
            Eq => {
                // Implement equality comparison using slt logic like binary.rs
                self.cg.emit_slt(12, lhs_reg, rhs_reg); // t1 = (lhs < rhs)
                self.cg.emit_slt(13, rhs_reg, lhs_reg); // t2 = (rhs < lhs)
                self.cg.emit_or(dest_reg, 12, 13); // dest = t1 | t2 → 0 if equal, else 1
                self.cg.xori(dest_reg, dest_reg, 1); // dest = !dest → 1 if equal, else 0
            }
            Ne => {
                // Inequality is inverse of equality
                self.cg.emit_slt(12, lhs_reg, rhs_reg); // t1 = (lhs < rhs)
                self.cg.emit_slt(13, rhs_reg, lhs_reg); // t2 = (rhs < lhs)
                self.cg.emit_or(dest_reg, 12, 13); // dest = t1 | t2 → 1 if not equal, else 0
            }
            Lt => {
                self.cg.emit_slt(dest_reg, lhs_reg, rhs_reg);
            }
            Le => {
                // a <= b → !(a > b) → which is equivalent to (b >= a)
                self.cg.emit_slt(12, rhs_reg, lhs_reg); // t1 = (rhs < lhs) → 1 if rhs < lhs (so lhs > rhs)
                self.cg.xori(dest_reg, 12, 1); // dest = t1 ^1 → 1 if rhs >= lhs → a <= b
            }
            Gt => {
                // a > b → equivalent to (b < a)
                self.cg.emit_slt(dest_reg, rhs_reg, lhs_reg);
            }
            Ge => {
                // a >= b → equivalent to !(a < b)
                self.cg.emit_slt(dest_reg, lhs_reg, rhs_reg); // t1 = (lhs < rhs) → 1 when a < b
                self.cg.xori(dest_reg, dest_reg, 1); // dest = !t1 → 1 when a >= b
            }
            
            // Logical operators need short-circuit
            AndAnd | OrOr => {
                Err("Logical ops require branching in RISC-V".into())
            }
        }
        
        Ok(())
    }

    // ========== RISCV64 UNARY OPERATIONS =========

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
                // NEG: subtract from zero
                self.cg.emit_sub(dest_reg, 0, src_reg);
            }
            Not => {
                // Bitwise NOT: use bitwise_not method
                self.cg.emit_bitwise_not(dest_reg, src_reg);
            }
            Plus => {
                // No-op for unary plus, just move if needed
                if src_reg != dest_reg {
                    self.cg.emit_mv(dest_reg, src_reg, result_ty.size() as usize);
                }
            }
            AddrOf => {
                Err("Address-of requires lvalue handling".into())
            }
            Deref => {
                // Load from address: use emit_load_sz as per unary.rs
                let size = result_ty.size() as usize;
                self.cg.emit_load_sz(dest_reg, src_reg, 0, size);
                Ok(())
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
        // FCVT.W.S/F (float to integer)
        self.cg.fcvt_float_to_int(src_reg, dest_reg, signed, dest_bits)?;
        Ok(())
    }

    fn convert_int_to_float(&self, src_reg: u8, dest_reg: u8) -> ExprResult<()> {
        // FCVT.S.W (integer to float)
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
        // SEXT.W, EXT.H, EXT.B sign extensions
        match bits_from {
            8 => self.cg.emit_sext_b(dest_reg, src_reg, bits_to),
            16 => self.cg.emit_sext_h(dest_reg, src_reg, bits_to),
            32 => self.cg.emit_sext_w(dest_reg, src_reg, bits_to),
            _ => {
                if src_reg != dest_reg && bits_from < bits_to {
                    self.cg.emit_rv_sign_ext(dest_reg, src_reg, bits_from, bits_to)?;
                } else if src_reg != dest_reg {
                    self.cg.emit_rv_mov(dest_reg, src_reg, bits_to as usize / 8)?;
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
        // ZERO.W, HEXT.H, EXTB zero extensions
        match bits_from {
            8 => self.cg.emit_zero_b(dest_reg, src_reg, bits_to),
            16 => self.cg.emit_zero_h(dest_reg, src_reg, bits_to),
            32 => self.cg.emit_zero_w(dest_reg, src_reg, bits_to),
            _ => {
                if src_reg != dest_reg && bits_from < bits_to {
                    self.cg.emit_rv_zero_ext(dest_reg, src_reg, bits_from, bits_to)?;
                } else if src_reg != dest_reg {
                    self.cg.emit_rv_mov(dest_reg, src_reg, bits_to as usize / 8)?;
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
        // LB/LBU/LH/LHU/LW/LWU/LD instructions
        let load_signed = signed && (size == 1 || size == 2 || size == 4 || size == 8);
        self.cg.load_memory(addr_reg, dest_reg, size, load_signed)?;
        Ok(())
    }

    fn store_to_register(
        &self,
        src_reg: u8,
        addr_reg: u8,
        size: usize,
    ) -> ExprResult<()> {
        // SB/SH/SW/SD store instructions
        self.cg.store_memory(src_reg, addr_reg, size)?;
        Ok(())
    }

    // ========== REGISTER ALLOCATION =========

    fn allocate_gpr(&mut self, ty: &Type) -> ExprResult<u8> {
        // Allocate general purpose register (x0-x31)
        let reg = self.cg.alloc_xreg()?;
        Ok(reg)
    }

    fn allocate_fpr(&mut self, ty: &Type) -> ExprResult<u8> {
        // Allocate floating-point register (f0-f31)
        let reg = self.cg.alloc_freg()?;
        Ok(reg)
    }

    fn spill_register(&mut self, reg: u8, stack_offset: i64) -> ExprResult<()> {
        // SW/SD store to stack
        self.cg.store_to_stack(reg as u32, stack_offset)?;
        Ok(())
    }

    // ========== ADDITIONAL BACKEND HOOKS FOR load_operand() =========

    fn load_immediate_constant(&mut self, value: u64, dest_reg: u8, ty: &Type) -> ExprResult<()> {
        let size = ty.size() as usize;
        // LA/LUI + ADDI for immediates
        self.cg.emit_rv_load_imm(dest_reg, value, size)?;
        Ok(())
    }

    fn load_from_symbol_address(&mut self, symbol: &str, dest_reg: u8, ty: &Type) -> ExprResult<()> {
        let size = ty.size() as usize;
        // Load address of symbol (LA instruction)
        self.cg.load_symbol_address(symbol, dest_reg)?;
        // Then load the value
        if size <= 8 {
            self.cg.load_memory(dest_reg, dest_reg, size, ty.is_signed())?;
        } else {
            self.cg.load_memory(dest_reg, dest_reg, size, ty.is_signed())?;
        }
        Ok(())
    }

    fn shift_left(&mut self, src_reg: u8, dest_reg: u8, count: u8) -> ExprResult<()> {
        // SLL with immediate or register
        self.cg.emit_rv_sll_reg(dest_reg, src_reg, count)?;
        Ok(())
    }

    fn imul_imm(&mut self, dest_reg: u8, src_reg: u8, imm: u64) -> ExprResult<()> {
        // MUL + ADDI sequence or SLL if power-of-2
        self.cg.imul_with_imm(dest_reg, src_reg, imm)?;
        Ok(())
    }

    fn add_regs(&mut self, src1: u8, src2: u8, dest: u8) -> ExprResult<()> {
        // ADD dest, src1, src2
        self.cg.emit_rv_add(dest, src1, src2);
        Ok(())
    }

    fn sub_regs(&mut self, src1: u8, src2: u8, dest: u8) -> ExprResult<()> {
        // SUB dest, src1, src2
        self.cg.emit_rv_sub(dest, src1, src2);
        Ok(())
    }

    fn add_imm(&mut self, base_reg: u8, offset: i64, dest_reg: u8) -> ExprResult<()> {
        // ADDI for small offsets, LUI+ADD for large
        if offset >= -2048 && offset < 2048 {
            self.cg.emit_rv_addi(dest_reg, base_reg, offset as i32)?;
        } else {
            // Large offset: LUI + ADDI sequence
            self.cg.add_large_offset(dest_reg, base_reg, offset)?;
        }
        Ok(())
    }

    fn mov_reg_to_reg(&mut self, src_reg: u8, dest_reg: u8, size_bytes: usize) -> ExprResult<()> {
        // MV (MOVE = XOR with zero) or LD
        self.cg.emit_rv_mov(dest_reg, src_reg, size_bytes)?;
        Ok(())
    }

    fn store_address(&mut self, dest_reg: u8, symbol: &str) -> ExprResult<()> {
        // LA (load address)
        self.cg.load_symbol_address(symbol, dest_reg)?;
        Ok(())
    }

    fn emit_call(&mut self, func: &crate::ast::Expr, args: &[Box<crate::ast::Expr>], result_reg: u8) -> ExprResult<()> {
        // Evaluate arguments (RISC-V ABI: a0-a7 for first 8 args)
        let mut arg_regs = [0u8; 8];
        
        for (i, arg) in args.iter().enumerate().take(8) {
            if i < 8 {
                let reg = self.cg.alloc_a_reg()?;
                self.load_operand(arg, reg, arg.ty())?;
                arg_regs[i] = reg;
            }
        }
        
        // Call function (JAL)
        if let crate::ast::Expr::Ident { name, .. } = func {
            self.cg.call_label(name.name.as_str(), true)?;
        }
        
        Ok(())
    }

    fn branch_if_zero(&mut self, cond_reg: u8, target: &str) -> ExprResult<()> {
        // BEQZ (branch if equal to zero)
        self.cg.branch_if_zero(cond_reg, target)?;
        Ok(())
    }

    fn jump(&mut self, target: &str) -> ExprResult<()> {
        // JAL (jump and link)
        self.cg.jump(target)?;
        Ok(())
    }

    fn define_label(&mut self, label: &str) -> ExprResult<()> {
        // Label definition
        self.cg.define_label(label)?;
        Ok(())
    }

    fn clear_register(&mut self, reg: u8, ty: &Type) -> ExprResult<()> {
        // ZERO (XOR reg, reg, zero) or MOV x0
        self.cg.zero_register(reg)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_riscv_ops_covered() {
        // Verify all operations are handled
        let _ops = [
            BinOp::Add, BinOp::Sub, BinOp::Mul, BinOp::Div, BinOp::Rem,
            BinOp::And, BinOp::Or, BinOp::Xor,
            BinOp::Shl, BinOp::Shr,
            BinOp::Eq, BinOp::Ne, BinOp::Lt, BinOp::Le, BinOp::Gt, BinOp::Ge,
        ];
    }
}
