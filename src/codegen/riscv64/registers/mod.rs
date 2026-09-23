// src/codegen/riscv64/registers/mod.rs
// RISC-V 64 Register Utilities and Constants
//
// RISC-V uses simple u8 values (0-31) for register indices.
// This module provides constants, naming conventions, and utility functions for
// working with RISC-V registers in the code generator.
//
// Phase B Day 1 - Registers Module Extraction (2026/9/22)
// Reference: RISC-V SysV ABI (psABI spec)

/// Maximum number of general-purpose registers in RISC-V 64
pub const NUM_GPR_REGS: usize = 31;

/// Maximum number of FPU vector registers in RISC-V
pub const NUM_FPU_REGS: usize = 32;

/// Maximum number of VEC vector registers (RVV extension)
pub const NUM_VEC_REGS: usize = 32;

// ============================================================================
// General Purpose Registers (x0-x31)
// x0 is hardwired zero, x1 is ra (return address), x2 is sp (stack pointer)
// ============================================================================

/// Zero register (reads always return 0, writes are discarded)
pub const X0: u8 = 0;

/// Return address (called "ra" - read-after-write hazard)
pub const RA: u8 = 1;
pub const X1_RA: u8 = 1;

/// Stack pointer (called "sp")
pub const SP: u8 = 2;
pub const X2_SP: u8 = 2;

/// Temporary registers (caller-saved)
pub const T0: u8 = 5;
pub const X5_T0: u8 = 5;
pub const X5_REG_5: u8 = 5;

pub const T1: u8 = 6;
pub const X6_T1: u8 = 6;
pub const X6_REG_6: u8 = 6;

pub const T2: u8 = 7;
pub const X7_T2: u8 = 7;
pub const X7_REG_7: u8 = 7;

pub const T3: u8 = 8;
pub const X8_T3: u8 = 8;

pub const T4: u8 = 9;
pub const X9_T4: u8 = 9;

pub const T5: u8 = 10;
pub const X10_T5: u8 = 10;

pub const T6: u8 = 11;
pub const X11_T6: u8 = 11;

/// Saved registers (callee-saved)
pub const S0_FP: u8 = 8;
pub const X8_S0_FP: u8 = 8;
/// Frame Pointer alias
pub const FP: u8 = 8;
pub const X8_FP: u8 = 8;

pub const S1: u8 = 9;
pub const X9_S1: u8 = 9;

pub const A0: u8 = 10;
pub const X10_A0: u8 = 10;

pub const A1: u8 = 11;
pub const X11_A1: u8 = 11;

/// Temporary registers
pub const A2: u8 = 12;
pub const X12_A2: u8 = 12;

pub const A3: u8 = 13;
pub const X13_A3: u8 = 13;

pub const A4: u8 = 14;
pub const X14_A4: u8 = 14;

pub const A5: u8 = 15;
pub const X15_A5: u8 = 15;

pub const A6: u8 = 16;
pub const X16_A6: u8 = 16;

pub const A7: u8 = 17;
pub const X17_A7: u8 = 17;

/// Saved registers
pub const S2: u8 = 18;
pub const X18_S2: u8 = 18;

pub const S3: u8 = 19;
pub const X19_S3: u8 = 19;

pub const S4: u8 = 20;
pub const X20_S4: u8 = 20;

pub const S5: u8 = 21;
pub const X21_S5: u8 = 21;

pub const S6: u8 = 22;
pub const X22_S6: u8 = 22;

pub const S7: u8 = 23;
pub const X23_S7: u8 = 23;

pub const S8: u8 = 24;
pub const X24_S8: u8 = 24;

pub const S9: u8 = 25;
pub const X25_S9: u8 = 25;

pub const S10: u8 = 26;
pub const X26_S10: u8 = 26;

pub const S11: u8 = 27;
pub const X27_S11: u8 = 27;

/// Temporary registers
pub const T7: u8 = 28;
pub const X28_T7: u8 = 28;

pub const T8: u8 = 29;
pub const X29_T8: u8 = 29;

pub const T9: u8 = 30;
pub const X30_T9: u8 = 30;

pub const T10: u8 = 31;
pub const X31_T10: u8 = 31;

// ============================================================================
// Floating-Point Registers (ft0-ft31 / fs0-fs11)
// Same bit encoding as vector registers v0-v31
// ============================================================================

/// Base index for floating-point registers (f0-f31)
pub const FT0: u8 = 0;
pub const FS0: u8 = 8;
pub const F0: u8 = 0;
pub const F8: u8 = 8;

pub const FT1: u8 = 1;
pub const FS1: u8 = 9;
pub const F1: u8 = 1;
pub const F9: u8 = 9;

pub const FT2: u8 = 2;
pub const FS2: u8 = 10;
pub const F2: u8 = 2;
pub const F10: u8 = 10;

