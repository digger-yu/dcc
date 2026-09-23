// src/codegen/aarch64/emit/stmt.rs
// Statement Emission Module
// 
// Handles conversion of high-level statement constructs into AArch64 assembly instructions.
// This module implements control flow logic for return, if/else, while/do, for loops,
// switch/case, break, continue, blocks, and inline assembly.
//
// Extracted from: src/codegen/aarch64/original_monolith.rs (~1147-1488)

use crate::ast::{Case, Stmt};
use crate::codegen::shared::asm_constraints::{resolve_asm_operands, resolve_asm_template_enhanced};
use crate::codegen::shared::elf::{ComdatGroup, ElfRela, ElfSection, ElfSym};
use crate::ir::TypedStmt;
use std::collections::HashMap;

#[derive(Clone, Copy)]
pub enum BranchKind {
    B,          // Unconditional branch
    Cbz,        // Conditionally branch zero/nonzero
}

/// Code generation state machine for AArch64
/// 
/// Maintains all necessary state during IR lowering to code emission.
/// This is extracted from the monolithic struct in original_monolith.rs
pub struct Aarch64Codegen {
    /// Output: encoded instruction bytes
    pub code: Vec<u8>,
    /// Read-only data section bytes
    pub rodata: Vec<u8>,
    /// Entry point address
    pub entry: u64,
    /// Relocatable build flag
    pub relocatable: bool,
    /// ELF symbols table
    pub symbols: Vec<ElfSym>,
    /// Relocations table
    pub relocations: Vec<ElfRela>,
    /// RODATA relocations
    pub rodata_relocations: Vec<ElfRela>,
    /// Function sizes: name -> size
    pub func_sizes: HashMap<String, u64>,
    /// Labels defined: name -> offset
    pub labels: HashMap<String, u64>,
    /// Local variables: name -> stack offset
    pub locals: HashMap<String, i32>,
    /// Global RODATA: name -> offset
    global_rodata: HashMap<String, u64>,
    /// Float reference tracking
    float_refs: Vec<(u64, u64)>,
    /// Symbol index
    sym_index: HashMap<String, u32>,
    /// Stack frame size
    stack_bytes: i32,
    /// Frame base size
    frame_bytes: i32,
    /// LR save offset in frame (-1 if not saving)
    lr_off: i32,
    /// Whether return has been emitted
    emitted_return: bool,
    /// Source file name
    source_name: String,
    /// Label serial number generator
    label_serial: u32,
    /// Pending branches to fix up
    pending_branches: Vec<(u64, String, BranchKind)>,
    /// Pending conditional branches
    pending_bcond: Vec<(u64, String, u8)>,
    /// Pending BL calls
    pending_bl_calls: Vec<(u64, String)>,
    /// Extra sections
    extra_sections: Vec<ElfSection>,
    /// COMDAT groups
    comdat_groups: Vec<ComdatGroup>,
    /// Current function return type
    current_return_ty: Option<crate::ir::ValueType>,
    /// Aggregate fields map
    aggregate_fields: HashMap<String, (std::sync::Arc<Vec<crate::ast::Field>>, bool)>,
    /// Is variadic function
    is_current_func_variadic: bool,
    /// va_arg save base offset
    va_save_base_off: i32,
    /// va_arg GP register offset
    va_gp_offs: i32,
    /// va_arg float register offset
    va_flt_offs: i32,
    /// MTE enabled flag
    mte_enabled: bool,
    /// MTE tag frame size
    tag_frame_size: i32,
    /// MTE save offset
    mte_save_off: i32,
    /// Saved sret pointer offset
    saved_sret_ptr: Option<i32>,
    /// Stack protector canary offset
    canary_slot_off: i32,
    /// TLS globals map
    tls_globals: HashMap<String, u64>,
    /// Raw global asm lines
    raw_global_asm_lines: Vec<String>,
    /// Raw function asm lines
    raw_func_asm_lines: Vec<(String, Vec<String>)>,
    /// ASM text mode flag
    asm_text_mode: bool,
    /// Diagnostics
    pub diagnostics: Vec<crate::diagnostics::Diagnostic>,
    /// mcount locations
    mcount_loc_entries: Vec<(u64, String)>,
    /// Break/continue stack
    break_continue_stack: Vec<(String, String)>,
    /// ASM label map
    asm_label_map: HashMap<String, String>,
    /// DWARF line entries
    pub line_entries: Vec<(u64, u32)>,
    _target: crate::target::Target,
}

impl Aarch64Codegen {
    /// Create new codegen instance
    pub fn new(target: crate::target::Target) -> Self {
        Self {
            code: Vec::new(),
            rodata: Vec::new(),
            entry: 0,
            relocatable: false,
            symbols: Vec::new(),
            relocations: Vec::new(),
            rodata_relocations: Vec::new(),
            func_sizes: HashMap::new(),
            labels: HashMap::new(),
            locals: HashMap::new(),
            global_rodata: HashMap::new(),
            float_refs: Vec::new(),
            sym_index: HashMap::new(),
            stack_bytes: 0,
            frame_bytes: 0,
            lr_off: -1,
            emitted_return: false,
            source_name: String::new(),
            label_serial: 0,
            pending_branches: Vec::new(),
            pending_bcond: Vec::new(),
            pending_bl_calls: Vec::new(),
            extra_sections: Vec::new(),
            comdat_groups: Vec::new(),
            current_return_ty: None,
            aggregate_fields: HashMap::new(),
            is_current_func_variadic: false,
            va_save_base_off: 0,
            va_gp_offs: 0,
            va_flt_offs: 0,
            mte_enabled: false,
            tag_frame_size: 0,
            mte_save_off: 0,
            saved_sret_ptr: None,
            canary_slot_off: 0,
            tls_globals: HashMap::new(),
            raw_global_asm_lines: Vec::new(),
            raw_func_asm_lines: Vec::new(),
            asm_text_mode: false,
            diagnostics: Vec::new(),
            mcount_loc_entries: Vec::new(),
            break_continue_stack: Vec::new(),
            asm_label_map: HashMap::new(),
            line_entries: Vec::new(),
            _target: target,
        }
    }
    
