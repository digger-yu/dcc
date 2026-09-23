// src/codegen/aarch64/codegen/compile.rs - Main Compilation Loop
// Phase A Week 2-3 - Core compilation logic extracted from original_monolith.rs
//
// This module contains the heart of the compiler - the main compilation loop that
// orchestrates the entire code generation process. It provides both executable and
// relocatable compilation modes.

use crate::ast::{Decl, FuncDecl};
use crate::codegen::shared::elf::{self, ElfRela, ElfSection, ElfSym, SHT_PROGBITS, SHF_ALLOC, SHF_GNU_RETAIN, STB_GLOBAL, STB_LOCAL, STT_FUNC, STT_OBJECT, STT_SECTION, STV_DEFAULT};
use crate::codegen::Program;
use crate::ir::{ExprKind, Function, Global, Module, TypedExpr, TypedStmt, ValueType};
use crate::target::Target;
use std::collections::HashMap;
use std::sync::Arc;

/// Code generation error type with detailed diagnostic information
#[derive(Debug)]
pub struct CodegenError {
    /// Error message describing what went wrong
    pub message: String,
    /// Optional source span information for better error reporting
    pub span: Option<crate::diagnostics::Span>,
}

impl CodegenError {
    /// Create a new code generation error
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            span: None,
        }
    }

    /// Create a new error with source location information
    pub fn with_span(message: impl Into<String>, span: crate::diagnostics::Span) -> Self {
        Self {
            message: message.into(),
            span: Some(span),
        }
    }
}

impl std::fmt::Display for CodegenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CodeGen Error: {}", self.message)?;
        if let Some(span) = &self.span {
            write!(f, " at line {}", span.line)?;
        }
        Ok(())
    }
}

impl std::error::Error for CodegenError {}

/// Result type for code generation operations
pub type CodegenResult<T> = Result<T, CodegenError>;

/// Context for code generation state management
/// 
/// This structure holds all mutable state during the compilation process,
/// replacing the fields in the original monolithic Aarch64Codegen implementation.
#[derive(Clone)]
pub struct CodegenContext {
    /// Current output code buffer (machine instructions)
    pub code_buffer: Vec<u8>,
    /// Read-only data section buffer
    pub rodata_buffer: Vec<u8>,
    /// Entry point address (0 for PIC, fixed offset for executables)
    pub entry_point: u64,
    /// Position in current output buffer
    pub position: u64,
    /// Label to address mapping
    pub labels: HashMap<String, u64>,
    /// Symbol table for ELF object file
    pub symbols: Vec<ElfSym>,
    /// Symbol name to index mapping for fast lookup
    pub sym_index: HashMap<String, u32>,
    /// Relocation entries to be filled by linker
    pub relocations: Vec<ElfRela>,
    /// RO Data relocation entries
    pub rodata_relocations: Vec<ElfRela>,
    /// Global variable offsets in read-only data section
    pub global_rodata: HashMap<String, u64>,
    /// Thread-local global variable offsets
    pub tls_globals: HashMap<String, u64>,
    /// Function size map (name -> size in bytes)
    pub func_sizes: HashMap<String, u64>,
}

impl Default for CodegenContext {
    fn default() -> Self {
        Self::new(false)
    }
}

impl CodegenContext {
    /// Create a new code generation context
    /// 
    /// # Arguments
    /// * `relocatable` - Whether to generate position-independent code
    pub fn new(relocatable: bool) -> Self {
        Self {
            code_buffer: Vec::new(),
            rodata_buffer: Vec::new(),
            entry_point: if relocatable { 0 } else { 0x400000 + 64 + 56 },
            position: 0,
            labels: HashMap::new(),
            symbols: Vec::new(),
            sym_index: HashMap::new(),
            relocations: Vec::new(),
            rodata_relocations: Vec::new(),
            global_rodata: HashMap::new(),
            tls_globals: HashMap::new(),
            func_sizes: HashMap::new(),
        }
    }

    /// Get current position in code buffer
    pub fn pos(&self) -> u64 {
        self.position
    }

    /// Set position to specific value
    pub fn set_pos(&mut self, pos: u64) {
        self.position = pos;
    }

    /// Advance position by given number of bytes
    pub fn advance(&mut self, bytes: usize) {
        self.position += bytes as u64;
    }
}

