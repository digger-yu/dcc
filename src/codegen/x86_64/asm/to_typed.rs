// src/codegen/x86_64/asm/to_typed.rs
// 内联汇编操作数 → 类型化 X86_64Opcode 的转换层.
//
// 方案 3: 消除"核心 codegen vs 内联汇编解析器"的第二份编码真相.
// 高频助记符 (add/mov/cmp/shl/...) 不再手写 REX/ModRM, 而是构造
// X86_64Opcode 变体, 复用统一的 encode().
//
// 设计原则:
// - 转换失败 (Seg/SegMem/无法映射的形态) 返回 None, 调用方落回原手写 match.
// - gas_text 由调用方用用户原文 gas_line_full 设置, 保证 -S 输出逐字节不变.

use crate::codegen::shared::asm_operand::Width;
use crate::codegen::x86_64::opcode::{
    AluOp, CondCode, MemOperand, Operand, ShiftAmount, ShiftOp, StackOp, UnaryOp, X86_64Opcode,
};

use super::operand::AsmOperand;

/// AsmOperand (内联汇编解析产物) → 类型化 Operand (核心 codegen 用).
///
/// None 表示无法映射 (段前缀/特殊形态), 调用方应落回手写分支.
///
/// `mnemonic_width`: 来自助记符后缀的宽度 (addl→B32, addq→B64). 内联汇编解析器
/// 的 parse_asm_reg 对所有 GPR 都返回 64 (rax/eax/ax/al 不区分), 但助记符后缀
/// 是权威尺寸来源 —— 如 `addl %rdx, %rax` 里 %rdx/%rax 实际指 eax/edx.
/// 显式用 mnemonic_width 覆盖寄存器位宽, 才能让 emit 正确生成 REX.B/W 位.
pub(crate) fn asm_to_operand(op: &AsmOperand, mnemonic_width: Option<Width>) -> Option<Operand> {
    match op {
        AsmOperand::None => None,
        AsmOperand::Reg(reg, kind) => {
            // AsmOperand::Reg(idx, kind): kind 编码寄存器类.
            // 对于 GPR, kind=64 (宽度, 单位 bit).
            // 对于非 GPR, kind 是类常量: XMM=128, YMM=129, ZMM=130, TMM=131,
            // K=132, CR=133, DR=134, MMX=135, ST=136.
            // 这些非 GPR 寄存器不由类型化编码器处理, 落回手写分支
            // (enc_mov.rs / enc_alu.rs 有专门的 MMX/XMM/x87 路径).
            if *kind != 64 {
                return None;
            }
            // 覆盖位宽: 助记符后缀是权威; 无后缀时回退到 kind (64).
            let w = mnemonic_width.unwrap_or(Width::B64);
            Some(Operand::Reg(*reg, w))
        },
        AsmOperand::Mem {
            disp,
            base,
            index,
            scale,
        } => Some(Operand::Mem(MemOperand {
            disp: *disp,
            base: Some(*base),
            index: index.map(|i| (i, *scale)),
            seg: None,
        })),
        AsmOperand::Imm(v) => Some(Operand::Imm(*v)),
        AsmOperand::Sym(s) => Some(Operand::Sym(s.clone())),
        AsmOperand::SymDisp(s, d) => Some(Operand::SymDisp(s.clone(), *d)),
        AsmOperand::Seg(_) | AsmOperand::SegMem { .. } => None,
    }
}

/// 助记符后缀 → 宽度. 如 addl→B32, addq→B64, addw→B16, addb→B8.
/// 无后缀 (add) 时由操作数宽度推断.
pub(crate) fn width_from_mnemonic(mnemonic: &str) -> Option<Width> {
    let suffix = mnemonic.chars().last()?;
    match suffix {
        'b' | 'B' => Some(Width::B8),
        'w' | 'W' => Some(Width::B16),
        'l' | 'L' => Some(Width::B32),
        'q' | 'Q' => Some(Width::B64),
        _ => None,
    }
}

