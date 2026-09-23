// src/codegen/aarch64/codegen/mod.rs - Core AArch64 CodeGen Module
// Phase A Week 2-3 - Main code generation entry points and module organization
// 
// This module contains the core compilation logic extracted from original_monolith.rs
// It provides modular, testable components for AArch64 object file generation.
//
// Directory structure:
// ├── mod.rs          - Module declarations and public API ✅ NEW
// ├── compile.rs      - Main compilation loop (exec/rel modes)
// ├── functions.rs    - Function-level code generation
// ├── globals.rs      - Global variable initialization
// └── blocks.rs       - Control flow and branch handling

mod compile;
mod functions;
mod globals;
mod blocks;

pub use compile::*;
pub use functions::*;
pub use globals::*;
pub use blocks::*;

/// Main entry point for AArch64 code generation
/// 
/// This is the primary interface for compiling IR modules to AArch64 object files.
/// It orchestrates the entire compilation process by coordinating between
/// different compilation phases (globals, functions, control flow).
///
/// # Arguments
/// * `module` - The IR module containing functions, globals, and program information
/// * `target` - Target architecture specifications (AArch64 configuration)
/// * `relocatable` - Whether to generate position-independent code (PIC)
///
/// # Returns
/// * `Ok(ObjectFile)` - Binary object file ready for linking
/// * `Err(Error)` - Compilation error with diagnostic information
///
/// # Compilation Modes
/// 
/// ## Executable Mode (relocatable=false)
/// - Generates fixed-address executable (entry at 0x400000)
/// - Patches absolute addresses post-generation
/// - Suitable for simple executables on known addresses
///
/// ## Relocatable Mode (relocatable=true)
/// - Generates PIC with relocation entries
/// - Creates symbol table and relocations for linker
/// - Required for shared libraries and modern ELF executables
///
/// # Example
/// ```rust,no_run
/// let module = ir::parse_source_code("int main() { return 0; }");
/// let target = Target::aarch64_linux();
/// let object = compile_aarch64(&module, &target, false)?;
/// std::fs::write("output.o", object.to_bytes())?;
/// ```
pub use compile::{
    compile_aarch64, CodegenError, CodegenResult, CodegenContext,
};
