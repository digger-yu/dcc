// src/codegen/mips_backend/registers/mod.rs - MIPS32 Register Definitions
// Per MIPS O32 ABI calling convention
//
// This module defines all MIPS32 register types including:
// - General Purpose Registers ($0-$31)
// - Floating Point Registers ($f0-$f31)
// - Special purpose registers and coprocessor state

#![allow(dead_code)]
#![allow(unused_variables)]

/// Total number of GPRs in MIPS (but $0 is hardcoded zero)
pub const NUM_GPRS: usize = 32;

/// Total number of floating-point registers (32 x 32-bit)
pub const NUM_FPRS: usize = 32;

/// Maximum integer arguments passed in registers (per O32 ABI)
pub const MAX_INREG_ARG_COUNT: usize = 4; // $a0-$a3

/// Maximum float arguments passed in registers (per O32 hard-float)
pub const MAX_FPINREG_ARG_COUNT: usize = 2; // $f12, $f14

/// MIPS Zero register ($0) - always reads as 0, writes are ignored
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MipsZero;

impl MipsZero {
    pub const ZERO: MipsZero = MipsZero;

    pub fn read(self) -> u32 {
        0
    }
}

/// MIPS32 General-Purpose Register enumeration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MipsGpr {
    Zero, // $0: Hard-wired zero
    At,   // $1: Assembler register
    V0,   // $2: Return value / expression
    V1,   // $3: Return value / expression
    A0,   // $4: Argument 1
    A1,   // $5: Argument 2
    A2,   // $6: Argument 3
    A3,   // $7: Argument 4
    T0,   // $8: Temporary (caller-saved)
    T1,   // $9: Temporary (caller-saved)
    T2,   // $10: Temporary (caller-saved)
    T3,   // $11: Temporary (caller-saved)
    T4,   // $12: Temporary (caller-saved)
    T5,   // $13: Temporary (caller-saved)
    T6,   // $14: Temporary (caller-saved)
    T7,   // $15: Temporary (caller-saved)
    S0,   // $16: Saved (callee-saved)
    S1,   // $17: Saved (callee-saved)
    S2,   // $18: Saved (callee-saved)
    S3,   // $19: Saved (callee-saved)
    S4,   // $20: Saved (callee-saved)
    S5,   // $21: Saved (callee-saved)
    T8,   // $24: Temp (caller-saved, often used for loop counter)
    T9,   // $25: Temp (caller-saved, often used for jump table)
    JP,   // $28: Jump pointer
    GP,   // $29: Global pointer ($gp)
    SP,   // $30: Stack pointer ($sp)
    FP,   // $31: Frame pointer ($fp)
}

impl MipsGpr {
    /// Convert to register index (0-31)
    pub fn index(&self) -> u8 {
        match self {
            Self::Zero => 0,
            Self::At => 1,
            Self::V0 => 2,
            Self::V1 => 3,
            Self::A0 => 4,
            Self::A1 => 5,
            Self::A2 => 6,
            Self::A3 => 7,
            Self::T0 => 8,
            Self::T1 => 9,
            Self::T2 => 10,
            Self::T3 => 11,
            Self::T4 => 12,
            Self::T5 => 13,
            Self::T6 => 14,
            Self::T7 => 15,
            Self::S0 => 16,
            Self::S1 => 17,
            Self::S2 => 18,
            Self::S3 => 19,
            Self::S4 => 20,
            Self::S5 => 21,
            Self::T8 => 24,
            Self::T9 => 25,
            Self::JP => 28,
            Self::GP => 29,
            Self::SP => 30,
            Self::FP => 31,
        }
    }

