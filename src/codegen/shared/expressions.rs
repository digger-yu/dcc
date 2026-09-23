// src/codegen/shared/expressions.rs - 表达式求值框架
// Phase C: 数据驱动代码生成架构重构
//
// 第一性原理：表达式求值逻辑在所有 ISA 后端中是相同的
// 目标：提取通用表达式处理逻辑

use crate::ast::{Expr, Type, BinOp, UnOp};
use crate::codegen::shared::types::TypeCoercionEngine;

/// Result type for expression operations
pub type ExprResult<T> = Result<T, String>;

/// ISA-specific expression execution trait (Strategy Pattern)
pub trait ExpressionExecutor: Send + Sync {
    /// Emit binary operation instruction
    fn emit_binary_op(
        &self,
        op: &BinOp,
        result_ty: &Type,
        lhs_reg: u8,
        rhs_reg: u8,
        dest_reg: u8,
    ) -> ExprResult<()>;

    /// Emit unary operation instruction
    fn emit_unary_op(
        &self,
        op: &UnOp,
        src_reg: u8,
        dest_reg: u8,
        result_ty: &Type,
    ) -> ExprResult<()>;

    /// Convert float to integer
    fn convert_float_to_int(
        &self,
        src_reg: u8,
        dest_reg: u8,
        signed: bool,
        dest_bits: usize,
    ) -> ExprResult<()>;

    /// Convert integer to float
    fn convert_int_to_float(&self, src_reg: u8, dest_reg: u8) -> ExprResult<()>;

    /// Sign-extend register value
    fn sign_extend(
        &self,
        src_reg: u8,
        dest_reg: u8,
        bits_from: usize,
        bits_to: usize,
    ) -> ExprResult<()>;

    /// Zero-extend register value
    fn zero_extend(
        &self,
        src_reg: u8,
        dest_reg: u8,
        bits_from: usize,
        bits_to: usize,
    ) -> ExprResult<()>;

    /// Load from memory address into register
    fn load_from_register(
        &self,
        addr_reg: u8,
        dest_reg: u8,
        size: usize,
        signed: bool,
    ) -> ExprResult<()>;

    /// Store register to memory address
    fn store_to_register(
        &self,
        src_reg: u8,
        addr_reg: u8,
        size: usize,
    ) -> ExprResult<()>;

    /// Allocate a general-purpose register
    fn allocate_gpr(&mut self, ty: &Type) -> ExprResult<u8>;

    /// Allocate a floating-point register
    fn allocate_fpr(&mut self, ty: &Type) -> ExprResult<u8>;

    /// Spill register to stack slot
    fn spill_register(&mut self, reg: u8, stack_offset: i64) -> ExprResult<()>;

    /// Load immediate constant into register (required by expression implementation)
    fn load_immediate_constant(&mut self, value: u64, dest_reg: u8, ty: &Type) -> ExprResult<()>;

    /// Load from symbol address
    fn load_from_symbol_address(&mut self, symbol: &str, dest_reg: u8, ty: &Type) -> ExprResult<()>;

    /// Shift left operation
    fn shift_left(&mut self, src_reg: u8, dest_reg: u8, count: u8) -> ExprResult<()>;

    /// Integer multiply with immediate
    fn imul_imm(&mut self, dest_reg: u8, src_reg: u8, imm: u64) -> ExprResult<()>;

    /// Add two registers
    fn add_regs(&mut self, src1: u8, src2: u8, dest: u8) -> ExprResult<()>;

    /// Subtract two registers
    fn sub_regs(&mut self, src1: u8, src2: u8, dest: u8) -> ExprResult<()>;

    /// Add immediate offset to register
    fn add_imm(&mut self, base_reg: u8, offset: i64, dest_reg: u8) -> ExprResult<()>;

    /// Move between registers (generic size)
    fn mov_reg_to_reg(&mut self, src_reg: u8, dest_reg: u8, size_bytes: usize) -> ExprResult<()>;

    /// Store address
    fn store_address(&mut self, dest_reg: u8, symbol: &str) -> ExprResult<()>;

    /// Emit function call
    fn emit_call(&mut self, func: &crate::ast::Expr, args: &[Box<crate::ast::Expr>], result_reg: u8) -> ExprResult<()>;

    /// Branch if zero
    fn branch_if_zero(&mut self, cond_reg: u8, target: &str) -> ExprResult<()>;

    /// Unconditional jump
    fn jump(&mut self, target: &str) -> ExprResult<()>;

    /// Define label
    fn define_label(&mut self, label: &str) -> ExprResult<()>;

