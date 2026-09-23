// src/codegen/aarch64/registers/mod.rs
// AArch64 Register Utilities and Constants
// 
// AArch64 uses simple u8 values (0-31) for register indices rather than typed enums.
// This module provides constants, naming conventions, and utility functions for
// working with AArch64 registers in the code generator.
//
// Phase A Day 3 - Registers Module Extraction (9/22/2026)
// Reference: AAPCS64 (ARM Architecture Procedure Call Standard)

/// Maximum number of general-purpose registers in AArch64
pub const NUM_GPR_REGS: usize = 31;

/// Maximum number of vector/FPU registers in AArch64  
pub const NUM_SIMD_REGS: usize = 32;

// ============================================================================
// General Purpose Registers (X0-X30, XZR)
// All GPRs are accessible as both 64-bit (Xn) and 32-bit (Wn) variants
// except XZR/WZR which has no 32-bit counterpart named WZR typically
// ============================================================================

/// Zero register (reads always return 0, writes are discarded)
pub const XZR: u8 = 31;
pub const WZR: u8 = 31;

/// General-purpose register range (X0-X30)
pub const X0: u8 = 0;
pub const X1: u8 = 1;
pub const X2: u8 = 2;
pub const X3: u8 = 3;
pub const X4: u8 = 4;
pub const X5: u8 = 5;
pub const X6: u8 = 6;
pub const X7: u8 = 7;
pub const X8: u8 = 8;

/// Temporary register used by some runtime libraries
pub const X8_TMP: u8 = 8;

pub const X9: u8 = 9;
pub const X10: u8 = 10;
pub const X11: u8 = 11;
pub const X12: u8 = 12;
pub const X13: u8 = 13;
pub const X14: u8 = 14;
pub const X15: u8 = 15;
pub const X16: u8 = 16;

/// Link register (stores return address after BL instruction)
pub const LR: u8 = 16;
pub const X16_LR: u8 = 16;
pub const W16_LR: u8 = 16;

pub const X17: u8 = 17;
pub const X18: u8 = 18;

/// Platform Link Register (implementation-defined use on Apple platforms)
pub const PLR: u8 = 18;
pub const X18_PLR: u8 = 18;

pub const X19: u8 = 19;
pub const X20: u8 = 20;
pub const X21: u8 = 21;
pub const X22: u8 = 22;
pub const X23: u8 = 23;
pub const X24: u8 = 24;
pub const X25: u8 = 25;
pub const X26: u8 = 26;
pub const X27: u8 = 27;
pub const X28: u8 = 28;

/// Frame Pointer (often dedicated to FP or X29)
pub const FP: u8 = 29;
pub const X29_FP: u8 = 29;
pub const W29_FP: u8 = 29;

pub const X30: u8 = 30;

/// Stack Pointer
pub const SP: u8 = 31;
pub const SP_REG: u8 = 31;
pub const X31_SP: u8 = 31;
pub const W31_SP: u8 = 31;

// ============================================================================
// Vector / SIMD Registers (V0-V31)
// Also accessible as Q/H/S/D sub-registers:
// - Vn [128-bit] = Qn [16-byte] = S[n*4+3:n*4] [4-byte] = D[n*2] [8-byte]
// ============================================================================

/// Base index for vector registers (same numbering as GPRs)
pub const V0: u8 = 0;
pub const V1: u8 = 1;
pub const V2: u8 = 2;
pub const V3: u8 = 3;
pub const V4: u8 = 4;
pub const V5: u8 = 5;
pub const V6: u8 = 6;
pub const V7: u8 = 7;
pub const V8: u8 = 8;
pub const V9: u8 = 9;
pub const V10: u8 = 10;
pub const V11: u8 = 11;
pub const V12: u8 = 12;
pub const V13: u8 = 13;
pub const V14: u8 = 14;
pub const V15: u8 = 15;
pub const V16: u8 = 16;
pub const V17: u8 = 17;
pub const V18: u8 = 18;
pub const V19: u8 = 19;
pub const V20: u8 = 20;
pub const V21: u8 = 21;
pub const V22: u8 = 22;
pub const V23: u8 = 23;
pub const V24: u8 = 24;
pub const V25: u8 = 25;
pub const V26: u8 = 26;
pub const V27: u8 = 27;
pub const V28: u8 = 28;
pub const V29: u8 = 29;
pub const V30: u8 = 30;
pub const V31: u8 = 31;

