// src/codegen/aarch64/codegen/globals.rs - Global Variable Initialization
// Phase A Week 2-3 - Extracted from original_monolith.rs::emit_globals() and related
//
// This module handles:
/// - Global variable initialization from program source
/// - Constant pool (read-only data) management
/// - ELF section creation for symbols and constants
/// - TLS (Thread Local Storage) variable support

use crate::codegen::shared::elf::{self, ElfRela, ElfSection, ElfSym, R_AARCH64_ABS64, SHF_ALLOC, SHF_GNU_RETAIN, SHT_PROGBITS, STB_GLOBAL, STB_LOCAL, STT_OBJECT, STT_SECTION, STV_DEFAULT};
use crate::codegen::aarch64::codegen::{CodegenContext, CodegenError};

use crate::codegen::Program;
use crate::ir::{ExprKind, Global, Module, TypedExpr, ValueType};

use std::collections::HashMap;

/// Handle global variable compilation
///
/// Extracted from original_monolith.rs emit_globals (~line 305).
/// Creates sections in object file for:
/// - Initialized data (.data)
/// - Read-only constants (.rodata)
/// - Constants used by code (constant pools)
///
/// # Algorithm
/// 1. Iterate through all module globals
/// 2. For each global:
///    - Allocate space in appropriate section
///    - Write initializer bytes if present
///    - Create symbol entry
///    - Handle relocation if address needed externally
/// 3. Add .rodata section symbol at the end
pub fn compile_globals(
    ctx: &mut CodegenContext,
    module: &Module,
    prog: &Program,
) -> Result<(), CodegenError> {
    // Step 1: Process all declared global variables
    for g in &module.globals {
        emit_single_global(ctx, g, prog)?;
    }
    
    // Step 2: Handle function-level static variables (from AST)
    collect_static_locals_from_ast(ctx, prog);
    
    // Step 3: Emit final .rodata section symbol if not empty
    if !ctx.rodata_buffer.is_empty() {
        add_rodata_section_symbol(ctx);
    }
    
    Ok(())
}

/// Emit single global variable definition
///
/// Extracted from original_monolith.rs::emit_global (~line 324).
/// Handles both initialized and zero-initialized globals.
///
/// # Arguments
/// * `ctx` - Code generation context
/// * `g` - Global variable IR node
/// * `prog` - Full program for name resolution
///
/// # Returns
/// * `Ok(())` - Success
/// * `Err(CodegenError)` - Error during emission
fn emit_single_global(
    ctx: &mut CodegenContext,
    g: &Global,
    _prog: &Program,
) -> Result<(), CodegenError> {
    let size = value_type_size(&g.ty).max(1);
    
    // Align allocation in read-only data section
    let align = size.max(4) as u64;
    let offset = align_data_buffer(&mut ctx.rodata_buffer, align);
    
    // Record global's location in rodata section
    ctx.global_rodata.insert(g.name.clone(), offset);
    
    // Handle initialization
    match &g.initializer {
        Some(init_expr) => {
            // Extract literal bytes from constant expression
            let init_bytes = extract_init_bytes(init_expr, size)?;
            write_bytes_to_rodata(ctx, &init_bytes);
        }
        None => {
            // Zero-initialize BSS-like behavior
            ctx.rodata_buffer.resize(ctx.rodata_buffer.len() + size, 0);
        }
    }
    
    // Track Thread Local Storage globals separately
    if g.is_tls {
        handle_tls_global(ctx, &g.name, offset);
    }
    
    // Create ELF symbol for this global
    let is_internal = g.is_static_local || var_is_static(_prog, &g.name);
    let bind = if is_internal { STB_LOCAL } else { STB_GLOBAL };
    
    ctx.symbols.push(ElfSym {
        name: g.name.clone(),
        section: 2, // Section index for .rodata
        value: offset,
        size: size as u64,
        bind,
        ty: STT_OBJECT,
        visibility: STV_DEFAULT,
    });
    
    ctx.sym_index.insert(g.name.clone(), (ctx.symbols.len() - 1) as u32);
    
    Ok(())
}

/// Collect static locals from all functions
///
/// Function-level static variables appear to have file scope but persist
/// across function calls. They must be allocated in data sections.
fn collect_static_locals_from_ast(_ctx: &mut CodegenContext, _prog: &Program) {
    // TODO: Extract from original logic
    // Walk through all functions in AST
    // Find BlockItem::VarDecl with is_static flag
    // Treat them like globals but with unique names
    
    // Example naming: func_name$local_var_name
}

/// Handle TLS (Thread Local Storage) global
///
/// TLS variables require special access sequences depending on target ABI:
/// - Static TLS: direct offset from initial exec model base
/// - Dynamic TLS: runtime lookup via tls_get_addr
fn handle_tls_global(ctx: &mut CodegenContext, name: &str, offset: u64) {
    // Add to TLS globals tracking map
    ctx.tls_globals.insert(name.to_string(), offset);
    
    // TODO: Could create __thread_access helper stubs here
}