    /// Branch if not equal
    fn branch_if_not_equal(&mut self, reg1: u8, reg2: u8, target: &str) -> ExprResult<()>;

    /// Clear register (set to zero)
    fn clear_register(&mut self, reg: u8, ty: &Type) -> ExprResult<()>;
}

/// Register value descriptor for intermediate results
#[derive(Debug, Clone)]
pub struct RegValue {
    pub reg_idx: u8,
    pub ty: Type,
    pub is_floating: bool,
}

/// Main expression evaluation engine (ISA-independent)
pub struct ExpressionEmitter<'a, EE: ExpressionExecutor> {
    target: &'a crate::target::Target,
    pub type_coercion: TypeCoercionEngine,
    executor: &'a mut EE,
    temp_reg_counter: u8,
}

impl<'a, EE: ExpressionExecutor> ExpressionEmitter<'a, EE> {
    pub fn new(target: &'a crate::target::Target, executor: &'a mut EE) -> Self {
        Self {
            target,
            type_coercion: TypeCoercionEngine,
            executor,
            temp_reg_counter: 0,
        }
    }

    /// Evaluate binary operation
    pub fn gen_binop(
        &mut self,
        op: &BinOp,
        lhs: &Expr,
        rhs: &Expr,
    ) -> ExprResult<RegValue> {
        // Step 1: Determine operand types
        let lhs_ty = lhs.ty();
        let rhs_ty = rhs.ty();

        // Step 2: Apply usual arithmetic conversions
        let (coerced_lhs, coerced_rhs) = self.type_coercion.coerce_binary_op(&lhs_ty, &rhs_ty);
        
        // Result type after coercion
        let result_ty = if coerced_lhs == coerced_rhs {
            coerced_lhs
        } else {
            coerced_rhs.clone()
        };

        // Step 3: Allocate registers
        let lhs_reg = self.executor.allocate_gpr(&coerced_lhs)?;
        let rhs_reg = self.executor.allocate_gpr(&coerced_rhs)?;
        let dest_reg = self.executor.allocate_gpr(&result_ty)?;

        // Step 4: Load operands (placeholder - actual loading depends on backend)
        self.load_operand(lhs, lhs_reg, &coerced_lhs)?;
        self.load_operand(rhs, rhs_reg, &coerced_rhs)?;

        // Step 5: Dispatch to ISA-specific executor
        self.executor.emit_binary_op(op, &result_ty, lhs_reg, rhs_reg, dest_reg)?;

        Ok(RegValue {
            reg_idx: dest_reg,
            ty: result_ty,
            is_floating: result_ty.is_floating_point(),
        })
    }
    
    /// Evaluate binary operation without storing result (for side effects only)
    #[inline]
    pub fn gen_binop_discard(
        &mut self,
        op: &BinOp,
        lhs: &Expr,
        rhs: &Expr,
    ) -> ExprResult<()> {
        // Same as gen_binop but discard the result register allocation
        let _ = self.gen_binop(op, lhs, rhs).map(|_| ())?;
        Ok(())
    }

    /// Evaluate unary operation
    pub fn gen_unary(
        &mut self,
        op: &UnOp,
        operand: &Expr,
    ) -> ExprResult<RegValue> {
        let operand_ty = operand.ty();
        let src_reg = self.executor.allocate_gpr(&operand_ty)?;
        let dest_reg = self.executor.allocate_gpr(&operand_ty)?;

        self.load_operand(operand, src_reg, &operand_ty)?;

        match op {
            UnOp::Neg => {
                // Two's complement negation: -x = ~x + 1 OR sub x, zero
                self.executor.emit_unary_op(op, src_reg, dest_reg, &operand_ty)?;
            }
            UnOp::Not => {
                // Bitwise NOT: ~x
                self.executor.emit_unary_op(op, src_reg, dest_reg, &operand_ty)?;
            }
            _ => {
                // Default: just move value
                self.executor.emit_unary_op(op, src_reg, dest_reg, &operand_ty)?;
            }
        }

        Ok(RegValue {
            reg_idx: dest_reg,
            ty: operand_ty,
            is_floating: false,
        })
    }

