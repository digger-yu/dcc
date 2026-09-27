// src/codegen/x86_64/mem/load_store.rs
// Load and store operations for x86_64 backend
// Stack register-based load/store with RIP-relative addressing
// Extracted from mod.rs (Turns 38-39)

use crate::ast::*;
use crate::codegen::x86_64::Codegen;

/// Store a qword (8 bytes) from register to RSP-relative stack slot.
///
/// Emits: mov [rsp + off], reg
///
/// # Arguments  
/// * `cg` - Codegen context (provides emit method)
/// * `reg` - Source register index (0=rax, 2=rdx, etc.)
/// * `off` - Stack offset in bytes from frame base
pub fn store_rsp_qword(cg: &mut Codegen, reg: u8, off: usize) {
    let rex_w = 0x48; // REX.W = 1 for 64-bit operations

    if off == 0 {
        // mov [rsp], reg (no displacement)
        cg.emit(&[rex_w, 0x89, 0xC0 | reg]); // REX.W + mov r/m64, r64
    } else if off < 128 {
        // mov [rsp + disp8], reg (8-bit signed displacement)
        cg.emit(&[rex_w, 0x89, 0x44 | ((reg & 7) << 3), off as u8]);
    } else {
        // mov [rsp + disp32], reg (32-bit displacement)
        cg.emit(&[rex_w, 0x89, 0x84 | ((reg & 7) << 3), 0x24]);
        cg.emit(&(off as i32).to_le_bytes());
    }
}

/// Load a qword (8 bytes) from RSP-relative stack slot to register.
///
/// Emits: mov reg, [rsp + off]
///
/// # Arguments
/// * `cg` - Codegen context
/// * `reg` - Destination register index
/// * `off` - Stack offset in bytes from frame base
pub fn load_rsp_qword(cg: &mut Codegen, reg: u8, off: usize) {
    let rex_w = 0x48; // REX.W = 1 for 64-bit

    if off == 0 {
        // mov reg, [rsp] (no displacement)
        cg.emit(&[rex_w, 0x8B, 0xC0 | reg]); // REX.W + mov r64, r/m64
    } else if off < 128 {
        // mov reg, [rsp + disp8]
        cg.emit(&[rex_w, 0x8B, 0x44 | ((reg & 7) << 3), off as u8]);
    } else {
        // mov reg, [rsp + disp32]
        cg.emit(&[rex_w, 0x8B, 0x84 | ((reg & 7) << 3), 0x24]);
        cg.emit(&(off as i32).to_le_bytes());
    }
}

/// Store a word (2 bytes) to memory with given base register and offset.
///
/// Emits: mov [base + off], ax
///
/// # Arguments
/// * `cg` - Codegen context
/// * `base` - Base register (e.g., 12=r12, 13=r13, rsp=14)
/// * `off` - Displacement in bytes
/// * `src` - Source AX register implicitly (always 0 for AX)
pub fn store_word(cg: &mut Codegen, base: u8, off: usize) {
    let rex_w = 0; // No REX.W for word operations
    let rex_r = ((base >= 8) as u8) << 2;

    if off == 0 {
        cg.emit(&[rex_w | rex_r, 0x89, 0x04 | (base << 3)]); // mov [reg], ax
    } else if off < 128 {
        cg.emit(&[rex_w | rex_r, 0x89, 0x40 | (base << 3), off as u8]);
    } else {
        cg.emit(&[rex_w | rex_r, 0x89, 0x80 | (base << 3), 0xFE]);
        cg.emit(&(off as i32).to_le_bytes());
    }
}

/// Load a word (2 bytes) from memory to AX register.
///
/// Emits: mov ax, [base + off]
///
/// # Arguments
/// * `cg` - Codegen context
/// * `base` - Base register
/// * `off` - Displacement in bytes
pub fn load_word(cg: &mut Codegen, base: u8, off: usize) {
    let rex_w = 0;
    let rex_r = ((base >= 8) as u8) << 2;

    if off == 0 {
        cg.emit(&[rex_w | rex_r, 0x8B, 0x04 | (base << 3)]); // mov ax, [reg]
    } else if off < 128 {
        cg.emit(&[rex_w | rex_r, 0x8B, 0x40 | (base << 3), off as u8]);
    } else {
        cg.emit(&[rex_w | rex_r, 0x8B, 0x80 | (base << 3), 0xFE]);
        cg.emit(&(off as i32).to_le_bytes());
    }
}

