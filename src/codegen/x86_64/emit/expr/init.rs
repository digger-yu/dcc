// src/codegen/x86_64/emit/expr/init.rs
// Initialization and assignment expressions for x86_64 backend
// Handles assignments, compound literals, sizeof operations
// Implementation extracted from x86_64.rs gen_expr() function (~400+ lines total)

use crate::ast::{Expr, Type};
use crate::codegen::x86_64::emit::expr::lvalue::gen_lvalue_addr;
use crate::const_eval;

/// Generate code for initialization and assignment expressions.
/// 
/// Extracted from x86_64.rs lines 10476-10950+ (~475 lines of core assignment logic)
/// plus related store operations (aggregates, complex, vectors) (~300+ lines).
/// 
/// Routes to specialized handlers based on expression type:
/// - Simple assignment (= operator): lhs = rhs evaluation pattern
/// - Compound assignment (+=, -=, *=, /=, etc.): (target op= rhs) desugaring
/// - Compound literals ({1, 2, 3}): array/struct initializer
/// - sizeof operator: compile-time constant emission
/// 
/// # Arguments
/// * `cg` - Codegen context (provides emit functions and primitives)
/// * `e` - Initialization expression variant
/// * `dst` - Destination register index (for assignment chain value propagation)
pub fn gen_init_expr(cg: &mut crate::codegen::x86_64::Codegen, e: &Expr, dst: u8) {
    match e {
        Expr::Assign { l, r, .. } => handle_assignment(cg, l, r),
        Expr::CompoundLit { ty, inits } => gen_compound_literal(cg, ty, inits.as_slice()),
        Expr::SizeOf { ty, .. } => gen_sizeof(cg, ty, dst),
        _ => {
            cg.emit_diagnostic(crate::diagnostics::Diagnostic::new(
                crate::diagnostics::Severity::Error,
                "unexpected initialization expression",
                None,
            ));
        }
    }
}

