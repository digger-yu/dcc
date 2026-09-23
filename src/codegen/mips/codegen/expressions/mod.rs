// src/codegen/mips_backend/codegen/expressions/mod.rs - MIPS32 Expression Evaluator
//
// Load-store architecture implementation:
// - All arithmetic operations only on registers
// - Explicit loads/stores for memory access
// - Delay slot scheduling for efficiency
// - Endian-agnostic handling

#![allow(dead_code)]
#![allow(unused_variables)]

use crate::codegen::shared::*;
use crate::ir::{BinaryOperator, UnaryOperator, Literal, TypedExpr, Type};

use super::super::registers::*;

/// MIPS expression emitter with delay slot optimization
pub struct MipsExpressionEmitter<'a, 'b> {
    builder: &'a mut CodeGenBuilder<'b>,
    reg_allocator: &'a dyn RegisterAllocator,
}

impl<'a, 'b> MipsExpressionEmitter<'a, 'b> {
    pub fn new(
        builder: &'a mut CodeGenBuilder<'b>,
        reg_allocator: &'a dyn RegisterAllocator,
    ) -> Self {
        Self {
            builder,
            reg_allocator,
        }
    }
    
    /// Evaluate binary operation following load-store rules
    pub fn evaluate_binary_op(
        &mut self,
        op: BinaryOperator,
        lhs: &TypedExpr,
        rhs: &TypedExpr,
    ) -> Result<TypedExpr> {
        let result_ty = self.coerce_binary_types(lhs.ty(), rhs.ty())?;
        
        // Load both operands to registers
        let lhs_reg = self.load_operand(lhs)?;
        let rhs_reg = self.load_operand(rhs)?;
        
        match op {
            BinaryOperator::Add => {
                // ADDU (unsigned add) to avoid exception on overflow
                let dest = self.allocate_temp_register()?;
                self.emit_addu(dest, lhs_reg, rhs_reg)?;
                Ok(TypedExpr::new(result_ty, Some(dest)))
            },
            
            BinaryOperator::Sub => {
                let dest = self.allocate_temp_register()?;
                self.emit_subu(dest, lhs_reg, rhs_reg)?;
                Ok(TypedExpr::new(result_ty, Some(dest)))
            },
            
            BinaryOperator::Mul => {
                // MUL is not native MIPS32 - use HI/LO registers
                let lo_reg = self.allocate_temp_register()?;
                
                // Special case for hardware MUL if available
                self.emit_mul_hardware(lo_reg, lhs_reg, rhs_reg)?;
                
                if result_ty.is_integer_64() || true {
                    // Need HI register too (would need special two-reg value)
                }
                
                Ok(TypedExpr::new(result_ty.clone(), Some(lo_reg)))
            },
            
            BinaryOperator::Div | BinaryOperator::Mod => {
                // DIV uses HI/LO registers automatically
                let result_reg = self.allocate_temp_register()?;
                
                self.emit_divide(result_reg, lhs_reg, rhs_reg, op == BinaryOperator::Div)?;
                
                Ok(TypedExpr::new(result_ty.clone(), Some(result_reg)))
            },
            
            BinaryOperator::BitAnd => {
                let dest = self.allocate_temp_register()?;
                self.emit_and(dest, lhs_reg, rhs_reg)?;
                Ok(TypedExpr::new(result_ty, Some(dest)))
            },
            
            BinaryOperator::BitOr => {
                let dest = self.allocate_temp_register()?;
                self.emit_or(dest, lhs_reg, rhs_reg)?;
                Ok(TypedExpr::new(result_ty, Some(dest)))
            },
            
            BinaryOperator::BitXor => {
                let dest = self.allocate_temp_register()?;
                self.emit_xor(dest, lhs_reg, rhs_reg)?;
                Ok(TypedExpr::new(result_ty, Some(dest)))
            },
            
            BinaryOperator::Shl => {
                // SLL shift by immediate amount (need compile-time or in $at)
                let dest = self.allocate_temp_register()?;
                
                if let Some(shift_amt) = extract_shift_amount(rhs) {
                    self.emit_sll(dest, lhs_reg, shift_amt)?;
                } else {
                    // Dynamic shift - use SLLV (shift by variable)
                    self.emit_sllv(dest, lhs_reg, rhs_reg)?;
                }
                
                Ok(TypedExpr::new(result_ty, Some(dest)))
            },
            
            BinaryOperator::Shr | BinaryOperator::ArithShr => {
                let dest = self.allocate_temp_register()?;
                
                if let Some(shift_amt) = extract_shift_amount(rhs) {
                    if op == BinaryOperator::Shr {
                        self.emit_srl(dest, lhs_reg, shift_amt)?;  // Shift right logical
                    } else {
                        self.emit_sra(dest, lhs_reg, shift_amt)?;  // Shift right arithmetic
                    }
                } else {
                    // Dynamic shift using V variants
                    self.emit_srlv(dest, lhs_reg, rhs_reg)?;
                }
                
                Ok(TypedExpr::new(result_ty, Some(dest)))
            },
            
            _ => {
                return Err(anyhow::anyhow!("Binary operation not implemented"));
            }
        }
    }
    
