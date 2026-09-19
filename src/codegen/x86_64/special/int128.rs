// src/codegen/x86_64/special/int128.rs
// __int128 / unsigned __int128 support for x86_64 backend
// 16-byte integers passed via rax:rdx or on stack

/// Load a __int128 value from memory into rax:rdx.
/// 
/// Emits two qword loads:
///   mov rax, [mem]      // low 64 bits
///   mov rdx, [mem+8]    // high 64 bits
/// 
/// # Arguments
/// * `cg` - Codegen context (provides emit method)
/// * `mem_addr` - Memory address base register+offset
/// * `disp_offset` - Displacement offset if using [reg + disp]
pub fn emit_int128_load(cg: &mut crate::codegen::x86_64::Codegen, mem_addr: u8, disp_offset: i32) {
    let rex_w = 0x48; // REX.W for 64-bit
    
    // mov rax, [rax + disp] (or appropriate base register)
    if disp_offset == 0 {
        cg.emit(&[rex_w, 0x8B, 0x00 | (mem_addr & 7)]); // mov rax, [rax_base]
    } else if disp_offset.abs() < 128 {
        cg.emit(&[rex_w, 0x8B, 0x40 | ((mem_addr & 7) << 3), (disp_offset as u8).to_be() as u8]);
    } else {
        cg.emit(&[rex_w, 0x8B, 0x80 | ((mem_addr & 7) << 3), 0x00]);
        cg.emit(&disp_offset.to_le_bytes());
    }
    
    // mov rdx, [rax_base + 8]
    let second_disp = if disp_offset == 0 { 8 } else { disp_offset + 8 };
    
    if second_disp == 8 {
        cg.emit(&[rex_w, 0x8B, 0x10 | ((mem_addr & 7) << 3)]); // mov rdx, [rax+8] or similar
    } else {
        // Full encoding needed
        unimplemented!("emit_int128_load complete implementation requires extraction from x86_64.rs");
    }
}

/// Store a __int128 value from rax:rdx to memory.
/// 
/// Emits two qword stores:
///   mov [mem], rax      // low 64 bits  
///   mov [mem+8], rdx    // high 64 bits
/// 
/// # Arguments
/// * `cg` - Codegen context
/// * `mem_addr` - Memory address base register
/// * `disp_offset` - Displacement offset
pub fn emit_int128_store(cg: &mut crate::codegen::x86_64::Codegen, mem_addr: u8, disp_offset: i32) {
    let rex_w = 0x48; // REX.W for 64-bit
    
    // mov [rax + disp], rax
    if disp_offset == 0 {
        cg.emit(&[rex_w, 0x89, 0x00 | (mem_addr & 7)]); // mov [rax_base], rax
    } else if disp_offset.abs() < 128 {
        cg.emit(&[rex_w, 0x89, 0x40 | ((mem_addr & 7) << 3), (disp_offset as u8).to_be() as u8]);
    } else {
        cg.emit(&[rex_w, 0x89, 0x80 | ((mem_addr & 7) << 3), 0x00]);
        cg.emit(&disp_offset.to_le_bytes());
    }
    
    // mov [rax + 8], rdx
    let second_disp = if disp_offset == 0 { 8 } else { disp_offset + 8 };
    
    if second_disp == 8 {
        cg.emit(&[rex_w, 0x89, 0x10 | ((mem_addr & 7) << 3)]); // mov [rax+8], rdx
    } else {
        unimplemented!("emit_int128_store complete implementation requires extraction from x86_64.rs");
    }
}
