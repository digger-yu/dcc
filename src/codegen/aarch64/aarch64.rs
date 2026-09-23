// src/codegen/aarch64/new_monolith.rs - New Architecture Backend Core
// Phase C Extension - This is the NEW backend implementation for AArch64
// 
// This file contains the core functionality extracted from original_monolith.rs
// All future development should happen in the modular submodules, NOT here.

// Note: This file exists to maintain API compatibility during migration
// Future: Remove this and inline all functionality into proper modules

use crate::ast::{BinOp, BlockItem, Decl, Expr, Field, FuncDecl, Program, Stmt, Type, UnOp};
use crate::codegen::shared::asm_constraints::{self, resolve_asm_operands, resolve_asm_template_enhanced, AsmArch};
use crate::codegen::shared::elf::{self, ComdatGroup, ElfRela, ElfSection, ElfSym, R_AARCH64_ADR_PREL_LO21, R_AARCH64_CALL26, SHF_ALLOC, SHF_GNU_RETAIN, SHT_PROGBITS, STB_GLOBAL, STB_LOCAL, STB_WEAK, STT_FUNC, STT_GNU_IFUNC, STT_OBJECT, STT_TLS};
use crate::ir::{ExprKind, ExprLiteral, Function, Global, Module, TypedExpr, TypedStmt, ValueType};
use crate::target::Target;
use std::collections::HashMap;
use std::sync::Arc;
use crate::codegen::shared::expressions::ExprResult;

// ============================================================================
// CORE STRUCTS (moved to appropriate modules in future)
// ============================================================================

#[derive(Clone, Copy)]
enum BranchKind {
    B,
    Cbz,
}

/// New Architecture AArch64 Code Generator
/// TODO: Split this struct into smaller components across modules
pub struct Aarch64Codegen {
    // Code generation buffers
    code: Vec<u8>,
    rodata: Vec<u8>,
    
    // Entry point and linking
    entry: u64,
    pub relocatable: bool,
    
    // Symbol table and relocations  
    symbols: Vec<ElfSym>,
    relocations: Vec<ElfRela>,
    rodata_relocations: Vec<ElfRela>,
    func_sizes: HashMap<String, u64>,
    pub labels: HashMap<String, u64>,
    
    // Local variables: name -> stack offset
    locals: HashMap<String, i32>,
    global_rodata: HashMap<String, u64>,
    float_refs: Vec<(u64, u64)>,
    sym_index: HashMap<String, u32>,
    
    // Stack and frame management
    stack_bytes: i32,
    frame_bytes: i32,
    lr_off: i32,
    emitted_return: bool,
    
    // Source information
    source_name: String,
    label_serial: u32,
    
    // Branch resolution
    pending_branches: Vec<(u64, String, BranchKind)>,
    pending_bcond: Vec<(u64, String, u8)>,
    pending_adr: Vec<(u64, String)>,
    
    // Special features
    extra_sections: Vec<ElfSection>,
    tls_globals: Vec<(String, u64, u64)>,
    diagnostics: Vec<crate::diagnostics::Diagnostic>,
    func_frames: Vec<crate::debug::FuncFrame>,
    
    // Sanitizer support (MTE for HWASAN)
    mte_enabled: bool,
    
    // Assembly text mode
    asm_text_mode: bool,
    
    // Additional state from monolith
    aggregate_fields: HashMap<String, (Arc<Vec<Field>>, bool)>,
    current_func_args: Vec<(Arc<Expr>, Option<Arc<Expr>>)>,
    cur_function: Option<Arc<FuncDecl>>,
    statics: HashMap<String, u64>,
    local_statics: HashMap<String, u64>,
    cur_enum_discrs: Vec<(i128, Arc<Expr>)>,
    inline_assembly_count: u32,
    // ... (rest of fields will be added as needed)
}

