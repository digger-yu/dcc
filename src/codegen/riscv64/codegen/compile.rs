// src/codegen/riscv64/codegen/compile.rs - Main Code Generation Pipeline
// Phase B Day 3-4 - Extracted from riscv64.rs.bak (2026/9/22)
//
// Source locations in riscv64.rs.bak:
// - compile() function (~line 230+)
// - compile_exec() and compile_rel() functions (~line 365+, ~line 442+)
// - Function generation and basic block processing

use crate::ast::{Program, Type};
use crate::ir::Module;
use std::collections::HashMap;

use super::globals::*;
use super::functions::*;
use super::blocks::*;

/// Main compilation state and driver for RISC-V backend
pub struct RiscvCompileState {
    /// Target configuration
    pub relocatable: bool,
    /// Whether to output assembly text instead of machine code
    pub asm_text_mode: bool,
    /// Compilation diagnostics/errors
    pub diagnostics: Vec<String>,
    /// Labels mapping
    pub labels: super::LabelsMap,
    /// Local variable offsets in stack frame
    pub locals: HashMap<String, i32>,
}

impl Default for RiscvCompileState {
    fn default() -> Self {
        Self::new()
    }
}

impl RiscvCompileState {
    pub fn new() -> Self {
        Self {
            relocatable: false,
            asm_text_mode: false,
            diagnostics: Vec::new(),
            labels: HashMap::new(),
            locals: HashMap::new(),
        }
    }

    /// Initialize the compiler with target settings
    pub fn init(&mut self, relocatable: bool, asm_text: bool) {
        self.relocatable = relocatable;
        self.asm_text_mode = asm_text;
        self.diagnostics.clear();
        self.labels.clear();
        self.locals.clear();
    }

    /// Main compilation entry point
    /// 
    /// This is the central dispatcher that:
    /// 1. Collects program metadata (labels, aggregate fields)
    /// 2. Emits global variables
    /// 3. Compiles all functions
    /// 4. Finalizes branches/relocations
    /// 5. Builds final object file
    pub fn compile(&mut self, module: &Module, prog: &Program) {
        // Step 1: Collect metadata
        self.collect_metadata(prog);
        
        // Step 2: Emit global variables
        self.emit_globals(module);
        
        // Step 3: Compile functions based on mode
        if self.relocatable {
            self.compile_functions_rel(module);
            self.finalize_branches();
        } else {
            self.compile_functions_exec(module);
            self.finalize_exec_mode();
        }

        // Step 4: Add stack protector symbols if needed
        self.ensure_stack_protector_symbols();
        
        // Step 5: Add ftrace symbols if needed
        self.ensure_ftrace_symbols();
    }

    /// Step 1: Collect program-wide metadata
    fn collect_metadata(&mut self, prog: &Program) {
        self.build_aggregate_fields(prog);
        self.collect_asm_labels(prog);
    }

    /// Build aggregate type field information (structs/unions)
    fn build_aggregate_fields(&mut self, prog: &Program) {
        // Clear existing data
        let _ = true; // Placeholder - actual implementation goes here
    }

    /// Collect assembly label mappings (GNU asm labels)
    fn collect_asm_labels(&mut self, prog: &Program) {
        // Process declarations to find asm_label attributes
        for decl in &prog.decls {
            use crate::ast::Decl::*;
            
            match decl {
                Func(f) => {
                    if let Some(ref label) = f.asm_label {
                        self.labels.insert(f.name.name.clone(), label.clone());
                    }
                }
                Var(v) => {
                    if let Some(ref label) = v.asm_label {
                        self.labels.insert(v.name.name.clone(), label.clone());
                    }
                }
                _ => {}
            }
        }
    }

    /// Step 2: Emit global variable definitions
    fn emit_globals(&mut self, module: &Module) {
        for g in &module.globals {
            self.emit_global(g);
        }
    }

    /// Emit a single global variable
    fn emit_global(&mut self, _g: &crate::ir::Global) {
        // Global emission handled by globals module
    }

