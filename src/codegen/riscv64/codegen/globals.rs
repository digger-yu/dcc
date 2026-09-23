// src/codegen/riscv64/codegen/globals.rs - Global Variable Handling
use crate::ast::{Expr, Type, Block, Decl, FuncDecl};
// Phase B Day 4 - Extracted from riscv64.rs.bak (2026/9/22)
//
// Source locations in riscv64.rs.bak:
// - emit_globals() ~line 295+
// - emit_global() function ~line 314+
// - TLS handling and symbol table creation (~line 325+)

use std::collections::HashMap;
use crate::ir::ValueType;
use crate::codegen::riscv64::var_is_static as riscv64_var_is_static;
use super::blocks::Expression;

/// Global variable state for RISC-V backend
pub struct GlobalState {
    /// Global variables map
    pub globals: HashMap<String, GlobalInfo>,
    /// ROdata buffer
    pub rodata: Vec<u8>,
    /// Global ROdata offset tracking
    pub global_rodata: HashMap<String, u64>,
    /// TLS global variables
    pub tls_globals: HashMap<String, u64>,
    /// Symbol table
    pub symbols: Vec<SymInfo>,
    /// Sym index mapping
    pub sym_index: HashMap<String, u32>,
}

impl Default for GlobalState {
    fn default() -> Self {
        Self::new()
    }
}

impl GlobalState {
    pub fn new() -> Self {
        Self {
            globals: HashMap::new(),
            rodata: Vec::new(),
            global_rodata: HashMap::new(),
            tls_globals: HashMap::new(),
            symbols: Vec::new(),
            sym_index: HashMap::new(),
        }
    }

    /// Emit all global variable definitions
    pub fn emit_all(&mut self, module: &crate::ir::Module) {
        for g in &module.globals {
            self.emit_single(g);
        }
        
        // Add .rodata section symbol if not empty
        if !self.rodata.is_empty() {
            self.add_section_symbol(".rodata");
        }
    }

    /// Emit a single global variable
    fn emit_single(&mut self, g: &crate::ir::Global) {
        let size = g.ty.size().max(1);

        // Align in rodata
        let off = self.align_rodata(size.max(4));

        // Record offset
        self.global_rodata.insert(g.name.clone(), off as u64);

        // Initialize with bytes if provided
        if let Some(init) = &g.initializer {
            self.write_initializer_bytes(init, size);
        } else {
            // Zero initialize
            self.rodata.resize(self.rodata.len() + size, 0);
        }

        // Track TLS variables
        if g.is_tls {
            self.tls_globals.insert(g.name.clone(), off as u64);
        }

        // Create ELF symbol entry
        self.create_elf_symbol(g, off, size, None);
    }

    /// Align rodata to specified boundary
    fn align_rodata(&mut self, align: usize) -> usize {
        let current = self.rodata.len();
        let aligned = ((current + align - 1) / align) * align;
        let padding = aligned - current;
        
        if padding > 0 {
            self.rodata.extend(std::iter::repeat(0).take(padding));
        }
        
        aligned
    }

    /// Write initializer bytes into rodata
    fn write_initializer_bytes(&mut self, init: &crate::ir::TypedExpr, size: usize) {
        use crate::ir::ExprLiteral;
        let start = self.rodata.len();
        self.rodata.resize(start + size, 0);

        match &init.literal {
            Some(ExprLiteral::Int(v)) => {
                let bytes = (*v as i64).to_le_bytes();
                self.rodata[start..start + size.min(8)]
                    .copy_from_slice(&bytes[..size.min(8)]);
            }
            Some(ExprLiteral::Float(v)) => match init.ty {
                ValueType::Float { bits: 64 } => {
                    let bits = *v as f64;
                    let b = bits.to_le_bytes();
                    self.rodata[start..start + 8.min(size)]
                        .copy_from_slice(&b[..8.min(size)]);
                }
                _ => {
                    let bits = *v as f32;
                    let b = bits.to_bits().to_le_bytes();
                    self.rodata[start..start + 4.min(size)]
                        .copy_from_slice(&b[..4.min(size)]);
                }
            },
            None => {}
        }
    }

    /// Add section symbol to ELF symbol table
    fn add_section_symbol(&mut self, name: &str) {
        use crate::codegen::shared::elf::{STB_LOCAL, STT_SECTION};

        self.symbols.push(SymInfo {
            name: name.to_string(),
            section: 2,
            value: 0,
            size: self.rodata.len() as u64,
            bind: STB_LOCAL,
            ty: STT_SECTION,
            visibility: 0,
        });

        self.sym_index.insert(name.to_string(), self.symbols.len() as u32);
    }

