// src/codegen/aarch64/abi/frame.rs
// Stack Frame Layout Algorithms for AAPCS64
//
// Implements stack frame creation, register save areas, and calling conventions.

use super::*;
use crate::ir::ValueType;

/// Stack frame layout information
#[derive(Debug, Clone)]
pub struct FrameInfo {
    /// Total frame size (must be 16-byte aligned)
    pub size: usize,
    
    /// Local variables offset from SP
    pub locals_offset: usize,
    
    /// Register save area offset (for variadic functions)
    pub va_save_area_offset: Option<usize>,
    
    /// Callee-saved register save slot size
    pub callee_saved_save_size: usize,
    
    /// Whether frame pointer required
    pub requires_fp: bool,
}

/// Calculate stack frame layout according to AAPCS64
/// 
/// Frame layout rules:
/// - All frames must be 16-byte aligned at function entry
/// - Callee-saved registers need save area if needed
/// - Variadic functions need 128-byte register spill area
/// - Frame pointer (X29) optionally saved if frame pointer used
/// 
/// # Arguments
/// * `local_vars` - List of local variable sizes
/// * `needs_variadic` - Whether this is a variadic function
/// * `uses_frame_pointer` - Whether FP register is used
/// * `callee_saved_count` - Number of callee-saved registers to preserve
/// 
/// # Returns
/// Complete frame layout information
pub fn calculate_frame_layout(
    local_vars: &[i32],
    needs_variadic: bool,
    uses_frame_pointer: bool,
    callee_saved_count: usize,
) -> FrameInfo {
    // Calculate total local space needed
    let mut locals_total = 0usize;
    for var in local_vars.iter() {
        locals_total += (*var as usize).max(16); // Minimum 16 bytes per local
    }
    
    // Add alignment padding
    locals_total = align_to_16b(locals_total);
    
    // Add callee-saved register save area
    let callee_saved_save_size = callee_saved_count * 8;
    
    // Add frame pointer if needed
    let fp_save_size = if uses_frame_pointer { 8 } else { 0 };
    
    // Total before variadic area
    let mut size = locals_total + callee_saved_save_size + fp_save_size;
    
    // Add variadic register save area if needed (AAPCS64 §5.3)
    let mut va_save_area_offset = None;
    if needs_variadic {
        va_save_area_offset = Some(size);
        size += VA_SAVE_AREA_SIZE;
        
        // Align again after adding variadic area
        size = align_to_16b(size);
    }
    
    // Ensure final alignment
    size = align_to_16b(size);
    
    FrameInfo {
        size,
        locals_offset: 0,
        va_save_area_offset,
        callee_saved_save_size,
        requires_fp: uses_frame_pointer,
    }
}

/// Generate prologue code for function entry
/// 
/// Prologue sequence (AAPCS64 standard):
/// 1. sub sp, sp, #frame_size  (allocate frame)
/// 2. stp x29, x30, [sp, #-16]!  (save frame/link pointers)
/// 3. mov x29, sp  (setup frame pointer)
/// 4. save callee-saved registers
/// 
/// # Arguments
/// * `frame_info` - Calculated frame layout
/// * `needs_variadic` - Whether to allocate register spill area
/// * `callee_saved_regs` - List of callee-saved registers to save
/// 
/// # Returns
/// Assembly instructions for function prologue
pub fn generate_prologue(frame_info: &FrameInfo, needs_variadic: bool, callee_saved_regs: &[u8]) -> Vec<Instruction> {
    let mut prologue = Vec::new();
    
    // 1. Subtract from stack pointer
    // Must keep SP 16-byte aligned
    if frame_info.size > 0 {
        prologue.push(Instruction::SubSpImm {
            rd: Registers::SP,
            imm: frame_info.size as u128,
        });
    }
    
    // 2. Save frame pointer and link register if using FP
    if frame_info.requires_fp {
        prologue.push(Instruction::StpFPOther {
            rt1: Registers::FP,  // X29
            rt2: Registers::LR,  // X30
            rn: Registers::SP,
            imm: -16,
        });
        
        prologue.push(Instruction::Mov {
            rd: Registers::FP,
            rm: Registers::SP,
        });
    }
    
    // 3. Allocate variadic register save area first (at top of frame)
    if needs_variadic {
        // Already allocated above by subtracting from SP
        // Mark the base offset
    }
    
    // 4. Save callee-saved registers (X19-X28)
    for &reg in callee_saved_regs.iter() {
        let instr = Instruction::StrReg {
            rt: reg,
            rn: Registers::SP,
            imm: 0, // Will be filled in with proper offset
        };
        prologue.push(instr);
    }
    
    prologue
}