pub const FT3: u8 = 3;
pub const FS3: u8 = 11;
pub const F3: u8 = 3;
pub const F11: u8 = 11;

pub const FT4: u8 = 4;
pub const FS4: u8 = 12;
pub const F4: u8 = 4;
pub const F12: u8 = 12;

pub const FT5: u8 = 5;
pub const FS5: u8 = 13;
pub const F5: u8 = 5;
pub const F13: u8 = 13;

pub const FT6: u8 = 6;
pub const FS6: u8 = 14;
pub const F6: u8 = 6;
pub const F14: u8 = 14;

pub const FT7: u8 = 7;
pub const FS7: u8 = 15;
pub const F7: u8 = 7;
pub const F15: u8 = 15;

pub const F16: u8 = 16;
pub const F17: u8 = 17;
pub const F18: u8 = 18;
pub const F19: u8 = 19;
pub const F20: u8 = 20;
pub const F21: u8 = 21;
pub const F22: u8 = 22;
pub const F23: u8 = 23;
pub const F24: u8 = 24;
pub const F25: u8 = 25;
pub const F26: u8 = 26;
pub const F27: u8 = 27;
pub const F28: u8 = 28;
pub const F29: u8 = 29;
pub const F30: u8 = 30;
pub const F31: u8 = 31;

// ============================================================================
// Vector Registers (RVV Extension: v0-v31)
// Same bit encoding as floating-point registers
// ============================================================================

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
// SysV ABI Caller-Saved (Volatility) Registers
// May be modified by callee without saving/restoring
// ============================================================================

/// Caller-saved GPRs: x5-x7 (t0-t2), x10-x11 (a0-a1), x12-x17 (a2-a7), x28-x31 (t7-t10)
#[inline]
pub fn caller_saved_gprs() -> &'static [u8] {
    &[T0, T1, T2, A0, A1, A2, A3, A4, A5, A6, A7, T7, T8, T9, T10]
}

/// Caller-saved FPU registers: ft0-ft7, fa0-fa7
#[inline]
pub fn caller_saved_fpu() -> &'static [u8] {
    &[FT0, FT1, FT2, FT3, FT4, FT5, FT6, FT7, F16, F17, F18, F19, F20, F21, F22, F23]
}

// ============================================================================
// SysV ABI Callee-Saved (Preserved) Registers  
// Must be preserved by callee if modified
// ============================================================================

/// Callee-saved GPRs: x8-x9 (s0-s1), x18-x27 (s2-s11)
#[inline]
pub fn callee_saved_gprs() -> &'static [u8] {
    &[S0_FP, S1, S2, S3, S4, S5, S6, S7, S8, S9, S10, S11]
}

/// Callee-saved FPU registers: fs0-fs7
#[inline]
pub fn callee_saved_fpu() -> &'static [u8] {
    &[FS0, FS1, FS2, FS3, FS4, FS5, FS6, FS7]
}

// ============================================================================
// Argument Passing Registers (SysV ABI)
// First 6 integer args: a0-a5 (x10-x15)
// First 8 floating-point args: fa0-fa7 (f0-f7 or f16-f23)
// ============================================================================

/// Integer argument registers (x10-x15 = a0-a5)
#[inline]
pub fn arg_regs_gp() -> &'static [u8] {
    &[A0, A1, A2, A3, A4, A5]
}

/// Floating-point argument registers (f0-f7)
#[inline]
pub fn arg_regs_fp() -> &'static [u8] {
    &[F0, F1, F2, F3, F4, F5, F6, F7]
}

/// Number of integer argument registers
#[inline]
pub fn num_arg_regs_gp() -> usize {
    6
}

/// Number of floating-point argument registers
#[inline]
pub fn num_arg_regs_fp() -> usize {
    8
}

// ============================================================================
// Return Value Registers (SysV ABI)
// Integer/pointer: a0 (x10)
// Floating-point: fa0 (f0)
// ============================================================================

/// Integer return register
#[inline]
pub fn ret_reg_gp() -> u8 {
    A0
}

/// Second integer return register
#[inline]
pub fn ret_reg_gp_2() -> u8 {
    A1
}

/// Floating-point return register
#[inline]
pub fn ret_reg_fp() -> u8 {
    F0
}

/// Second floating-point return register
#[inline]
pub fn ret_reg_fp_2() -> u8 {
    F1
}

// ============================================================================
// Utility Functions
// ============================================================================

/// Check if a register is a valid GPR index (0-31)
#[inline]
pub fn is_valid_gpr(reg: u8) -> bool {
    reg <= 31
}

/// Check if a register is a valid FPU/VEC register (0-31)
#[inline]
pub fn is_valid_fpu(reg: u8) -> bool {
    reg <= 31
}

/// Check if register is x0 (zero register)
#[inline]
pub fn is_zero_register(reg: u8) -> bool {
    reg == X0
}

/// Check if register is SP (stack pointer)
#[inline]
pub fn is_stack_pointer(reg: u8) -> bool {
    reg == SP || reg == X2_SP
}