    /// Create ELF symbol for a global variable
    fn create_elf_symbol(&mut self, g: &crate::ir::Global, off: usize, size: usize, _prog: Option<&crate::ast::Program>) {
        use crate::codegen::shared::elf::{STB_GLOBAL, STB_LOCAL, STT_OBJECT, STT_TLS};

        // Determine linkage based on static/local modifier
        let is_internal = g.is_static_local || _prog.map_or(false, |p| riscv64_var_is_static(p, &g.name));
        let bind = if is_internal { STB_LOCAL } else { STB_GLOBAL };
        let ty = if g.is_tls { STT_TLS } else { STT_OBJECT };

        self.symbols.push(SymInfo {
            name: g.name.clone(),
            section: 2, // .data/.bss section
            value: off as u64,
            size: size as u64,
            bind,
            ty,
            visibility: 0,
        });

        self.sym_index.insert(g.name.clone(), self.symbols.len() as u32);
    }

    /// Collect TLS global variables from program
    fn collect_tls_globals(&mut self, prog: &crate::ast::Program) {
        prog.decls.iter()
            .filter_map(|d| {
                if let crate::ast::Decl::Var(v) = d {
                    if v.is_thread_local {
                        Some(v.name.name.clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .for_each(|name| {
                self.tls_globals.entry(name).or_insert(0);
            });
    }
}

// ========================================================================
// Helper Structures
// ========================================================================

#[derive(Debug, Clone)]
pub struct GlobalInfo {
    pub name: String,
    pub ty: ValueType,
    pub is_tls: bool,
    pub is_static_local: bool,
    pub asm_label: Option<String>,
    pub initializers: Vec<TypedExpr>,
}

#[derive(Debug, Clone)]
pub struct TypedExpr {
    pub literal: Option<LiteralValue>,
    pub ty: ValueType,
}

#[derive(Debug, Clone)]
pub enum LiteralValue {
    Int(i64),
    Float(f32),
    Str(String),
}

#[derive(Debug, Clone)]
pub struct SymInfo {
    pub name: String,
    pub section: usize,
    pub value: u64,
    pub size: u64,
    pub bind: u8,
    pub ty: u8,
    pub visibility: u8,
}

#[derive(Debug, Clone)]
pub struct FieldDesc {
    pub name: Option<String>,
    pub ty: ValueKind,
    pub packed: bool,
}

#[derive(Debug, Clone)]
pub enum ValueKind {
    Bool { width: u8 },
    Char { signed: bool },
    Int { width: u8, signed: bool },
    Ptr { base: String },
    Array { el_ty: Box<ValueKind>, len: u32 },
    Struct { fields: Arc<Vec<FieldDesc>>, packed: bool },
    Union { fields: Arc<Vec<FieldDesc>> },
    Func { ret: Box<ValueKind>, params: Arc<Vec<ValueKind>> },
}

#[derive(Debug, Clone)]
pub struct ArgInfo {
    pub name: NameInfo,
    pub ty: ValueKind,
    pub reg: u8,
}

#[derive(Debug, Clone)]
pub struct NameInfo {
    pub name: String,
}

use std::sync::Arc;

impl GlobalState {
    /// Get size of a type
    pub fn get_type_size(ty: &ValueType) -> Option<usize> {
        match ty {
            ValueType::Void => Some(1), // Minimum unit
            ValueType::Integer { bits, .. } => Some((*bits / 8).max(1) as usize),
            ValueType::Float { bits } => Some((*bits / 8) as usize),
            ValueType::Pointer(_) => Some(8), // 64-bit address space
            _ => None,
        }
    }

    /// Check if variable has static storage
    pub fn var_is_static(g: &GlobalInfo) -> bool {
        false // Placeholder
    }

    /// Ensure symbol exists in table
    pub fn ensure_sym(&mut self, name: &str) {
        if !self.sym_index.contains_key(name) {
            self.symbols.push(SymInfo {
                name: name.to_string(),
                section: 0,
                value: 0,
                size: 0,
                bind: 1, // STB_GLOBAL
                ty: 0,  // STT_NOTYPE
                visibility: 0,
            });
            self.sym_index.insert(name.to_string(), self.symbols.len() as u32);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_global_state_creation() {
        let state = GlobalState::new();
        assert!(state.globals.is_empty());
        assert!(state.rodata.is_empty());
    }

    #[test]
    fn test_align_rodata() {
        let mut state = GlobalState::new();
        
        state.rodata.resize(17, 0);
        let aligned = state.align_rodata(8);
        
        assert_eq!(aligned % 8, 0);
        assert!(aligned >= 17);
    }

    #[test]
    fn test_get_type_size() {
        use GlobalState;
        
        let size = GlobalState::get_type_size(&ValueType::Integer { bits: 32, signed: true });
        assert_eq!(size, Some(4));

        let size = GlobalState::get_type_size(&ValueType::Float { bits: 32 });
        assert_eq!(size, Some(4));
    }

    #[test]
    fn test_symbol_table_operations() {
        let mut state = GlobalState::new();
        
        state.ensure_sym("test_var");
        assert!(state.sym_index.contains_key("test_var"));
    }
}
