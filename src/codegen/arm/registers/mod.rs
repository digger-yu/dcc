// src/codegen/arm_backend/registers/mod.rs - ARM32 Register Definitions
// Per ARM Architecture Procedure Call Standard (AAPCS32)
//
// This module defines all ARM32 register types including:
// - General Purpose Registers (R0-R15)
// - Floating Point Registers (S0-S31)
// - Special purpose registers and flags

#![allow(dead_code)]
#![allow(unused_variables)]

/// Total number of GPRs in ARM32 (R0-R15)
pub const NUM_GPRS: usize = 16;

/// Total number of floating-point registers (S0-S31)
pub const NUM_FPRS: usize = 32;

/// Maximum integer arguments passed in registers (per AAPCS32)
pub const MAX_INREG_ARG_COUNT: usize = 4;

/// Maximum float arguments passed in registers (per AAPCS32)
pub const MAX_FPINREG_ARG_COUNT: usize = 8;

/// ARM32 General-Purpose Register enumeration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArmGpr {
    R0,   // Argument/scratch register
    R1,   // Argument/scratch register
    R2,   // Argument/scratch register
    R3,   // Argument/scratch register
    R4,   // Callee-saved
    R5,   // Callee-saved
    R6,   // Callee-saved
    R7,   // Callee-saved (often used as frame pointer in debug builds)
    R8,   // Callee-saved
    R9,   // Callee-saved
    R10,  // Callee-saved (sl: stack limit)
    R11,  // Callee-saved (fp: frame pointer)
    R12,  // Callee-saved (ip: intra-function pointer)
    R13,  // Stack pointer (sp)
    R14,  // Link register (lr)
    R15,  // Program counter (pc)
    /// Special: No register (used for imm operands or zero value)
    Zero, // Implicit zero register
}

impl ArmGpr {
    /// Convert to register index (0-15)
    pub fn index(&self) -> u8 {
        match self {
            Self::R0 => 0,
            Self::R1 => 1,
            Self::R2 => 2,
            Self::R3 => 3,
            Self::R4 => 4,
            Self::R5 => 5,
            Self::R6 => 6,
            Self::R7 => 7,
            Self::R8 => 8,
            Self::R9 => 9,
            Self::R10 => 10,
            Self::R11 => 11,
            Self::R12 => 12,
            Self::R13 => 13,
            Self::R14 => 14,
            Self::R15 => 15,
            Self::Zero => 255, // Invalid/none
        }
    }

    /// Create from index (panics if invalid)
    pub fn from_index(idx: u8) -> Option<Self> {
        match idx {
            0..=15 => Some(ArmGpr::R0 as u8 + idx),
            255 => Some(Self::Zero),
            _ => None,
        }
        .and_then(|idx| match idx {
            0 => Some(Self::R0),
            1 => Some(Self::R1),
            2 => Some(Self::R2),
            3 => Some(Self::R3),
            4 => Some(Self::R4),
            5 => Some(Self::R5),
            6 => Some(Self::R6),
            7 => Some(Self::R7),
            8 => Some(Self::R8),
            9 => Some(Self::R9),
            10 => Some(Self::R10),
            11 => Some(Self::R11),
            12 => Some(Self::R12),
            13 => Some(Self::R13),
            14 => Some(Self::R14),
            15 => Some(Self::R15),
            255 => Some(Self::Zero),
            _ => None,
        })
    }

    /// Is this register callee-saved per AAPCS32?
    pub fn is_callee_saved(&self) -> bool {
        matches!(
            self,
            Self::R4 | Self::R5 | Self::R6 | Self::R7
                | Self::R8 | Self::R9 | Self::R10 | Self::R11
                | Self::R12
        )
    }

    /// Is this a special-purpose register?
    pub fn is_special(&self) -> bool {
        matches!(self, Self::R13 | Self::R14 | Self::R15 | Self::Zero)
    }

    /// Get human-readable register name
    pub fn name(&self) -> &'static str {
        match self {
            Self::R0 => "r0",
            Self::R1 => "r1",
            Self::R2 => "r2",
            Self::R3 => "r3",
            Self::R4 => "r4",
            Self::R5 => "r5",
            Self::R6 => "r6",
            Self::R7 => "r7",
            Self::R8 => "r8",
            Self::R9 => "r9",
            Self::R10 => "r10",
            Self::R11 => "r11",
            Self::R12 => "r12",
            Self::R13 => "sp",
            Self::R14 => "lr",
            Self::R15 => "pc",
            Self::Zero => "zero",
        }
    }

    /// Is this register caller-saved?
    pub fn is_caller_saved(&self) -> bool {
        !self.is_callee_saved() && !self.is_special()
    }
}

