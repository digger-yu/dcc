// src/codegen/x86_64/asm/emitter.rs  
// Low-level x86_64 assembly instruction encoding utilities
// Extracted from src/codegen/x86_64/mod.rs (Turns 36-37)
// Handles REX prefix, ModRM, SIB, displacement, and immediate value encoding

use crate::codegen::x86_64::asm::expansion::ExplicitRegKind;

/// REX prefix encoding for wide registers (r8-r15)
/// W: extension bit for 64-bit operations
/// R: extension bit for destination register  
/// X: extension bit for index register
/// B: extension bit for base register
#[inline]
pub fn encode_rex(w: bool, r: u8, x: u8, b: u8) -> u8 {
    let mut v = 0x40u8;
    if w {
        v |= 0x08; // REX.W
    }
    if r >= 8 {
        v |= 0x04; // REX.R
    }
    if x >= 8 {
        v |= 0x02; // REX.X
    }
    if b >= 8 {
        v |= 0x01; // REX.B
    }
    v
}

/// Build ModRM byte for register-to-register operands
/// reg field encodes destination, rm field encodes source
#[inline]
pub fn modrm_reg_reg(reg: u8, rm: u8) -> u8 {
    0xC0 | ((reg & 7) << 3) | (rm & 7)
}

/// Build ModRM byte for memory operand only
/// Used when opcode already specifies register in reg field
#[inline]
fn modrm_mem_only(reg: u8, has_sib: bool, need_disp: bool, disp: i32, base: u8) -> u8 {
    let mut modrm = (reg & 7) << 3;
    
    // Determine mod bits
    if !need_disp && disp == 0 {
        modrm |= 0x00; // mod=00
    } else if (-128..=127).contains(&disp) {
        modrm |= 0x40; // mod=01 (disp8)
    } else {
        modrm |= 0x80; // mod=10 (disp32)
    }
    
    if has_sib {
        modrm |= 0x04; // rm=100 (SIB follows)
    } else {
        modrm |= base & 7; // rm bits
    }
    
    modrm
}

/// Encode SIB byte for scaled indexed addressing
/// Scale bits encode multiplier (1, 2, 4, 8)
/// Index bits encode index register
/// Base bits encode base register
#[inline]
pub fn encode_sib(base: u8, index: Option<u8>, scale: u8) -> u8 {
    let mut sib = base & 7;
    
    // Index bits (bits 3-5)
    if let Some(idx) = index {
        sib |= (idx & 7) << 3;
    } else {
        sib |= 4 << 3; // No index: idx=100
    }
    
    // Scale bits (bits 6-7)
    let sc = match scale {
        2 => 1,   // scale*2
        4 => 2,   // scale*4  
        8 => 3,   // scale*8
        _ => 0,   // scale*1
    };
    sib |= sc << 6;
    
    sib
}

/// Output displacement field with automatic size selection
/// Emits disp8 (-128..+127) or disp32 (-32MB..+32MB) as needed
pub fn emit_displacement(emitter: &mut impl AsmEmitter, disp: i32) {
    if (-128..=127).contains(&disp) {
        emitter.emit_byte(disp as u8);
    } else {
        emitter.emit_bytes(&disp.to_le_bytes());
    }
}

/// Output immediate value with automatic size selection
/// Encodes 8-bit (-128..+127), 32-bit, or 64-bit immediates
pub fn emit_immediate(emitter: &mut impl AsmEmitter, imm: i128, bits: u8) {
    match bits {
        8 => {
            // Sign-extend to i8 range check
            if (-128..=127).contains(&imm) {
                emitter.emit_byte(imm as i8 as u8);
            } else {
                emitter.emit_bytes(&imm.to_le_bytes());
            }
        }
        16 => {
            emitter.emit_bytes(&(imm as i16).to_le_bytes());
        }
        32 => {
            emitter.emit_bytes(&(imm as i32).to_le_bytes());
        }
        _ => {
            // 64-bit or larger
            emitter.emit_bytes(&imm.to_le_bytes());
        }
    }
}

/// Encode RIP-relative addressing for global symbols
/// ModRM = 0x05 (mod=00, rm=101), followed by disp32
pub fn encode_rip_relative(reg: u8) -> [u8; 2] {
    [(reg & 7) << 3 | 0x05, 0x00]
}

/// Emit address descriptor placeholder for relocations
/// Called before emitting instruction that requires symbol resolution
pub fn emit_address_placeholder(emitter: &mut impl AsmEmitter, size: usize) {
    emitter.emit_bytes(&vec![0u8; size]);
}