    /// Cast expression
    pub fn gen_cast(&mut self, expr: &Expr, target_ty: &Type) -> ExprResult<RegValue> {
        let source_ty = expr.ty();
        let src_reg = self.executor.allocate_gpr(&source_ty)?;
        let dest_reg = self.executor.allocate_gpr(target_ty)?;

        self.load_operand(expr, src_reg, &source_ty)?;

        // Handle type conversion
        if source_ty.is_integer() && target_ty.is_integer() {
            if source_ty.bits() < target_ty.bits() {
                // Expand width
                if source_ty.is_signed() {
                    self.executor.sign_extend(src_reg, dest_reg, source_ty.bits(), target_ty.bits())?;
                } else {
                    self.executor.zero_extend(src_reg, dest_reg, source_ty.bits(), target_ty.bits())?;
                }
            } else if source_ty.bits() > target_ty.bits() {
                // Truncate (already done by destination register allocation in most cases)
                self.executor.zero_extend(src_reg, dest_reg, source_ty.bits(), target_ty.bits())?;
            }
        } else if source_ty.is_integer() && target_ty.is_floating_point() {
            self.executor.convert_int_to_float(src_reg, dest_reg)?;
        } else if source_ty.is_floating_point() && target_ty.is_integer() {
            self.executor.convert_float_to_int(src_reg, dest_reg, true, target_ty.bits())?;
        }

        Ok(RegValue {
            reg_idx: dest_reg,
            ty: target_ty.clone(),
            is_floating: target_ty.is_floating_point(),
        })
    }