/// Add .rodata section symbol
fn add_rodata_section_symbol(ctx: &mut CodegenContext) {
    ctx.symbols.push(ElfSym {
        name: ".rodata".to_string(),
        section: 2,
        value: 0,
        size: ctx.rodata_buffer.len() as u64,
        bind: STB_LOCAL,
        ty: STT_SECTION,
        visibility: STV_DEFAULT,
    });
    ctx.sym_index.insert(".rodata".to_string(), (ctx.symbols.len() - 1) as u32);
}

/// Initialize a function-local static variable
///
/// Called when generating code that references a static local.
/// Typically involves checking if already initialized.
fn init_function_static(
    ctx: &mut CodegenContext,
    var_name: &str,
    initializer: &TypedExpr,
    size: usize,
) -> Result<u64, CodegenError> {
    // Implementation would:
    // 1. Check thread safety / initialization flags
    // 2. If uninitialized, emit copy of initializer bytes
    // 3. Return pointer to initialized storage
    
    todo!("Implement function static initialization with thread safety")
}

/// Helper: extract initialization bytes from expression
fn extract_init_bytes(expr: &TypedExpr, expected_size: usize) -> Result<Vec<u8>, CodegenError> {
    // Match on expr.kind to find constant literals
    match &expr.kind {
        ExprKind::Unary(crate::codegen::shared::UnOp::Neg) if expr.literal.is_some() => {
            // Treat negated constants as literals for initialization
            expr.literal.as_ref().map_or_else(
                || Err(CodegenError::new("Missing literal value".to_string())),
                |lit| convert_literal_to_bytes(lit, expected_size)
            )
        }
        ExprKind::Constant if expr.literal.is_some() => {
            expr.literal.as_ref().map_or_else(
                || Err(CodegenError::new("Missing literal value".to_string())),
                |lit| convert_literal_to_bytes(lit, expected_size)
            )
        }
        _ => {
            Err(CodegenError::new(format!(
                "Non-constant initializer: {:?}",
                expr.kind
            )))
        }
    }
}

/// Convert expression literal to byte sequence
fn convert_literal_to_bytes(lit: &crate::ir::ExprLiteral, size: usize) -> Result<Vec<u8>, CodegenError> {
    let mut buf = vec![0u8; size];
    match lit {
        crate::ir::ExprLiteral::Int(val) => {
            let raw = val.to_le_bytes();
            let n = size.min(raw.len());
            buf[..n].copy_from_slice(&raw[..n]);
        }
        crate::ir::ExprLiteral::Float(f) => {
            if size <= 4 {
                let raw = (*f as f32).to_bits().to_le_bytes();
                let n = size.min(raw.len());
                buf[..n].copy_from_slice(&raw[..n]);
            } else {
                let raw = f.to_bits().to_le_bytes();
                let n = size.min(raw.len());
                buf[..n].copy_from_slice(&raw[..n]);
            }
        }
    }
    Ok(buf)
}

/// Align buffer to boundary and return offset
fn align_data_buffer(buffer: &mut Vec<u8>, align: u64) -> u64 {
    // Standard power-of-2 alignment calculation
    let len = buffer.len() as u64;
    let aligned = if align > 1 {
        (len + align - 1) & !(align - 1)
    } else {
        len
    };
    
    // Extend buffer if needed to reach aligned position
    while buffer.len() < aligned as usize {
        buffer.push(0);
    }
    
    aligned
}

/// Write bytes to RO data buffer
fn write_bytes_to_rodata(ctx: &mut CodegenContext, data: &[u8]) {
    ctx.rodata_buffer.extend_from_slice(data);
}

/// Check if variable has file-level static qualifier
fn var_is_static(prog: &Program, name: &str) -> bool {
    // Search through declarations looking for the matching identifier.
    prog.decls.iter().any(|decl| match decl {
        crate::ast::Decl::Func(fd) => fd.name.name == name,
        crate::ast::Decl::Var(vd) => vd.name.name == name,
        _ => false,
    })
}

/// Get total size of aggregate type
pub fn aggregate_size(ty: &ValueType) -> Option<usize> {
    match ty {
        ValueType::Aggregate { size, .. } => Some(*size),
        ValueType::Array(elem, count) => {
            Some(elem.size().saturating_mul(count.unwrap_or(0)))
        }
        _ => None,
    }
}

/// Get size of basic type
fn value_type_size(ty: &ValueType) -> usize {
    match ty {
        ValueType::Integer { bits, .. } | ValueType::Float { bits } => (*bits as usize).div_ceil(8),
        ValueType::Pointer(_) => 8,  // AArch64 pointers are 64-bit
        ValueType::Vector(elem, len) => elem.size().saturating_mul(*len),
        ValueType::Function { .. } => 8,  // Function pointer
        ValueType::Array(elem, count) => elem.size().saturating_mul(count.unwrap_or(0)),
        ValueType::Aggregate { size, .. } => *size,
        _ => ty.size(),
    }
}