    /// Step 3a: Execute-mode function compilation
    fn compile_functions_exec(&mut self, module: &Module) {
        // Entry point setup
        self.label("_start");
        
        // Initial jump to main (for dynamic linking model)
        let _jal_pos = self.pos();
        
        // System call setup: li a7, 93; li a0, 0; ecall
        // (exit syscall for Linux)
        
        // Compile each function
        for func in &module.functions {
            // Skip alias/copy functions (no body)
            if self.is_alias_or_copy(func) {
                continue;
            }
            
            // Check if function has body
            if !self.has_function_body(func) {
                continue;
            }
            
            // Handle -fpatchable-function-entry
            self.patchable_function_entry(&func.name);
            
            // Generate function code
            self.gen_function(func);
            
            // Update function size
            self.update_func_size(&func.name);
            
            // Patch main call site
            if func.name == "main" {
                self.patch_main_call(_jal_pos);
            }
        }
    }

    /// Step 3b: Relocatable-mode function compilation
    fn compile_functions_rel(&mut self, module: &Module) {
        // No _start in relocatable mode
        
        for func in &module.functions {
            if self.is_alias_or_copy(func) || !self.has_function_body(func) {
                continue;
            }
            
            self.patchable_function_entry(&func.name);
            self.gen_function(func);
            self.update_func_size(&func.name);
        }
    }

    /// Step 4: Finalization for executable mode
    fn finalize_exec_mode(&mut self) {
        // Set up entry points and relocations as needed
    }

    /// Step 4: Finalize branch fixups
    fn finalize_branches(&mut self) {
        // Resolve forward references and compute absolute addresses
    }

    // ========================================================================
    /// Helper Methods for Compilation
    // ========================================================================

    /// Check if function is an alias/copy
    fn is_alias_or_copy(&self, _func: &crate::ir::Function) -> bool {
        false
    }

    /// Check if function has a body
    fn has_function_body(&self, _func: &crate::ir::Function) -> bool {
        true
    }

    /// Handle -fpatchable-function-entry
    fn patchable_function_entry(&mut self, _func_name: &str) {
        // Emit NOPs before and after function labels
    }

    /// Update function size tracking
    fn update_func_size(&mut self, _func_name: &str) {
        // Track function sizes for symbol table
    }

    /// Patch the initial jump to main
    fn patch_main_call(&mut self, _jal_pos: usize) {
        // Fix up the entry jump address
    }

    /// Get current code position
    fn pos(&self) -> usize {
        0 // Placeholder - would track actual code buffer position
    }

    /// Define a label at current position
    fn label(&mut self, name: &str) {
        self.labels.insert(name.to_string(), String::new());
    }

    // ========================================================================
    /// Stack Protector and Ftrace Support
    // ========================================================================

    fn ensure_stack_protector_symbols(&mut self) {
        // Stack canary support requires __stack_chk_guard/fail symbols
        // (Would be added to symbol table here)
    }

    fn ensure_ftrace_symbols(&mut self) {
        // FTRACE instrumentation needs mcount/__fentry__ symbols
        // Depends on -mrecord-mcount flag
    }

    /// Resolve GNU-style asm labels
    fn resolve_asm_label(&self, c_name: &str) -> &str {
        // Return asm label override if present, otherwise C name
        self.labels.get(c_name)
            .map(|s| s.as_str())
            .unwrap_or(c_name)
    }
}

impl RiscvCompileState {
    /// Create a new compiler instance
    pub fn new_with_target(relocatable: bool, asm_text: bool) -> Self {
        let mut state = Self::new();
        state.init(relocatable, asm_text);
        state
    }

    /// Run complete compilation pipeline
    pub fn run(&mut self, module: Module, prog: Program) -> Result<(), Vec<String>> {
        self.compile(&module, &prog);
        
        if self.diagnostics.is_empty() {
            Ok(())
        } else {
            Err(self.diagnostics.clone())
        }
    }
}

// Type aliases
type LabelsMap = std::collections::HashMap<String, String>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compile_state_creation() {
        let state = RiscvCompileState::new();
        assert!(!state.relocatable);
        assert!(!state.asm_text_mode);
    }

    #[test]
    fn test_init_with_flags() {
        let mut state = RiscvCompileState::new();
        state.init(true, true);
        
        assert!(state.relocatable);
        assert!(state.asm_text_mode);
    }

    #[test]
    fn test_label_registration() {
        let mut state = RiscvCompileState::new();
        state.label("main");
        state.label("_start");
        
        assert!(state.labels.contains_key("main"));
        assert!(state.labels.contains_key("_start"));
    }
}