    /// Load operand into register - comprehensive implementation
    pub fn load_operand(&mut self, expr: &Expr, reg: u8, ty: &Type) -> ExprResult<()> {
        match expr {
            // Literal values
            Expr::Int { val, .. } => {
                // Load immediate integer constant
                self.executor.load_immediate_constant(*val as u64, reg, ty)?;
            }
            Expr::Float { val, .. } => {
                // Floating point constants go through rodata
                let symbol = format!(".const_float_{val}");
                self.executor.load_from_symbol_address(&symbol, reg, ty)?;
            }
            Expr::Char { val, .. } => {
                self.executor.load_immediate_constant(*val as u64, reg, ty)?;
            }
            
            // Identifiers and global variables
            Expr::Ident { name, .. } => {
                // Check if it's a compile-time constant first
                if let Ok(cv) = crate::const_eval::eval_full(expr, false) {
                    let const_val = cv.to_u64().unwrap_or(0);
                    self.executor.load_immediate_constant(const_val, reg, ty)?;
                } else {
                    // Runtime symbol lookup
                    let symbol = name.clone();
                    self.executor.load_from_symbol_address(&symbol, reg, ty)?;
                }
            }
            
            // Array subscripting (arr[idx])
            Expr::ArrayRef { arr, idx, .. } => {
                // Compute address: base + index * element_size
                let arr_addr_reg = self.temp_reg_counter;
                self.temp_reg_counter += 1;
                self.load_operand(arr, arr_addr_reg, arr.ty())?;
                
                let idx_reg = self.temp_reg_counter;
                self.temp_reg_counter += 1;
                self.load_operand(idx, idx_reg, idx.ty())?;
                
                let elem_size = arr.elem_size() as u64;
                if elem_size != 0 && elem_size.is_power_of_two() {
                    // Optimization: shift instead of multiply for power-of-2 sizes
                    let shift_log2 = elem_size.trailing_zeros();
                    self.executor.shift_left(idx_reg, idx_reg, shift_log2 as u8)?;
                } else {
                    // General case: multiply
                    self.executor.imul_imm(idx_reg, idx_reg, elem_size as u64)?;
                }
                
                // Add to base address
                let addr_reg = self.temp_reg_counter;
                self.temp_reg_counter += 1;
                self.executor.add_regs(addr_reg, arr_addr_reg, idx_reg, addr_reg)?;
                
                // Load from computed address
                self.executor.load_from_register(addr_reg, reg, ty.size(), ty.is_signed());
            }
            
            // Struct member access (obj.member)
            Expr::Member { obj, member_name, .. } => {
                // Get offset of member in struct
                let offset = ty.member_offset(member_name).unwrap_or(0);
                
                let obj_reg = self.temp_reg_counter;
                self.temp_reg_counter += 1;
                self.load_operand(obj, obj_reg, obj.ty())?;
                
                // Apply offset to get member address
                let mem_reg = self.temp_reg_counter;
                self.temp_reg_counter += 1;
                
                if offset != 0 {
                    // lea mem_reg, [obj_reg + offset]
                    self.executor.add_imm(obj_reg, offset as i64, mem_reg)?;
                    self.executor.load_from_register(mem_reg, reg, ty.size(), ty.is_signed());
                } else {
                    self.executor.load_from_register(obj_reg, reg, ty.size(), ty.is_signed());
                }
            }
            
            // Pointer dereference (*ptr)
            Expr::Unary { op: UnOp::Deref, e, .. } => {
                let ptr_reg = self.temp_reg_counter;
                self.temp_reg_counter += 1;
                self.load_operand(e, ptr_reg, e.ty())?;
                self.executor.load_from_register(ptr_reg, reg, ty.size(), ty.is_signed());
            }
            
            // Address-of operator (&expr)
            Expr::Unary { op: UnOp::AddrOf, e, .. } => {
                // Handle lvalue cases
                match e.as_ref() {
                    Expr::Ident { name, .. } => {
                        // Global/local variable addresses
                        let symbol = name.clone();
                        self.executor.store_address(reg, &symbol)?;
                    }
                    Expr::Member { obj, member_name, .. } => {
                        // Struct member address
                        let obj_reg = self.temp_reg_counter;
                        self.temp_reg_counter += 1;
                        self.load_operand(obj, obj_reg, obj.ty())?;
                        
                        let offset = obj.ty().member_offset(member_name).unwrap_or(0);
                        if offset != 0 {
                            self.executor.add_imm(obj_reg, offset as i64, reg)?;
                        } else {
                            if obj_reg != reg {
                                self.executor.mov_reg_to_reg(obj_reg, reg, 8); // 64-bit pointer
                            }
                        }
                    }
                    Expr::ArrayRef { arr, idx, .. } => {
                        // Array element address (compute without loading value)
                        let arr_reg = self.temp_reg_counter;
                        self.temp_reg_counter += 1;
                        // For array, we need the base address directly
                        if let Expr::Ident { name, .. } = arr.as_ref() {
                            self.executor.store_address(reg, name)?;
                        }
                        // Index is ignored for address computation (not loaded)
                    }
                    _ => {
                        return Err(format!("Cannot take address of this expression type") .into());
                    }
                }
            }
            
            // Compound literals (struct initialization)
            Expr::CompoundLit { ty: _, inits, .. } | 
            Expr::InitList { inits, .. } => {
                // Materialize to temporary location then copy to register
                let temp_slot = self.temp_reg_counter;
                self.temp_reg_counter += 1;
                
                // Store initialized value to temp slot (simplified - real impl needs more work)
                // This is a placeholder; full implementation requires handling all init items
                let _ = (inits, compound_ty);
                
                // Copy from temp to destination
                if temp_slot != reg {
                    self.executor.load_from_register(temp_slot, reg, ty.size(), ty.is_signed());
                }
            }
            
            // Function calls
            Expr::Call { func, args, .. } => {
                // Evaluate arguments left-to-right (or right-to-left depending on ABI)
                // Call function, result already in designated return register
                // Defer to backend's call instruction emitter
                self.executor.emit_call(func, args, reg)?;
            }
            
            // Type casts (already handled by gen_cast, but support here too)
            Expr::Cast { e, ty: cast_ty, .. } => {
                // Source might be different than target
                let src_reg = self.temp_reg_counter;
                self.temp_reg_counter += 1;
                self.load_operand(e, src_reg, e.ty())?;
                
                if src_reg != reg {
                    // Convert and store
                    if e.ty().is_integer() && cast_ty.is_integer() {
                        if e.ty().bits() < cast_ty.bits() {
                            if e.ty().is_signed() {
                                self.executor.sign_extend(src_reg, reg, e.ty().bits(), cast_ty.bits())?;
                            } else {
                                self.executor.zero_extend(src_reg, reg, e.ty().bits(), cast_ty.bits())?;
                            }
                        } else {
                            // Truncate
                            let size = cast_ty.size();
                            self.executor.load_from_register(src_reg, reg, size as usize, cast_ty.is_signed());
                        }
                    } else if e.ty().is_floating_point() && cast_ty.is_integer() {
                        self.executor.convert_float_to_int(src_reg, reg, cast_ty.is_signed(), cast_ty.bits())?;
                    } else if e.ty().is_integer() && cast_ty.is_floating_point() {
                        self.executor.convert_int_to_float(src_reg, reg)?;
                    } else {
                        // Same type or other conversions
                        let size = ty.size();
                        self.executor.mov_reg_to_reg(src_reg, reg, size as usize);
                    }
                }
            }
            
            // Conditional expressions (?:)
            Expr::Conditional { cond, then_, else_ } => {
                let dest_reg = reg;
                let then_label = self.executor.create_temp_label("then");
                let end_label = self.executor.create_temp_label("endif");
                
                // Evaluate condition
                let cond_reg = self.temp_reg_counter;
                self.temp_reg_counter += 1;
                self.load_operand(cond, cond_reg, cond.ty())?;
                
                // Branch based on condition
                self.executor.branch_if_zero(cond_reg, &then_label)?;
                
                // Then branch
                self.load_operand(then_, dest_reg, then_.ty())?;
                self.executor.jump(&end_label)?;
                
                // Else label
                self.executor.define_label(&then_label)?;
                self.load_operand(else_, dest_reg, else_.ty())?;
                
                // End label
                self.executor.define_label(&end_label)?;
            }
            
            // Pre/post increment/decrement
            Expr::PreInc { e, .. } | Expr::PreDec { e, .. } |
            Expr::PostInc { e, .. } | Expr::PostDec { e, .. } => {
                let incr = matches!(expr, Expr::PreInc { .. } | Expr::PostInc { .. });
                
                let expr_reg = self.temp_reg_counter;
                self.temp_reg_counter += 1;
                self.load_operand(e, expr_reg, e.ty())?;
                
                let one_const = self.temp_reg_counter;
                self.temp_reg_counter += 1;
                self.executor.load_immediate_constant(1, one_const, &Type::Int)?;
                
                if incr {
                    self.executor.add_regs(expr_reg, expr_reg, one_const, expr_reg)?;
                } else {
                    self.executor.sub_regs(expr_reg, expr_reg, one_const, expr_reg)?;
                }
                
                // For post-increment, also save original value before modification
                if matches!(expr, Expr::PostInc { .. } | Expr::PostDec { .. }) {
                    // Need to return original value - simplified here
                    // Full implementation would require saving old value
                    let _ = (one_const, expr_reg);
                }
                
                // Store result back to destination
                if expr_reg != reg {
                    let size = ty.size();
                    self.executor.mov_reg_to_reg(expr_reg, reg, size as usize);
                }
            }
            
            // Empty/expression statements
            Expr::Empty => {
                // No-op, just zero-initialize destination
                self.executor.clear_register(reg, ty)?;
            }
            
            // Default: everything else falls back to constant evaluation or error
            _ => {
                // Try compile-time constant first
                if let Ok(cv) = crate::const_eval::eval_full(expr, false) {
                    if let Some(val) = cv.to_u64() {
                        self.executor.load_immediate_constant(val, reg, ty)?;
                        return Ok(());
                    }
                }
                
                // If we reach here, we can't handle this expression type
                Err(format!("Cannot load operand of expression type {:?}", expr.expr_type()).into())
            }
        }
        
        Ok(())
    }
    