/// Handle simple and compound assignment expressions.
/// 
/// Core extraction from x86_64.rs lines 10476-10950+ covering:
/// - Compound assignment pattern detection (target op= rhs desugaring)
/// - Single-evaluation path for lvalues with side effects (*--ptr += 1)
/// - Complex number assignments with result_off materialization
/// - Vector assignments with rep movsb bulk copy
/// - Float precision conversions (int→float, float→int, float→double)
/// - Aggregate initialization (structs, unions, arrays >8 bytes)
/// - Nested upvar stores (parent frame offset calculation)
/// - __int128 dual-register handling (rdx/rax pair storage)
/// - Array/Deref element assignments via semantic address computation
fn handle_assignment(cg: &mut crate::codegen::x86_64::Codegen, lvalue: &Expr, rhs: &Expr) {
    // Detect compound assignment pattern: Assign { l: target, r: Binop { l: read, r: rhs } }
    // where target and read are structurally equal (produced by parser desugaring).
    // If lvalue has side effects (e.g., *--ptr += 1), must evaluate address only once.
    // Strategy: compute address first, save it, load from saved address, compute binop, store back.
    if let Expr::Binop { l: ref binop_l, op: binop_op, r: ref binop_r, .. } = *rhs {
        let compound_rhs: Option<&Expr> =
            if exprs_structurally_equal(lvalue, binop_l) {
                Some(binop_r.as_ref())
            } else if exprs_structurally_equal(lvalue, binop_r) {
                Some(binop_l.as_ref())
            } else {
                None
            };
        
        if let Some(compound_rhs) = compound_rhs {
            // Use single-evaluation path only when lvalue has side effects,
            // otherwise use original path (correct for all types).
            if expr_has_side_effects(lvalue) {
                cg.gen_compound_assign_single_eval(lvalue, compound_rhs, binop_op);
                return;
            }
        }
    }
    
    // Sanitizer instrumentation may wrap LHS as Comma(check, lvalue):
    // Execute check call first, then store to real lvalue; otherwise
    // pattern matching fails causing silent store drop (KASAN *p=v becomes NOP)
    let mut lv: &Expr = lvalue;
    while let Expr::Comma { l: check, r: inner, .. } = lv {
        cg.gen_expr(check, 0);
        lv = inner;
    }
    
    // 16-byte vector assignment to Member/Index/Deref lvalue:
    // Materialize RHS to vector temp slot 0, then bulk copy to LHS address
    // (All vector sizes follow this path: 8B or smaller vectors that fall into scalar
    // store leave rax as residual last loaded element value,
    // overwriting entire vector slot with single element bit pattern,
    // pr23135: res.v = i causes i2[0] to change to i2[1]'s value)
    let assign_l_ty = lv.ty();
    if matches!(assign_l_ty, Type::Vector(_, _)) && !matches!(lv, Expr::Ident { .. }) {
        cg.gen_expr(rhs, 0); // RHS → vector temp slot 0
        if gen_lvalue_addr(cg, lv, 0) {
            let toff = cg.vector_temp_base(0, assign_l_ty.size());
            cg.emit(&[0x48, 0x89, 0xC7]); // mov rdi, rax
            cg.emit_lea_r13(6, toff); // rsi = temp slot
            cg.mov_imm(1, assign_l_ty.size() as i128);
            cg.emit(&[0xFC, 0xF3, 0xA4]); // cld; rep movsb
            return;
        }
    }
    
    // Complex lvalue assignment (non-Identifier): unified materialization + component copy.
    // Below Index/Deref/Member scalar store writes fixed 8-byte rax directly:
    // Scalar RHS corrupts imaginary part (a[i] = -1 gives (-1,-1) instead of (-1,0), pr56837),
    // complex RHS uses result_off materialization address as data target.
    if matches!(assign_l_ty, Type::Complex(_)) && !matches!(lv, Expr::Ident { .. })
        && gen_lvalue_addr(cg, lv, 0) {
            cg.push(0); // save destination address (gen_expr may clobber rcx)
            cg.gen_complex_store_rcx(rhs, assign_l_ty);
            return;
        }
    
    if let Expr::Ident { name, ty: l_ty, .. } = lv {
        let sz = l_ty.size();
        // VM struct/union (with VLA fields local type) even if compile-time
        // size() fallback <= 8 still needs bulk copy: its semantic size is
        // runtime value; 8-byte scalar store would treat aggregate rvalue (address convention)
        // as content (20070919-1: b = p[2] after b.w becomes address byte)
        let is_aggregate = matches!(
            l_ty,
            Type::Struct(_, _, _, _) | Type::Union(_, _, _, _)
                | Type::Array(_, _) | Type::Atomic()
                | Type::Vector(_, _)
        ) && (sz > 8 || matches!(rhs.as_ref(), Expr::CompoundLit { .. }) || l_ty.has_vla());
        
        if is_aggregate {
            // struct/array full assignment: bulk copy per-element
            cg.gen_aggregate_assign(&name.name, l_ty, rhs);
            // Assignment expression result = lvalue-converted value (C11 6.5.16p3).
            // Aggregate rvalue unified convention: address for chained aggregate assignment
            // (a = (b = c)) and statement expression block values (20070919-1:
            // a = ({ ...; p[3] = b; })) pass-through.
            if cg.is_global_not_local(&name.name) {
                cg.emit_global_addr(0, &name.name);
            } else if let Some(&off) = cg.vla_offsets.get(&name.name) {
                // VM struct/VLA variables: address = data region base
                cg.load_stack(0, off);
            } else {
                let aoff = cg.var_offset(&name.name);
                cg.emit_lea_r13(0, aoff);
            }
        } else if matches!(l_ty, Type::Complex(_)) {
            // Complex assignment: unified bulk copy. 8-byte rax single-slot path
            // cannot express float complex rvalue (value in xmm0, rax is address/residual)
            cg.gen_aggregate_assign(&name.name, l_ty, rhs);
        } else {
            // Standard scalar assignment
            cg.gen_expr_decay(rhs, 0); // array decay: int *p = arr
            
            // Implicit float→int conversion when RHS is float but LHS is integer (e.g., `a = 0.0;`).
            let r_ty = rhs.ty();
            if matches!(&r_ty, Type::Float | Type::Double | Type::LongDouble) && cg.is_integer_type(l_ty) {
                cg.emit_implicit_float_to_int(0, &r_ty, l_ty);
            }
            
            // Implicit float→float precision conversion when both sides are float but different precision (e.g., `float f = double_expr;`).
            // Without cvtsd2ss, movss would store raw lower 32 bits of double, not float representation.
            if matches!(l_ty, Type::Float | Type::Double | Type::LongDouble) && matches!(&r_ty, Type::Float | Type::Double | Type::LongDouble) {
                cg.emit_float_precision_convert(&r_ty, l_ty);
            }
            
            // Implicit int→float conversion when RHS is integer but LHS is float (e.g., `ping = 0;`).
            // Integer result sits in eax; convert to xmm0 before gen_float_store.
            if matches!(l_ty, Type::Float | Type::Double | Type::LongDouble) && cg.is_integer_type(&r_ty) {
                cg.emit_scalar_to_xmm0_if_needed(rhs, l_ty);
            }
            
            // Assignment expression result is lvalue-converted value (C11 6.5.16p3),
            // not original RHS value: `ul = us = -1` means inner us=-1 result must
            // truncate to unsigned short (65535) then propagate to outer, otherwise
            // untruncated 64-bit -1 in dst passed as inner expression result,
            // ul gets 0xFFFFFFFFFFFFFFFF.
            // Only narrow-than-4-byte integer leftvalues need re-extension: 4/8-byte slots by
            // mov_from_stack/emit_global_store read at declared width,
            // lower bits already contain correct value; float RHS in xmm0 bypasses dst.
            let r_ty = rhs.ty();
            let l_sz = l_ty.size();
            if l_sz < 4 && (cg.is_integer_type(&r_ty) || matches!(r_ty, Type::Enum(_, _, _))) {
                // Re-extension direction determined by leftvalue type signedness (char uses signed,
                // fixed underlying type enum uses underlying type). Zero/sign-extend dst low 8/16 bits
                // to 64-bit: signed movsx, unsigned movzx.
                let l_signed = match l_ty {
                    Type::Enum(_, _, Some(u)) => cg.is_signed_int(&u.base),
                    Type::Bool | Type::UChar | Type::UShort => false,
                    _ => true,
                };
                
                let rex = 0x48 | if 0 >= 8 { 0x05 } else { 0 };
                let modrm = 0xC0 | ((0 & 7) << 3) | (0 & 7);
                match (l_sz, l_signed) {
                    (1, true) => cg.emit(&[rex, 0x0F, 0xBE, modrm]),
                    (1, false) => cg.emit(&[rex, 0x0F, 0xB6, modrm]),
                    (2, true) => cg.emit(&[rex, 0x0F, 0xBF, modrm]),
                    (2, false) => cg.emit(&[rex, 0x0F, 0xB7, modrm]),
                    _ => {}
                }
            }
            
            if matches!(l_ty, Type::Int128 | Type::UInt128) {
                // When RHS not __int128, rdx uninitialized; need zero/sign extension
                let r_ty_for_128 = rhs.ty();
                if !matches!(r_ty_for_128, Type::Int128 | Type::UInt128) {
                    if matches!(l_ty, Type::UInt128) || matches!(r_ty_for_128, Type::Bool | Type::UChar | Type::UShort | Type::UInt | Type::ULong | Type::ULongLong) {
                        cg.emit(&[0x48, 0x31, 0xD2]); // xor rdx, rdx
                    } else {
                        cg.emit(&[0x48, 0x99]); // cqo (sign-extend rax→rdx:rax)
                    }
                }
                
                if cg.is_global_not_local(&name.name) {
                    cg.gen_int128_store(&name.name);
                } else if cg.current_func_is_nested && cg.upvar_offsets.contains_key(&name.name) {
                    let parent_off = cg.upvar_offsets.get(&name.name).copied().unwrap_or(0);
                    cg.store_upvar(0, parent_off, 8, &name.name);
                    cg.store_upvar(2, parent_off + 8, 8, &name.name);
                } else {
                    let off = cg.var_offset(&name.name);
                    cg.store_stack(0, off);
                    cg.store_stack(2, off + 8);
                }
            } else if cg.is_global_not_local(&name.name) {
                if matches!(l_ty, Type::Vector(_, _)) {
                    cg.gen_vector_store(&name.name);
                } else if matches!(l_ty, Type::Float | Type::Double | Type::LongDouble) || matches!(l_ty, Type::Imaginary(_)) {
                    // Float global: value in xmm0, use movsd/movss store
                    cg.emit_global_float_store(&name.name, l_ty);
                } else {
                    cg.emit_global_store(0, &name.name);
                }
            } else if cg.current_func_is_nested && cg.upvar_offsets.contains_key(&name.name) {
                let parent_off = cg.upvar_offsets.get(&name.name).copied().unwrap_or(0);
                cg.store_upvar(0, parent_off, l_ty.size(), &name.name);
            } else if matches!(l_ty, Type::Vector(_, _)) {
                cg.gen_vector_store(&name.name);
            } else if matches!(l_ty, Type::Float | Type::Double | Type::LongDouble) || matches!(l_ty, Type::Imaginary(_)) {
                cg.gen_float_store(&name.name, l_ty);
            } else {
                cg.mov_to_stack(0, &name.name);
            }
        }
    } else if let Expr::Index { arr, idx, ty: elem_ty, .. } = lv {
        // Array element assignment: arr[i] = val
        // First try semantic layer path to compute address
        use crate::semantics::{compute_lvalue_address, AddressContext, MaterializeAction};
        let ctx = AddressContext { is_nested: cg.current_func_is_nested };
        
        // Nested runtime index: fallback to old path (semantic layer would lose second runtime offset)
        let is_nested_runtime_index = matches!(arr.as_ref(), Expr::Index { .. })
            && const_eval::eval(idx).is_err()
            && match arr.as_ref() {
                Expr::Index { idx: inner_idx, .. } => const_eval::eval(inner_idx).is_err(),
                _ => false,
            };
        
        let desc = if !is_nested_runtime_index {
            Some(compute_lvalue_address(cg, lv, &ctx))
        } else {
            None
        };
        
        let es = elem_ty.size() as i32;
        
        // Aggregate element assignment (VM struct / >8B struct): rep movsb bulk copy.
        // Scalar store would treat aggregate rvalue (address convention)
        // or 8-byte content truncatedly written, semantics both wrong (20070919-1)
        let is_agg_elem = matches!(elem_ty, Type::Struct(_, _, _, _) | Type::Union(_, _, _, _) | Type::Array(_) | Type::Atomic()) && (es > 8 || elem_ty.has_vla());
        
        if let Some(ref desc) = desc {
            if is_agg_elem {
                // Aggregate elements: semantic layer computes address to rax, then push + gen_agg_elem_store
                if cg.materialize_address(desc, MaterializeAction::TakeAddress, Some(idx), lv, Some(arr), 0) {
                    cg.push(0); // save addr
                    cg.gen_agg_elem_store(rhs, elem_ty, es);
                    return;
                }
            } else if matches!(elem_ty, Type::Complex(ref c) if matches!(&c.base, Type::Float | Type::Double | Type::LongDouble)) {
                // Complex float element: semantic layer computes address to rax, then store_complex_at_rcx
                if cg.materialize_address(desc, MaterializeAction::TakeAddress, Some(idx), lv, Some(arr), 0) {
                    cg.push(0); // save addr
                    cg.gen_expr(rhs, 0); // rax = val (complex)
                    cg.pop(1); // rcx = addr
                    if let Type::Complex(ref cinner) = elem_ty {
                        cg.store_complex_at_rcx(rhs, &cinner.base);
                    }
                    return;
                }
            } else if matches!(elem_ty, Type::Float | Type::Double | Type::LongDouble) {
                // Float element: semantic layer computes address, then store
                if cg.materialize_address(desc, MaterializeAction::TakeAddress, Some(idx), lv, Some(arr), 0) {
                    cg.push(0); // save addr
                    cg.gen_expr(rhs, 0); // rax = val
                    cg.pop(1); // rcx = addr
                    cg.emit_scalar_to_xmm0_if_needed(rhs, elem_ty);
                    if matches!(elem_ty, Type::Float) {
                        cg.emit(&[0xF3, 0x0F, 0x11, 0x01]); // movss [rcx], xmm0
                    } else {
                        cg.emit(&[0xF2, 0x0F, 0x11, 0x01]); // movsd [rcx], xmm0
                    }
                    return;
                }
            } else {
                // Scalar element: semantic layer computes address, then store
                if cg.materialize_address(desc, MaterializeAction::TakeAddress, Some(idx), lv, Some(arr), 0) {
                    cg.push(0); // save addr
                    cg.gen_expr(rhs, 0); // rax = val
                    cg.pop(1); // rcx = addr
                    match es {
                        1 => cg.emit(&[0x88, 0x01]),
                        2 => cg.emit(&[0x66, 0x89, 0x01]),
                        4 => cg.emit(&[0x89, 0x01]),
                        _ => cg.emit(&[0x48, 0x89, 0x01]),
                    }
                    return;
                }
            }
        }
        
        // Fallback: old path (used when semantic layer cannot handle)
        if let Expr::Ident { name: arr_name, ty: arr_ty, .. } = arr.as_ref() {
            // Unified base address computation (fix BUG-C: use resolve_global_key for static local)
            cg.emit_ident_base_addr(&arr_name.name, arr_ty, 0);
            cg.push(0); // save base address (gen_expr(idx) may clobber rax)
            cg.gen_expr(idx, 1); // rcx = index
            cg.pop(0); // restore base to rax
            cg.emit_index_scale(es, arr, elem_ty);
            cg.emit(&[0x48, 0x01, 0xC8]); // add rax, rcx (addr in rax)
            cg.push(0);
            cg.gen_expr(rhs, 0);
            cg.pop(1);
            if is_agg_elem {
                cg.gen_agg_elem_store(rhs, elem_ty, es);
                return;
            }
            // Complex float element: write components separately (real [rcx], imag [rcx+isz]). If falls to below integer store, rax residual (like conj flip mask) covers real part.
            if matches!(elem_ty, Type::Complex(ref c) if matches!(&c.base, Type::Float | Type::Double | Type::LongDouble)) {
                if let Type::Complex(ref cinner) = elem_ty {
                    cg.store_complex_at_rcx(rhs, &cinner.base);
                }
            } else if matches!(elem_ty, Type::Float | Type::Double | Type::LongDouble) {
                cg.emit_scalar_to_xmm0_if_needed(rhs, elem_ty);
                if matches!(elem_ty, Type::Float) {
                    cg.emit(&[0xF3, 0x0F, 0x11, 0x01]); // movss [rcx], xmm0
                } else {
                    cg.emit(&[0xF2, 0x0F, 0x11, 0x01]); // movsd [rcx], xmm0
                }
            } else {
                match es {
                    1 => cg.emit(&[0x88, 0x01]),
                    2 => cg.emit(&[0x66, 0x89, 0x01]),
                    4 => cg.emit(&[0x89, 0x01]),
                    _ => cg.emit(&[0x48, 0x89, 0x01]),
                }
            }
        } else {
            // Generic path: base from arbitrary lvalue/pointer expression (e.g., s.i[k] = v)
            let arr_ty = arr.ty();
            if matches!(arr_ty, Type::Array(_) | Type::Vector(_, _)) {
                if !gen_lvalue_addr(cg, arr, 0) {
                    cg.gen_expr(arr, 0);
                }
            } else {
                cg.gen_expr(arr, 0);
            }
            cg.push(0);
            cg.gen_expr(idx, 1);
            cg.pop(0);
            cg.emit_index_scale(es, arr, elem_ty);
            cg.emit(&[0x48, 0x01, 0xC8]); // add rax, rcx (addr in rax)
            cg.push(0);
            cg.gen_expr(rhs, 0);
            cg.pop(1);
            if is_agg_elem {
                cg.gen_agg_elem_store(rhs, elem_ty, es);
                return;
            }
            // Complex float element: write components separately (same as above, prevents integer store from writing rax residual to real part)
            if matches!(elem_ty, Type::Complex(ref c) if matches!(&c.base, Type::Float | Type::Double | Type::LongDouble)) {
                if let Type::Complex(ref cinner) = elem_ty {
                    cg.store_complex_at_rcx(rhs, &cinner.base);
                }
            } else if matches!(elem_ty, Type::Float | Type::Double | Type::LongDouble) {
                cg.emit_scalar_to_xmm0_if_needed(rhs, elem_ty);
                if matches!(elem_ty, Type::Float) {
                    cg.emit(&[0xF3, 0x0F, 0x11, 0x01]); // movss [rcx], xmm0
                } else {
                    cg.emit(&[0xF2, 0x0F, 0x11, 0x01]); // movsd [rcx], xmm0
                }
            } else {
                match es {
                    1 => cg.emit(&[0x88, 0x01]),
                    2 => cg.emit(&[0x66, 0x89, 0x01]),
                    4 => cg.emit(&[0x89, 0x01]),
                    _ => cg.emit(&[0x48, 0x89, 0x01]),
                }
            }
        }
    } else if let Expr::Deref { e: ptr_e, .. } = lv {
        // Pointer dereference assignment: *ptr = val
        let seg_prefix = ptr_seg_prefix(&ptr_e.ty());
        let is_deref_aggregate = matches!(lv.ty(), Type::Struct(_, _, _, _) | Type::Union(_, _, _, _) | Type::Array(_)) && (lv.ty().size() > 8 || lv.ty().has_vla());
        if is_deref_aggregate {
            // Aggregate *ptr assignment: use rep movsb
            // to correctly handle packed/misaligned structs.
            let agg_sz = lv.ty().size();
            cg.gen_expr(ptr_e, 0); // Get pointer address to rax
            cg.push(0); // save dest addr
            cg.gen_expr(rhs, 1); // rhs to rcx (temp vec slot)
            cg.emit_lea_r13(6, -agg_sz as i32 + 8); // rsi = temp area
            cg.emit(&[0x48, 0x89, 0xC7]); // mov rdi, rax
            cg.mov_imm(1, agg_sz as i128);
            cg.emit(&[0xFC, 0xF3, 0xA4]); // cld; rep movsb
            cg.pop(0); // restore
            return;
        } else {
            cg.gen_expr(ptr_e, 0);
            cg.push(0);
            cg.gen_expr(rhs, 0);
            cg.pop(1);
            let es = lv.ty().size();
            match es {
                1 => cg.emit(&[0x88, 0x01]),
                2 => cg.emit(&[0x66, 0x89, 0x01]),
                4 => cg.emit(&[0x89, 0x01]),
                _ => cg.emit(&[0x48, 0x89, 0x01]),
            }
            return;
        }
    }
}

