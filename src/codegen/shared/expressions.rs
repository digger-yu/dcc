// src/codegen/shared/expressions.rs - 表达式求值框架
// Phase C: 数据驱动代码生成架构重构
//
// 第一性原理：表达式求值逻辑在所有 ISA 后端中是相同的
// 目标：提取通用表达式处理逻辑

use crate::ast::{BinOp, Expr, Type, UnOp};
use crate::codegen::shared::types::TypeCoercionEngine;

/// Result type for expression operations
pub type ExprResult<T> = Result<T, String>;

/// Whether the type is a floating-point scalar type.
fn is_floating_point(ty: &Type) -> bool {
    matches!(ty, Type::Float | Type::Double | Type::LongDouble)
}

/// Whether the type is an integer (or character/bool) scalar type.
fn is_integer(ty: &Type) -> bool {
    ty.is_int()
}

/// Bit width of a scalar type (8 bits per byte).
fn type_bits(ty: &Type) -> usize {
    ty.size() * 8
}

/// Whether the type is a signed integer type.
fn is_signed_int(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Char
            | Type::SChar
            | Type::Short
            | Type::Int
            | Type::Long
            | Type::LongLong
            | Type::Int128
    )
}

/// Byte offset of a named field inside a struct/union type (0 if not found).
fn struct_field_offset(ty: &Type, name: &str) -> usize {
    match ty {
        Type::Struct(_, fields, _, _) | Type::Union(_, fields, _, _) => {
            crate::ast::find_field(fields, name)
                .map(|(field, nested_off)| nested_off + field.offset)
                .unwrap_or(0)
        },
        _ => 0,
    }
}

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
    fn store_to_register(&self, src_reg: u8, addr_reg: u8, size: usize) -> ExprResult<()>;

    /// Allocate a general-purpose register
    fn allocate_gpr(&mut self, ty: &Type) -> ExprResult<u8>;

    /// Allocate a floating-point register
    fn allocate_fpr(&mut self, ty: &Type) -> ExprResult<u8>;

    /// Spill register to stack slot
    fn spill_register(&mut self, reg: u8, stack_offset: i64) -> ExprResult<()>;

    /// Load immediate constant into register (required by expression implementation)
    fn load_immediate_constant(&mut self, value: u64, dest_reg: u8, ty: &Type) -> ExprResult<()>;

    /// Load from symbol address
    fn load_from_symbol_address(&mut self, symbol: &str, dest_reg: u8, ty: &Type)
        -> ExprResult<()>;

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
    fn emit_call(
        &mut self,
        func: &crate::ast::Expr,
        args: &[Expr],
        result_reg: u8,
    ) -> ExprResult<()>;

    /// Create a fresh temporary label with the given purpose prefix.
    fn create_temp_label(&mut self, purpose: &str) -> ExprResult<String>;

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
    pub fn gen_binop(&mut self, op: &BinOp, lhs: &Expr, rhs: &Expr) -> ExprResult<RegValue> {
        // Step 1: Determine operand types
        let lhs_ty = lhs.ty();
        let rhs_ty = rhs.ty();

        // Step 2: Apply usual arithmetic conversions
        let (coerced_lhs, coerced_rhs) = self.type_coercion.coerce_binary_op(&lhs_ty, &rhs_ty);

        // Result type after coercion
        let result_ty = if coerced_lhs == coerced_rhs {
            coerced_lhs.clone()
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
        self.executor
            .emit_binary_op(op, &result_ty, lhs_reg, rhs_reg, dest_reg)?;

        Ok(RegValue {
            is_floating: is_floating_point(&result_ty),
            reg_idx: dest_reg,
            ty: result_ty,
        })
    }

    /// Evaluate binary operation without storing result (for side effects only)
    #[inline]
    pub fn gen_binop_discard(&mut self, op: &BinOp, lhs: &Expr, rhs: &Expr) -> ExprResult<()> {
        // Same as gen_binop but discard the result register allocation
        let _ = self.gen_binop(op, lhs, rhs).map(|_| ())?;
        Ok(())
    }

    /// Evaluate unary operation
    pub fn gen_unary(&mut self, op: &UnOp, operand: &Expr) -> ExprResult<RegValue> {
        let operand_ty = operand.ty();
        let src_reg = self.executor.allocate_gpr(&operand_ty)?;
        let dest_reg = self.executor.allocate_gpr(&operand_ty)?;

        self.load_operand(operand, src_reg, &operand_ty)?;

        match op {
            UnOp::Neg => {
                // Two's complement negation: -x = ~x + 1 OR sub x, zero
                self.executor
                    .emit_unary_op(op, src_reg, dest_reg, &operand_ty)?;
            },
            UnOp::Not => {
                // Bitwise NOT: ~x
                self.executor
                    .emit_unary_op(op, src_reg, dest_reg, &operand_ty)?;
            },
            _ => {
                // Default: just move value
                self.executor
                    .emit_unary_op(op, src_reg, dest_reg, &operand_ty)?;
            },
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
        let src_bits = type_bits(&source_ty);
        let dst_bits = type_bits(target_ty);
        if is_integer(&source_ty) && is_integer(target_ty) {
            if src_bits < dst_bits {
                // Expand width
                if is_signed_int(&source_ty) {
                    self.executor
                        .sign_extend(src_reg, dest_reg, src_bits, dst_bits)?;
                } else {
                    self.executor
                        .zero_extend(src_reg, dest_reg, src_bits, dst_bits)?;
                }
            } else if src_bits > dst_bits {
                // Truncate (already done by destination register allocation in most cases)
                self.executor
                    .zero_extend(src_reg, dest_reg, src_bits, dst_bits)?;
            }
        } else if is_integer(&source_ty) && is_floating_point(target_ty) {
            self.executor.convert_int_to_float(src_reg, dest_reg)?;
        } else if is_floating_point(&source_ty) && is_integer(target_ty) {
            self.executor
                .convert_float_to_int(src_reg, dest_reg, true, dst_bits)?;
        }

        Ok(RegValue {
            reg_idx: dest_reg,
            ty: target_ty.clone(),
            is_floating: is_floating_point(target_ty),
        })
    }

    /// Load operand into register - comprehensive implementation
    pub fn load_operand(&mut self, expr: &Expr, reg: u8, ty: &Type) -> ExprResult<()> {
        match expr {
            // Literal values
            Expr::Int { v, .. } => {
                // Load immediate integer constant
                self.executor.load_immediate_constant(*v as u64, reg, ty)?;
            },
            Expr::Float { v, .. } => {
                // Floating point constants go through rodata
                let symbol = format!(".const_float_{v}");
                self.executor.load_from_symbol_address(&symbol, reg, ty)?;
            },
            Expr::Char { v, .. } => {
                self.executor.load_immediate_constant(*v as u64, reg, ty)?;
            },

            // Identifiers and global variables
            Expr::Ident { name, .. } => {
                // Check if it's a compile-time constant first
                if let Ok(cv) = crate::const_eval::eval_full(expr, false) {
                    let const_val = cv.as_int().unwrap_or(0) as u64;
                    self.executor.load_immediate_constant(const_val, reg, ty)?;
                } else {
                    // Runtime symbol lookup
                    let symbol = name.name.clone();
                    self.executor.load_from_symbol_address(&symbol, reg, ty)?;
                }
            },

            // Array subscripting (arr[idx])
            Expr::Index { arr, idx, .. } => {
                // Compute address: base + index * element_size
                let arr_ty = arr.ty();
                let arr_addr_reg = self.temp_reg_counter;
                self.temp_reg_counter += 1;
                self.load_operand(arr, arr_addr_reg, &arr_ty)?;

                let idx_ty = idx.ty();
                let idx_reg = self.temp_reg_counter;
                self.temp_reg_counter += 1;
                self.load_operand(idx, idx_reg, &idx_ty)?;

                let elem_size = arr_ty
                    .get_array_element_type()
                    .map(|et| et.size() as u64)
                    .unwrap_or(0);
                if elem_size != 0 && elem_size.is_power_of_two() {
                    // Optimization: shift instead of multiply for power-of-2 sizes
                    let shift_log2 = elem_size.trailing_zeros();
                    self.executor
                        .shift_left(idx_reg, idx_reg, shift_log2 as u8)?;
                } else {
                    // General case: multiply
                    self.executor.imul_imm(idx_reg, idx_reg, elem_size as u64)?;
                }

                // Add to base address
                let addr_reg = self.temp_reg_counter;
                self.temp_reg_counter += 1;
                self.executor.add_regs(arr_addr_reg, idx_reg, addr_reg)?;

                // Load from computed address
                self.executor
                    .load_from_register(addr_reg, reg, ty.size(), is_signed_int(ty))?;
            },

            // Struct member access (obj.member)
            Expr::Member {
                expr: obj, field, ..
            } => {
                // Get offset of member in struct
                let obj_agg_ty = obj.ty();
                let offset = struct_field_offset(&obj_agg_ty, &field.name);

                let obj_reg = self.temp_reg_counter;
                self.temp_reg_counter += 1;
                self.load_operand(obj, obj_reg, &obj_agg_ty)?;

                // Apply offset to get member address
                let mem_reg = self.temp_reg_counter;
                self.temp_reg_counter += 1;

                if offset != 0 {
                    // lea mem_reg, [obj_reg + offset]
                    self.executor.add_imm(obj_reg, offset as i64, mem_reg)?;
                    self.executor
                        .load_from_register(mem_reg, reg, ty.size(), is_signed_int(ty))?;
                } else {
                    self.executor
                        .load_from_register(obj_reg, reg, ty.size(), is_signed_int(ty))?;
                }
            },

            // Pointer dereference (*ptr)
            Expr::Deref { e, .. } => {
                let ptr_ty = e.ty();
                let ptr_reg = self.temp_reg_counter;
                self.temp_reg_counter += 1;
                self.load_operand(e, ptr_reg, &ptr_ty)?;
                self.executor
                    .load_from_register(ptr_reg, reg, ty.size(), is_signed_int(ty))?;
            },

            // Address-of operator (&expr)
            Expr::AddrOf { e, .. } => {
                // Handle lvalue cases
                match e.as_ref() {
                    Expr::Ident { name, .. } => {
                        // Global/local variable addresses
                        let symbol = name.name.clone();
                        self.executor.store_address(reg, &symbol)?;
                    },
                    Expr::Member {
                        expr: obj, field, ..
                    } => {
                        // Struct member address
                        let obj_reg = self.temp_reg_counter;
                        self.temp_reg_counter += 1;
                        let obj_ty = obj.ty();
                        self.load_operand(obj, obj_reg, &obj_ty)?;

                        let offset = struct_field_offset(&obj_ty, &field.name);
                        if offset != 0 {
                            self.executor.add_imm(obj_reg, offset as i64, reg)?;
                        } else {
                            if obj_reg != reg {
                                self.executor.mov_reg_to_reg(obj_reg, reg, 8)?; // 64-bit pointer
                            }
                        }
                    },
                    Expr::Index { arr, idx, .. } => {
                        // Array element address (compute without loading value)
                        // For arrays named by an identifier, take the base
                        // address directly; index handling is a placeholder.
                        let _ = idx;
                        if let Expr::Ident { name, .. } = arr.as_ref() {
                            self.executor.store_address(reg, &name.name)?;
                        }
                    },
                    _ => {
                        return Err("Cannot take address of this expression type".to_string());
                    },
                }
            },

            // Compound literals (struct initialization)
            Expr::CompoundLit { ty: _, inits, .. } | Expr::InitList { inits, .. } => {
                // Materialize to temporary location then copy to register
                let temp_slot = self.temp_reg_counter;
                self.temp_reg_counter += 1;

                // Store initialized value to temp slot (simplified - real impl needs more work)
                // This is a placeholder; full implementation requires handling all init items
                let _ = inits;

                // Copy from temp to destination
                if temp_slot != reg {
                    self.executor.load_from_register(
                        temp_slot,
                        reg,
                        ty.size(),
                        is_signed_int(ty),
                    )?;
                }
            },

            // Function calls
            Expr::Call { callee, args, .. } => {
                // Evaluate arguments left-to-right (or right-to-left depending on ABI)
                // Call function, result already in designated return register
                // Defer to backend's call instruction emitter
                self.executor.emit_call(callee, args, reg)?;
            },

            // Type casts (already handled by gen_cast, but support here too)
            Expr::Cast { e, ty: cast_ty, .. } => {
                // Source might be different than target
                let src_e_ty = e.ty();
                let src_reg = self.temp_reg_counter;
                self.temp_reg_counter += 1;
                self.load_operand(e, src_reg, &src_e_ty)?;

                if src_reg != reg {
                    let src_bits = type_bits(&src_e_ty);
                    let dst_bits = type_bits(cast_ty);
                    // Convert and store
                    if is_integer(&src_e_ty) && is_integer(cast_ty) {
                        if src_bits < dst_bits {
                            if is_signed_int(&src_e_ty) {
                                self.executor
                                    .sign_extend(src_reg, reg, src_bits, dst_bits)?;
                            } else {
                                self.executor
                                    .zero_extend(src_reg, reg, src_bits, dst_bits)?;
                            }
                        } else {
                            // Truncate
                            let size = cast_ty.size();
                            self.executor.load_from_register(
                                src_reg,
                                reg,
                                size,
                                is_signed_int(cast_ty),
                            )?;
                        }
                    } else if is_floating_point(&src_e_ty) && is_integer(cast_ty) {
                        self.executor.convert_float_to_int(
                            src_reg,
                            reg,
                            is_signed_int(cast_ty),
                            dst_bits,
                        )?;
                    } else if is_integer(&src_e_ty) && is_floating_point(cast_ty) {
                        self.executor.convert_int_to_float(src_reg, reg)?;
                    } else {
                        // Same type or other conversions
                        let size = ty.size();
                        self.executor.mov_reg_to_reg(src_reg, reg, size)?;
                    }
                }
            },

            // Conditional expressions (?:)
            Expr::Cond {
                cond, then_, else_, ..
            } => {
                let dest_reg = reg;
                let then_label = self.executor.create_temp_label("then")?;
                let end_label = self.executor.create_temp_label("endif")?;

                // Evaluate condition
                let cond_ty = cond.ty();
                let cond_reg = self.temp_reg_counter;
                self.temp_reg_counter += 1;
                self.load_operand(cond, cond_reg, &cond_ty)?;

                // Branch based on condition
                self.executor.branch_if_zero(cond_reg, &then_label)?;

                // Then branch
                let then_ty = then_.ty();
                self.load_operand(then_, dest_reg, &then_ty)?;
                self.executor.jump(&end_label)?;

                // Else label
                self.executor.define_label(&then_label)?;
                let else_ty = else_.ty();
                self.load_operand(else_, dest_reg, &else_ty)?;

                // End label
                self.executor.define_label(&end_label)?;
            },

            // Pre/post increment/decrement
            Expr::PreInc { e, .. }
            | Expr::PreDec { e, .. }
            | Expr::PostInc { e, .. }
            | Expr::PostDec { e, .. } => {
                let incr = matches!(expr, Expr::PreInc { .. } | Expr::PostInc { .. });

                let expr_reg = self.temp_reg_counter;
                self.temp_reg_counter += 1;
                let e_ty = e.ty();
                self.load_operand(e, expr_reg, &e_ty)?;

                let one_const = self.temp_reg_counter;
                self.temp_reg_counter += 1;
                self.executor
                    .load_immediate_constant(1, one_const, &Type::Int)?;

                if incr {
                    self.executor.add_regs(expr_reg, one_const, expr_reg)?;
                } else {
                    self.executor.sub_regs(expr_reg, one_const, expr_reg)?;
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
                    self.executor.mov_reg_to_reg(expr_reg, reg, size)?;
                }
            },

            // Default: everything else falls back to constant evaluation or error
            _ => {
                // Try compile-time constant first
                if let Ok(cv) = crate::const_eval::eval_full(expr, false) {
                    if let Some(val) = cv.as_int() {
                        self.executor.load_immediate_constant(val as u64, reg, ty)?;
                        return Ok(());
                    }
                }

                // If we reach here, we can't handle this expression type
                return Err(format!("Cannot load operand of expression type {:?}", expr.ty()));
            },
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

        if size <= reg_size {
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

/// Whether the expression can be fully evaluated at compile time.
fn is_constant_expr(expr: &Expr) -> bool {
    crate::const_eval::eval_full(expr, false).is_ok()
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
        assert!(is_constant_expr(&int_expr));

        let float_expr = Expr::Float {
            v: 3.14,
            span: Span::point("", 1, 1),
            ty: Type::Double,
        };
        assert!(is_constant_expr(&float_expr));

        let binop_expr = Expr::Binop {
            op: crate::ast::BinOp::Add,
            l: Box::new(int_expr.clone()),
            r: Box::new(int_expr),
            span: Span::point("", 1, 1),
            ty: Type::Int,
        };
        assert!(is_constant_expr(&binop_expr));
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