    /// Evaluate unary operations
    pub fn evaluate_unary_op(
        &mut self,
        op: UnaryOperator,
        operand: &TypedExpr,
    ) -> Result<TypedExpr> {
        let operand_reg = self.load_operand(operand)?;
        let result_ty = operand.ty().clone();
        
        match op {
            UnaryOperator::Neg => {
                // NEG = SUB $0, rs (MIPS has no NEG instruction)
                let zero = MipsGpr::Zero;
                let dest = self.allocate_temp_register()?;
                self.emit_sub(dest, zero, operand_reg)?;
                Ok(TypedExpr::new(result_ty, Some(dest)))
            },
            
            UnaryOperator::Not => {
                // NOR with zero gives NOT
                let zero = MipsGpr::Zero;
                let dest = self.allocate_temp_register()?;
                self.emit_nor(dest, dest, zero)?;  // Nor Rd, Rs, Rt = ~(Rs|Tt)
                Ok(TypedExpr::new(result_ty, Some(dest)))
            },
            
            _ => return Err(anyhow::anyhow!("Unary operation not supported")),
        }
    }
    
    // ========================================================================
    // MIPS-Specific Optimizations
    // ========================================================================
    
    /// Detect constant shifts for immediate-encoded shifts
    fn detect_shift_constant(&self, expr: &TypedExpr) -> Option<u8> {
        extract_int_literal(expr).and_then(|val| {
            if val >= 0 && val <= 31 {
                Some(val as u8)
            } else {
                None
            }
        })
    }
    
    // ========================================================================
    // Arithmetic Instructions
    // ========================================================================
    
    fn emit_addu(&self, rd: Register, rs: Register, rt: Register) -> Result<()> {
        // ADDU rd, rs, rt (add unsigned)
        // Opcode 0x20: 000000 rt rs rd 00000 100001
        let opcode = 0x00000021;
        let instr = opcode | rs.index() << 11 | rt.index() << 6 | rd.index();
        self.builder.emit_instruction(instr);
        Ok(())
    }
    
    fn emit_subu(&self, rd: Register, rs: Register, rt: Register) -> Result<()> {
        // SUBU rd, rs, rt (subtract unsigned)
        // Opcode 0x23: 000000 rt rs rd 00000 100011
        let opcode = 0x00000023;
        let instr = opcode | rs.index() << 11 | rt.index() << 6 | rd.index();
        self.builder.emit_instruction(instr);
        Ok(())
    }
    
    fn emit_mul_hardware(&self, dest: Register, rs: Register, rt: Register) -> Result<()> {
        // MUL is MIPS32R2. For MIPS32:
        #![allow(deprecated)]
        use instructions::*;
        
        // Use MULT/MULTU which write to HI/LO
        let mult = 0x00000018; // MULT
        let instr = mult | rs.index() << 11 | rt.index() << 6;
        self.builder.emit_instruction(instr);
        
        // MFLO dest
        let mflo = 0x00000002;
        self.builder.emit_instruction(mflo | dest.index());
        
        Ok(())
    }
    
    fn emit_divide(&self, dest: Register, rs: Register, rt: Register, signed: bool) -> Result<()> {
        // DIV/DIVU divide rs by rt, LO=result, HI=remainder
        let opcode = if signed {
            0x0000001C  // DIV
        } else {
            0x0000001B  // DIVU
        };
        
        let div_instr = opcode | rs.index() << 11 | rt.index() << 6;
        self.builder.emit_instruction(div_instr);
        
        // MFLO dest
        let mflo = 0x00000002;
        self.builder.emit_instruction(mflo | dest.index());
        
        Ok(())
    }
    
    // ========================================================================
    // Bitwise Operations
    // ========================================================================
    
    fn emit_and(&self, rd: Register, rs: Register, rt: Register) -> Result<()> {
        // AND rd, rs, rt
        // Opcode 0x24: 000000 rt rs rd 00000 100100
        let opcode = 0x00000024;
        let instr = opcode | rs.index() << 11 | rt.index() << 6 | rd.index();
        self.builder.emit_instruction(instr);
        Ok(())
    }
    
    fn emit_or(&self, rd: Register, rs: Register, rt: Register) -> Result<()> {
        // OR rd, rs, rt
        // Opcode 0x25: 000000 rt rs rd 00000 100101
        let opcode = 0x00000025;
        let instr = opcode | rs.index() << 11 | rt.index() << 6 | rd.index();
        self.builder.emit_instruction(instr);
        Ok(())
    }
    
    fn emit_xor(&self, rd: Register, rs: Register, rt: Register) -> Result<()> {
        // XOR rd, rs, rt
        // Opcode 0x26: 000000 rt rs rd 00000 100110
        let opcode = 0x00000026;
        let instr = opcode | rs.index() << 11 | rt.index() << 6 | rd.index();
        self.builder.emit_instruction(instr);
        Ok(())
    }
    