    /// Create from index
    pub fn from_index(idx: u8) -> Option<Self> {
        match idx {
            0 => Some(Self::Zero),
            1 => Some(Self::At),
            2 => Some(Self::V0),
            3 => Some(Self::V1),
            4 => Some(Self::A0),
            5 => Some(Self::A1),
            6 => Some(Self::A2),
            7 => Some(Self::A3),
            8 => Some(Self::T0),
            9 => Some(Self::T1),
            10 => Some(Self::T2),
            11 => Some(Self::T3),
            12 => Some(Self::T4),
            13 => Some(Self::T5),
            14 => Some(Self::T6),
            15 => Some(Self::T7),
            16 => Some(Self::S0),
            17 => Some(Self::S1),
            18 => Some(Self::S2),
            19 => Some(Self::S3),
            20 => Some(Self::S4),
            21 => Some(Self::S5),
            24 => Some(Self::T8),
            25 => Some(Self::T9),
            28 => Some(Self::JP),
            29 => Some(Self::GP),
            30 => Some(Self::SP),
            31 => Some(Self::FP),
            _ => None,
        }
    }

    /// Is this register callee-saved per O32 ABI?
    pub fn is_callee_saved(&self) -> bool {
        matches!(self, Self::S0 | Self::S1 | Self::S2 | Self::S3 | Self::S4 | Self::S5)
    }

    /// Is this a special-purpose register?
    pub fn is_special(&self) -> bool {
        matches!(self, Self::Zero | Self::At | Self::GP | Self::SP | Self::FP)
    }

    /// Get human-readable register name (shell-style: $n)
    pub fn name(&self) -> &'static str {
        match self {
            Self::Zero => "$0",
            Self::At => "$at",
            Self::V0 => "$v0",
            Self::V1 => "$v1",
            Self::A0 => "$a0",
            Self::A1 => "$a1",
            Self::A2 => "$a2",
            Self::A3 => "$a3",
            Self::T0 => "$t0",
            Self::T1 => "$t1",
            Self::T2 => "$t2",
            Self::T3 => "$t3",
            Self::T4 => "$t4",
            Self::T5 => "$t5",
            Self::T6 => "$t6",
            Self::T7 => "$t7",
            Self::S0 => "$s0",
            Self::S1 => "$s1",
            Self::S2 => "$s2",
            Self::S3 => "$s3",
            Self::S4 => "$s4",
            Self::S5 => "$s5",
            Self::T8 => "$t8",
            Self::T9 => "$t9",
            Self::JP => "$jp",
            Self::GP => "$gp",
            Self::SP => "$sp",
            Self::FP => "$fp",
        }
    }

    /// Is this register caller-saved?
    pub fn is_caller_saved(&self) -> bool {
        !self.is_callee_saved() && !self.is_special()
    }
}

/// MIPS32 Floating-Point Register enumeration
/// All registers are 32-bit single precision
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MipsFpr {
    F0,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    F13,
    F14,
    F15,
    F16,
    F17,
    F18,
    F19,
    F20,
    F21,
    F22,
    F23,
    F24,
    F25,
    F26,
    F27,
    F28,
    F29,
    F30,
    F31,
}

impl MipsFpr {
    /// Get floating-point register index (0-31)
    pub fn index(&self) -> u8 {
        match self {
            Self::F0 => 0,
            Self::F1 => 1,
            Self::F2 => 2,
            Self::F3 => 3,
            Self::F4 => 4,
            Self::F5 => 5,
            Self::F6 => 6,
            Self::F7 => 7,
            Self::F8 => 8,
            Self::F9 => 9,
            Self::F10 => 10,
            Self::F11 => 11,
            Self::F12 => 12,
            Self::F13 => 13,
            Self::F14 => 14,
            Self::F15 => 15,
            Self::F16 => 16,
            Self::F17 => 17,
            Self::F18 => 18,
            Self::F19 => 19,
            Self::F20 => 20,
            Self::F21 => 21,
            Self::F22 => 22,
            Self::F23 => 23,
            Self::F24 => 24,
            Self::F25 => 25,
            Self::F26 => 26,
            Self::F27 => 27,
            Self::F28 => 28,
            Self::F29 => 29,
            Self::F30 => 30,
            Self::F31 => 31,
        }
    }

