// src/codegen/arm_backend/codegen/expressions/mod.rs - ARM32 Expression Evaluator
// 
// Per ARM Architecture Procedure Call Standard (AAPCS32)
// This module handles expression evaluation with ARM-specific optimizations:
// - Barrel shifter for shift operations  
// - 8-bit rotated immediates in ALU instructions
// - UMULL/SMULL for full-precision multiplication
// - Load/store single register semantics

#![allow(dead_code)]
#![allow(unused_variables)]

use crate::codegen::shared::*;
use crate::ir::{BinaryOperator, Literal, TypedExpr, Type};

use super::registers::*;

/// ARM expression evaluation engine with ISA-specific optimizations
pub struct ArmExpressionEmitter<'a, 'b> {
    builder: &'a mut CodeGenBuilder<'b>,
    reg_allocator: &'a dyn RegisterAllocator,
}

impl<'a, 'b> ArmExpressionEmitter<'a, 'b> {
    pub fn new(
        builder: &'a mut CodeGenBuilder<'b>,
        reg_allocator: &'a dyn RegisterAllocator,
    ) -> Self {
        Self {
            builder,
            reg_allocator,
        }
    }
    
    /// Evaluate binary operation with ARM-specific optimizations
    pub fn evaluate_binary_op(
        &mut self,
        op: BinaryOperator,
        lhs: &TypedExpr,
        rhs: &TypedExpr,
    ) -> Result<TypedExpr> {
        let result_ty = self.coerce_binary_types(lhs.ty(), rhs.ty())?;
        
        // Load LHS into register
        let lhs_reg = self.load_operand(lhs)?;
        
        match op {
            BinaryOperator::Add => {
                let rhs_reg = self.load_operand(rhs)?;
                
                // Check if RHS is valid ARM immediate
                if let Some(imm) = self.as_valid_arm_immediate(rhs) {
                    self.emit_add_imm(lhs_reg, imm)?;
                } else {
                    self.emit_add_reg(lhs_reg, rhs_reg)?;
                }
                
                Ok(TypedExpr::new(result_ty.clone(), Some(lhs_reg)))
            },
            
            BinaryOperator::Sub => {
                let rhs_reg = self.load_operand(rhs)?;
                
                if let Some(imm) = self.as_valid_arm_immediate(rhs) {
                    self.emit_subs_imm(lhs_reg, imm)?;
                } else {
                    self.emit_sub_reg(lhs_reg, rhs_reg)?;
                }
                
                Ok(TypedExpr::new(result_ty.clone(), Some(lhs_reg)))
            },
            
            BinaryOperator::Mul => {
                let rhs_reg = self.load_operand(rhs)?;
                
                // Handle int32 and int64 differently
                if result_ty.is_integer_64() || rhs.ty().is_integer_64() || lhs.ty().is_integer_64() {
                    // 64-bit multiply using UMULL/SMULL
                    let high_reg = self.allocate_temp_register()?;
                    let low_reg = lhs_reg;
                    
                    // Use signed or unsigned based on type
                    if result_ty.is_signed() {
                        self.emit_smull(low_reg, high_reg, lhs_reg, rhs_reg)?;
                    } else {
                        self.emit_umull(low_reg, high_reg, lhs_reg, rhs_reg)?;
                    }
                    
                    // Return two-register value (can pack later if needed)
                    Ok(TypedExpr::new(result_ty, None)) // Complex value requiring special handling
                } else {
                    // 32x32→32 multiply - simpler case
                    // Use MUL when hi-part not needed
                    self.emit_mul_32(lhs_reg, rhs_reg)?;
                    Ok(TypedExpr::new(result_ty.clone(), Some(lhs_reg)))
                }
            },
            
            BinaryOperator::Div => {
                let rhs_reg = self.load_operand(rhs)?;
                let result_reg = self.allocate_temp_register()?;
                
                // Division requires specific register setup
                self.emit_div(result_reg, lhs_reg, rhs_reg)?;
                Ok(TypedExpr::new(result_ty.clone(), Some(result_reg)))
            },
            
            BinaryOperator::Mod => {
                let rhs_reg = self.load_operand(rhs)?;
                let result_reg = self.allocate_temp_register()?;
                
                self.emit_mod(result_reg, lhs_reg, rhs_reg)?;
                Ok(TypedExpr::new(result_ty.clone(), Some(result_reg)))
            },
            
            BinaryOperator::BitAnd => {
                let rhs_reg = self.load_operand(rhs)?;
                self.emit_and_regs(lhs_reg, rhs_reg)?;
                Ok(TypedExpr::new(result_ty.clone(), Some(lhs_reg)))
            },
            
            BinaryOperator::BitOr => {
                let rhs_reg = self.load_operand(rhs)?;
                self.emit_or_regs(lhs_reg, rhs_reg)?;
                Ok(TypedExpr::new(result_ty.clone(), Some(lhs_reg)))
            },
            
            BinaryOperator::BitXor => {
                let rhs_reg = self.load_operand(rhs)?;
                self.emit_xor_regs(lhs_reg, rhs_reg)?;
                Ok(TypedExpr::new(result_ty.clone(), Some(lhs_reg)))
            },
            
            BinaryOperator::Shl => {
                let shift_reg = self.load_operand(rhs)?;
                
                // Use barrel shifter: ADD R0, R0, R1, LSL #shift
                self.emit_shl_barrel(lhs_reg, shift_reg)?;
                Ok(TypedExpr::new(result_ty.clone(), Some(lhs_reg)))
            },
            
            BinaryOperator::Shr => {
                let shift_reg = self.load_operand(rhs)?;
                
                // Logical shift right with barrel shifter
                self.emit_lsr_barrel(lhs_reg, shift_reg)?;
                Ok(TypedExpr::new(result_ty.clone(), Some(lhs_reg)))
            },
            
            BinaryOperator::ArithShr => {
                let shift_reg = self.load_operand(rhs)?;
                
                // Arithmetic shift right
                self.emit_asr_barrel(lhs_reg, shift_reg)?;
                Ok(TypedExpr::new(result_ty.clone(), Some(lhs_reg)))
            },
            
            _ => {
                // Default: load both and use generic approach
                return Err(anyhow::anyhow!("Binary operation {:?} not yet implemented", binary_op));
            }
        }
    }
    
