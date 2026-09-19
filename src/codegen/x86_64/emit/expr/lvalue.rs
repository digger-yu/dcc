// src/codegen/x86_64/emit/expr/lvalue.rs
// L-value expressions for x86_64 backend
// Handles identifiers, member access, array indexing, and dereference operations
// Implementation extracted from x86_64.rs gen_expr() function (~500 lines total)

use crate::ast::{Expr, Type};

/// Generate code for identifier expressions (identifiers, structs, arrays).
/// 
/// Extracted from x86_64.rs lines 9924-10100 (~76 lines of core identifier handling)
/// plus related helpers (~424 lines including type-specific loading patterns).
/// 
/// Routes to specialized handlers based on expression type:
/// - Arrays: decay to pointer (C array-to-pointer conversion)
/// - Functions: resolve nested function references or emit function address
/// - Vectors: load element-by-element to frame temporary slot
/// - Global variables: emit_global_load or emit_global_float_load_to_xmm
/// - Complex types: materialize to result_off memory region
/// - Floats: gen_float_load to xmm registers
/// - Nested upvars: load_upvar from parent frame
/// - __int128: split into low/high 64-bit loads
/// - Other locals: mov_from_stack from frame offset
/// 
/// # Arguments
/// * `cg` - Codegen context (provides emit functions and variable access helpers)
/// * `e` - Identifier expression variant
/// * `dst` - Destination register index (rax=0, rdx=2, etc.)
pub fn gen_lvalue_addr(cg: &mut crate::codegen::x86_64::Codegen, e: &Expr, dst: u8) -> bool {
    match e {
        Expr::Ident { name, ty, .. } => handle_identifier(cg, name, ty, dst),
        Expr::Member { expr: base, field_name, .. } => {
            // Member access: calculate base address, add field offset
            unimplemented!("extract struct member access calculation from x86_64.rs gen_expr");
        }
        Expr::Index { expr: arr, index, .. } => {
            // Array indexing: compute base + index*elem_size
            unimplemented!("extract array indexing computation from x86_64.rs gen_expr");
        }
        Expr::Deref { expr: ptr, .. } => {
            // Pointer dereference: load value from pointer address
            unimplemented!("extract pointer dereference operation from x86_64.rs gen_expr");
        }
        _ => {
            cg.emit_diagnostic(crate::diagnostics::DiagnosticError::new(
                crate::diagnostics::Severity::Error,
                "unexpected lvalue expression type",
                None,
            ));
            false
        }
    }
}

