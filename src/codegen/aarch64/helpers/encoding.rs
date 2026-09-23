// src/codegen/aarch64/helpers/encoding.rs
// ARM Instruction Encoding Utilities
//
// Provides functions for encoding AArch64 instructions into machine code.
// Handles register numbers, immediate values, shift operations, and branch targets.
//
// Extracted from helper functions throughout original_monolith.rs

/// Encode ARM64 ADD instruction: add xD, xN, xM
pub fn encode_add_x(dn: u8, rn: u8, rm: u8) -> u32 {
    0x91000000u32
        | (rn as u32 & 0x1f) << 16
        | (rm as u32 & 0x1f) << 12
        | (dn as u32 & 0x1f)
}

/// Encode ARM64 SUB instruction: sub xD, xN, xM
pub fn encode_sub_x(dn: u8, rn: u8, rm: u8) -> u32 {
    0x51000000u32
        | (rn as u32 & 0x1f) << 16
        | (rm as u32 & 0x1f) << 12
        | (dn as u32 & 0x1f)
}

/// Encode ARM64 MOV (ORR xD, xZR, xN)
pub fn encode_mov_x(dn: u8, rn: u8) -> u32 {
    0xaa0003e0u32 | (rn as u32 & 0x1f) | ((dn as u32 & 0x1f) << 5)
}

/// Encode ARM64 STR (store register): str wt, [xn, #imm12]
pub fn encode_str_wt(rn: u8, rt: u8, imm12: u32) -> u32 {
    0xb900_0000u32
        | (imm12 & 0xfff) << 10
        | (rt as u32 & 0x1f) << 5
        | (rn as u32 & 0x1f)
}

/// Encode ARM64 LDR (load register): ldr wt, [xn, #imm12]  
pub fn encode_ldr_wt(rn: u8, rt: u8, imm12: u32) -> u32 {
    0xb940_0000u32
        | (imm12 & 0xfff) << 10
        | (rt as u32 & 0x1f) << 5
        | (rn as u32 & 0x1f)
}

/// Encode ARM64 CBZ (branch if w0 is zero): cbz wn, <label>
pub fn encode_cbz(wn: u8, pc_offset: i32) -> u32 {
    // CBZ encoding: 0b10110 = 0x18, cond field varies
    0x1800_0000u32
        | (wn as u32 & 0x1f) << 5
        | (((pc_offset / 4) as i16 & 0x7ffff) as u32) & 0x1ffff
}

/// Encode ARM64 CBNZ (branch if w0 not zero)
pub fn encode_cbnz(wn: u8, pc_offset: i32) -> u32 {
    0x3500_0000u32
        | (wn as u32 & 0x1f) << 5
        | (((pc_offset / 4) as i16 & 0x7ffff) as u32) & 0x1ffff
}

/// Encode ARM64 unconditional branch: b <label>
pub fn encode_branch(pc_offset: i32) -> u32 {
    let offset = (pc_offset / 4) as i32;
    // B instruction: 0b000101 = 0x14
    0x1400_0000u32 | ((offset >> 2) & 0x3ffffff) as u32
}

/// Encode ARM64 RET instruction
pub fn encode_ret() -> u32 {
    0xd65f_03c0u32
}

/// Encode ARM64 DMB ISH (memory barrier)
pub fn encode_dmb_ish() -> u32 {
    0xf51ff05fu32
}

/// Encode ARM64 CMP instruction
pub fn encode_cmp(rn: u8, rm: u8) -> u32 {
    0xcb00_0000u32 | (rn as u32 & 0x1f) << 16 | (rm as u32 & 0x1f) << 12
}

/// Encode ARM64 conditional select: csel xd, xn, xm, cond
pub fn encode_csel(xd: u8, xn: u8, xm: u8, cond: u8) -> u32 {
    0x1ac00000u32
        | (cond as u32 & 0xf) << 12
        | (xm as u32 & 0x1f) << 16
        | (xn as u32 & 0x1f)
        | ((xd as u32 & 0x1f) << 5)
}

/// Encode MOVZ (mov immediate to register with clear): movz wd, #imm16, lsl #shift
pub fn encode_movz_w(rd: u8, imm16: u32, shift: u32) -> u32 {
    0x5280_0000u32
        | (imm16 & 0xffff) << 5
        | (shift & 3) << 21
        | (rd as u32 & 0x1f)
}

/// Encode MOVK (movk - modify bottom bits of register): movk wd, #imm16, lsl #shift
pub fn encode_movk_w(rd: u8, imm16: u32, shift: u32) -> u32 {
    0x7280_0000u32
        | (imm16 & 0xffff) << 5
        | (shift & 3) << 21
        | (rd as u32 & 0x1f)
}

/// Encode FMOV for floating point moves
pub fn encode_fmov_s(rd: u8, rn: u8) -> u32 {
    0x1e80_4000u32 | (rd as u32 & 0x1f) | ((rn as u32 & 0x1f) << 5)
}

/// Encode LDR single copy (load double precision): ldr d0, [xn, #imm]
pub fn encode_ldr_d(rn: u8, rt: u8, imm12: u32) -> u32 {
    0xf200_0000u32
        | (imm12 & 0xfff) << 10
        | (rt as u32 & 0x1f) << 5
        | (rn as u32 & 0x1f)
}

/// Encode STR single copy (store double precision)
pub fn encode_str_d(rn: u8, rt: u8, imm12: u32) -> u32 {
    0xf240_0000u32
        | (imm12 & 0xfff) << 10
        | (rt as u32 & 0x1f) << 5
        | (rn as u32 & 0x1f)
}

/// Encode SCVTF (signed integer to float conversion): scvtf sD, wN
pub fn encode_scvtfs(dd: u8, nn: u8) -> u32 {
    0x1e260000u32 | (dd as u32 & 0x1f) | ((nn as u32 & 0x1f) << 5)
}

/// Encode FCVTZS (float to signed int truncation): fcvts wD, sN
pub fn encode_fcvtzs(dd: u8, nn: u8) -> u32 {
    0x1eb20000u32 | (dd as u32 & 0x1f) | ((nn as u32 & 0x1f) << 5)
}

/// Check if immediate fits in 12-bit unsigned
pub fn imm12_fits(imm: u32) -> bool {
    imm <= 0xfff
}

/// Shift amount for MOVZ/MOVK (0, 16, 32, or 48 bits)
#[derive(Debug, Clone, Copy)]
pub enum ShiftAmount {
    Shift0 = 0,
    Shift16 = 1,
    Shift32 = 2,
    Shift48 = 3,
}

impl ShiftAmount {
    pub fn bits(&self) -> u32 {
        match self {
            ShiftAmount::Shift0 => 0,
            ShiftAmount::Shift16 => 16,
            ShiftAmount::Shift32 => 32,
            ShiftAmount::Shift48 => 48,
        }
    }
    
    pub fn hw_field(&self) -> u32 {
        match self {
            ShiftAmount::Shift0 => 0,
            ShiftAmount::Shift16 => 1,
            ShiftAmount::Shift32 => 2,
            ShiftAmount::Shift48 => 3,
        }
    }
}