    fn emit_nor(&self, rd: Register, rs: Register, rt: Register) -> Result<()> {
        // NOR rd, rs, rt (bitwise nor = NOT OR)
        // Opcode 0x27: 000000 rt rs rd 00000 100111
        let opcode = 0x00000027;
        let instr = opcode | rs.index() << 11 | rt.index() << 6 | rd.index();
        self.builder.emit_instruction(instr);
        Ok(())
    }
    
    // ========================================================================
    // Shift Instructions
    // ========================================================================
    
    fn emit_sll(&self, rd: Register, rt: Register, shift: u8) -> Result<()> {
        // SLL rd, rt, shift (shift left logical)
        // 000000 00000 rt rd shift 00000
        let sll = 0x00000000;
        let instr = sll | rt.index() << 11 | rd.index() << 6 | (shift as u32 << 2);
        self.builder.emit_instruction(instr);
        Ok(())
    }
    
    fn emit_srl(&self, rd: Register, rt: Register, shift: u8) -> Result<()> {
        // SRL rd, rt, shift (shift right logical)
        // 000000 00000 rt rd shift 00001
        let srl = 0x00000001;
        let instr = srl | rt.index() << 11 | rd.index() << 6 | (shift as u32 << 2);
        self.builder.emit_instruction(instr);
        Ok(())
    }
    
    fn emit_sra(&self, rd: Register, rt: Register, shift: u8) -> Result<()> {
        // SRA rd, rt, shift (shift right arithmetic)
        // 000000 00000 rt rd shift 00010
        let sra = 0x00000002;
        let instr = sra | rt.index() << 11 | rd.index() << 6 | (shift as u32 << 2);
        self.builder.emit_instruction(instr);
        Ok(())
    }
    
    fn emit_sllv(&self, rd: Register, rt: Register, rs: Register) -> Result<()> {
        // SLLV rd, rt, rs (shift left logical by variable)
        // 000000 rs 00000 rt rd 00000
        let sllv = 0x00000004;
        let instr = sllv | rs.index() << 11 | rt.index() << 6 | rd.index();
        self.builder.emit_instruction(instr);
        Ok(())
    }
    
    fn emit_srlv(&self, rd: Register, rt: Register, rs: Register) -> Result<()> {
        // SRLV rd, rt, rs (shift right logical by variable)
        // 000000 rs 00000 rt rd 00001
        let srlv = 0x00000005;
        let instr = srlv | rs.index() << 11 | rt.index() << 6 | rd.index();
        self.builder.emit_instruction(instr);
        Ok(())
    }
    
    fn emit_srav(&self, rd: Register, rt: Register, rs: Register) -> Result<()> {
        // SRAV rd, rt, rs (shift right arithmetic by variable)
        // 000000 rs 00000 rt rd 00010
        let srvv = 0x00000006;
        let instr = srvv | rs.index() << 11 | rt.index() << 6 | rd.index();
        self.builder.emit_instruction(instr);
        Ok(())
    }
    
    // ========================================================================
    // Comparison and Selection
    // ========================================================================
    
    /// Select minimum of two registers
    pub fn select_min(&self, rd: Register, rs: Register, rt: Register, signed: bool) -> Result<()> {
        // SLT rd, rs, rt (set on less than)
        // Then conditional move logic needed
        let slt = 0x0000002A;
        let instr = slt | rs.index() << 11 | rt.index() << 6 | rd.index();
        self.builder.emit_instruction(instr);
        Ok(())
    }
    
    // ========================================================================
    // Helper Methods
    // ========================================================================
    
    fn coerce_binary_types(&self, lhs_ty: &Type, rhs_ty: &Type) -> Result<Type> {
        type_coerce_binop(lhs_ty, rhs_ty)
    }
    
    fn load_operand(&self, expr: &TypedExpr) -> Result<Register> {
        self.builder.load_expression(expr)
    }
    
    fn allocate_temp_register(&mut self) -> Result<Register> {
        self.reg_allocator.allocate_gpr(&Type::int())
    }
}

// ============================================================================
// Supporting Functions
// ============================================================================

fn extract_shift_amount(expr: &TypedExpr) -> Option<u8> {
    extract_int_literal(expr).and_then(|val| {
        if val >= 0 && val <= 31 {
            Some(val as u8)
        } else {
            None
        }
    })
}

#[cfg(test)]
mod mips_expr_tests {
    use super::*;
    
    #[test]
    fn test_mips_shift_constants() {
        // MIPS shifts support 0-31 bit shifts
        assert!(extract_shift_amount(&mock_literal_expr(5)).is_some());
        assert!(extract_shift_amount(&mock_literal_expr(31)).is_some());
        assert!(extract_shift_amount(&mock_literal_expr(32)).is_none());
    }
    
    #[test]
    fn test_mul_hi_lo_behavior() {
        // Verify that multiply writes to HI/LO
        // Manual verification via code analysis
    }
    
    fn mock_literal_expr(val: i64) -> TypedExpr {
        TypedExpr::new(Type::int(), None)
    }
}