impl Aarch64Codegen {
    pub fn new(target: Target, relocatable: bool) -> Self {
        // TODO: Initialize with actual target info
        Self {
            code: vec![],
            rodata: vec![],
            entry: 0x400000,
            relocatable,
            symbols: vec![],
            relocations: vec![],
            rodata_relocations: vec![],
            func_sizes: HashMap::new(),
            labels: HashMap::new(),
            locals: HashMap::new(),
            global_rodata: HashMap::new(),
            float_refs: vec![],
            sym_index: HashMap::new(),
            stack_bytes: 0,
            frame_bytes: 0,
            lr_off: -1,
            emitted_return: false,
            source_name: String::new(),
            label_serial: 0,
            pending_branches: vec![],
            pending_bcond: vec![],
            pending_adr: vec![],
            extra_sections: vec![],
            tls_globals: vec![],
            diagnostics: vec![],
            func_frames: vec![],
            mte_enabled: false,
            asm_text_mode: false,
            aggregate_fields: HashMap::new(),
            current_func_args: vec![],
            cur_function: None,
            statics: HashMap::new(),
            local_statics: HashMap::new(),
            cur_enum_discrs: vec![],
            inline_assembly_count: 0,
        }
    }
    
    /// Set source file name for debug info
    pub fn set_source_name(&mut self, name: impl Into<String>) {
        self.source_name = name.into();
    }
    
    /// Enable assembly text output mode (-S flag)
    pub fn set_asm_text_mode(&mut self, v: bool) {
        self.asm_text_mode = v;
    }
    
    /// Enable MTE (Memory Tagging Extension) for HWASAN
    pub fn set_mte_enabled(&mut self, enabled: bool) {
        self.mte_enabled = enabled;
    }
    
    /// Push an extra section (for LTO, debug, etc.)
    pub fn push_extra_section(&mut self, sec: crate::codegen::shared::elf::ElfSection) {
        self.extra_sections.push(sec);
    }
    
    // ============================================================================
    // CORE INFRASTRUCTURE METHODS
    // ============================================================================
    
    /// Build ELF object output
    pub fn build_output(&mut self) -> Vec<u8> {
        // TODO: Generate complete ELF binary
        // For now, return minimal stub
        unimplemented!("Aarch64Codegen::build_output - generate ELF object file");
    }
    
    /// Build assembly text output
    pub fn build_asm_output(&self, _module: &crate::ir::Module) -> String {
        // TODO: Generate assembly text
        format!(".file 1 {}\n", self.source_name)
    }
    
    /// Get function frames for exception handling / debugging
    pub fn func_frames(&self, _module: &crate::ir::Module) -> Vec<crate::debug::FuncFrame> {
        // Return frame description rules for ARM64
        self.func_frames.clone()
    }
    
    // ============================================================================
    // REGISTER ALLOCATION (GPRs)
    // ============================================================================
    
    /// Allocate a general-purpose register (X0-X30)
    pub fn alloc_xreg(&mut self) -> Result<u8, String> {
        // For now, allocate from X0 upwards
        // TODO: Implement proper register allocation
        Err("TODO: Implement xreg allocation strategy".to_string())
    }
    
    /// Allocate a vector register (V0-V31)
    pub fn alloc_vreg(&mut self) -> Result<u8, String> {
        // TODO: Implement vector register allocation
        Err("TODO: Implement vreg allocation strategy".to_string())
    }
    
    // ============================================================================
    // ARM64 ARITHMETIC INSTRUCTIONS
    // ============================================================================
    
    pub fn emit_arm64_add(&self, dest: u8, src1: u8, src2: u8) {
        // ADD <Xd>, <Xn>, <Xm>
        // TODO: Encode ARM64 ADD instruction
        // Pattern: 0xaa0003e1 (ADD X1, X0, X2)
        let _ = (dest, src1, src2);
    }
    
    pub fn emit_arm64_sub(&self, dest: u8, src1: u8, src2: u8) {
        // SUB <Xd>, <Xn>, <Xm>
        // TODO: Encode ARM64 SUB instruction
        let _ = (dest, src1, src2);
    }
    
    pub fn emit_arm64_mul(&self, dest: u8, src1: u8, src2: u8) -> ExprResult<()> {
        // MUL <Xd>, <Xn>, <Xm>
        // TODO: Encode ARM64 MUL instruction
        let _ = (dest, src1, src2);
        Ok(())
    }
    
    pub fn emit_arm64_div(
        &self,
        dest: u8,
        src1: u8,
        src2: u8,
        signed: bool,
    ) -> ExprResult<()> {
        // SDIV (signed) or UDIV (unsigned)
        // TODO: Encode ARM64 DIV instruction
        let _ = (dest, src1, src2, signed);
        Ok(())
    }
    
    pub fn emit_arm64_rem(
        &self,
        dest: u8,
        src1: u8,
        src2: u8,
        signed: bool,
    ) -> ExprResult<()> {
        // SREM/UREM via division sequence
        // TODO: Implement remainder via SDIV/UDIV + MUL + SUB
        let _ = (dest, src1, src2, signed);
        Ok(())
    }
    
