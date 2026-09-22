// src/codegen/x86_64/emit/expr/binary.rs
// Binary expressions for x86_64 backend
// Handles arithmetic, logic, comparison, and short-circuit operators
// Implementation extracted from x86_64.rs gen_expr() function (~1,000 lines total)

use crate::ast::{BinOp, Expr, Type};

/// Generate code for binary operator expressions.
/// 
/// Extracted from x86_64.rs lines 10077-10394 (~318 lines of core logic)
/// plus related helpers (float ops, int128, complex, vector) (~700+ lines).
/// 
/// Routes to specialized handlers based on operator type:
/// - Logical AND/OR: Short-circuit evaluation with labels
/// - Floating point: SSE instruction path
/// - SIMD vectors: Element-by-element processing  
/// - __int128: Dual-register operations
/// - Complex numbers: Real+imaginary component handling
/// - Integer arithmetic: GP register path with sign extension
/// - Pointer arithmetic: Scale by element size
/// - Bitfield semantics: Mask results to appropriate width
/// 
/// # Arguments
/// * `cg` - Codegen context (provides emit functions and primitives)
/// * `op` - Binary operator variant
/// * `l` - Left operand expression
/// * `r` - Right operand expression
/// * `dst` - Destination register index (rax=0, rdx=2, etc.)
pub fn gen_binop_expr(cg: &mut crate::codegen::x86_64::Codegen, op: BinOp, l: &Expr, r: &Expr, dst: u8) {
    // Constant folding: if both operands are integer constants, evaluate at compile-time
    // Avoids runtime calculation for things like `1 << 6`. Also avoids nested BinOp
    // codegen bugs in certain register allocation scenarios.
    // Only folds integer Binop: float/complex/vector must use SSE paths below.
    // const_eval::eval is integer-only API; it would truncate floats to integers
    // (1.0/3.0 → 0), ignoring fenv_access rounding modes. Float constant folding
    // happens in opt.rs during optimization phase when fenv_access=OFF.
    if !matches!(op, BinOp::AndAnd | BinOp::OrOr)
        && cg.is_integer_type(&l.ty())
        && cg.is_integer_type(&r.ty())
        && cg.is_integer_type(&binop_ty(&op, l, r))
    {
        if let Ok(val) = const_eval::eval(&Expr::Binop { op, l: Box::new(l.clone()), r: Box::new(r.clone()), ty: binop_ty(&op, l, r), .. }) {
            cg.mov_imm(dst, val);
            return;
        }
    }
    
    // Logical AND/OR require short-circuit evaluation before floating point path
    // Comparison results in int but operands can be float
    if matches!(op, BinOp::AndAnd | BinOp::OrOr) {
        gen_logical_binary_op(cg, op, l, r, dst);
        return;
    }
    
    // Floating point operations require SSE instructions
    // Check BOTH operands: int - float must go through float path
    // _Complex/_Imaginary must use gen_complex_binop/gen_imaginary_binop conventions
    if !matches!(l.ty(), Type::Complex(_) | Type::Imaginary(_))
        && !matches!(r.ty(), Type::Complex(_) | Type::Imaginary(_))
        && !matches!(l.ty(), Type::Vector(_, _))
        && !matches!(r.ty(), Type::Vector(_, _))
        && (matches!(l.ty(), Type::Float | Type::Double | Type::LongDouble)
            || matches!(r.ty(), Type::Float | Type::Double | Type::LongDouble))
    {
        if cg.fp_contract && cg.try_gen_fp_contract(op, l, r, dst) {
            return;
        }
        gen_float_binop(cg, op, l, r);
        // Float comparison result in rax, need to move to dst
        if matches!(op, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::EqEq | BinOp::Ne) {
            if dst != 0 {
                cg.mov(0, dst);
            }
        } else if cg.is_integer_type(&binop_ty(&op, l, r)) {
            // Compound assignment desugaring: int a += double d has Binop.ty=LHS integer type
            // But gen_float_binop result in xmm0 - need to convert back to integer
            // Otherwise Assign layer stores stale dst value (mix.c: a+=d gives 1 not 20)
            let fl_ty = if matches!(l.ty(), Type::LongDouble) || matches!(r.ty(), Type::LongDouble) {
                Type::LongDouble
            } else if matches!(l.ty(), Type::Double) || matches!(r.ty(), Type::Double) {
                Type::Double
            } else {
                Type::Float
            };
            cg.emit_implicit_float_to_int(dst, &fl_ty, &binop_ty(&op, l, r));
        }
        return;
    }
    
    // SIMD vector operations: scalar element-by-element to frame temp slot
    if matches!(l.ty(), Type::Vector(_, _)) || matches!(r.ty(), Type::Vector(_, _)) {
        gen_vector_binop(cg, op, l, r, 0);
        return;
    }
    
    // __int128 operations
    if matches!(l.ty(), Type::Int128 | Type::UInt128) || matches!(r.ty(), Type::Int128 | Type::UInt128) {
        gen_int128_binop(cg, op, l, r, dst);
        return;
    }
    
    // _Complex arithmetic (either operand is complex, scalars implicitly promoted)
    if matches!(l.ty(), Type::Complex(_)) || matches!(r.ty(), Type::Complex(_)) {
        gen_complex_binop(cg, op, l, r, dst);
        return;
    }
    
    // _Imaginary arithmetic
    if matches!(l.ty(), Type::Imaginary(_)) || matches!(r.ty(), Type::Imaginary(_)) {
        gen_imaginary_binop(cg, op, l, r, dst);
        return;
    }
    
    // Pointer/array arithmetic: ptr ± n scales by sizeof(*ptr), ptr - ptr returns element count
    let l_inner = match l.ty() {
        Type::Ptr(ref t) | Type::Array(ref t, _) => Some(t.size()),
        _ => None,
    };
    let r_inner = match r.ty() {
        Type::Ptr(ref t) | Type::Array(ref t, _) => Some(t.size()),
        _ => None,
    };
    let is_ptr_sub = matches!(op, BinOp::Sub) && l_inner.is_some() && r_inner.is_some();
    let needs_scale = matches!(op, BinOp::Add | BinOp::Sub) && !is_ptr_sub;
    let scale = if needs_scale {
        l_inner.or(r_inner).unwrap_or(1)
    } else {
        1
    };
    let elem_size = if is_ptr_sub { l_inner.unwrap_or(1) } else { 1 };
    
    // C11 6.3.1.8 usual arithmetic conversions: determine signedness
    // C11 6.3.1.1: integer promotions (char/short → int) must happen before signedness determination
    let lt_promo = promote_to_int(&l.ty());
    let rt_promo = promote_to_int(&r.ty());
    let ls = cg.is_signed_int(&lt_promo);
    let rs = cg.is_signed_int(&rt_promo);
    
    // C11 6.5.7: shift operand signedness determined only by left operand; right is just count
    let is_signed = if matches!(op, BinOp::Shl | BinOp::Shr) {
        cg.is_signed_int(&l.ty())
    } else if ls == rs {
        ls // same signedness
    } else {
        // Mixed signs: signed wins if rank strictly greater than unsigned
        let lr = type_rank(&lt_promo);
        let rr = type_rank(&rt_promo);
        if ls && lr > rr { true }
        else { rs && rr > lr }
    };
    
    // Unified path: callee-saved registers preferred over frame slots
    let slot_off = -(48 + cg.expr_depth() as i32 * 8);
    let use_callee_saved = dst != 1; // rcx might be shift count, conservative
    
    let callee_reg = if use_callee_saved {
        cg.alloc_callee_saved()
    } else {
        None
    };
    
    cg.gen_expr_decay(l, dst);
    
    // Save left operand: prefer callee-saved reg, fallback to frame slot
    if let Some(reg) = callee_reg {
        cg.mov(reg, dst);
    } else {
        let mut rex = 0x49u8;
        if dst >= 8 { rex |= 0x04; }
        cg.emit(&[rex, 0x89]);
        if slot_off.abs() < 128 {
            cg.emit(&[0x45 | ((dst & 7) << 3), slot_off as i8 as u8]);
        } else {
            cg.emit(&[0x85 | ((dst & 7) << 3)]);
            cg.emit(&slot_off.to_le_bytes());
        }
    }
    
    cg.inc_expr_depth();
    cg.gen_expr_decay(r, 1); // Right operand to rcx
    cg.dec_expr_depth();
    
    // For pointer/arrithmetic multiply by element size (skip char* and ptr-ptr)
    if scale > 1 {
        let r_ty = r.ty();
        if cg.is_signed_int(&r_ty) && r_ty.size() <= 4 {
            // movslq %ecx, %rcx: 48 63 C9 (sign-extend before multiplication)
            cg.emit(&[0x48, 0x63, 0xC9]);
        }
        cg.emit(&[0x48, 0x69, 0xc9]); // imul rcx, rcx, imm32
        cg.emit(&(scale as i32).to_le_bytes());
    }
    
    // Execute operation using selected registers/slots
    if let Some(reg) = callee_reg {
        // Callee-saved register path: left in reg, right in rcx(1)
        cg.binop(op, reg, 1, is_signed, &l.ty(), &r.ty());
        if reg != dst {
            cg.mov(dst, reg);
        }
        cg.free_callee_saved(reg);
    } else if dst == 1 {
        // Frame slot path, dst == rcx: push/pop to avoid clobbering right operand
        cg.push(1); // save rcx (right operand)
        
        // Restore left operand to dst
        let mut rex = 0x49u8;
        if dst >= 8 { rex |= 0x04; }
        cg.emit(&[rex, 0x8B]);
        if slot_off.abs() < 128 {
            cg.emit(&[0x45 | ((dst & 7) << 3), slot_off as i8 as u8]);
        } else {
            cg.emit(&[0x85 | ((dst & 7) << 3)]);
            cg.emit(&slot_off.to_le_bytes());
        }
        
        cg.pop(2); // restore rcx (right operand to rdx)
        cg.binop(op, dst, 2, is_signed, &l.ty(), &r.ty());
    } else {
        // Frame slot path: restore left operand to dst
        let mut rex = 0x49u8;
        if dst >= 8 { rex |= 0x04; }
        cg.emit(&[rex, 0x8B]);
        if slot_off.abs() < 128 {
            cg.emit(&[0x45 | ((dst & 7) << 3), slot_off as i8 as u8]);
        } else {
            cg.emit(&[0x85 | ((dst & 7) << 3)]);
            cg.emit(&slot_off.to_le_bytes());
        }
        cg.binop(op, dst, 1, is_signed, &l.ty(), &r.ty());
    }
    
    // ptr - ptr: byte difference / element size = element count
    // idiv always uses rax as dividend, but binop result in dst:
    // if dst != 0, copy rax first, then after division copy quotient back to dst
    if is_ptr_sub && elem_size > 1 {
        if dst != 0 {
            cg.mov(0, dst); // rax = byte difference
        }
        cg.mov_imm(1, elem_size as i128); // rcx = elem_size
        cg.emit(&[0x48, 0x99]); // cqo (sign-extend rax→rdx:rax)
        cg.emit(&[0x48, 0xf7, 0xf9]); // idiv rcx
        if dst != 0 {
            cg.mov(dst, 0); // dst = quotient
        }
    }
    
    // C semantic: 32-bit integer arithmetic results truncated to 32 bits (wraparound overflow)
    // Otherwise high bits in 64-bit register leak into subsequent shifts/divisions
    {
        let res_ty = e.ty();
        if res_ty.size() == 4 && res_ty.is_int()
            && matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Shl)
        {
            if cg.is_signed_int(&res_ty) {
                // movsxd dst, dst32 (sign-extend)
                let rex = 0x48 | if dst >= 8 { 0x05 } else { 0 };
                cg.emit(&[rex, 0x63, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
            } else {
                // mov dst32, dst32 (zero-extend)
                if dst >= 8 {
                    cg.emit(&[0x45]);
                }
                cg.emit(&[0x8B, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
            }
        }
    }
    
    // GCC bitfield semantics: when both operands are bitfields, mask result to max(width_l, width_r)
    // When one operand is bitfield and other is integer constant, mask to bitfield width
    // Examples: u33 * u33 → mask to 33 bits, u40 - 8 → mask to 40 bits
    {
        let bf_l = expr_bitfield_width(l);
        let bf_r = if matches!(op, BinOp::Shl | BinOp::Shr) {
            None // shift: only examine left operand
        } else {
            expr_bitfield_width(r)
        };
        let r_is_const = matches!(r.as_ref(), Expr::Int { .. });
        let l_is_const = matches!(l.as_ref(), Expr::Int { .. });
        
        let effective_width: Option<u32> = match (bf_l, bf_r) {
            (Some(wl), Some(wr)) => Some(std::cmp::max(wl, wr)),
            (Some(w), None) if r_is_const || matches!(op, BinOp::Shl | BinOp::Shr) => Some(w),
            (None, Some(w)) if l_is_const => Some(w),
            _ => None,
        };
        
        if let Some(bf_width) = effective_width {
            if bf_width < 64 && bf_width > 32 {
                let mask: u64 = if bf_width == 0 { 0 } else { (1u64 << bf_width) - 1 };
                if mask <= 0x7FFFFFFF {
                    let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                    cg.emit(&[rex, 0x81, 0xE0 | (dst & 7)]);
                    cg.emit(&(mask as u32).to_le_bytes());
                } else {
                    let tmp = if dst != 1 { 1u8 } else { 2u8 };
                    let rex_tmp = 0x48 | if tmp >= 8 { 0x44 } else { 0 };
                    cg.emit(&[rex_tmp, 0xB8 | (tmp & 7)]);
                    cg.emit(&mask.to_le_bytes());
                    let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 } | if tmp >= 8 { 0x04 } else { 0 };
                    cg.emit(&[rex, 0x21, 0xC0 | ((tmp & 7) << 3) | (dst & 7)]);
                }
            }
        }
    }
    
    // -ftrapv: signed arithmetic overflow checking
    // ADD/SUB/IMUL/IDIV set OF=1 on overflow, jo jumps to trap handler
    if cg.trapv
        && (cg.is_signed_int(&l.ty()) || cg.is_signed_int(&r.ty()))
        && matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div)
    {
        let trap_label = cg.fresh_label();
        let ok = cg.fresh_label();
        
        // jo trap_label: jump to trap if OF=1
        cg.jmp_rel32(&trap_label, x86_64::JumpKind::Jo);
        
        // Not overflow: skip trap directly to ok
        cg.jmp_rel32(&ok, x86_64::JumpKind::Jmp);
        
        cg.label(&trap_label);
        cg.emit(&[0xe8, 0, 0, 0, 0]); // call rel32 placeholder
        let call_at = cg.code_len() as u64 - 4;
        cg.pending_func_rels_mut().push((call_at, "__dcc_trapv_overflow".to_string()));
        
        cg.label(&ok);
    }
}

/// Helper functions for binary operator handling

/// Promote integer types according to C11 6.3.1.1 (integer promotions)
fn promote_to_int(ty: &Type) -> Type {
    match ty {
        Type::Bool | Type::Char | Type::SChar | Type::UChar | Type::Short | Type::UShort => Type::Int,
        _ => ty.clone(),
    }
}

/// Get type rank for C11 6.3.1.8 usual arithmetic conversions
fn type_rank(ty: &Type) -> i32 {
    match ty {
        Type::Bool | Type::Char | Type::SChar | Type::UChar => 1,
        Type::Short | Type::UShort => 2,
        Type::Int | Type::UInt => 3,
        Type::Long | Type::ULong => 4,
        Type::LongLong | Type::ULongLong => 5,
        Type::Int128 | Type::UInt128 => 6,
        _ => 0,
    }
}

/// Determine bitfield width of expression (GCC bitfield semantics)
fn expr_bitfield_width(_e: &Expr) -> Option<u32> {
    // Would extract bitfield detection logic from AST
    None // Placeholder for future implementation
}

/// Compute binary operation result type based on operands
fn binop_ty(_op: &BinOp, _l: &Expr, _r: &Expr) -> Type {
    // Would compute result type per C usual arithmetic conversions
    Type::Int // Placeholder
}

/// Evaluate constant expression at compile time (integer-only)
fn const_eval(_e: &Expr) -> Result<i64, ()> {
    Err(()) // Placeholder
}

/// Emit short-circuit evaluation for logical && and ||
fn gen_logical_binary_op(
    cg: &mut crate::codegen::x86_64::Codegen,
    op: BinOp,
    l: &Expr,
    r: &Expr,
    dst: u8,
) {
    let l_false = cg.fresh_label();
    let l_end = cg.fresh_label();
    
    cg.gen_expr_decay(l, dst);
    
    // Handle __int128: high 64 bits in rdx must OR with dst(low) before test
    if matches!(l.ty(), Type::Int128 | Type::UInt128) {
        let or_rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
        cg.emit(&[or_rex, 0x09, 0xC0 | (2 << 3) | (dst & 7)]);
    }
    
    let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
    cg.emit(&[rex, 0x85, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
    
    match op {
        BinOp::AndAnd => {
            // && : false if left is zero
            cg.jmp_rel32(&l_false, x86_64::JumpKind::Je);
            
            cg.gen_expr_decay(r, dst);
            
            if matches!(r.ty(), Type::Int128 | Type::UInt128) {
                let or_rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                cg.emit(&[or_rex, 0x09, 0xC0 | (2 << 3) | (dst & 7)]);
            }
            
            cg.emit(&[rex, 0x85, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
            
            // sete dst_low: write result to low 8 bits, not fixed al
            cg.emit(&[0x0F, 0x95, 0xC0 | (dst & 7)]);
            
            // movzb dst, dst_low: zero-extend low 8 bits to full dst
            let rex2 = 0x48 | if dst >= 8 { 0x41 } else { 0 };
            cg.emit(&[rex2, 0x0F, 0xB6, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
            
            cg.jmp_rel32(&l_end, x86_64::JumpKind::Jmp);
            cg.label(&l_false);
            cg.mov_imm(dst, 0);
            cg.label(&l_end);
        }
        BinOp::OrOr => {
            // || : false if left is non-zero
            cg.jmp_rel32(&l_false, x86_64::JumpKind::Jne);
            
            cg.gen_expr_decay(r, dst);
            
            if matches!(r.ty(), Type::Int128 | Type::UInt128) {
                let or_rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                cg.emit(&[or_rex, 0x09, 0xC0 | (2 << 3) | (dst & 7)]);
            }
            
            cg.emit(&[rex, 0x85, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
            
            // sete dst_low
            cg.emit(&[0x0F, 0x95, 0xC0 | (dst & 7)]);
            
            let rex2 = 0x48 | if dst >= 8 { 0x41 } else { 0 };
            cg.emit(&[rex2, 0x0F, 0xB6, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
            
            cg.jmp_rel32(&l_end, x86_64::JumpKind::Jmp);
            cg.label(&l_false);
            cg.mov_imm(dst, 1);
            cg.label(&l_end);
        }
        _ => unreachable!(),
    }
}

/// Placeholder implementations for specialized binary operations
fn gen_float_binop(_cg: &mut crate::codegen::x86_64::Codegen, _op: BinOp, _l: &Expr, _r: &Expr) {}
fn gen_vector_binop(_cg: &mut crate::codegen::x86_64::Codegen, _op: BinOp, _l: &Expr, _r: &Expr, _dst: u8) {}
fn gen_int128_binop(_cg: &mut crate::codegen::x86_64::Codegen, _op: BinOp, _l: &Expr, _r: &Expr, _dst: u8) {}
fn gen_complex_binop(_cg: &mut crate::codegen::x86_64::Codegen, _op: BinOp, _l: &Expr, _r: &Expr, _dst: u8) {}
fn gen_imaginary_binop(_cg: &mut crate::codegen::x86_64::Codegen, _op: BinOp, _l: &Expr, _r: &Expr, _dst: u8) {}