// ============================================================================
// AAPCS64 Caller-Saved (Volatility) Registers
// May be modified by callee without saving/restoring
// ============================================================================

/// Caller-saved GPRs: X0-X7 (argument/result registers)
#[inline]
pub fn caller_saved_gprs() -> &'static [u8] {
    &[X0, X1, X2, X3, X4, X5, X6, X7]
}

/// Caller-saved SIMD registers: V0-V7 (argument/result registers)
#[inline]
pub fn caller_saved_simd() -> &'static [u8] {
    &[V0, V1, V2, V3, V4, V5, V6, V7]
}

// ============================================================================
// AAPCS64 Callee-Saved (Preserved) Registers  
// Must be preserved by callee if modified (save at entry, restore before return)
// ============================================================================

/// Callee-saved GPRs: X19-X28 (excluding X29-FP and X30-LR from strict requirement)
#[inline]
pub fn callee_saved_gprs() -> &'static [u8] {
    &[X19, X20, X21, X22, X23, X24, X25, X26, X27, X28]
}

/// Callee-saved special registers: LR (X30) and FP (X29)
#[inline]
pub fn callee_saved_special() -> &'static [u8] {
    &[LR, FP]
}

/// All callee-saved GPRs including LR and FP
#[inline]
pub fn all_callee_saved_gprs() -> &'static [u8] {
    &[X19, X20, X21, X22, X23, X24, X25, X26, X27, X28, LR, FP]
}

/// Callee-saved SIMD registers: V8-V15
#[inline]
pub fn callee_saved_simd() -> &'static [u8] {
    &[V8, V9, V10, V11, V12, V13, V14, V15]
}

// ============================================================================
// Argument Passing Registers (AAPCS64)
// First 8 integer args: X0-X7
// First 8 floating-point args: V0-V7
// ============================================================================

/// Integer argument registers
#[inline]
pub fn arg_regs_gp() -> &'static [u8] {
    &[X0, X1, X2, X3, X4, X5, X6, X7]
}

/// Floating-point/vector argument registers  
#[inline]
pub fn arg_regs_fp() -> &'static [u8] {
    &[V0, V1, V2, V3, V4, V5, V6, V7]
}

/// Number of integer argument registers
#[inline]
pub fn num_arg_regs_gp() -> usize {
    8
}

/// Number of floating-point argument registers
#[inline]
pub fn num_arg_regs_fp() -> usize {
    8
}

// ============================================================================
// Return Value Registers (AAPCS64)
// Integer/pointer: X0-X1
// Floating-point: V0-V1
// ============================================================================

/// Integer return register (first value)
#[inline]
pub fn ret_reg_gp() -> u8 {
    X0
}

/// Integer return register (second value for aggregates)
#[inline]
pub fn ret_reg_gp_2() -> u8 {
    X1
}

/// Floating-point return register (first value)
#[inline]
pub fn ret_reg_fp() -> u8 {
    V0
}

/// Floating-point return register (second value)
#[inline]
pub fn ret_reg_fp_2() -> u8 {
    V1
}

// ============================================================================
// Utility Functions
// ============================================================================

/// Check if a register is a valid GPR index (0-30) or zero register (31)
#[inline]
pub fn is_valid_gpr(reg: u8) -> bool {
    reg <= 31
}

/// Check if a register is a valid vector register (0-31)
#[inline]
pub fn is_valid_simd(reg: u8) -> bool {
    reg <= 31
}

/// Check if register is XZR/WZR (zero register)
#[inline]
pub fn is_zero_register(reg: u8) -> bool {
    reg == XZR
}

/// Check if register is SP (stack pointer)
#[inline]
pub fn is_stack_pointer(reg: u8) -> bool {
    reg == SP
}

/// Check if register is LR (link register)
#[inline]
pub fn is_link_register(reg: u8) -> bool {
    reg == LR
}

/// Check if register is FP (frame pointer)
#[inline]
pub fn is_frame_pointer(reg: u8) -> bool {
    reg == FP
}