    /// Evaluate unary operations
    pub fn evaluate_unary_op(
        &mut self,
        op: UnaryOperator,
        operand: &TypedExpr,
    ) -> Result<TypedExpr> {
        let operand_ty = operand.ty();
        let result_ty = operand_ty.clone();
        
        let operand_reg = self.load_operand(operand)?;
        
        match op {
            UnaryOperator::Neg => {
                // Negation: RSDI Rx, Ry, #0 followed by SUBS Rx, Ry, Rs
                let zero_reg = self.get_zero_register()?;
                let temp_reg = self.allocate_temp_register()?;
                
                self.emit_mov_imm(temp_reg, 0)?;
                self.emit_subs(temp_reg, zero_reg, operand_reg)?;
                
                Ok(TypedExpr::new(result_ty, Some(temp_reg)))
            },
            
            UnaryOperator::Not => {
                // Bitwise not: EOR Rx, Ry, #0xFFFFFFFF
                self.emit_eor_not(operand_reg)?;
                Ok(TypedExpr::new(result_ty, Some(operand_reg)))
            },
            
            UnaryOperator::AddressOf => {
                // Already loaded address, just return it
                Ok(TypedExpr::new(result_ty, Some(operand_reg)))
            },
            
            _ => return Err(anyhow::anyhow!("Unary operation {:?} not supported", unary_op)),
        }
    }
    
    // ========================================================================
    // ARM-Specific Optimizations
    // ========================================================================
    
    /// Check if value fits ARM immediate encoding
    /// ARM instructions encode immediates as 8-bit value rotated right by even number 0-30
    fn as_valid_arm_immediate(&self, expr: &TypedExpr) -> Option<u16> {
        let literal_val = extract_int_literal(expr)?;
        
        for rotate in (0..=30).step_by(2) {
            let val_u32 = literal_val as u32;
            let rotated = (val_u32 >> rotate) | (val_u32 << (32 - rotate));
            
            // High byte must be zero AND bit 8 must match bit 0
            if (rotated & 0xFF0000FF) == 0 {
                return Some((rotate / 2) as u16);
            }
        }
        None
    }
    
    /// Verify ARM immediate encoding validity
    pub fn verify_arm_immediate(value: u32) -> bool {
        for rotate in (0..=30).step_by(2) {
            let rotated = (value >> rotate) | (value << (32 - rotate));
            if (rotated & 0xFF0000FF) == 0 {
                return true;
            }
        }
        false
    }
    
    // ========================================================================
    // Instruction Emit Helpers
    // ========================================================================
    
    fn emit_add_imm(&self, rd: Register, imm: u16) -> Result<()> {
        // Generate: ADD Rd, Rn, #imm (with rotation)
        // We'll generate assembly string via builder
        let rn = rd; // Keep same register
        
        let opcode = 0b00010; // ADD
        let encoded = self.create_arm_instruction(opcode, rn, rn, Some(imm as u32));
        self.builder.emit_instruction(encoded);
        Ok(())
    }
    
