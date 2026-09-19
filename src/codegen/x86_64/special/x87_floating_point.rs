// src/codegen/x86_64/special/x87_floating_point.rs
// x87 floating point (80-bit long double) support for x86_64 backend
// DCC internally represents long double as 64-bit double in xmm, calls expect [rsp] format

/// Emit x87 FLDD instruction to load from memory into st(0).
/// 
/// Generates: fldl [mem]    // Load 64-bit double into x87 stack
/// For long double (80-bit), caller stores on stack as 64-bit + 16-byte padding
/// 
/// # Arguments
/// * `cg` - Codegen context
/// * `mem_addr` - Memory address base register  
/// * `disp_offset` - Displacement offset from base
pub fn emit_x87_fld(cg: &mut crate::codegen::x86_64::Codegen, mem_addr: u8, disp_offset: i32) {
    let rex_w = 0xF2; // Prefix for fldl (double precision)
    
    // fldl [rax + disp] or similar
    if disp_offset == 0 {
        cg.emit(&[rex_w, 0x0F, 0xE8, 0x00 | (mem_addr & 7)]);
    } else if disp_offset.abs() < 128 {
        cg.emit(&[rex_w, 0x0F, 0xE8, 0x40 | ((mem_addr & 7) << 3), (disp_offset as u8).to_be() as u8]);
    } else {
        cg.emit(&[rex_w, 0x0F, 0xE8, 0x80 | ((mem_addr & 7) << 3)]);
        cg.emit(&disp_offset.to_le_bytes());
    }
}

/// Emit x87 FSTP instruction to store from st(0) to memory.
/// 
/// Generates: fstp [mem]   // Store and pop x87 stack
/// Releases space on x87 stack after storing long double
/// 
/// # Arguments
/// * `cg` - Codegen context
/// * `mem_addr` - Memory address base register
/// * `disp_offset` - Displacement offset from base
pub fn emit_x87_fst(cg: &mut crate::codegen::x86_64::Codegen, mem_addr: u8, disp_offset: i32) {
    let rex_w = 0xF2; // Prefix for fstpl
    
    // fstp [rax + disp]
    if disp_offset == 0 {
        cg.emit(&[rex_w, 0x0F, 0xD9, 0x00 | (mem_addr & 7)]);
    } else if disp_offset.abs() < 128 {
        cg.emit(&[rex_w, 0x0F, 0xD9, 0x40 | ((mem_addr & 7) << 3), (disp_offset as u8).to_be() as u8]);
    } else {
        cg.emit(&[rex_w, 0x0F, 0xD9, 0x80 | ((mem_addr & 7) << 3)]);
        cg.emit(&disp_offset.to_le_bytes());
    }
}