/// ARM32 Floating-Point Register enumeration (VFPv1)
/// Single precision: S0-S31
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArmFpr {
    S0,
    S1,
    S2,
    S3,
    S4,
    S5,
    S6,
    S7,
    S8,
    S9,
    S10,
    S11,
    S12,
    S13,
    S14,
    S15,
    S16,
    S17,
    S18,
    S19,
    S20,
    S21,
    S22,
    S23,
    S24,
    S25,
    S26,
    S27,
    S28,
    S29,
    S30,
    S31,
}

impl ArmFpr {
    /// Get floating-point register index (0-31)
    pub fn index(&self) -> u8 {
        match self {
            Self::S0 => 0,
            Self::S1 => 1,
            Self::S2 => 2,
            Self::S3 => 3,
            Self::S4 => 4,
            Self::S5 => 5,
            Self::S6 => 6,
            Self::S7 => 7,
            Self::S8 => 8,
            Self::S9 => 9,
            Self::S10 => 10,
            Self::S11 => 11,
            Self::S12 => 12,
            Self::S13 => 13,
            Self::S14 => 14,
            Self::S15 => 15,
            Self::S16 => 16,
            Self::S17 => 17,
            Self::S18 => 18,
            Self::S19 => 19,
            Self::S20 => 20,
            Self::S21 => 21,
            Self::S22 => 22,
            Self::S23 => 23,
            Self::S24 => 24,
            Self::S25 => 25,
            Self::S26 => 26,
            Self::S27 => 27,
            Self::S28 => 28,
            Self::S29 => 29,
            Self::S30 => 30,
            Self::S31 => 31,
        }
    }

    /// Create from index (panics if invalid)
    pub fn from_index(idx: u8) -> Option<Self> {
        if idx <= 31 {
            Some(ArmFpr::S0 as u8 + idx).and_then(|i| match i {
                0 => Some(Self::S0),
                1 => Some(Self::S1),
                2 => Some(Self::S2),
                3 => Some(Self::S3),
                4 => Some(Self::S4),
                5 => Some(Self::S5),
                6 => Some(Self::S6),
                7 => Some(Self::S7),
                8 => Some(Self::S8),
                9 => Some(Self::S9),
                10 => Some(Self::S10),
                11 => Some(Self::S11),
                12 => Some(Self::S12),
                13 => Some(Self::S13),
                14 => Some(Self::S14),
                15 => Some(Self::S15),
                16 => Some(Self::S16),
                17 => Some(Self::S17),
                18 => Some(Self::S18),
                19 => Some(Self::S19),
                20 => Some(Self::S20),
                21 => Some(Self::S21),
                22 => Some(Self::S22),
                23 => Some(Self::S23),
                24 => Some(Self::S24),
                25 => Some(Self::S25),
                26 => Some(Self::S26),
                27 => Some(Self::S27),
                28 => Some(Self::S28),
                29 => Some(Self::S29),
                30 => Some(Self::S30),
                31 => Some(Self::S31),
                _ => None,
            })
        } else {
            None
        }
    }

    /// Get human-readable register name
    pub fn name(&self) -> &'static str {
        format!("s{}", self.index())
    }

    /// Is this register callee-saved? (In AAPCS32 with VFP, D8-D15 are callee-saved)
    pub fn is_callee_saved(&self) -> bool {
        let idx = self.index();
        idx >= 8 && idx <= 15
    }

    /// Calculate double register pair start index
    pub fn as_double_start(&self) -> Option<u8> {
        let idx = self.index();
        if idx % 2 == 0 && idx < 31 {
            Some(idx / 2)
        } else {
            None
        }
    }
}

/// Condition flags (N, Z, C, V)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConditionFlags {
    pub negative: bool,
    pub zero: bool,
    pub carry: bool,
    pub overflow: bool,
}

impl ConditionFlags {
    pub fn new() -> Self {
        Self {
            negative: false,
            zero: false,
            carry: true, // Initial carry is set
            overflow: false,
        }
    }

    pub fn reset(&mut self) {
        self.negative = false;
        self.zero = false;
        self.carry = true;
        self.overflow = false;
    }