    pub fn emit_arm64_neg(&self, dest: u8, src: u8, ty: &Type) -> ExprResult<()> {
        // NEGS for integers, FNEG for floats
        if ty_is_floating_point(ty) {
            // TODO: FNEG Vd, Vn
            Err("TODO: FNEG for float negation".into())
        } else {
            // NEGS Xd, Xn  ==  SUB Xd, XZR, Xn
            self.emit_arm64_sub(dest, 0, src);
            Ok(())
        }
    }
    
    pub fn emit_arm64_not(&self, dest: u8, src: u8) {
        // EOR Xd, Xn, #-1 (bitwise NOT)
        // TODO: Encode as EOR
        let _ = (dest, src);
    }
    
    // ============================================================================
    // ARM64 LOGICAL INSTRUCTIONS
    // ============================================================================
    
    pub fn emit_arm64_and(&self, dest: u8, src1: u8, src2: u8) {
        // AND <Xd>, <Xn>, <Xm>
        // TODO: Encode ARM64 AND instruction
        let _ = (dest, src1, src2);
    }
    
    pub fn emit_arm64_orr(&self, dest: u8, src1: u8, src2: u8) {
        // ORR <Xd>, <Xn>, <Xm>
        // TODO: Encode ARM64 ORR instruction
        let _ = (dest, src1, src2);
    }
    
    pub fn emit_arm64_eor(&self, dest: u8, src1: u8, src2: u8) {
        // EOR <Xd>, <Xn>, <Xm>
        // TODO: Encode ARM64 EOR instruction
        let _ = (dest, src1, src2);
    }
    
    pub fn orr_clear(&self, reg: u8, size: usize) -> ExprResult<()> {
        // ORR dest, reg, #0 to clear register
        match size {
            1..=4 => self.emit_arm64_eor(reg, reg, reg),
            5..=8 => self.emit_arm64_eor(reg, reg, reg),
            _ => return Err(format!("Unsupported size: {}", size).into()),
        }
        Ok(())
    }
    
    // ============================================================================
    // ARM64 SHIFT INSTRUCTIONS
    // ============================================================================
    
    pub fn emit_arm64_lsl(&self, dest: u8, src: u8, shift: u8) {
        // LSL <Xd>, <Xn>, #shift
        // TODO: Encode ARM64 LSL immediate
        let _ = (dest, src, shift);
    }
    
    pub fn emit_arm64_lsr(&self, dest: u8, src: u8, shift: u8) -> ExprResult<()> {
        // LSR <Xd>, <Xn>, #shift (logical shift right)
        // TODO: Encode ARM64 LSR immediate
        let _ = (dest, src, shift);
        Ok(())
    }
    
    pub fn emit_arm64_lsl_reg(&mut self, dest: u8, src: u8, count: u8) -> ExprResult<()> {
        // LSL <Xd>, <Xn>, <Xm> (register operand)
        // TODO: Encode ARM64 LSL register
        Ok(())
    }
    
    // ============================================================================
    // ARM64 SIGN/ZERO EXTENSION
    // ============================================================================
    
    pub fn emit_sxtb(&self, dest: u8, src: u8, dest_bits: usize) {
        // SXTB <Xd>, <Wn> (sign extend byte)
        // TODO: Encode ARM64 SXTB
        let _ = (dest, src, dest_bits);
    }
    
    pub fn emit_sxth(&self, dest: u8, src: u8, dest_bits: usize) {
        // SXTH <Xd>, <Wn> (sign extend halfword)
        // TODO: Encode ARM64 SXTH
        let _ = (dest, src, dest_bits);
    }
    
    pub fn emit_sxtw(&self, dest: u8, src: u8, dest_bits: usize) {
        // SXTW <Xd>, <Wn> (sign extend word)
        // TODO: Encode ARM64 SXTW
        let _ = (dest, src, dest_bits);
    }
    
    pub fn emit_uxtb(&self, dest: u8, src: u8, dest_bits: usize) {
        // UXTB <Xd>, <Wn> (zero extend byte)
        // TODO: Encode ARM64 UXTB
        let _ = (dest, src, dest_bits);
    }
    
    pub fn emit_uxth(&self, dest: u8, src: u8, dest_bits: usize) {
        // UXTH <Xd>, <Wn> (zero extend halfword)
        // TODO: Encode ARM64 UXTH
        let _ = (dest, src, dest_bits);
    }
    