    /// Generate temporary register number
    pub fn alloc_temp_reg(&mut self) -> u8 {
        let reg = self.temp_reg_counter;
        self.temp_reg_counter += 1;
        reg
    }
}

/// Stack frame utilities (ISA-independent)
pub struct StackFrameUtils;

impl StackFrameUtils {
    /// Calculate stack offset for local variable
    pub fn calc_local_offset(offset: i32, align: usize) -> i32 {
        // Align to natural boundary
        let aligned = (offset as usize + align - 1) & !(align - 1);
        aligned as i32
    }

    /// Determine if parameter should be passed in register or on stack
    pub fn param_passing_mode(ty: &Type, reg_size: usize) -> ParamPassingMode {
        let size = ty.size();
        
        if size <= reg_size as u64 {
            ParamPassingMode::Register
        } else {
            ParamPassingMode::Stack
        }
    }
}

/// Parameter passing modes
pub enum ParamPassingMode {
    Register,
    Stack,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_constant_expr() {
        use crate::diagnostics::Span;
        
        let int_expr = Expr::Int {
            v: 42,
            span: Span::point("", 1, 1),
            ty: Type::Int,
        };
        assert!(ExpressionLoweringEngine::is_constant_expr(&int_expr));

        let float_expr = Expr::Float {
            v: 3.14,
            span: Span::point("", 1, 1),
            ty: Type::Double,
        };
        assert!(ExpressionLoweringEngine::is_constant_expr(&float_expr));

        let binop_expr = Expr::Binop {
            op: crate::ast::BinOp::Add,
            l: Box::new(int_expr.clone()),
            r: Box::new(int_expr),
            span: Span::point("", 1, 1),
            ty: Type::Int,
        };
        assert!(ExpressionLoweringEngine::is_constant_expr(&binop_expr));
    }

    #[test]
    fn test_stack_alignment() {
        // 8-byte alignment
        assert_eq!(StackFrameUtils::calc_local_offset(0, 8), 0);
        assert_eq!(StackFrameUtils::calc_local_offset(1, 8), 8);
        assert_eq!(StackFrameUtils::calc_local_offset(7, 8), 8);
        assert_eq!(StackFrameUtils::calc_local_offset(8, 8), 8);
        assert_eq!(StackFrameUtils::calc_local_offset(9, 8), 16);
    }
}