/// 尝试把 (mnemonic, ops) 映射为类型化 X86_64Opcode.
///
/// 返回 None 时调用方应落回原手写 match 分支 (不影响现有行为).
///
/// 保守策略: 只对**带明确尺寸后缀**的助记符启用映射 (movq/movl/addq/addl...).
/// 裸 mov/add 等无后缀的宽度推断在内联汇编解析器里有特殊语义 (通常按 C 表达式
/// 宽度推断, 不是简单的 64 位), 直接传 B64 会与手写分支产出不同字节.
/// 这些无后缀的落回手写分支, 零风险.
pub(crate) fn try_map_typed(mnemonic: &str, ops: &[AsmOperand]) -> Option<X86_64Opcode> {
    // ---- 无尺寸后缀的指令直接处理 (jcc/call/jmp) ----
    // 注: ret 故意不在这里映射 —— emit_return_tail() 会根据编译选项
    // (-mfunction-return=thunk-extern) 把 ret 换成 jmp *__x86_return_thunk,
    // 直接映射成 Ret 会丢失该 thunk 语义 (测试 function_return_thunk 依赖).
    match mnemonic {
        "call" => {
            if ops.len() != 1 {
                return None;
            }
            let target = asm_to_operand(&ops[0], None)?;
            return Some(X86_64Opcode::Call {
                target,
                pending_rel: true,
            });
        },
        "jmp" => {
            if ops.len() != 1 {
                return None;
            }
            let target = asm_to_operand(&ops[0], None)?;
            return Some(X86_64Opcode::Jmp {
                target,
                pending_rel: true,
            });
        },
        "je" | "jz" => {
            return Some(X86_64Opcode::Jcc {
                cc: CondCode::Eq,
                pending_rel: true,
            })
        },
        "jne" | "jnz" => {
            return Some(X86_64Opcode::Jcc {
                cc: CondCode::Ne,
                pending_rel: true,
            })
        },
        "jl" | "jnge" => {
            return Some(X86_64Opcode::Jcc {
                cc: CondCode::Lt,
                pending_rel: true,
            })
        },
        "jge" | "jnl" => {
            return Some(X86_64Opcode::Jcc {
                cc: CondCode::Ge,
                pending_rel: true,
            })
        },
        "jg" | "jnle" => {
            return Some(X86_64Opcode::Jcc {
                cc: CondCode::Gt,
                pending_rel: true,
            })
        },
        "jle" | "jng" => {
            return Some(X86_64Opcode::Jcc {
                cc: CondCode::Le,
                pending_rel: true,
            })
        },
        "jb" | "jc" | "jnae" => {
            return Some(X86_64Opcode::Jcc {
                cc: CondCode::B,
                pending_rel: true,
            })
        },
        "jae" | "jnb" | "jnc" => {
            return Some(X86_64Opcode::Jcc {
                cc: CondCode::Ae,
                pending_rel: true,
            })
        },
        "ja" | "jnbe" => {
            return Some(X86_64Opcode::Jcc {
                cc: CondCode::A,
                pending_rel: true,
            })
        },
        "jbe" | "jna" => {
            return Some(X86_64Opcode::Jcc {
                cc: CondCode::Be,
                pending_rel: true,
            })
        },
        "js" => {
            return Some(X86_64Opcode::Jcc {
                cc: CondCode::S,
                pending_rel: true,
            })
        },
        "jns" => {
            return Some(X86_64Opcode::Jcc {
                cc: CondCode::Ns,
                pending_rel: true,
            })
        },
        "jp" | "jpe" => {
            return Some(X86_64Opcode::Jcc {
                cc: CondCode::P,
                pending_rel: true,
            })
        },
        "jnp" | "jpo" => {
            return Some(X86_64Opcode::Jcc {
                cc: CondCode::Np,
                pending_rel: true,
            })
        },
        "jo" => {
            return Some(X86_64Opcode::Jcc {
                cc: CondCode::O,
                pending_rel: true,
            })
        },
        "jno" => {
            return Some(X86_64Opcode::Jcc {
                cc: CondCode::No,
                pending_rel: true,
            })
        },
        _ => {},
    }

    // ---- 以下只对带明确尺寸后缀的助记符生效 (movq/movl/addw/addb...) ----
    // 裸 mov/add/sub 等无后缀的宽度推断在内联汇编解析器里有特殊语义
    // (通常按 C 表达式宽度推断), 直接传 B64 会与手写分支产出不同字节.
    let explicit_width = width_from_mnemonic(mnemonic)?;

    // 去掉尺寸后缀, 提取核心助记符 (addl → add).
    let core = mnemonic
        .strip_suffix(['b', 'w', 'l', 'q'])
        .unwrap_or(mnemonic);

    match core {
        // ---- ALU 族: add/sub/and/or/xor/cmp/test/adc/sbb ----
        // AT&T 语法: op src, dst (src 在前, dst 在后). 手写分支 emit_asm_alu(src, dst)
        // 的调用约定也是 op src,dst. AluRm { dst, src } 里 dst 是接收写入的目的寄存器,
        // 所以需要把 ops 顺序反一下: ops[1] 是 AT&T 的 dst, ops[0] 是 AT&T 的 src.
        "add" | "sub" | "and" | "or" | "xor" | "cmp" | "test" | "adc" | "sbb" => {
            if ops.len() != 2 {
                return None;
            }
            let op = alu_op(core)?;
            let at_t_src = asm_to_operand(&ops[0], Some(explicit_width))?;
            let at_t_dst = asm_to_operand(&ops[1], Some(explicit_width))?;
            let width = explicit_width;
            Some(X86_64Opcode::AluRm {
                op,
                width,
                dst: at_t_dst,
                src: at_t_src,
            })
        },

        // ---- inc/dec/not/neg ----
        "inc" | "dec" | "not" | "neg" => {
            if ops.len() != 1 {
                return None;
            }
            let op = unary_op(core)?;
            let dst = asm_to_operand(&ops[0], Some(explicit_width))?;
            let width = explicit_width;
            Some(X86_64Opcode::UnaryRm { op, width, dst })
        },

        // ---- shl/shr/sar/rol/ror/sal ----
        // AT&T 语法: shl $imm, %reg  (src=imm 在前, dst=reg 在后).
        "shl" | "shr" | "sar" | "rol" | "ror" | "sal" => {
            if ops.is_empty() || ops.len() > 2 {
                return None;
            }
            let op = shift_op(core)?;
            let width = explicit_width;
            let (amount, dst) = if ops.len() == 2 {
                // ops[0] = imm, ops[1] = reg (AT&T 顺序).
                let amt = match &ops[0] {
                    AsmOperand::Imm(v) => ShiftAmount::Imm8(*v as u8),
                    _ => return None,
                };
                let dst = asm_to_operand(&ops[1], Some(width))?;
                (amt, dst)
            } else {
                // 无操作数: shl %cl, %reg (单操作数形态).
                (ShiftAmount::Cl, asm_to_operand(&ops[0], Some(width))?)
            };
            Some(X86_64Opcode::Shift {
                op,
                width,
                dst,
                amount,
            })
        },

        // ---- lea ----
        "lea" => {
            if ops.len() != 2 {
                return None;
            }
            // LEA 恒取 64 位目的寄存器 (AT&T 语法 lea 只写 64 位地址宽).
            let dst = asm_to_operand(&ops[0], Some(Width::B64))?;
            let mem = match &ops[1] {
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                } => MemOperand {
                    disp: *disp,
                    base: Some(*base),
                    index: index.map(|i| (i, *scale)),
                    seg: None,
                },
                AsmOperand::SymDisp(_s, d) => MemOperand {
                    disp: *d,
                    base: None,
                    index: None,
                    seg: None,
                },
                _ => return None,
            };
            let rip_relative = matches!(&ops[1], AsmOperand::SymDisp(..))
                || matches!(&ops[1], AsmOperand::Mem { base, .. } if *base == 0xff);
            Some(X86_64Opcode::Lea {
                dst,
                mem,
                rip_relative,
            })
        },

        // ---- push/pop ----
        "push" | "pop" => {
            if ops.len() != 1 {
                return None;
            }
            let op = if core == "push" { StackOp::Push } else { StackOp::Pop };
            let dst = asm_to_operand(&ops[0], Some(explicit_width))?;
            let width = explicit_width;
            Some(X86_64Opcode::Stack { op, width, dst })
        },

        // ---- mov (带尺寸后缀: movq/movl/movw/movb) ----
        // AT&T 语法: mov src, dst. MovRm { dst, src }: dst 是接收写入的那个.
        "mov" => {
            if ops.len() != 2 {
                return None;
            }
            let at_t_src = asm_to_operand(&ops[0], Some(explicit_width))?;
            let at_t_dst = asm_to_operand(&ops[1], Some(explicit_width))?;
            let width = explicit_width;
            Some(X86_64Opcode::MovRm {
                width,
                dst: at_t_dst,
                src: at_t_src,
                rip_relative: false,
            })
        },

        _ => None,
    }
}

fn alu_op(core: &str) -> Option<AluOp> {
    match core {
        "add" => Some(AluOp::Add),
        "sub" => Some(AluOp::Sub),
        "and" => Some(AluOp::And),
        "or" => Some(AluOp::Or),
        "xor" => Some(AluOp::Xor),
        "cmp" => Some(AluOp::Cmp),
        "test" => Some(AluOp::Test),
        "adc" => Some(AluOp::Adc),
        "sbb" => Some(AluOp::Sbb),
        _ => None,
    }
}

fn unary_op(core: &str) -> Option<UnaryOp> {
    match core {
        "inc" => Some(UnaryOp::Inc),
        "dec" => Some(UnaryOp::Dec),
        "not" => Some(UnaryOp::Not),
        "neg" => Some(UnaryOp::Neg),
        _ => None,
    }
}

fn shift_op(core: &str) -> Option<ShiftOp> {
    match core {
        "shl" | "sal" => Some(ShiftOp::Shl),
        "shr" => Some(ShiftOp::Shr),
        "sar" => Some(ShiftOp::Sar),
        "rol" => Some(ShiftOp::Rol),
        "ror" => Some(ShiftOp::Ror),
        _ => None,
    }
}