    /// Parse condition code string (EQ, NE, LT, LE, GT, GE, etc.)
    pub fn from_condition(condition: &str) -> Option<&'static str> {
        match condition {
            "EQ" | "eq" => Some("eq"),
            "NE" | "ne" => Some("ne"),
            "LT" | "lt" => Some("lt"),
            "LE" | "le" => Some("le"),
            "GT" | "gt" => Some("gt"),
            "GE" | "ge" => Some("ge"),
            "HI" | "hi" => Some("hi"),
            "LS" | "ls" => Some("ls"),
            "CC" | "cs" | "hs" => Some("cc"),
            "MI" | "mi" => Some("mi"),
            "PL" | "pl" => Some("pl"),
            "VS" | "vs" => Some("vs"),
            "VC" | "vc" => Some("vc"),
            "HV" | "hv" => Some("hv"),
            "NV" | "nv" => Some("nv"),
            "AL" | "al" => None, // Always executed
            _ => None,
        }
    }
}

impl Default for ConditionFlags {
    fn default() -> Self {
        Self::new()
    }
}

/// CPSR (Current Program Status Register) - simplified representation
#[derive(Debug, Clone)]
pub struct ArmCpsr {
    /// Condition flags
    pub flags: ConditionFlags,
    /// Current exception level
    pub current_mode: ArmExceptionMode,
    /// Interrupt disable bits
    pub irq_disabled: bool,
    pub fiq_disabled: bool,
}

impl ArmCpsr {
    pub fn new() -> Self {
        Self {
            flags: ConditionFlags::new(),
            current_mode: ArmExceptionMode::User,
            irq_disabled: false,
            fiq_disabled: false,
        }
    }
}

impl Default for ArmCpsr {
    fn default() -> Self {
        Self::new()
    }
}

/// ARM processor exception modes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmExceptionMode {
    User,      // 0xF1
    FIQ,       // 0xF2
    IRQ,       // 0xF3
    Supervisor,// 0xF4
    Abort,     // 0xF5
    Undefined, // 0xF6
    System,    // 0xF7
}

/// Barrel shifter operations
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarrelShiftOp {
    Lsl, // Logical shift left
    Lsr, // Logical shift right
    Asr, // Arithmetic shift right
    Ror, // Rotate right
    Rrx, // Rotate right extend
}

impl BarrelShiftOp {
    pub fn mnemonic(&self) -> &'static str {
        match self {
            Self::Lsl => "lsl",
            Self::Lsr => "lsr",
            Self::Asr => "asr",
            Self::Ror => "ror",
            Self::Rrx => "rrx",
        }
    }
}

/// ARM addressing modes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmAddressingMode {
    Offset,       // [reg, #offset]
    PreIndex,     // [reg, #offset]!
    PostIndex,    // [reg], #offset
    UnsignedImm,  // Immediate offset (unsigned 12-bit)
    SignedImm,    // Signed immediate offset
    Register,     // [reg, reg] (optional shift)
    RegScaled,    // [reg, reg, lsl #imm]
}

/// Load/store base register info
#[derive(Debug, Clone)]
pub struct AddressRegInfo {
    pub base_reg: ArmGpr,
    pub offset_reg: Option<ArmGpr>,
    pub offset_imm: Option<i32>,
    pub addressing_mode: ArmAddressingMode,
}

impl AddressRegInfo {
    pub fn new_offset(base: ArmGpr, offset: i32) -> Self {
        Self {
            base_reg: base,
            offset_reg: None,
            offset_imm: Some(offset),
            addressing_mode: ArmAddressingMode::Offset,
        }
    }

    pub fn new_preindex(base: ArmGpr, offset: i32) -> Self {
        Self {
            base_reg: base,
            offset_reg: None,
            offset_imm: Some(offset),
            addressing_mode: ArmAddressingMode::PreIndex,
        }
    }

    pub fn new_postindex(base: ArmGpr, offset: i32) -> Self {
        Self {
            base_reg: base,
            offset_reg: None,
            offset_imm: Some(offset),
            addressing_mode: ArmAddressingMode::PostIndex,
        }
    }
}

#[cfg(test)]
mod arm_registers_tests {
    use super::*;

    #[test]
    fn test_gpr_allocation_order() {
        assert_eq!(ArmGpr::R0.index(), 0);
        assert_eq!(ArmGpr::R15.index(), 15);
        assert_eq!(ArmGpr::from_index(0), Some(ArmGpr::R0));
        assert_eq!(ArmGpr::from_index(15), Some(ArmGpr::R15));
        assert_eq!(ArmGpr::from_index(255), Some(ArmGpr::Zero));
        assert_eq!(ArmGpr::from_index(16), None);
    }