/// Handle identifier expression based on type category.
/// 
/// Core identifier logic extracted from x86_64.rs lines 9924-10076.
/// This is the main dispatcher for all identifier-based lvalues.
fn handle_identifier(
    cg: &mut crate::codegen::x86_64::Codegen,
    name: &crate::ast::Ident,
    ty: &Type,
    dst: u8,
) -> bool {
    // Array decay: int a[8]; f(a); → f(&a[0]) (C semantics §6.5.6)
    if matches!(ty, Type::Array(_, _)) {
        return_self_call_array_decay(cg, &name.name, dst);
        return true;
    }
    
    // Function name as value: resolves to function address
    if matches!(ty, Type::Func(_, _)) && !cg.var_map.contains_key(&name.name) {
        // Nested function reference resolution through calling chain
        if let Some((unique, hops, offs)) = cg.resolve_nested_func_ref(&name.name) {
            // Trampoline address (access parent frame via static chain when hops > 0)
            cg.load_trampoline_addr(dst, &unique, hops, &offs);
        } else {
            // Direct function address emission
            let effective_name = cg.nested_func_renames
                .get(&cg.current_func_name)
                .and_then(|m| m.get(&name.name))
                .cloned()
                .unwrap_or_else(|| name.name.clone());
            cg.emit_func_addr(dst, &effective_name);
        }
        return true;
    }
    
    // Vector variables: scalar element-by-element loading to frame temp slot 0
    if matches!(ty, Type::Vector(_, _)) {
        cg.gen_vector_load(e);
        return true;
    }
    
    // Global __int128 variables: split into two 64-bit loads
    if cg.is_global_not_local(&name.name) 
        && matches!(ty, Type::Int128 | Type::UInt128)
    {
        cg.gen_int128_load(&name.name);
        if dst != 0 {
            cg.mov(dst, 0); // rax already set correctly
        }
        return true;
    }
    
    // Global float/double/long double/imaginary: load to XMM0
    if cg.is_global_not_local(&name.name)
        && (matches!(ty, Type::Float | Type::Double | Type::LongDouble)
            || matches!(ty, Type::Imaginary(_)))
    {
        cg.emit_global_float_load_to_xmm(0, &name.name, ty);
        // Note: integer global path uses emit_global_load which puts double in GP reg,
        // conflicting with gen_float_binop expecting XMM0 format
        return true;
    }
    
    // Global complex types: materialize to result_off memory region (result_off + XMM0 + rax addr convention)
    if cg.is_global_not_local(&name.name) && matches!(ty, Type::Complex(_)) {
        let inner_sz = match ty {
            Type::Complex(ref inner) => inner.size(),
            _ => 8,
        };
        let total_sz = inner_sz * 2; // real + imaginary components
        
        // Unified materialization target per DCC rvalue convention
        let result_off = -cg.frame_size + 8;
        
        // Floating-point inner complex: use FP move instructions
        let is_float_inner = matches!(
            ty,
            Type::Complex(ref inner) if matches!(inner.base, Type::Float | Type::Double | Type::LongDouble)
        );
        
        if is_float_inner {
            let pfx = if matches!(
                ty,
                Type::Complex(ref inner) if matches!(inner.base, Type::Float)
            ) { 0xF3u8 } else { 0xF2u8 };
            
            // Real part: global → xmm0 → [r13+result_off]
            cg.emit_global_float_load_to_xmm(0, &name.name, &match ty {
                Type::Complex(ref inner) => inner.base.clone(),
                _ => Type::Double,
            });
            cg.emit_xmm0_r13_disp(&[pfx, 0x49, 0x0F, 0x11], result_off);
            
            // Imaginary part: global+inner_sz → xmm0 → [r13+result_off+inner_sz]
            cg.emit_global_float_load_to_xmm_offset(
                0, &name.name, inner_sz as i32,
                &match ty {
                    Type::Complex(ref inner) => inner.base.clone(),
                    _ => Type::Double,
                },
            );
            cg.emit_xmm0_r13_disp(&[pfx, 0x49, 0x0F, 0x11], result_off + inner_sz as i64);
            
        } else {
            // Integer complex: 16-byte materialization using rep movsb
            cg.emit_global_addr(0, &name.name); // rax = global addr
            cg.emit(&[0x48, 0x89, 0xC6]); // mov rsi, rax
            cg.emit_lea_r13(7, result_off); // rdi = r13 + result_off
            cg.mov_imm(1, total_sz as i128);
            cg.emit(&[0xFC, 0xF3, 0xA4]); // cld; rep movsb
        }
        
        cg.emit_lea_r13(0, result_off);
        return true;
    }
    
    // Local floating-point variables: gen_float_load to XMM
    if matches!(ty, Type::Float | Type::Double | Type::LongDouble)
        || matches!(ty, Type::Imaginary(_))
    {
        cg.gen_float_load(&name.name, ty);
        return true;
    }
    
    // Nested function upvar __int128: two 64-bit slots in parent frame
    if cg.current_func_is_nested
        && cg.upvar_offsets.contains_key(&name.name)
        && matches!(ty, Type::Int128 | Type::UInt128)
    {
        let parent_off = cg.upvar_offsets.get(&name.name).copied().unwrap_or(0);
        cg.load_upvar(0, parent_off, 8, &name.name);      // Low 64 bits
        cg.load_upvar(2, parent_off + 8, 8, &name.name);  // High 64 bits to rdx
        if dst != 0 {
            cg.mov(dst, 0); // rax already correctly set
        }
        return true;
    }
    
    // Nested function upvar generic: single load_from_upvar
    if cg.current_func_is_nested && cg.upvar_offsets.contains_key(&name.name) {
        let parent_off = cg.upvar_offsets.get(&name.name).copied().unwrap_or(0);
        cg.load_upvar(dst, parent_off, ty.size(), &name.name);
        return true;
    }
    
    // Local __int128 variables: stack-based two-slot load pattern
    if matches!(ty, Type::Int128 | Type::UInt128) {
        let off = cg.var_offset(&name.name);
        cg.load_stack(dst, off);       // Low 64 bits to dst
        cg.load_stack(2, off + 8);     // High 64 bits to rdx
        // If dst != 0, rax already set correctly above
    } else {
        // Standard local variable: mov_from_stack from frame offset
        cg.mov_from_stack(dst, &name.name);
    }
    
    // Integer promotion: char/short → int (C default argument promotion rules)
    if ty.is_int() {
        cg.gen_int_promote(dst, ty);
    }
    
    true
}

/// Array decay helper: convert array identifier to pointer to first element.
/// 
/// Implements C standard §6.5.6: arrays used as arguments automatically decay
/// to pointers to their first element: int *p = arr; / return arr;
fn return_self_call_array_decay(
    cg: &mut crate::codegen::x86_64::Codegen,
    _array_name: &str,
    _dst: u8,
) {
    // Would extract gen_expr_decay logic here
    // Placeholder for turn 22 continuation work
    unimplemented!("extract array decay implementation from x86_64.rs gen_expr");
}