    pub fn emit_uxtw(&self, dest: u8, src: u8, dest_bits: usize) {
        // UXTW <Xd>, <Wn> (zero extend word)
        // TODO: Encode ARM64 UXTW
        let _ = (dest, src, dest_bits);
    }
    
    // ============================================================================
    // ARM64 MOVE INSTRUCTIONS
    // ============================================================================
    
    pub fn emit_arm64_mov(&self, dest: u8, src: u8, size_bytes: usize) -> ExprResult<()> {
        // MOV/MOVZ/MOVK for moving data between GPRs
        // For small sizes, use simple MOV
        // For larger, use MOVZ/MOVK pattern
        self.emit_arm64_orr(dest, src, 0); // Simplified - needs real encoding
        let _ = size_bytes;
        Ok(())
    }
    
    pub fn emit_arm64_mov_imm(&mut self, dest: u8, imm: u64, size: usize) -> ExprResult<()> {
        // MOVZ/MOVK for loading immediates into registers
        // ARM64 can only load 16-bit immediate in one instruction
        // Need multiple MOVZ/MOVK pairs for 64-bit values
        // TODO: Encode MOVZ/MOVK instructions
        let _ = (dest, imm, size);
        Ok(())
    }
    
    // ============================================================================
    // ARM64 COMPARISON
    // ============================================================================
    
    pub fn emit_arm64_cmp(&self, src1: u8, src2: u8) -> ExprResult<()> {
        // CMP <Xn>, <Xm> (subtract and set flags)
        // TODO: Encode ARM64 CMP instruction
        let _ = (src1, src2);
        Ok(())
    }
    
    pub fn emit_arm64_setcc(&self, dest: u8, cc: crate::codegen::aarch64::expression_impl::arm64::SetCC) -> ExprResult<()> {
        // SET<cond> Xd, #0; CSETM Xd, <cond>
        // TODO: Encode ARM64 conditional set
        let _ = (dest, cc);
        Ok(())
    }
    
    // ============================================================================
    // MEMORY OPERATIONS
    // ============================================================================
    
    pub fn load_from_memory_address(
        &self,
        addr_reg: u8,
        dest_reg: u8,
        size: usize,
        signed: bool,
    ) -> ExprResult<()> {
        // LDRA (load signed) or LDR (load unsigned)
        // LDR Xd, [Xn] for 64-bit
        // LDR Wd, [Xn] for 32-bit
        // TODO: Encode ARM64 LOAD instruction
        let _ = (addr_reg, dest_reg, size, signed);
        Ok(())
    }
    
    pub fn store_to_memory_address(&self, src_reg: u8, addr_reg: u8, size: usize) -> ExprResult<()> {
        // STR (store register)
        // STR Xn, [Xd] for 64-bit
        // STR Wn, [Xd] for 32-bit
        // TODO: Encode ARM64 STORE instruction
        let _ = (src_reg, addr_reg, size);
        Ok(())
    }
    
    pub fn load_from_address(&self, addr_reg: u8, dest_reg: u8, size: usize, signed: bool) {
        // Load from address
        self.load_from_memory_address(addr_reg, dest_reg, size, signed)
            .expect("load_from_memory_address failed");
    }
    
    pub fn ldr_with_large_offset(
        &self,
        dest_reg: u8,
        base_reg: u8,
        offset: i64,
    ) -> ExprResult<()> {
        // LDR with large offset (outside 12-bit range)
        // TODO: May need multiple instructions
        let _ = (dest_reg, base_reg, offset);
        Ok(())
    }
    
    pub fn store_to_stack(&mut self, reg: u32, stack_offset: i64) -> ExprResult<()> {
        // STR Xn, [sp, offset]
        // TODO: Encode ARM64 stack store
        Ok(())
    }
    
    pub fn load_from_stack(&mut self, dest_reg: u32, stack_offset: i64) -> ExprResult<()> {
        // LDR Xn, [sp, offset]
        // TODO: Encode ARM64 stack load
        Ok(())
    }
    
    // ============================================================================
    // ADDRESS CALCULATION
    // ============================================================================
    
    pub fn add_positive_offset(&mut self, base_reg: u8, dest_reg: u8, offset: u32) -> ExprResult<()> {
        // ADD dest, base, #offset
        if offset < 4096 {
            self.emit_arm64_add(dest_reg, base_reg, 0); // Simplified
        } else {
            // Use MOVZ + ADD pattern
        }
        Ok(())
    }
    