/// Get register name string for debugging
pub fn reg_name_gpr(reg: u8) -> &'static str {
    // Note: XZR=WZR=SP=31, LR=X16_LR=16, FP=X29_FP=29 are aliases
    match reg {
        0 => "x0",
        1 => "x1",
        2 => "x2",
        3 => "x3",
        4 => "x4",
        5 => "x5",
        6 => "x6",
        7 => "x7",
        8 => "x8",
        9 => "x9",
        10 => "x10",
        11 => "x11",
        12 => "x12",
        13 => "x13",
        14 => "x14",
        15 => "x15",
        16 => "lr/x16",  // Also LR, X16_LR
        17 => "x17",
        18 => "x18",      // Also PLR, X18_PLR
        19 => "x19",
        20 => "x20",
        21 => "x21",
        22 => "x22",
        23 => "x23",
        24 => "x24",
        25 => "x25",
        26 => "x26",
        27 => "x27",
        28 => "x28",
        29 => "fp/x29",   // Also FP, X29_FP
        30 => "x30",
        31 => "sp/xzr",   // SP or zero register (context determines)
        _ => "unknown",
    }
}

/// Get vector register name for debugging
pub fn reg_name_simd(reg: u8) -> &'static str {
    match reg {
        0 => "v0",
        1 => "v1",
        2 => "v2",
        3 => "v3",
        4 => "v4",
        5 => "v5",
        6 => "v6",
        7 => "v7",
        8 => "v8",
        9 => "v9",
        10 => "v10",
        11 => "v11",
        12 => "v12",
        13 => "v13",
        14 => "v14",
        15 => "v15",
        16 => "v16",
        17 => "v17",
        18 => "v18",
        19 => "v19",
        20 => "v20",
        21 => "v21",
        22 => "v22",
        23 => "v23",
        24 => "v24",
        25 => "v25",
        26 => "v26",
        27 => "v27",
        28 => "v28",
        29 => "v29",
        30 => "v30",
        31 => "v31",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_constants() {
        assert_eq!(XZR, 31);
        assert_eq!(WZR, 31);
        assert_eq!(SP, 31);
        assert_eq!(LR, 16);
        assert_eq!(FP, 29);
        assert_eq!(X0, 0);
        assert_eq!(V0, 0);
    }

    #[test]
    fn test_caller_saved_registers() {
        let caller_saved = caller_saved_gprs();
        assert_eq!(caller_saved.len(), 8);
        assert_eq!(caller_saved[0], X0);
        assert_eq!(caller_saved[7], X7);
        
        let caller_simd = caller_saved_simd();
        assert_eq!(caller_simd.len(), 8);
        assert_eq!(caller_simd[0], V0);
    }

    #[test]
    fn test_callee_saved_registers() {
        let callee_saved = callee_saved_gprs();
        assert_eq!(callee_saved.len(), 10);
        assert_eq!(callee_saved[0], X19);
        
        let callee_simd = callee_saved_simd();
        assert_eq!(callee_simd.len(), 8);
        assert_eq!(callee_simd[0], V8);
    }

    #[test]
    fn test_arg_registers() {
        assert_eq!(arg_regs_gp().len(), 8);
        assert_eq!(arg_regs_fp().len(), 8);
    }

    #[test]
    fn test_return_registers() {
        assert_eq!(ret_reg_gp(), X0);
        assert_eq!(ret_reg_fp(), V0);
    }

    #[test]
    fn test_is_zero_register() {
        assert!(is_zero_register(XZR));
        assert!(!is_zero_register(X0));
    }

    #[test]
    fn test_is_valid_registers() {
        for reg in 0..=31 {
            assert!(is_valid_gpr(reg));
            assert!(is_valid_simd(reg));
        }
        assert!(!is_valid_gpr(32));
    }

    #[test]
    fn test_reg_names() {
        assert_eq!(reg_name_gpr(X0), "x0");
        assert_eq!(reg_name_gpr(LR), "lr/x16");
        assert_eq!(reg_name_gpr(SP), "sp/xzr");
        assert_eq!(reg_name_gpr(FP), "fp/x29");
        assert_eq!(reg_name_simd(V0), "v0");
        assert_eq!(reg_name_gpr(99), "unknown");
    }
}