    /// Emit raw bytes to code buffer
    pub fn emit(&mut self, bytes: &[u8]) {
        self.code.extend_from_slice(bytes);
    }
    
    /// Allocate local variable slot
    pub fn alloc_local(&mut self, name: &str, size: i32) -> i32 {
        let off = -(self.stack_bytes + size);
        self.stack_bytes += size;
        self.locals.insert(name.to_string(), off);
        off
    }
    
    /// Generate fresh label name
    pub fn fresh_label(&mut self, prefix: &str) -> String {
        let label = format!("{}{}/L{:x}", prefix, self.source_name, self.label_serial);
        self.label_serial += 1;
        label
    }
    
    /// Store w0 to stack
    fn store_w0(&mut self, off: i32) {
        self.str_wt_sp(0, off);
    }
    
    /// Load w0 from stack
    fn load_w0(&mut self, off: i32) {
        self.ldr_wt_sp(0, off);
    }
    
    /// Store w register to [sp, off]
    fn str_wt_sp(&mut self, reg: u8, off: i32) {
        let imm12 = (off / 4) as u32 & 0xfff;
        let insn = 0xb900_0000u32 | imm12 << 10 | 31 << 5 | (reg as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }
    
    /// Load w register from [sp, off]
    fn ldr_wt_sp(&mut self, reg: u8, off: i32) {
        let imm12 = (off / 4) as u32 & 0xfff;
        let insn = 0xb940_0000u32 | imm12 << 10 | 31 << 5 | (reg as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }
    
    /// Move x register with sp+offset
    fn mov_x_reg_sp_off(&mut self, reg: u8, off: i32) {
        if (0..4096).contains(&off) {
            let insn = 0x910003e0u32 | ((off as u32) << 10) | (reg as u32 & 0x1f);
            self.emit(&insn.to_le_bytes());
        } else if off < 0 && off > -4096 {
            let pos = (-off) as u32;
            let insn = 0x510003e0u32 | (pos << 10) | (reg as u32 & 0x1f);
            self.emit(&insn.to_le_bytes());
        } else {
            let tmp: u8 = 9;
            let abs = off.unsigned_abs() as u64;
            self.mov_x_reg_imm(tmp, abs);
            if off >= 0 {
                self.emit_add_x_reg_sp_reg(reg, tmp);
            } else {
                self.emit_sub_x_reg_sp_reg(reg, tmp);
            }
        }
    }
    
    /// Move x0 from sp+off
    pub fn mov_x0_sp_off(&mut self, off: i32) {
        self.mov_x_reg_sp_off(0, off);
    }
    
    /// Move immediate to w0
    fn mov_w0_imm(&mut self, imm: u32) {
        let insn = 0xd280_0000u32 | ((imm & 0xffff) << 5);
        self.emit(&insn.to_le_bytes());
    }
    
    /// Move immediate to x register
    fn mov_x_reg_imm(&mut self, reg: u8, imm: u64) {
        for shift in [0u32, 16, 32, 48] {
            let part = ((imm >> shift) & 0xffff) as u32;
            let higher_nonzero = (imm >> (shift + 16)) != 0;
            if part == 0 && shift > 0 && !higher_nonzero {
                continue;
            }
            let hw = shift / 16;
            let insn: u32 = (1 << 31)
                | (0b10 << 29)
                | (0b100101 << 23)
                | (hw << 21)
                | (part << 5)
                | (reg as u32 & 0x1f);
            self.emit(&insn.to_le_bytes());
        }
    }
    
    /// Conditional branch if w0 is zero
    fn cbz_w0(&mut self, label: &str) {
        let pos = self.code.len() as u64;
        self.pending_bcond.push((pos, label.to_string(), 0)); // CBZ = cond 0
    }
    
    /// Unconditional branch
    fn b(&mut self, label: &str) {
        let pos = self.code.len() as u64;
        self.pending_branches.push((pos, label.to_string(), BranchKind::B));
    }
    
    /// Define label at current position
    fn label(&mut self, name: &str) {
        self.labels.insert(name.to_string(), self.code.len() as u64);
    }
    
    /// Compare w0 with w14
    fn cmp_w14_zero(&mut self) {
        let insn = 0x6f00001eu32; // cmp w0, w14
        self.emit(&insn.to_le_bytes());
    }
    
    /// Conditional select
    fn csel_w0_w0_w14_ne(&mut self) {
        let insn = 0x1ac02800u32; // csel w0, w0, w14, ne
        self.emit(&insn.to_le_bytes());
    }
    
    /// Return instruction
    pub fn ret(&mut self) {
        self.emit(&[0xd65f_03c0u32.to_le_bytes()[0]]);
    }
    
    /// Emit DMB ISH barrier
    fn emit_dmb_ish(&mut self) {
        self.emit(&[0xf51ff05fu32.to_le_bytes()[0]]);
    }
    
    /// Add xN = sp + xM  
    fn emit_add_x_reg_sp_reg(&mut self, dst: u8, src: u8) {
        let insn = 0x91000000u32 | (src as u32 & 0x1f) << 16 | (dst as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }
    
    /// Sub xN = sp - xM
    fn emit_sub_x_reg_sp_reg(&mut self, dst: u8, src: u8) {
        let insn = 0x51000000u32 | (src as u32 & 0x1f) << 16 | (dst as u32 & 0x1f);
        self.emit(&insn.to_le_bytes());
    }
}