/// Get segment override prefix for explicit segment registers
#[inline]
pub fn seg_prefix(seg: u8) -> u8 {
    match seg {
        0 => 0x26, // ES
        1 => 0x2E, // CS
        2 => 0x36, // SS
        3 => 0x3E, // DS
        4 => 0x64, // FS
        5 => 0x65, // GS
        _ => 0x00,  // No prefix
    }
}

/// Check if branch is near vs far based on target distance
pub fn is_near_branch(distance: i64) -> bool {
    (-128_i64..=127_i64).contains(&distance)
}

/// Get branch instruction encoding based on condition
pub fn cc_to_jmp_encoding(condition: &str) -> u8 {
    match condition {
        "o" => 0x70, // jo
        "no" => 0x71, // jno
        "b" | "c" | "ae" => 0x72, // jb, jc, jnae
        "ae" | "nb" | "nc" => 0x73, // jae, jnb, jnc
        "e" | "z" => 0x74, // je, jz
        "ne" | "nz" => 0x75, // jne, jnz
        "be" | "na" => 0x76, // jbe, jna
        "a" | "nbe" => 0x77, // ja, jnbe
        "s" => 0x78, // js
        "ns" => 0x79, // jns
        "p" | "pe" => 0x7A, // jp, jpe
        "np" | "po" => 0x7B, // jnp, jpo
        "l" | "nge" => 0x7C, // jl, jnge
        "ge" | "nl" => 0x7D, // jge, jnl
        "le" | "ng" => 0x7E, // jle, jng
        "g" | "nle" => 0x7F, // jg, jnle
        _ => 0x90, // Default to nop (shouldn't happen)
    }
}

/// Setcc instruction encoding based on condition code
#[inline]
pub fn cc_to_setcc(code: &str) -> Option<u8> {
    let off = match code {
        "o" => 0x00,
        "no" => 0x01,
        "b" | "c" | "nae" => 0x02,
        "ae" | "nb" | "nc" => 0x03,
        "e" | "z" => 0x04,
        "ne" | "nz" => 0x05,
        "be" | "na" => 0x06,
        "a" | "nbe" => 0x07,
        "s" => 0x08,
        "ns" => 0x09,
        "p" | "pe" => 0x0A,
        "np" | "po" => 0x0B,
        "l" | "nge" => 0x0C,
        "ge" | "nl" => 0x0D,
        "le" | "ng" => 0x0E,
        "g" | "nle" => 0x0F,
        _ => return None,
    };
    Some(0x90 + off)
}

/// Generate VEX2 prefix for SIMD instructions
/// Format: C4 Lpp bb bmmmmW
pub fn encode_vex2(l: bool, pp: u8, b: u8, m: u8, w: bool) -> [u8; 2] {
    let c4 = 0xC4;
    let byte2 = ((if l { 1 } else { 0 }) << 5)
        | ((pp & 3) << 3)
        | ((b & 1) << 2)
        | m;
    let byte3 = if w { 0x80 } else { 0x00 };
    [c4, byte2, byte3]
}

/// Emit VEX-encoded instruction prefix
pub fn emit_vex_prefix(emitter: &mut impl AsmEmitter, vex_bytes: &[u8]) {
    for &byte in vex_bytes {
        emitter.emit_byte(byte);
    }
}

/// Build lock prefix for atomic instructions
#[inline]
pub fn lock_prefix() -> u8 {
    0xF0
}

/// Build REP/REPZ prefix for string instructions
#[inline]
pub fn rep_prefix(is_zero: bool) -> u8 {
    if is_zero {
        0xF3 // repz/repe
    } else {
        0xF2 // repnz
    }
}

/// Build optional prefixes for AT&T syntax instructions
/// Order: LOCK, SEGMENT, REP, OPCODE_GROUP, DATA
pub fn build_instruction_prefixes(
    lock: bool,
    segment: Option<u8>,
    rep: bool,
    rep_zero: bool,
) -> Vec<u8> {
    let mut prefixes = Vec::new();
    
    if lock {
        prefixes.push(0xF0);
    }
    
    if let Some(seg) = segment {
        prefixes.push(seg_prefix(seg));
    }
    
    if rep {
        prefixes.push(if rep_zero { 0xF3 } else { 0xF2 });
    }
    
    prefixes
}

/// Assemble complete instruction encoding sequence
/// Includes all prefixes, opcode bytes, ModRM, SIB, displacements, immediates
pub struct InstructionEncoder<'a> {
    emitter: &'a mut dyn AsmEmitter,
    pending_prefixes: Vec<u8>,
    rex_w: bool,
    rex_r: bool,
    rex_x: bool,
    rex_b: bool,
    data_size: u8, // 16/32/64
}