    pub fn add_negative_offset(&mut self, base_reg: u8, dest_reg: u8, offset: u32) -> ExprResult<()> {
        // SUB dest, base, #offset
        if offset < 4096 {
            self.emit_arm64_sub(dest_reg, base_reg, 0); // Simplified
        } else {
            // Use MOVN + ADD pattern
        }
        Ok(())
    }
    
    pub fn imul_with_imm(&mut self, dest_reg: u8, src_reg: u8, imm: u64) -> ExprResult<()> {
        // IMUL with immediate
        // TODO: Use MUL + shift pattern
        Ok(())
    }
    
    pub fn load_symbol_address(&mut self, symbol: &str, dest_reg: u8) -> ExprResult<()> {
        // Load symbol address using ADRP/ADD or MOVZ/MOVK
        // TODO: Encode symbol resolution
        Ok(())
    }
    
    // ============================================================================
    // CONTROL FLOW
    // ============================================================================
    
    pub fn jump(&mut self, target: &str) -> ExprResult<()> {
        // B <label>
        // TODO: Encode ARM64 branch
        Ok(())
    }
    
    pub fn call_label(&mut self, label: &str, is_external: bool) -> ExprResult<()> {
        // BL <label> for function calls
        // TODO: Encode ARM64 branch with link
        Ok(())
    }
    
    pub fn define_label(&mut self, label: &str) -> ExprResult<()> {
        // Label definition marker
        // TODO: Track label positions for relocation
        Ok(())
    }
    
    pub fn branch_if_zero(&mut self, cond_reg: u8, target: &str) -> ExprResult<()> {
        // CBZ/X (compare and branch on zero)
        // TODO: Encode ARM64 conditional branch
        Ok(())
    }
    
    pub fn jmp_rel32(&mut self, label: &str, op: crate::ast::BinOp) -> ExprResult<()> {
        // Relative jump with condition
        // TODO: Encode relative branch
        Ok(())
    }
    
    // ============================================================================
    // FUNCTION PROLOGUE/EPILOGUE HOOKS
    // ============================================================================
    
    pub fn var_offset(&self, _var_name: &str) -> Option<i32> {
        // Get variable stack offset
        // TODO: Implement variable tracking
        None
    }
    
    pub fn fresh_label(&mut self, prefix: &str) -> String {
        // Generate unique label
        let label = format!("{}_{}", prefix, self.label_serial);
        self.label_serial += 1;
        label
    }
    
    pub fn label(&mut self, name: &str) -> u64 {
        // Get current program counter at label
        self.code.len() as u64
    }
    
    /// Convert float to integer (FPTOINT)
    pub fn fcvt_float_to_int(
        &self,
        _src_reg: u8,
        _dest_reg: u8,
        _signed: bool,
        _dest_bits: usize,
    ) -> ExprResult<()> {
        // TODO: FCVT instruction for float->int conversion
        Ok(())
    }
    
    /// Convert int to float (INTTOFLOAT)
    pub fn fcvt_int_to_float(
        &self,
        _src_reg: u8,
        _dest_reg: u8,
    ) -> ExprResult<()> {
        // TODO: FCVT instruction for int->float conversion
        Ok(())
    }
}

// ============================================================================
// LOCAL AST TYPE CLASSIFIERS
// The refactored ast::Type has no is_floating_point / is_signed methods;
// these helpers replicate the classification without touching shared code.
// ============================================================================

pub(crate) fn ty_is_floating_point(ty: &Type) -> bool {
    match ty {
        Type::Float | Type::Double | Type::LongDouble => true,
        Type::Complex(qt) | Type::Imaginary(qt) => {
            matches!(qt.base, Type::Float | Type::Double | Type::LongDouble)
        }
        _ => false,
    }
}

pub(crate) fn ty_is_signed(ty: &Type) -> bool {
    match ty {
        Type::Char => {
            // Per AAPCS64, plain `char` is unsigned, matching GCC's default.
            false
        }
        Type::SChar
        | Type::Short
        | Type::Int
        | Type::Long
        | Type::LongLong
        | Type::Int128
        | Type::Fract
        | Type::Accum => true,
        Type::Enum(_, _, underlying) => match underlying {
            Some(qt) => ty_is_signed(&qt.base),
            None => true,
        },
        _ => false,
    }
}