    #[test]
    fn test_gpr_callee_saved_classification() {
        assert_eq!(ArmGpr::R0.is_callee_saved(), false);
        assert_eq!(ArmGpr::R4.is_callee_saved(), true);
        assert_eq!(ArmGpr::R11.is_callee_saved(), true);
        assert_eq!(ArmGpr::R13.is_callee_saved(), false); // Sp is caller-saved
        assert_eq!(ArmGpr::R14.is_callee_saved(), false); // Lr is caller-saved
        assert_eq!(ArmGpr::R15.is_callee_saved(), false); // PC never saved
    }

    #[test]
    fn test_fpr_index_mapping() {
        assert_eq!(ArmFpr::S0.index(), 0);
        assert_eq!(ArmFpr::S31.index(), 31);
        assert_eq!(ArmFpr::from_index(0), Some(ArmFpr::S0));
        assert_eq!(ArmFpr::from_index(31), Some(ArmFpr::S31));
        assert_eq!(ArmFpr::from_index(32), None);
    }

    #[test]
    fn test_fpr_names() {
        assert_eq!(ArmFpr::S0.name(), "s0");
        assert_eq!(ArmFpr::S15.name(), "s15");
        assert_eq!(ArmFpr::S31.name(), "s31");
    }

    #[test]
    fn test_gpr_names() {
        assert_eq!(ArmGpr::R0.name(), "r0");
        assert_eq!(ArmGpr::R13.name(), "sp");
        assert_eq!(ArmGpr::R14.name(), "lr");
        assert_eq!(ArmGpr::R15.name(), "pc");
        assert_eq!(ArmGpr::Zero.name(), "zero");
    }

    #[test]
    fn test_callee_saved_registers() {
        // Callee-saved: R4-R12
        for i in 0..9 {
            let reg = ArmGpr::from_index(i + 4).unwrap();
            assert!(reg.is_callee_saved(), "{:?} should be callee-saved", reg);
        }
        
        // Caller-saved: R0-R3, R12 also used as ip
        for i in 0..4 {
            let reg = ArmGpr::from_index(i).unwrap();
            assert!(reg.is_caller_saved(), "{:?} should be caller-saved", reg);
        }
    }

    #[test]
    fn test_fpr_callee_saved_vfp() {
        // D8-D15 (which map to S16-S31) are callee-saved per AAPCS with VFP
        for idx in 8..=15 {
            let s_idx = idx * 2;
            if s_idx <= 31 {
                let fpr = ArmFpr::from_index(s_idx as u8).unwrap();
                assert!(fpr.is_callee_saved());
            }
        }
    }

    #[test]
    fn test_condition_codes() {
        assert_eq!(ConditionFlags::from_condition("EQ"), Some("eq"));
        assert_eq!(ConditionFlags::from_condition("NE"), Some("ne"));
        assert_eq!(ConditionFlags::from_condition("LT"), Some("lt"));
        assert_eq!(ConditionFlags::from_condition("LE"), Some("le"));
        assert_eq!(ConditionFlags::from_condition("GT"), Some("gt"));
        assert_eq!(ConditionFlags::from_condition("GE"), Some("ge"));
        assert_eq!(ConditionFlags::from_condition("AL"), None); // Always
        assert_eq!(ConditionFlags::from_condition("INVALID"), None);
    }

    #[test]
    fn test_barrel_shift_ops() {
        assert_eq!(BarrelShiftOp::Lsl.mnemonic(), "lsl");
        assert_eq!(BarrelShiftOp::Lsr.mnemonic(), "lsr");
        assert_eq!(BarrelShiftOp::Asr.mnemonic(), "asr");
        assert_eq!(BarrelShiftOp::Ror.mnemonic(), "ror");
        assert_eq!(BarrelShiftOp::Rrx.mnemonic(), "rrx");
    }

    #[test]
    fn test_addressing_modes() {
        let addr = AddressRegInfo::new_offset(ArmGpr::R0, 4);
        assert_eq!(addr.offset_imm, Some(4));
        assert_eq!(addr.addressing_mode, ArmAddressingMode::Offset);

        let addr = AddressRegInfo::new_preindex(ArmGpr::R1, -8);
        assert_eq!(addr.offset_imm, Some(-8));
        assert_eq!(addr.addressing_mode, ArmAddressingMode::PreIndex);

        let addr = AddressRegInfo::new_postindex(ArmGpr::R2, 4);
        assert_eq!(addr.offset_imm, Some(4));
        assert_eq!(addr.addressing_mode, ArmAddressingMode::PostIndex);
    }
}
