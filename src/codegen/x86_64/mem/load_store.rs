// src/codegen/x86_64/mem/load_store.rs
// Load and store operations for x86_64 backend
// Stack register-based load/store with RIP-relative addressing

/// Store a qword (8 bytes) from register to RSP-relative stack slot.
/// 
/// Emits: mov [rsp + off], reg
/// 
/// # Arguments  
/// * `cg` - Codegen context (provides emit method)
/// * `reg` - Source register index (0=rax, 2=rdx, etc.)
/// * `off` - Stack offset in bytes from frame base
pub fn store_rsp_qword(cg: &mut crate::codegen::x86_64::Codegen, reg: u8, off: usize) {
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
pub fn load_rsp_qword(cg: &mut crate::codegen::x86_64::Codegen, reg: u8, off: usize) {
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
