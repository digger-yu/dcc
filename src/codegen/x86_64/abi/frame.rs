// src/codegen/x86_64/abi/frame.rs
// Stack frame management utilities for x86_64 backend
// Frame setup, teardown, and stack manipulation

/// Subtract from RSP (adjust stack pointer)
///
/// Handles both immediate offsets and large offsets requiring temporary register.
/// Emits: sub rsp, <bytes>
///
/// # Arguments
/// * `cg` - Codegen context
/// * `bytes` - Number of bytes to subtract (must be positive)
pub fn emit_sub_rsp(cg: &mut crate::codegen::x86_64::Codegen, bytes: i64) {
    if bytes <= 0 {
        return;
    }

    // Small offset: single instruction with 32-bit displacement
    if bytes <= i32::MAX as i64 {
        cg.emit(&[0x48, 0x81, 0xEC]); // sub rax, imm32 (with REX.W)
        cg.emit(&(bytes as i32).to_le_bytes());
    } else {
        // Large offset: use scratch register (r11)
        cg.mov_imm(11, bytes as i128);
        cg.emit(&[0x4C, 0x29, 0xDC]); // sub rsp, r11
    }
}

/// Add to RSP (adjust stack pointer)  
///
/// Handles both immediate offsets and large offsets requiring temporary register.
/// Emits: add rsp, <bytes>
///
/// # Arguments
/// * `cg` - Codegen context
/// * `bytes` - Number of bytes to add (must be positive)
pub fn emit_add_rsp(cg: &mut crate::codegen::x86_64::Codegen, bytes: i64) {
    if bytes <= 0 {
        return;
    }

    // Small offset: single instruction with 32-bit displacement
    if bytes <= i32::MAX as i64 {
        cg.emit(&[0x48, 0x81, 0xC4]); // add rax, imm32 (with REX.W)
        cg.emit(&(bytes as i32).to_le_bytes());
    } else {
        // Large offset: use scratch register (r11)
        cg.mov_imm(11, bytes as i128);
        cg.emit(&[0x4C, 0x01, 0xDC]); // add rsp, r11
    }
}

/// Zero-initialize entire stack frame for security (anti-undefined-behavior).
/// Uses rep stosq for efficient bulk zeroing.
///
/// Implements GCC's -ftrivial-auto-var-init=zero semantics.
/// Preserves registers rcx, rdi, rax during operation.
///
/// # Arguments
/// * `cg` - Codegen context
/// * `frame_size` - Total size of stack frame to initialize (must be positive)
pub fn emit_zero_init_frame(cg: &mut crate::codegen::x86_64::Codegen, frame_size: i64) {
    if frame_size <= 0 {
        return;
    }

    // Save rcx, rdi, rax (caller-saved, may be used elsewhere)
    // push rax; push rcx; push rdi
    cg.emit(&[0x50]); // push rax
    cg.emit(&[0x51]); // push rcx
    cg.emit(&[0x57]); // push rdi

    // xor eax, eax (rax = 0 for stosb/stosq)
    cg.emit(&[0x31, 0xC0]);

    // mov rcx, frame_size / 8 (count for rep stosq)
    cg.emit(&[0x48, 0xB9]);
    cg.emit(&((frame_size as u64) / 8).to_le_bytes());

    // mov rdi, rsp (destination for stosq)
    cg.emit(&[0x48, 0xBF]);
    cg.emit(&cg.rsp().to_le_bytes());

    // rep stosq (rcx iterations of stosq rax, [rdi])
    cg.emit(&[0xF3, 0x48, 0xAB]); // rep stos qword ptr [rdi]

    // Restore rcx, rdi, rax
    cg.emit(&[0x5F]); // pop rdi
    cg.emit(&[0x59]); // pop rcx
    cg.emit(&[0x58]); // pop rax
}