/// Generate epilogue code for function exit
/// 
/// Epilogue sequence (reverse of prologue):
/// 1. Load callee-saved registers
/// 2. Load frame pointer and link register
/// 3. Add to stack pointer (deallocate frame)
/// 
/// # Arguments
/// * `frame_info` - Frame layout from prologue generation
/// * `callee_saved_regs` - Same list as used in prologue
/// 
/// # Returns
/// Assembly instructions for function epilogue
pub fn generate_epilogue(frame_info: &FrameInfo, callee_saved_regs: &[u8]) -> Vec<Instruction> {
    let mut epilogue = Vec::new();
    
    // 1. Restore callee-saved registers
    for &reg in callee_saved_regs.iter().rev() { // Reverse order
        let instr = Instruction::LdrReg {
            rt: reg,
            rn: Registers::SP,
            imm: 0, // Will be filled in
        };
        epilogue.push(instr);
    }
    
    // 2. Restore frame pointer and link register
    if frame_info.requires_fp {
        epilogue.push(Instruction::LdpFPOther {
            rt1: Registers::FP,  // X29
            rt2: Registers::LR,  // X30
            rn: Registers::SP,
            imm: 0,
        });
    }
    
    // 3. Add to stack pointer (deallocating frame)
    if frame_info.size > 0 {
        epilogue.push(Instruction::AddSpImm {
            rd: Registers::SP,
            imm: frame_info.size as u128,
        });
    }
    
    epilogue
}

/// Handle variadic function argument passing
/// 
/// AAPCS64 variadic calling convention (§5.3):
/// - First 8 args in X0-X7/V0-V7 (if no overflow)
/// - Additional args on stack
/// - Need 128-byte register save area for potential overflow
/// - gp_offset tracks GP register spill usage
/// - fp_offset tracks FP register spill usage
/// 
/// # Arguments
/// * `args` - Parameter types
/// * `frame_info` - Current frame layout
/// 
/// # Returns
/// Updated frame info with variadic save area
pub fn handle_variadic_args(args: &[ValueType], frame_info: &FrameInfo) -> Option<VarArgsLayout> {
    if !frame_info.va_save_area_offset.is_some() {
        return None;
    }
    
    let va_offset = frame_info.va_save_area_offset.unwrap();
    
    // Count how many GP/FP arguments fit in registers vs overflow to save area
    let mut gp_count = 0usize;
    let mut fp_count = 0usize;
    
    for arg in args.iter() {
        match classify_type(arg) {
            ArgClass::Gp => gp_count = gp_count.saturating_add(1),
            ArgClass::Fp => fp_count = fp_count.saturating_add(1),
            ArgClass::Memory => {} // Not tracked in save area
        }
    }
    
    // Calculate offsets into save area
    let gp_save_base = va_offset;
    let fp_save_base = va_offset + 64; // After GP area
    
    VarArgsLayout {
        gp_offset: 0,
        fp_offset: 0,
        gp_save_base,
        fp_save_base,
        gp_reg_used: gp_count.min(6),  // First 6 GP regs potentially used
        fp_reg_used: fp_count.min(8),  // First 8 FP regs potentially used
    }
}

/// VA start implementation (__builtin_va_start)
/// 
/// Initialize va_list structure:
/// ```c
/// typedef struct {
///     unsigned long gp_offset;      // 4 bytes (aligned to 4B)
///     unsigned long fp_offset;      // 4 bytes (offset 4)
///     void* overflow_arg_area;      // 8 bytes (offset 8)
///     void* reg_save_area;          // 8 bytes (offset 16)
/// } __va_list[1];
/// ```
/// 
/// # Arguments
/// * `va_list_sp_offset` - Stack offset of va_list variable
/// * `frame_bytes` - Total frame size (for overflow_arg_area calculation)
/// * `va_save_base` - Base offset of register save area
/// 
/// # Returns
/// Instructions to initialize va_list
pub fn gen_va_start(va_list_sp_offset: i32, frame_bytes: usize, va_save_base: i32) -> Vec<Instruction> {
    let mut instrs = Vec::new();
    
    // gp_offset = 0
    instrs.push(Instruction::MovImmWp {
        rd: Registers::W0,
        imm: 0,
    });
    instrs.push(Instruction::StrRegOff {
        rt: Registers::X0,
        rn: Registers::SP,
        imm: va_list_sp_offset,
        size: 4,
    });
    
    // fp_offset = 0
    instrs.push(Instruction::MovImmWp {
        rd: Registers::W0,
        imm: 0,
    });
    instrs.push(Instruction::StrRegOff {
        rt: Registers::X0,
        rn: Registers::SP,
        imm: va_list_sp_offset + 4,
        size: 4,
    });
    
    // overflow_arg_area = sp + frame_bytes
    instrs.push(Instruction::AddImmX {
        rd: Registers::X9,
        rn: Registers::SP,
        imm: frame_bytes as u64,
    });
    instrs.push(Instruction::StrRegOff {
        rt: Registers::X9,
        rn: Registers::SP,
        imm: va_list_sp_offset + 8,
        size: 8,
    });
    
    // reg_save_area = sp + va_save_base
    instrs.push(Instruction::AddImmX {
        rd: Registers::X9,
        rn: Registers::SP,
        imm: va_save_base.abs() as u64,
    });
    instrs.push(Instruction::StrRegOff {
        rt: Registers::X9,
        rn: Registers::SP,
        imm: va_list_sp_offset + 16,
        size: 8,
    });
    
    instrs
}