impl<'a> InstructionEncoder<'a> {
    pub fn new(emitter: &'a mut dyn AsmEmitter) -> Self {
        Self {
            emitter,
            pending_prefixes: Vec::new(),
            rex_w: false,
            rex_r: false,
            rex_x: false,
            rex_b: false,
            data_size: 64, // default 64-bit
        }
    }
    
    /// Set REX prefix components
    pub fn with_rex(&mut self, w: bool, r: bool, x: bool, b: bool) {
        self.rex_w = w;
        self.rex_r = r;
        self.rex_x = x;
        self.rex_b = b;
    }
    
    /// Add data size prefix (0x66 for 16-bit override in 64-bit mode)
    pub fn with_data_prefix(&mut self, size_16bit: bool) {
        if size_16bit {
            self.pending_prefixes.push(0x66);
        }
    }
    
    /// Emit instruction opcode byte(s)
    pub fn emit_opcode(&mut self, opcode: &[u8]) {
        // First emit accumulated prefixes
        for &pfx in &self.pending_prefixes {
            self.emitter.emit_byte(pfx);
        }
        
        // Emit REX prefix if needed
        if self.rex_w || self.rex_r || self.rex_x || self.rex_b {
            let rex = encode_rex(self.rex_w, 
                                 if self.rex_r { 8 } else { 0 },
                                 if self.rex_x { 8 } else { 0 },
                                 if self.rex_b { 8 } else { 0 });
            self.emitter.emit_byte(rex);
        }
        
        // Clear REX state after use
        self.rex_w = false;
        self.rex_r = false;
        self.rex_x = false;
        self.rex_b = false;
        
        // Now emit opcode bytes
        for &byte in opcode {
            self.emitter.emit_byte(byte);
        }
    }
    
    /// Emit ModRM byte
    pub fn emit_modrm(&mut self, modrm: u8) {
        self.emitter.emit_byte(modrm);
    }
    
    /// Emit SIB byte if present
    pub fn emit_sib(&mut self, sib: u8) {
        self.emitter.emit_byte(sib);
    }
    
    /// Emit displacement if required
    pub fn emit_disp(&mut self, disp: i32) {
        if disp != 0 || true { // Always emit for simplicity
            emit_displacement(self.emitter, disp);
        }
    }
    
    /// Emit immediate operand
    pub fn emit_imm(&mut self, imm: i128, size_bits: u8) {
        emit_immediate(self.emitter, imm, size_bits);
    }
}

/// Trait for objects that can receive encoded bytes
pub trait AsmEmitter {
    fn emit_byte(&mut self, byte: u8);
    fn emit_bytes(&mut self, bytes: &[u8]);
}

// Implementation for Vec<u8>
impl AsmEmitter for Vec<u8> {
    fn emit_byte(&mut self, byte: u8) {
        self.push(byte);
    }
    
    fn emit_bytes(&mut self, bytes: &[u8]) {
        self.extend_from_slice(bytes);
    }
}

/// Common instruction opcodes
pub mod opcodes {
    // MOV variants
    pub const MOV_R64_R64: &[u8] = &[0x48, 0x8B]; // mov rax, rbx
    pub const MOV_R64_M64: &[u8] = &[0x48, 0x8B]; // mov rax, [rbx]
    pub const MOV_M64_R64: &[u8] = &[0x48, 0x89]; // mov [rax], rbx
    
    // ADD variants  
    pub const ADD_R64_I32: &[u8] = &[0x48, 0x81, 0xC0]; // add rax, imm32
    pub const ADD_R64_R64: &[u8] = &[0x48, 0x01];       // add rax, rbx
    pub const ADD_M64_I32: &[u8] = &[0x48, 0x81, 0x05]; // add [rip+off], imm32
    
    // LEA (Load Effective Address)
    pub const LEA_R64_M64: &[u8] = &[0x48, 0x8D]; // lea rax, [rbx]
    
    // CMP
    pub const CMP_R64_I32: &[u8] = &[0x48, 0x83, 0x7D]; // cmp rsp, imm8
    pub const CMP_R64_R64: &[u8] = &[0x48, 0x39];       // cmp rax, rbx
    
    // Test
    pub const TEST_R64_I8: &[u8] = &[0x84];             // test al, imm8
    pub const TEST_R64_R64: &[u8] = &[0x48, 0x85];     // test rax, rbx
    
    // Jumps
    pub const JMP_REL8: u8 = 0xEB;                      // jmp short
    pub const JMP_REL32: u8 = 0xE9;                     // jmp long
    pub const CALL_REL32: &[u8] = &[0xE8, 0x00, 0x00, 0x00, 0x00]; // call rel32
    
    // Return
    pub const RET: u8 = 0xC3;                           // ret
    pub const RET_I16: &[u8] = &[0xC2, 0x00, 0x00];    // ret imm16
}