    fn emit_add_reg(&self, rd: Register, rn: Register, rm: Register) -> Result<()> {
        let opcode = 0b00010; // ADD
        let encoded = self.create_arm_instruction(opcode, rd, rn, Some(mnemonic_to_bits(m)));
        self.builder.emit_instruction(encoded);
        Ok(())
    }
    
    fn emit_subs_imm(&self, rd: Register, imm: u16) -> Result<()> {
        // SUBS Rd, Rn, #imm (sets flags)
        let opcode = 0b00011; // SUB
        let encoded = self.create_arm_instruction(opcode, rd, rd, Some(imm as u32));
        self.builder.emit_instruction(encoded);
        Ok(())
    }
    
    fn emit_sub_reg(&self, rd: Register, rn: Register, rm: Register) -> Result<()> {
        let opcode = 0b00011; // SUB
        let encoded = self.create_arm_instruction(opcode, rd, rn, Some(mnemonic_to_bits(m)));
        self.builder.emit_instruction(encoded);
        Ok(())
    }
    
    fn emit_mul_32(&self, rd: Register, rn: Register, rm: Register) -> Result<()> {
        // MUL Rd, Rn, Rm (Rd = Rn * Rm[31:0])
        let opcode = 0b10010; // MUL
        let encoded = self.create_arm_instruction(opcode, rd, rn, Some(mnemonic_to_bits(m)));
        self.builder.emit_instruction(encoded);
        Ok(())
    }
    
    fn emit_umull(&self, rdlo: Register, rdhi: Register, rn: Register, rm: Register) -> Result<()> {
        // UMULL Rdlo, Rdhi, Rn, Rm (64-bit unsigned multiply)
        let opcode = 0b11110; // UMULL
        let encoded = self.create_arm_instruction(opcode, rdlo, rdhi, Some(mnemonic_to_bits(rm)));
        self.builder.emit_instruction(encoded);
        Ok(())
    }
    
    fn emit_smull(&self, rdlo: Register, rdhi: Register, rn: Register, rm: Register) -> Result<()> {
        // SMULL Rdlo, Rdhi, Rn, Rm (64-bit signed multiply)
        let opcode = 0b11111; // SMULL
        let encoded = self.create_arm_instruction(opcode, rdlo, rdhi, Some(mnemonic_to_bits(rm)));
        self.builder.emit_instruction(encoded);
        Ok(())
    }
    
    fn emit_div(&self, rd: Register, rn: Register, rm: Register) -> Result<()> {
        // SDIV Rd, Rn, Rm (signed divide)
        let opcode = 0b11101; // SDIV
        let encoded = self.create_arm_instruction(opcode, rd, rn, Some(mnemonic_to_bits(rm)));
        self.builder.emit_instruction(encoded);
        Ok(())
    }
    
    fn emit_mod(&self, rd: Register, rn: Register, rm: Register) -> Result<()> {
        // UMLAL + SUB combination for MOD
        // Or use SDIV + SMSUBL
        Err(anyhow::anyhow!("MOD needs specialized implementation"))
    }
    
    fn emit_and_regs(&self, rd: Register, rn: Register, rm: Register) -> Result<()> {
        // AND Rd, Rn, Rm
        let opcode = 0b00000; // AND
        let encoded = self.create_arm_instruction(opcode, rd, rn, Some(mnemonic_to_bits(rm)));
        self.builder.emit_instruction(encoded);
        Ok(())
    }
    
    fn emit_or_regs(&self, rd: Register, rn: Register, rm: Register) -> Result<()> {
        // ORR Rd, Rn, Rm
        let opcode = 0b00010; // ORR (same as ADD but no carry)
        let encoded = self.create_arm_instruction(opcode, rd, rn, Some(mnemonic_to_bits(rm)));
        self.builder.emit_instruction(encoded);
        Ok(())
    }
    
    fn emit_xor_regs(&self, rd: Register, rn: Register, rm: Register) -> Result<()> {
        // EOR Rd, Rn, Rm
        let opcode = 0b00110; // EOR
        let encoded = self.create_arm_instruction(opcode, rd, rn, Some(mnemonic_to_bits(rm)));
        self.builder.emit_instruction(encoded);
        Ok(())
    }
    
    fn emit_shl_barrel(&self, rd: Register, rm: Register) -> Result<()> {
        // Use barrel shifter in ALU: ADD Rd, Rd, Rm, LSL #0 (will need actual shift amount)
        Err(anyhow::anyhow!("Need shift amount for barrel shift"))
    }
    