/// Check if register is RA (return address)
#[inline]
pub fn is_return_address(reg: u8) -> bool {
    reg == RA || reg == X1_RA
}

/// Get register name string for debugging
pub fn reg_name_gpr(reg: u8) -> &'static str {
    match reg {
        0 => "x0",
        1 => "ra/x1",
        2 => "sp/x2",
        3 => "x3",
        4 => "x4",
        5 => "t0/x5",
        6 => "t1/x6",
        7 => "t2/x7",
        8 => "s0/sfp/x8",  // Also FP, S0
        9 => "s1/x9",
        10 => "a0/x10",
        11 => "a1/x11",
        12 => "a2/x12",
        13 => "a3/x13",
        14 => "a4/x14",
        15 => "a5/x15",
        16 => "a6/x16",
        17 => "a7/x17",
        18 => "s2/x18",
        19 => "s3/x19",
        20 => "s4/x20",
        21 => "s5/x21",
        22 => "s6/x22",
        23 => "s7/x23",
        24 => "s8/x24",
        25 => "s9/x25",
        26 => "s10/x26",
        27 => "s11/x27",
        28 => "t7/x28",
        29 => "t8/x29",
        30 => "t9/x30",
        31 => "t10/x31",
        _ => "unknown",
    }
}

/// Get FPU register name for debugging
pub fn reg_name_fpu(reg: u8) -> &'static str {
    match reg {
        0 => "ft0/f0",
        1 => "ft1/f1",
        2 => "ft2/f2",
        3 => "ft3/f3",
        4 => "ft4/f4",
        5 => "ft5/f5",
        6 => "ft6/f6",
        7 => "ft7/f7",
        8 => "fs0/f8",
        9 => "fs1/f9",
        10 => "fs2/f10",
        11 => "fs3/f11",
        12 => "fs4/f12",
        13 => "fs5/f13",
        14 => "fs6/f14",
        15 => "fs7/f15",
        16 => "fa0/f16",
        17 => "fa1/f17",
        18 => "fa2/f18",
        19 => "fa3/f19",
        20 => "fa4/f20",
        21 => "fa5/f21",
        22 => "fa6/f22",
        23 => "fa7/f23",
        24 => "f24",
        25 => "f25",
        26 => "f26",
        27 => "f27",
        28 => "f28",
        29 => "f29",
        30 => "f30",
        31 => "f31",
        _ => "unknown",
    }
}

/// Get vector register name for debugging (RVV)
pub fn reg_name_vec(reg: u8) -> &'static str {
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
        assert_eq!(X0, 0);
        assert_eq!(RA, 1);
        assert_eq!(SP, 2);
        assert_eq!(FP, 8);
        assert_eq!(A0, 10);
        assert_eq!(F0, 0);
        assert_eq!(V0, 0);
    }

    #[test]
    fn test_caller_saved_registers() {
        let caller_saved = caller_saved_gprs();
        assert_eq!(caller_saved.len(), 15);
        assert_eq!(caller_saved[0], T0);
        assert_eq!(caller_saved[10], A7);
        
        let caller_fpu = caller_saved_fpu();
        assert_eq!(caller_fpu.len(), 16);
    }

    #[test]
    fn test_callee_saved_registers() {
        let callee_saved = callee_saved_gprs();
        assert_eq!(callee_saved.len(), 12);
        assert_eq!(callee_saved[0], S0_FP);
        
        let callee_fpu = callee_saved_fpu();
        assert_eq!(callee_fpu.len(), 8);
    }

    #[test]
    fn test_arg_registers() {
        assert_eq!(arg_regs_gp().len(), 6);
        assert_eq!(arg_regs_fp().len(), 8);
    }

    #[test]
    fn test_return_registers() {
        assert_eq!(ret_reg_gp(), A0);
        assert_eq!(ret_reg_fp(), F0);
    }

    #[test]
    fn test_is_zero_register() {
        assert!(is_zero_register(X0));
        assert!(!is_zero_register(RA));
    }

    #[test]
    fn test_is_valid_registers() {
        for reg in 0..=31 {
            assert!(is_valid_gpr(reg));
            assert!(is_valid_fpu(reg));
        }
        assert!(!is_valid_gpr(32));
    }

    #[test]
    fn test_reg_names() {
        assert_eq!(reg_name_gpr(X0), "x0");
        assert_eq!(reg_name_gpr(RA), "ra/x1");
        assert_eq!(reg_name_gpr(SP), "sp/x2");
        assert_eq!(reg_name_gpr(FP), "s0/sfp/x8");
        assert_eq!(reg_name_gpr(A0), "a0/x10");
        assert_eq!(reg_name_fpu(F0), "ft0/f0");
        assert_eq!(reg_name_vec(V0), "v0");
        assert_eq!(reg_name_gpr(99), "unknown");
    }
}