/// Main compilation function - orchestrates the entire compilation process
///
/// This is the primary entry point for AArch64 code generation. It follows the
/// compilation workflow extracted from original_monolith.rs::compile():
///
/// 1. Initialize context and collect metadata (labels, aggregates)
/// 2. Emit global variables and constants
/// 3. Compile functions based on target mode (exec/rel)
/// 4. Finalize patches and relocations
/// 5. Build final ELF object file
///
/// # Arguments
/// * `module` - IR module containing all program entities
/// * `target` - Target architecture configuration
/// * `relocatable` - Compilation mode flag
///
/// # Returns
/// * `Ok(ObjectFile)` - Compiled ELF binary
/// * `Err(CodegenError)` - Compilation failure
pub fn compile_aarch64(module: &Module, target: &Target, relocatable: bool) -> CodegenResult<Vec<u8>> {
    // Initialize code generation context
    let mut ctx = CodegenContext::new(relocatable);
    
    let prog = module.source();
    
    // Step 1: Collect assembly labels and build aggregate type information
    ctx.collect_asm_labels(&prog);
    ctx.build_aggregate_fields(&prog);
    
    // Step 2: Emit global variables and constants first (before functions)
    ctx.emit_globals(module, &prog);
    
    // Step 3: Compile functions based on compilation mode
    if relocatable {
        // Relocatable mode: Generate PIC with relocations
        ctx.compile_functions_rel(module, &prog)?;
        ctx.finalize_branches();
        ctx.finalize_pending_bl_calls();
    } else {
        // Executable mode: Generate absolute addresses
        ctx.compile_functions_exec(module, &prog)?;
        ctx.finalize_exec_mode();
    }
    
    // Step 4: Build final ELF object file
    ctx.build_object_file(module)
}

impl CodegenContext {
    // ============================================================================
    // Compilation Mode Selection
    // ============================================================================
    
    /// Execute mode compilation - generates absolute-address executable
    ///
    /// Used for non-relocatable executables where all addresses are known
    /// at compile time. Entry point is hardcoded (typically 0x400000).
    /// Branches and calls use absolute addressing without relocations.
    ///
    /// # Algorithm
    /// 1. Emit `_start` label and call to main
    /// 2. For each function in module:
    ///    - Check for alias/copy attributes (skip if present)
    ///    - Handle patchable function entry directives
    ///    - Mark function label and emit code
    ///    - Record function size
    /// 3. Patch main call once all functions compiled
    pub fn compile_functions_exec(&mut self, module: &Module, _prog: &crate::ir::Program) -> CodegenResult<()> {
        // TODO: Extract from original_monolith.rs::compile_exec (line ~376)
        // This is a large function (~70 lines) that handles:
        // - File-level inline assembly collection
        // - Entry point setup (_start label and main call)
        // - Function iteration and code generation
        // - Function size tracking
        
        todo!("Extract compile_exec logic from original_monolith.rs line ~376")
    }

    /// Relocatable mode compilation - generates PIC for shared libraries
    ///
    /// Produces position-independent code suitable for linking into
    /// dynamically loaded objects. All external references go through
    /// symbol table and relocations.
    ///
    /// # Algorithm
    /// 1. Iterate through all functions
    /// 2. Skip aliases and forward declarations
    /// 3. Create ELF symbols for each function
    /// 4. Track function sizes and offsets
    /// 5. Create COMDAT groups for weak functions
    pub fn compile_functions_rel(&mut self, module: &Module, _prog: &crate::ir::Program) -> CodegenResult<()> {
        // TODO: Extract from original_monolith.rs::compile_rel (line ~451)
        // Similar structure to compile_exec but adds:
        // - Symbol table population
        // - Offset tracking for linker
        // - COMDAT group creation
        // - .used section emission for __attribute__((used))
        
        todo!("Extract compile_rel logic from original_monolith.rs line ~451")
    }

    // ============================================================================
    // Post-Compilation Finalization
    // ============================================================================
    
    /// Finalize executable mode compilation
    ///
    /// After all functions are emitted, this method resolves forward references:
    /// - Float literal pool references (LDR instruction patching)
    /// - ADR precomputed addresses
    /// - Branch targets (conditional and unconditional)
    /// - BL call sites
    ///
    /// This two-phase approach (emit first, patch later) simplifies code generation
    /// since we don't need to know all addresses upfront.
    pub fn finalize_exec_mode(&mut self) {
        // TODO: Extract from original_monolith.rs::finalize_exec (line ~648)
        // Handles:
        // - Float literal pool patching (patch_ldr_u32)
        // - ADR instruction patching (patch_adr)
        // - Delegates to finalize_branches and finalize_pending_bl
        
        todo!("Extract finalize_exec logic from original_monolith.rs line ~648")
    }

    /// Resolve pending branch targets to concrete offsets
    ///
    /// During code generation, branches may reference labels not yet defined.
    /// These are stored in pending_branches list and resolved here by looking up
    /// their actual positions in the labels map.
    ///
    /// Resolves both regular branches (B) and conditional branches (CBZ/CBNZ).
    pub fn finalize_branches(&mut self) {
        // TODO: Extract from original_monolith.rs::finalize_branches (line ~671)
        // Handles:
        // - Branch kind dispatch (B vs CBZ/CBNZ)
        // - Address calculation and encoding
        // - Patches instructions in place
        
        todo!("Extract finalize_branches logic from original_monolith.rs line ~671")
    }