    fn emit_lsr_barrel(&self, rd: Register, rm: Register) -> Result<()> {
        Err(anyhow::anyhow!("Need shift amount"))
    }
    
    fn emit_asr_barrel(&self, rd: Register, rm: Register) -> Result<()> {
        Err(anyhow::anyhow!("Need shift amount"))
    }
    
    fn emit_eor_not(&self, rd: Register) -> Result<()> {
        // EOR Rd, Rd, #0xFFFFFFFF (bitwise NOT)
        let imm = 0xFFFFFFFFu32;
        let encoded = self.create_arm_instruction_with_imm(0b00110, rd, rd, imm);
        self.builder.emit_instruction(encoded);
        Ok(())
    }
    
    // ========================================================================
    // Helper Methods
    // ========================================================================
    
    fn coerce_binary_types(&self, lhs_ty: &Type, rhs_ty: &Type) -> Result<Type> {
        // Use shared coercion logic
        type_coerce_binop(lhs_ty, rhs_ty)
    }
    
    fn load_operand(&self, expr: &TypedExpr) -> Result<Register> {
        self.builder.load_expression(expr)
    }
    
    fn allocate_temp_register(&mut self) -> Result<Register> {
        self.reg_allocator.allocate_gpr(&Type::int())
    }
    
    fn get_zero_register(&self) -> Result<Register> {
        // Return a virtual zero register that produces 0
        Ok(Register::from_index(255))
    }
    
    fn create_arm_instruction(&self, opcode: u32, rd: Register, rn: Register, rm_or_imm: Option<u32>) -> u32 {
        // Basic ARM instruction format:
        // 31-28: condition
        // 27: 0=data-processing, 1=block-data
        // 26: 0=standard, 1=special
        // 25: I=immediate (1), register (0)
        // 24-21: opcode
        // 20: S suffix (set flags)
        // 19-16: Rn
        // 15-12: Rd
        // 11-0: Rm/imm
        
        let mut instr = 0b01110 << 26; // Data-processing
        instr |= opcode << 21;
        instr |= (rn.index() as u32) << 16;
        instr |= (rd.index() as u32) << 12;
        
        if let Some(val) = rm_or_imm {
            instr |= val & 0xFFF; // Lower 12 bits
        }
        
        instr
    }
    
    fn create_arm_instruction_with_imm(&self, opcode: u32, rd: Register, rn: Register, imm: u32) -> u32 {
        let mut instr = self.create_arm_instruction(opcode, rd, rn, None);
        instr |= 0b1 << 25; // Set immediate flag
        instr |= imm; // Place immediate in lower bits
        instr
    }
}

// ============================================================================
// Supporting Functions
// ============================================================================

fn extract_int_literal(expr: &TypedExpr) -> Option<i64> {
    if let TypedExprKind::Literal(Literal::Int(val)) = &expr.kind {
        Some(val)
    } else {
        None
    }
}

fn mnemonic_to_bits(reg: Register) -> u32 {
    reg.index() as u32
}

// Test helpers
#[allow(dead_code)]
fn emulate_arm_umull(op1: u32, op2: u32) -> (u32, u32) {
    let prod = (op1 as u64) * (op2 as u64);
    ((prod & 0xFFFFFFFF) as u32, (prod >> 32) as u32)
}

#[cfg(test)]
mod arm_expr_tests {
    use super::*;
    
    #[test]
    fn test_arm_immediate_valid() {
        assert!(ArmExpressionEmitter::verify_arm_immediate(0x000000FF));
        assert!(ArmExpressionEmitter::verify_arm_immediate(0x0000FFFF));
        assert!(ArmExpressionEmitter::verify_arm_immediate(0x00010001));
        assert!(ArmExpressionEmitter::verify_arm_immediate(0));
    }
    
    #[test]
    fn test_arm_immediate_invalid() {
        assert!(!ArmExpressionEmitter::verify_arm_immediate(0x00FFFFFF));
        assert!(!ArmExpressionEmitter::verify_arm_immediate(0x80000000));
    }
    
    #[test]
    fn test_umull_emulation() {
        let (lo, hi) = emulate_arm_umull(0x00000010, 0x00000010);
        assert_eq!(lo, 0);
        assert_eq!(hi, 1);
        
        let (lo, hi) = emulate_arm_umull(0xFFFFF000, 0xFFFFF000);
        println!("Result: lo={:08X}, hi={:08X}", lo, hi);
    }
}