    /// Create from index
    pub fn from_index(idx: u8) -> Option<Self> {
        if idx <= 31 {
            Some(MipsFpr::F0 as u8 + idx).and_then(|i| match i {
                0 => Some(Self::F0),
                1 => Some(Self::F1),
                2 => Some(Self::F2),
                3 => Some(Self::F3),
                4 => Some(Self::F4),
                5 => Some(Self::F5),
                6 => Some(Self::F6),
                7 => Some(Self::F7),
                8 => Some(Self::F8),
                9 => Some(Self::F9),
                10 => Some(Self::F10),
                11 => Some(Self::F11),
                12 => Some(Self::F12),
                13 => Some(Self::F13),
                14 => Some(Self::F14),
                15 => Some(Self::F15),
                16 => Some(Self::F16),
                17 => Some(Self::F17),
                18 => Some(Self::F18),
                19 => Some(Self::F19),
                20 => Some(Self::F20),
                21 => Some(Self::F21),
                22 => Some(Self::F22),
                23 => Some(Self::F23),
                24 => Some(Self::F24),
                25 => Some(Self::F25),
                26 => Some(Self::F26),
                27 => Some(Self::F27),
                28 => Some(Self::F28),
                29 => Some(Self::F29),
                30 => Some(Self::F30),
                31 => Some(Self::F31),
                _ => None,
            })
        } else {
            None
        }
    }

    /// Get human-readable register name
    pub fn name(&self) -> String {
        format!("${}", self.index())
    }

    /// In O32 ABI with hardfloat, $f12 and $f14 are used for argument passing
    pub fn is_arg_register(&self) -> bool {
        matches!(self, Self::F12 | Self::F14)
    }

    /// Calculate double register pair start index
    pub fn as_double_start(&self) -> Option<u8> {
        let idx = self.index();
        if idx.is_multiple_of(2) && idx < 31 {
            Some(idx / 2)
        } else {
            None
        }
    }
}

/// MIPS Coprocessor status flags
#[derive(Debug, Clone, Copy)]
pub struct CoprocStatus {
    /// Enable floating point
    pub fp_enabled: bool,
    /// Current exception cause
    pub cause: u32,
    /// Exception handler address
    pub epc_address: u32,
}

impl CoprocStatus {
    pub fn new() -> Self {
        Self {
            fp_enabled: true,
            cause: 0,
            epc_address: 0,
        }
    }
}

impl Default for CoprocStatus {
    fn default() -> Self {
        Self::new()
    }
}

/// MIPS HI/LO registers (for multiply/divide instructions)
#[derive(Debug, Clone, Copy)]
pub struct MipsHiLo {
    pub hi: u32,
    pub lo: u32,
}

impl MipsHiLo {
    pub fn new() -> Self {
        Self { hi: 0, lo: 0 }
    }

    pub fn reset(&mut self) {
        self.hi = 0;
        self.lo = 0;
    }
}

impl Default for MipsHiLo {
    fn default() -> Self {
        Self::new()
    }
}

/// Branch delay slot state
#[derive(Debug, Clone, Copy)]
pub struct DelaySlotState {
    pub filled: bool,
    pub instruction: Option<MipsInstruction>,
    pub has_data_dependency: bool,
}

impl DelaySlotState {
    pub fn new() -> Self {
        Self {
            filled: false,
            instruction: None,
            has_data_dependency: false,
        }
    }

    pub fn clear(&mut self) {
        self.filled = false;
        self.instruction = None;
        self.has_data_dependency = false;
    }
}

impl Default for DelaySlotState {
    fn default() -> Self {
        Self::new()
    }
}