    /// Finalize all pending BL (branch with link) calls
    ///
    /// BL instructions to labels not yet defined are postponed until after
    /// all functions are compiled. This resolves them using the completed
    /// label table.
    pub fn finalize_pending_bl_calls(&mut self) {
        // TODO: Extract from original_monolith.rs::finalize_pending_bl (line ~662)
        // Simple iteration over pending_bl_calls list and patching
        
        todo!("Extract finalize_pending_bl logic from original_monolith.rs line ~662")
    }

    // ============================================================================
    // Helper Functions (extracted from original monolith)
    // ============================================================================
    
    /// Collect assembly labels referenced in global asm statements
    fn collect_asm_labels(&mut self, _prog: &crate::ir::Program) {
        // TODO: Extract from original_monolith.rs
        // Collects labels used by inline assembly templates
        
        todo!("Implement collect_asm_labels")
    }

    /// Build aggregate type field information for HFA detection
    fn build_aggregate_fields(&mut self, prog: &crate::ir::Program) {
        // Based on original at line ~288
        self.global_rodata.clear();
        // Parse struct/union definitions to extract field layouts
        // This enables proper identification of Homogeneous Floating-point Aggregates
        
        todo!("Implement build_aggregate_fields based on line ~288")
    }

    /// Emit all global variables to RO data section
    fn emit_globals(&mut self, module: &Module, _prog: &crate::ir::Program) {
        // Based on original at line ~305
        for g in &module.globals {
            self.emit_single_global(g);
        }
        
        // Add .rodata section symbol if there's any constant data
        if !self.rodata_buffer.is_empty() {
            self.symbols.push(ElfSym {
                name: ".rodata".to_string(),
                section: 2,
                value: 0,
                size: self.rodata_buffer.len() as u64,
                bind: STB_LOCAL,
                ty: STT_SECTION,
                visibility: STV_DEFAULT,
            });
            self.sym_index.insert(".rodata".to_string(), self.symbols.len() as u32);
        }
    }

    /// Emit single global variable
    fn emit_single_global(&mut self, g: &Global) {
        // Based on original at line ~324
        let size = value_type_size(&g.ty).max(1);
        // Align and allocate space in rodata
        let align = size.max(4) as u64;
        let off = self.align_rodata(align);
        
        self.global_rodata.insert(g.name.clone(), off);
        
        if let Some(init) = &g.initializer {
            // Serialize the constant initializer into its storage bytes.
            let data = serialize_init_bytes(init, size);
            self.write_bytes(&data, size);
        } else {
            // Zero-initialize
            self.rodata_buffer.resize(self.rodata_buffer.len() + size, 0);
        }
        
        // Track TLS globals separately
        if g.is_tls {
            // TODO: TLS handling
        }
    }

    /// Align data buffer to specified boundary
    fn align_rodata(&self, mut align: u64) -> u64 {
        // Standard power-of-2 alignment
        if align > 1 {
            align = align.next_power_of_two();
        }
        let off = (self.rodata_buffer.len() as u64 + align - 1) & ! (align - 1);
        off
    }

    /// Write bytes to RO data buffer
    fn write_bytes(&mut self, data: &[u8], _size: usize) {
        self.rodata_buffer.extend_from_slice(data);
    }

    /// Build final ELF object file from all collected components
    pub fn build_object_file(&self, _module: &Module) -> CodegenResult<Vec<u8>> {
        // TODO: Call elf::build_elf_aarch64 or similar
        // Assembles all sections, symbols, relocations into binary ELF format
        
        todo!("Implement build_object_file calling ELF builder")
    }
}

/// Utility: serialize a constant global initializer into its storage bytes.
///
/// Only scalar literal constants are emitted directly; aggregate or
/// non-constant initializers remain zero-filled pending full lowering.
fn serialize_init_bytes(init: &TypedExpr, size: usize) -> Vec<u8> {
    let mut buf = vec![0u8; size];
    if let Some(lit) = &init.literal {
        match lit {
            crate::ir::ExprLiteral::Int(v) => {
                let raw = v.to_le_bytes();
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
    }
    buf
}

/// Utility: get size of a type in bytes
fn value_type_size(ty: &ValueType) -> usize {
    match ty {
        ValueType::Integer { bits, .. } | ValueType::Float { bits } => (*bits as usize).div_ceil(8),
        ValueType::Array(elem_ty, count) => elem_ty.size().saturating_mul(count.unwrap_or(0)),
        ValueType::Pointer(_) => 8,  // AArch64 pointers are 64-bit
        ValueType::Vector(elem, len) => elem.size().saturating_mul(*len),
        ValueType::Aggregate { size, .. } => *size,
        ValueType::Function { .. } => 8,  // Function pointer
        _ => ty.size(),
    }
}

/// Alignment utility for AArch64
fn align_up_u64(x: u64, align: u64) -> u64 {
    (x + align - 1) & !(align - 1)
}
