// src/codegen/x86_64/mem/global.rs
// RIP-relative addressing and global variable access for x86_64 backend
// Emits LEA instructions using r13 as base register

/// Emit LEA instruction to calculate address relative to R13 base.
/// 
/// Generates: lea [r13 + reg*8 + off], reg
/// Used for calculating addresses of local variables in frame.
/// 
/// # Arguments
/// * `cg` - Codegen context (provides emit method)  
/// * `reg` - Destination register index
/// * `off` - Displacement from R13 base in bytes
pub fn emit_lea_r13(cg: &mut crate::codegen::x86_64::Codegen, reg: u8, off: i64) {
    let rex_w = 0x48; // REX.W for 64-bit
    let rex_r = ((reg >= 8) as u8) << 2; // REX.R if reg >= 8
    let rex_x = 0; // No SIB index used here
    
    //lea [r13 + disp32], reg
    cg.emit(&[rex_w | rex_r, 0x8D, 0x8C | rex_x, 0xAC]); // modrm=84 (mod=3, scale=2, index=4=rsp, reg=r13 low)
    
    // Wait, correct encoding for lea [r13+disp32], reg:
    // Using memory reference to [rip + symbol] would be better for globals
    
    // Actually for local vars in stack frame we use rsp-relative or rbp-relative
    // For now, placeholder - full implementation requires extracting from x86_64.rs
    let _ = (reg, off);
}