/// MIPS instruction type (simplified)
#[derive(Debug, Clone, Copy)]
pub enum MipsInstruction {
    Nop,
    Add(MipsGpr, MipsGpr, MipsGpr), // add rd, rs, rt
    Mul(MipsGpr, MipsGpr, MipsGpr), // mul rd, rs, rt
    Lw(MipsGpr, i16, MipsGpr),      // lw rt, offset(rs)
    Sw(MipsGpr, i16, MipsGpr),      // sw rt, offset(rs)
    Beq(MipsGpr, MipsGpr, i32),     // beq rs, rt, offset
    Bne(MipsGpr, MipsGpr, i32),     // bne rs, rt, offset
    J(u32),                         // j target
    Jal(u32),                       // jal target
    /// Other instructions...
    Other,
}

impl MipsInstruction {
    /// Check if instruction has data dependency on given register
    pub fn has_dependency(&self, reg: MipsGpr) -> bool {
        match self {
            Self::Lw(dest, _, _) => dest == &reg,
            Self::Sw(_, _, src) => src == &reg,
            Self::Beq(rs, rt, _) | Self::Bne(rs, rt, _) => *rs == reg || *rt == reg,
            Self::Add(rd, rs, rt) => *rd == reg || *rs == reg || *rt == reg,
            Self::Mul(rd, rs, rt) => *rd == reg || *rs == reg || *rt == reg,
            _ => false,
        }
    }
}

#[cfg(test)]
mod mips_registers_tests {
    use super::*;

    #[test]
    fn test_gpr_index_mapping() {
        assert_eq!(MipsGpr::Zero.index(), 0);
        assert_eq!(MipsGpr::A0.index(), 4);
        assert_eq!(MipsGpr::SP.index(), 30);
        assert_eq!(MipsGpr::FP.index(), 31);
        assert_eq!(MipsGpr::from_index(0), Some(MipsGpr::Zero));
        assert_eq!(MipsGpr::from_index(31), Some(MipsGpr::FP));
    }

    #[test]
    fn test_gpr_argument_registers() {
        assert_eq!(MipsGpr::A0.index(), 4);
        assert_eq!(MipsGpr::A3.index(), 7);

        // O32 uses $a0-$a3 for up to 4 integer arguments
        for i in 0..4 {
            let reg = MipsGpr::from_index(4 + i as u8).unwrap();
            assert!(matches!(reg, MipsGpr::A0 | MipsGpr::A1 | MipsGpr::A2 | MipsGpr::A3));
        }
    }

    #[test]
    fn test_gpr_callee_saved_classification() {
        // O32 saved registers: $s0-$s5 ($16-$21)
        for i in 0..6 {
            let reg = MipsGpr::from_index(16 + i as u8).unwrap();
            assert!(reg.is_callee_saved(), "{:?} should be callee-saved", reg);
        }

        // Caller-saved: $t0-$t9 ($8-$15, $24-$25)
        for i in 0..8 {
            let reg = MipsGpr::from_index(8 + i as u8).unwrap();
            assert!(reg.is_caller_saved(), "{:?} should be caller-saved", reg);
        }
    }

    #[test]
    fn test_fpr_names() {
        assert_eq!(MipsFpr::F0.name(), "$0");
        assert_eq!(MipsFpr::F31.name(), "$31");
    }

    #[test]
    fn test_zero_register_behavior() {
        let zero = MipsZero::ZERO;
        assert_eq!(zero.read(), 0);

        // $0 cannot be written to
        assert!(MipsGpr::Zero.is_special());
        assert!(!MipsGpr::Zero.is_callee_saved());
        assert!(!MipsGpr::Zero.is_caller_saved());
    }

    #[test]
    fn test_fpr_argument_registers() {
        // O32 hardfloat: $f12, $f14 for floating-point args
        assert!(MipsFpr::F12.is_arg_register());
        assert!(MipsFpr::F14.is_arg_register());
        assert!(!MipsFpr::F0.is_arg_register());
    }

    #[test]
    fn test_delay_slot_state() {
        let mut state = DelaySlotState::new();
        assert!(!state.filled);

        // Simulate filling delay slot
        state.filled = true;
        state.instruction = Some(MipsInstruction::Nop);
        state.has_data_dependency = false;

        assert!(state.filled);

        state.clear();
        assert!(!state.filled);
    }
}