/// VA arg next argument retrieval
/// 
/// Follows the pattern from original_monolith.rs gen_va_arg_int:
/// - Check gp_offset against budget (48 bytes for GP)
/// - If within budget: load from reg_save_area
/// - If over budget: load from overflow_arg_area and update offset
/// 
/// # Arguments
/// * `gp_offset` - Current GP register offset
/// * `budget` - Remaining budget in register save area
/// 
/// # Returns
/// Overflow status and new offset
pub fn va_arg_next(gp_offset: i32, budget: i32) -> VaArgResult {
    if gp_offset < budget {
        // From register save area
        VaArgResult::FromSaveArea { offset_in_save: gp_offset }
    } else {
        // From overflow area (would need runtime call)
        VaArgResult::FromOverflow
    }
}

/// Result of VA arg calculation
#[derive(Debug, Clone, Copy)]
pub enum VaArgResult {
    FromSaveArea { offset_in_save: i32 },
    FromOverflow,
}

/// Variable arguments layout information
#[derive(Debug, Clone)]
pub struct VarArgsLayout {
    pub gp_offset: usize,
    pub fp_offset: usize,
    pub gp_save_base: usize,
    pub fp_save_base: usize,
    pub gp_reg_used: usize,
    pub fp_reg_used: usize,
}

/// Alignment utility
fn align_to_16b(value: usize) -> usize {
    (value + 15) & !15
}

/// Register definitions
mod Registers {
    pub const X0: u8 = 0;
    pub const X1: u8 = 1;
    pub const X2: u8 = 2;
    pub const X3: u8 = 3;
    pub const X4: u8 = 4;
    pub const X5: u8 = 5;
    pub const X6: u8 = 6;
    pub const X7: u8 = 7;
    pub const X8: u8 = 8;
    pub const X9: u8 = 9;
    pub const X10: u8 = 10;
    pub const X11: u8 = 11;
    pub const X12: u8 = 12;
    pub const X13: u8 = 13;
    pub const X14: u8 = 14;
    pub const X15: u8 = 15;
    pub const X16: u8 = 16;
    pub const X17: u8 = 17;
    pub const X18: u8 = 18;
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
    pub const X29: u8 = 29;  // Frame pointer alias
    pub const X30: u8 = 30;  // Link register alias
    
    pub const W0: u8 = 0;  // Lower 32 bits of X0
    
    pub const V0: u8 = 0;
    pub const V1: u8 = 1;
    pub const V2: u8 = 2;
    pub const V3: u8 = 3;
    pub const V4: u8 = 4;
    pub const V5: u8 = 5;
    pub const V6: u8 = 6;
    pub const V7: u8 = 7;
    
    pub const FP: u8 = 29; // Frame pointer alias
    pub const LR: u8 = 30; // Link register alias
    
    pub const SP: u8 = 31;  // Stack pointer
}

/// Instruction enumeration (placeholder - will use actual IR later)
pub enum Instruction {
    Mov { rd: u8, rm: u8 },
    MovImmWp { rd: u8, imm: u32 },
    AddImmX { rd: u8, rn: u8, imm: u64 },
    StrReg { rt: u8, rn: u8, imm: i32 },
    StrRegOff { rt: u8, rn: u8, imm: i32, size: u32 },
    LdrReg { rt: u8, rn: u8, imm: i32 },
    SubSpImm { rd: u8, imm: u128 },
    AddSpImm { rd: u8, imm: u128 },
    StpFPOther { rt1: u8, rt2: u8, rn: u8, imm: i16 },
    LdpFPOther { rt1: u8, rt2: u8, rn: u8, imm: i16 },
}

impl Default for Instruction {
    fn default() -> Self {
        Instruction::Mov { rd: 0, rm: 0 }
    }
}
