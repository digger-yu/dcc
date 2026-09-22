// src/codegen/x86_64/abi/call.rs
// SysV AMD64 calling convention implementation
// Function call setup, argument passing, and return value handling

use crate::codegen::x86_64::Codegen;
use crate::ast::{Expr, Type, ArraySize};

/// Argument location during SysV call
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ArgLoc {
    Gp(u8),        // General-purpose register (rdi, rsi, rdx, rcx, r8, r9)
    Xmm(u8),       // XMM register (xmm0-xmm7 for args)
    Stack(i64),    // Stack slot offset from frame base
}

/// Emit SysV AMD64 function call.
/// 
/// Implements caller-side function call sequence:
/// 1. Calculate register vs stack argument locations per SysV ABI rules
/// 2. Align stack to 16 bytes before call instruction
/// 3. Stage arguments in appropriate registers or spill to stack
/// 4. Execute call (direct or indirect via scratch slot)
/// 
/// # Arguments
/// * `cg` - Codegen context
/// * `direct` - Direct function name, None for indirect calls
/// * `callee` - Expression for indirect callee pointer
/// * `args` - Function argument expressions
/// * `sret_off` - Optional sret hidden pointer offset (for memory returns)
/// 
/// SysV AMD64 ABI Register Passing Rules:
/// - First 6 integer/pointer args: rdi, rsi, rdx, rcx, r8, r9
/// - First 8 SSE/vector args: xmm0-xmm7
/// - Overflow goes to stack (right-to-left)
/// - Structs > 16 bytes or mixed-class always use sret pointer
pub(crate) fn emit_sysv_call(
    cg: &mut Codegen,
    direct: Option<&str>,
    callee: Option<&Expr>,
    args: &[Expr],
    sret_off: Option<i64>,
) {
    // Placeholder: Full implementation requires moving ~1,500 lines from x86_64.rs
    // The actual emit_sysv_call function will be extracted from x86_64.rs lines 8253-~9500
    
    // Implementation details:
    // 1. Classify each argument's ABI classes using abi_classes()
    // 2. Determine gp_regs usage [7,6,2,1,8,9] = [rdi,rsi,rdx,rcx,r8,r9]
    // 3. Determine xmm_regs usage [0..7] for SSE args
    // 4. Calculate stack spill locations for overflow args
    // 5. Handle force_mem cases (aggregates, x87, complex types)
    // 6. Ensure proper 16-byte alignment before call
    // 7. Materialize arguments to staging area
    // 8. Emit call instruction (direct or indirect)
    
    let _gp_regs = [7u8, 6, 2, 1, 8, 9];
    let _frame_align = 16u8; // SysV ABI requirement
    
    // TODO: Complete implementation by extracting from x86_64.rs
    unimplemented!("extract sysv_call from x86_64.rs lines 8253-9500");
}