/// Store value from RAX to memory pointed to by RDI
fn store_to_memory_at_rdi(cg: &mut crate::codegen::x86_64::Codegen, ty: &Type) {
    match ty.size() {
        1 => {
            // BYTE store: mov [rdi], al
            cg.emit(&[0x88, 0x07]); // mov byte ptr [rdi], al
        }
        2 => {
            // WORD store: mov [rdi], ax
            cg.emit(&[0x66, 0x89, 0x07]); // mov word ptr [rdi], ax
        }
        4 => {
            // DWORD store: mov [rdi], eax
            cg.emit(&[0x89, 0x07]); // mov dword ptr [rdi], eax
        }
        _ => {
            // QWORD or larger: use REP MOVSB for complex types
            let size = ty.size();
            cg.mov_imm(2, size as i128); // rcx = size
            cg.emit(&[0xFC, 0xF3, 0xA5]); // cld; rep movsd/qword
        }
    }
}

/// Generate compound literal initializer ({1, 2, 3})
fn gen_compound_literal(cg: &mut crate::codegen::x86_64::Codegen, ty: &Type, inits: &[Expr], dst: u8) {
    // Allocate stack space for compound literal
    let size = ty.size();
    
    // Calculate frame offset (would be negative from frame_size)
    let offset = -cg.frame_size() + 8; // Placeholder calculation
    
    // Initialize each element
    for (i, init) in inits.iter().enumerate() {
        let init_offset = offset + (i * 8) as i64;
        
        // Evaluate initializer
        cg.gen_expr(init, 0); // → rax
        
        // Store to allocated slot
        cg.store_rsp_qword(0, init_offset as usize);
    }
    
    // Return pointer to compound literal
    cg.emit_lea_r13(dst, offset);
}

/// Generate sizeof operation
fn gen_sizeof(cg: &mut crate::codegen::x86_64::Codegen, ty: &Type, dst: u8) {
    let size = ty.size() as i64;
    
    // Load constant value into destination register
    if size <= i32::MAX as i64 {
        // Small constants: mov reg, imm32
        cg.mov_imm(dst, size as i128);
    } else {
        // Large constants: use immediate load
        cg.mov_imm(dst, size);
    }
}