/// Push a 64-bit value onto the stack (decrement RSP by 8).
///
/// Emits: push reg
///
/// # Arguments
/// * `cg` - Codegen context
/// * `reg` - Register to push
pub fn push_reg(cg: &mut Codegen, reg: u8) {
    if reg == 0 {
        // rax
        cg.emit(&[0x50]); // push rax
    } else if reg == 2 {
        // rdx
        cg.emit(&[0x52]); // push rdx
    } else if reg >= 8 {
        // push r8-r15: 0x41 0x50 + (reg-8)
        cg.emit(&[0x41, 0x50 | (reg - 8)]);
    } else {
        // push rdi, rsi, rbp, rsp, r8-r12: 0x57-0x54, 0x41 0x50-0x54
        let push_codes = [
            0, 0, 0x52, 0x54, 0x55, 0x56, 0x57, 0, 0x41, 0x41, 0x41, 0x41, 0x41,
        ];
        if (reg as usize) < push_codes.len() && push_codes[reg as usize] != 0 {
            let code = push_codes[reg as usize];
            if code >= 0x41 {
                // r8-r12 with REX prefixes
                cg.emit(&[0x41, code]);
            } else {
                cg.emit(&[code]);
            }
        } else {
            // Generic encoding for other registers
            cg.emit(&[0x50 | reg]);
        }
    }
}

/// Pop a 64-bit value from stack into register.
///
/// Emits: pop reg
///
/// # Arguments
/// * `cg` - Codegen context
/// * `reg` - Destination register
pub fn pop_reg(cg: &mut Codegen, reg: u8) {
    if reg == 0 {
        // rax
        cg.emit(&[0x58]); // pop rax
    } else if reg >= 8 {
        // pop r8-r15: 0x41 0x58 + (reg-8)
        cg.emit(&[0x41, 0x58 | (reg - 8)]);
    } else if reg == 4 {
        // rsp handled specially
        cg.emit(&[0x5C]); // pop rsp
    } else {
        cg.emit(&[0x58 | reg]); // pop other regs
    }
}

/// Align stack pointer to specified alignment (typically 16 or 32 bytes).
/// Adjusts RSP after function prologue.
///
/// # Arguments
/// * `cg` - Codegen context
/// * `alignment` - Desired alignment (e.g., 16, 32)
pub fn align_stack(_cg: &mut Codegen, alignment: usize) {
    // Mask RSP to alignment boundary
    let mask = !(alignment - 1) as i64;
    // This requires dynamic calculation based on current RSP
    // Simplified version: just ensure 16-byte alignment typically done in prologue
    let _ = mask; // Placeholder - full implementation in context of prologue
}

/// Allocate space on stack for local variables.
/// Deallocates by adjusting RSP.
///
/// # Arguments
/// * `cg` - Codegen context
/// * `bytes` - Number of bytes to allocate
pub fn alloc_stack_space(cg: &mut Codegen, bytes: usize) {
    if bytes > 0 {
        if bytes <= 128 {
            // Small allocation: sub rsp, imm8
            cg.emit(&[0x48, 0x83, 0xEC, bytes as u8]);
        } else {
            // Large allocation: sub rsp, imm32
            cg.emit(&[0x48, 0x81, 0xEC]);
            cg.emit(&(bytes as i32).to_le_bytes());
        }
    }
}

/// Deallocate stack space by adjusting RSP.
///
/// # Arguments
/// * `cg` - Codegen context
/// * `bytes` - Number of bytes to deallocate
pub fn dealloc_stack_space(cg: &mut Codegen, bytes: usize) {
    if bytes > 0 {
        if bytes <= 128 {
            cg.emit(&[0x48, 0x83, 0xC4, bytes as u8]);
        } else {
            cg.emit(&[0x48, 0x81, 0xC4]);
            cg.emit(&(bytes as i32).to_le_bytes());
        }
    }
}
