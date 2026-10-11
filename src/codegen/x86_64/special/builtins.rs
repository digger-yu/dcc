//! special/builtins.rs —— 自历史完整实现（git 28a9063 单文件 Codegen）按职责簇恢复的模块。
//! 仅移动/可见性调整，不改语义（backend_refactor_plan §3.1 原则 1）。
#![allow(clippy::all)]
#![allow(unused_imports)]
use crate::ast::*;
use crate::codegen::shared::elf as shared_elf;
use crate::codegen::shared::elf::*;
use crate::codegen::shared::*;
use crate::codegen::x86_64::asm::*;
use crate::codegen::x86_64::insn::Insn;
use crate::codegen::x86_64::opcode::{SimdArithOp, SimdWidth};
use crate::codegen::x86_64::Codegen;
use crate::codegen::x86_64::*;
use crate::const_eval::{self, ConstValue};
use crate::debug::*;
use crate::diagnostics::{Diagnostic, Severity, Span};
use crate::ir::{self, *};
use crate::semantics::{self, *};
use crate::target::Target;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;
impl Codegen {
    fn emit_atomic_xadd(&mut self, sz: usize) {
        match sz {
            1 => { self.emit_insn(&Insn::raw_bytes(&[0xF0, 0x40, 0x0F, 0xC0, 0x37])); } // lock xadd [rdi], sil
            2 => { self.emit_insn(&Insn::raw_bytes(&[0x66, 0xF0, 0x0F, 0xC1, 0x37])); } // lock xadd [rdi], si
            4 => { self.emit_insn(&Insn::raw_bytes(&[0xF0, 0x0F, 0xC1, 0x37])); }       // lock xadd [rdi], esi
            _ => { self.emit_insn(&Insn::raw_bytes(&[0xF0, 0x48, 0x0F, 0xC1, 0x37])); } // lock xadd [rdi], rsi
        }
        // 零扩展 8/16 位子寄存器返回值到 64 位 rsi
        match sz {
            1 => { self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                sign: false,
                from: crate::codegen::shared::asm_operand::Width::B8,
                to: crate::codegen::shared::asm_operand::Width::B32,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(6, crate::codegen::shared::asm_operand::Width::B32),
                src: crate::codegen::x86_64::opcode::Operand::Reg(6, crate::codegen::shared::asm_operand::Width::B8),
            }, &[])); } // movzbl sil, esi
            2 => { self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                sign: false,
                from: crate::codegen::shared::asm_operand::Width::B16,
                to: crate::codegen::shared::asm_operand::Width::B32,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(6, crate::codegen::shared::asm_operand::Width::B32),
                src: crate::codegen::x86_64::opcode::Operand::Reg(6, crate::codegen::shared::asm_operand::Width::B16),
            }, &[])); } // movzwl si, esi
            _ => {},
        }
    }
    fn eval_mem_order(&self, arg: &Expr) -> i32 {
        match arg {
            Expr::Int { v, .. } => *v as i32,
            _ => 5, // default SEQ_CST for safety
        }
    }
    pub(crate) fn gen_atomic_builtin(&mut self, name: &str, args: &[Expr], dst: u8) -> bool {
        match name {
            "__atomic_fetch_add" => {
                // args: (ptr, val, order)
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7); // rdi = ptr
                self.gen_expr(&args[1], 6); // rsi = val
                                            // lock xadd [rdi], rsi (按 sz 选操作数尺寸)
                self.emit_atomic_xadd(sz);
                // lock prefix is full barrier; SEQ_CST needs explicit mfence
                let ord = self.eval_mem_order(&args[2]);
                if ord == 5 {
                    self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Mfence));
                }
                if dst != 6 {
                    self.mov(dst, 6);
                }
                true
            },
            "__atomic_fetch_sub" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7); // rdi = ptr
                self.gen_expr(&args[1], 6); // rsi = val
                                            // neg rsi (按 sz 选操作数尺寸)
                self.emit_neg(sz);
                self.emit_atomic_xadd(sz);
                let ord = self.eval_mem_order(&args[2]);
                if ord == 5 {
                    self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Mfence));
                }
                if dst != 6 {
                    self.mov(dst, 6);
                }
                true
            },
            "__atomic_fetch_and" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7);
                self.gen_expr(&args[1], 6);
                self.emit_cas_loop(sz, |cg, s| {
                    cg.emit_and_rdx_rsi(s);
                });
                let ord = self.eval_mem_order(&args[2]);
                if ord == 5 {
                    self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Mfence));
                }
                if dst != 1 {
                    self.mov(dst, 1);
                }
                true
            },
            "__atomic_fetch_or" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7);
                self.gen_expr(&args[1], 6);
                self.emit_cas_loop(sz, |cg, s| {
                    cg.emit_or_rdx_rsi(s);
                });
                let ord = self.eval_mem_order(&args[2]);
                if ord == 5 {
                    self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Mfence));
                }
                if dst != 1 {
                    self.mov(dst, 1);
                }
                true
            },
            "__atomic_fetch_xor" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7);
                self.gen_expr(&args[1], 6);
                self.emit_cas_loop(sz, |cg, s| {
                    cg.emit_xor_rdx_rsi(s);
                });
                let ord = self.eval_mem_order(&args[2]);
                if ord == 5 {
                    self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Mfence));
                }
                if dst != 1 {
                    self.mov(dst, 1);
                }
                true
            },
            "__atomic_fetch_nand" => {
                // args: (ptr, val, order)
                // new = ~(old & val) (NAND)
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7);
                self.gen_expr(&args[1], 6);
                self.emit_cas_loop(sz, |cg, s| {
                    cg.emit_and_rdx_rsi(s);
                    // P11 Bucket L: not dl/dx/edx/rdx via UnaryRm::Not.
                    let w = match s {
                        1 => crate::codegen::shared::asm_operand::Width::B8,
                        2 => crate::codegen::shared::asm_operand::Width::B16,
                        4 => crate::codegen::shared::asm_operand::Width::B32,
                        _ => crate::codegen::shared::asm_operand::Width::B64,
                    };
                    cg.emit_insn(&crate::codegen::x86_64::insn::Insn::unary_rm_reg(
                        crate::codegen::x86_64::opcode::UnaryOp::Not, w, 2,
                    ));
                });
                let ord = self.eval_mem_order(&args[2]);
                if ord == 5 {
                    self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Mfence));
                }
                if dst != 1 {
                    self.mov(dst, 1);
                }
                true
            },
            "__atomic_compare_exchange_n" => {
                // args: (ptr, expected, desired, weak, success_order, failure_order)
                if args.len() < 3 {
                    return false;
                }
                let sord = if args.len() > 4 {
                    self.eval_mem_order(&args[4])
                } else {
                    5
                };
                // SEQ_CST pre-barrier
                if sord == 5 {
                    self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Mfence));
                }
                self.gen_expr(&args[0], 7); // rdi = ptr
                self.gen_expr(&args[1], Self::EVAL_BASE); // rax = expected (cmpxchg 用 rax)
                                            // 保存 expected 到帧临时槽, 因为还需要写入
                let slot = -8 - (self.expr_depth as i32 * 8);
                {
                    let rex = 0x49u8;
                    self.emit_insn(&Insn::raw_bytes(&[rex, 0x89]));
                    if slot.abs() < 128 {
                        self.emit_insn(&Insn::raw_bytes(&[0x45, slot as i8 as u8]));
                    } else {
                        self.emit_insn(&Insn::raw_bytes(&[0x85]));
                        self.emit_insn(&Insn::raw_bytes(&slot.to_le_bytes()));
                    }
                }
                // 解引用 expected 指针, rax = *expected (实际期望值)
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B64
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0, base: Some(0), index: None, seg: None,
                        }
                    ),
                },
                &[],
            )); // mov rax, [rax]
                self.gen_expr(&args[2], 6); // rsi = desired
                                            // lock cmpxchg [rdi], rsi: 按 sz 选尺寸
                let sz = self.atomic_elem_size(args, 0);
                self.emit_lock_cmpxchg_rdi_rsi(sz);
                // ZF=1 成功(rax=old), ZF=0 失败(rax=old)
                self.emit_insn(&Insn::setcc_reg(crate::codegen::x86_64::opcode::CondCode::Eq, 2)); // sete dl
                let l_skip = self.fresh_label();
                self.jmp_rel32(&l_skip, JumpKind::Je);
                // 失败: 将 rax (当前值) 写入 [expected_ptr]
                // 把 expected 指针加载到 rsi (而非 rax, rax 保留当前值)
                {
                    let rex = 0x49u8;
                    self.emit_insn(&Insn::raw_bytes(&[rex, 0x8B]));
                    if slot.abs() < 128 {
                        self.emit_insn(&Insn::raw_bytes(&[0x75, slot as i8 as u8])); // modrm: [r13+disp8], reg=rsi
                    } else {
                        self.emit_insn(&Insn::raw_bytes(&[0xB5])); // modrm: [r13+disp32], reg=rsi
                        self.emit_insn(&Insn::raw_bytes(&slot.to_le_bytes()));
                    }
                }
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(6),
                            index: None,
                            seg: None,
                        }
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B64
                    ),
                },
                &[],
            )); // mov [rsi], rax
                self.label(&l_skip);
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                    sign: false,
                    from: crate::codegen::shared::asm_operand::Width::B8,
                    to: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(2, crate::codegen::shared::asm_operand::Width::B8),
                }, &[])); // movzx rax, dl
                                                      // SEQ_CST post-barrier
                if sord == 5 {
                    self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Mfence));
                }
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            "__atomic_compare_exchange" => {
                // Non-_n variant: desired is also a pointer (must dereference)
                // args: (ptr, expected_ptr, desired_ptr, weak, succ_order, fail_order)
                if args.len() < 3 {
                    return false;
                }
                let sord = if args.len() > 4 {
                    self.eval_mem_order(&args[4])
                } else {
                    5
                };
                if sord == 5 {
                    self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Mfence));
                }
                self.gen_expr(&args[0], 7); // rdi = ptr
                                            // First: compute desired value into rsi
                                            // (must be done BEFORE expected, since cmpxchg needs rax = expected)
                if !self.gen_lvalue_addr(&args[2], Self::EVAL_BASE) {
                    self.gen_expr(&args[2], Self::EVAL_BASE);
                }
                // rax = address of desired variable
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        6, crate::codegen::shared::asm_operand::Width::B64
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(0),
                            index: None,
                            seg: None,
                        }
                    ),
                },
                &[],
            )); // mov rsi, [rax] (rsi = desired value)
                                                // Then: compute expected value into rax (last, for cmpxchg)
                self.gen_expr(&args[1], Self::EVAL_BASE); // rax = expected_ptr
                let slot = -8 - (self.expr_depth as i32 * 8);
                // Save expected_ptr to stack slot
                {
                    let rex = 0x49u8;
                    self.emit_insn(&Insn::raw_bytes(&[rex, 0x89]));
                    if slot.abs() < 128 {
                        self.emit_insn(&Insn::raw_bytes(&[0x45, slot as i8 as u8]));
                    } else {
                        self.emit_insn(&Insn::raw_bytes(&[0x85]));
                        self.emit_insn(&Insn::raw_bytes(&slot.to_le_bytes()));
                    }
                }
                // Dereference expected: rax = *expected_ptr
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B64
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0, base: Some(0), index: None, seg: None,
                        }
                    ),
                },
                &[],
            )); // mov rax, [rax]
                                                // Now: rax = expected value, rsi = desired value, rdi = ptr
                let sz = self.atomic_elem_size(args, 0);
                self.emit_lock_cmpxchg_rdi_rsi(sz);
                self.emit_insn(&Insn::setcc_reg(crate::codegen::x86_64::opcode::CondCode::Eq, 2)); // sete dl
                let l_skip = self.fresh_label();
                self.jmp_rel32(&l_skip, JumpKind::Je);
                // Failure: write current rax to *expected_ptr
                {
                    let rex = 0x49u8;
                    self.emit_insn(&Insn::raw_bytes(&[rex, 0x8B]));
                    if slot.abs() < 128 {
                        self.emit_insn(&Insn::raw_bytes(&[0x75, slot as i8 as u8]));
                    } else {
                        self.emit_insn(&Insn::raw_bytes(&[0xB5]));
                        self.emit_insn(&Insn::raw_bytes(&slot.to_le_bytes()));
                    }
                }
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(6),
                            index: None,
                            seg: None,
                        }
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B64
                    ),
                },
                &[],
            )); // mov [rsi], rax
                self.label(&l_skip);
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                    sign: false,
                    from: crate::codegen::shared::asm_operand::Width::B8,
                    to: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(2, crate::codegen::shared::asm_operand::Width::B8),
                }, &[])); // movzx rax, dl
                if sord == 5 {
                    self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Mfence));
                }
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            "__atomic_store_n" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                let ord = self.eval_mem_order(&args[2]);
                self.gen_expr(&args[0], 7); // rdi = ptr
                self.gen_expr(&args[1], 6); // rsi = val
                match ord {
                    0 => {
                        // RELAXED: plain store
                        self.emit_mov_to_mem(sz);
                    },
                    3 | 4 => {
                        // RELEASE: compiler barrier + store
                        self.emit_mov_to_mem(sz);
                    },
                    _ => {
                        // SEQ_CST/ACQUIRE: mfence + store
                        self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Mfence)); // mfence
                        self.emit_mov_to_mem(sz);
                    },
                }
                true
            },
            "__atomic_load_n" => {
                if args.len() < 2 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                let ord = if args.len() > 1 {
                    self.eval_mem_order(&args[1])
                } else {
                    5
                };
                self.gen_expr(&args[0], 7); // rdi = ptr
                self.emit_mov_from_mem(sz, dst); // mov reg, [rdi]
                match ord {
                    0 | 3 => {}, // RELAXED/RELEASE: no barrier needed
                    _ => {
                        // ACQUIRE/SEQ_CST: mfence after load
                        self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Mfence));
                    },
                }
                true
            },
            "__atomic_exchange_n" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7); // rdi = ptr
                self.gen_expr(&args[1], 6); // rsi = val
                                            // lock xchg [rdi], rsi: 按 sz 选尺寸
                self.emit_lock_xchg_rdi_rsi(sz);
                let ord = self.eval_mem_order(&args[2]);
                if ord == 5 {
                    self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Mfence));
                }
                if dst != 6 {
                    self.mov(dst, 6);
                }
                true
            },
            // ===== __sync_* legacy atomic operations (GCC 3.x+) =====
            // These are always sequentially consistent (no memory order parameter)
            "__sync_fetch_and_add" => {
                if args.len() < 2 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7); // rdi = ptr
                self.gen_expr(&args[1], 6); // rsi = val
                self.emit_atomic_xadd(sz); // lock xadd [rdi], rsi
                                           // rsi now contains old value
                if dst != 6 {
                    self.mov(dst, 6);
                }
                true
            },
            "__sync_fetch_and_sub" => {
                if args.len() < 2 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7); // rdi = ptr
                self.gen_expr(&args[1], 6); // rsi = val
                self.emit_neg(sz); // neg rsi
                self.emit_atomic_xadd(sz); // lock xadd [rdi], rsi
                if dst != 6 {
                    self.mov(dst, 6);
                }
                true
            },
            "__sync_fetch_and_or" => {
                if args.len() < 2 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7);
                self.gen_expr(&args[1], 6);
                self.emit_cas_loop(sz, |cg, s| {
                    cg.emit_or_rdx_rsi(s);
                });
                if dst != 1 {
                    self.mov(dst, 1);
                }
                true
            },
            "__sync_fetch_and_and" => {
                if args.len() < 2 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7);
                self.gen_expr(&args[1], 6);
                self.emit_cas_loop(sz, |cg, s| {
                    cg.emit_and_rdx_rsi(s);
                });
                if dst != 1 {
                    self.mov(dst, 1);
                }
                true
            },
            "__sync_fetch_and_xor" => {
                if args.len() < 2 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7);
                self.gen_expr(&args[1], 6);
                self.emit_cas_loop(sz, |cg, s| {
                    cg.emit_xor_rdx_rsi(s);
                });
                if dst != 1 {
                    self.mov(dst, 1);
                }
                true
            },
            "__sync_fetch_and_nand" => {
                if args.len() < 2 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7);
                self.gen_expr(&args[1], 6);
                self.emit_cas_loop(sz, |cg, s| {
                    // and rdx, rsi; not rdx
                    cg.emit_and_rdx_rsi(s);
                    let w = match s {
                        1 => crate::codegen::shared::asm_operand::Width::B8,
                        2 => crate::codegen::shared::asm_operand::Width::B16,
                        4 => crate::codegen::shared::asm_operand::Width::B32,
                        _ => crate::codegen::shared::asm_operand::Width::B64,
                    };
                    cg.emit_insn(&crate::codegen::x86_64::insn::Insn::unary_rm_reg(
                        crate::codegen::x86_64::opcode::UnaryOp::Not, w, 2,
                    ));
                });
                if dst != 1 {
                    self.mov(dst, 1);
                }
                true
            },
            "__sync_add_and_fetch" => {
                if args.len() < 2 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7); // rdi = ptr
                self.gen_expr(&args[1], 6); // rsi = val
                self.emit_atomic_xadd(sz); // lock xadd [rdi], rsi
                                           // rsi = old value, [rdi] = new value
                                           // add rsi, val to get new value
                self.gen_expr(&args[1], Self::EVAL_BASE); // rax = val
                // P11: 收敛到 AluRm{Add, rsi, rax}.
                let mk_add = |sz: usize| -> crate::codegen::x86_64::opcode::X86_64Opcode {
                    use crate::codegen::shared::asm_operand::Width;
                    use crate::codegen::x86_64::opcode::{AluOp, Operand as Op};
                    let w = match sz { 1 => Width::B8, 2 => Width::B16, 4 => Width::B32, _ => Width::B64 };
                    crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                        op: AluOp::Add, width: w,
                        dst: Op::Reg(6, w), src: Op::Reg(0, w),
                    }
                };
                match sz {
                    1 => {
                        let insn = Insn::new(mk_add(1), &[]);
                        if !self.emit_insn(&insn) { self.emit_insn(&Insn::raw_bytes(&[0x40, 0x00, 0xC6])); }
                    } // add sil, al
                    2 => {
                        let insn = Insn::new(mk_add(2), &[]);
                        if !self.emit_insn(&insn) { self.emit_insn(&Insn::raw_bytes(&[0x66, 0x01, 0xC6])); }
                    } // add si, ax
                    4 => {
                        let insn = Insn::new(mk_add(4), &[]);
                        if !self.emit_insn(&insn) { self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::Add, crate::codegen::shared::asm_operand::Width::B32, 6, 0)); }
                    }       // add esi, eax
                    _ => {
                        let insn = Insn::new(mk_add(8), &[]);
                        if !self.emit_insn(&insn) { self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::Add, crate::codegen::shared::asm_operand::Width::B64, 6, 0)); }
                    } // add rsi, rax
                }
                if dst != 6 {
                    self.mov(dst, 6);
                }
                true
            },
            "__sync_sub_and_fetch" => {
                if args.len() < 2 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7); // rdi = ptr
                self.gen_expr(&args[1], 6); // rsi = val
                self.emit_neg(sz); // neg rsi
                self.emit_atomic_xadd(sz); // lock xadd [rdi], rsi
                                           // rsi = old value (negated val was added)
                                           // add rsi, val (original) to get new value
                self.gen_expr(&args[1], Self::EVAL_BASE); // rax = val
                // P11: 收敛到 AluRm{Add, rsi, rax}.
                let mk_add2 = |sz: usize| -> crate::codegen::x86_64::opcode::X86_64Opcode {
                    use crate::codegen::shared::asm_operand::Width;
                    use crate::codegen::x86_64::opcode::{AluOp, Operand as Op};
                    let w = match sz { 1 => Width::B8, 2 => Width::B16, 4 => Width::B32, _ => Width::B64 };
                    crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                        op: AluOp::Add, width: w,
                        dst: Op::Reg(6, w), src: Op::Reg(0, w),
                    }
                };
                match sz {
                    1 => {
                        let insn = Insn::new(mk_add2(1), &[]);
                        if !self.emit_insn(&insn) { self.emit_insn(&Insn::raw_bytes(&[0x40, 0x00, 0xC6])); }
                    } // add sil, al
                    2 => {
                        let insn = Insn::new(mk_add2(2), &[]);
                        if !self.emit_insn(&insn) { self.emit_insn(&Insn::raw_bytes(&[0x66, 0x01, 0xC6])); }
                    } // add si, ax
                    4 => {
                        let insn = Insn::new(mk_add2(4), &[]);
                        if !self.emit_insn(&insn) { self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::Add, crate::codegen::shared::asm_operand::Width::B32, 6, 0)); }
                    }       // add esi, eax
                    _ => {
                        let insn = Insn::new(mk_add2(8), &[]);
                        if !self.emit_insn(&insn) { self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::Add, crate::codegen::shared::asm_operand::Width::B64, 6, 0)); }
                    } // add rsi, rax
                }
                if dst != 6 {
                    self.mov(dst, 6);
                }
                true
            },
            "__sync_or_and_fetch" => {
                if args.len() < 2 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7);
                self.gen_expr(&args[1], 6);
                self.emit_cas_loop(sz, |cg, s| {
                    cg.emit_or_rdx_rsi(s);
                });
                // rdx = new value after CAS loop
                if dst != 2 {
                    self.mov(dst, 2);
                }
                true
            },
            "__sync_and_and_fetch" => {
                if args.len() < 2 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7);
                self.gen_expr(&args[1], 6);
                self.emit_cas_loop(sz, |cg, s| {
                    cg.emit_and_rdx_rsi(s);
                });
                if dst != 2 {
                    self.mov(dst, 2);
                }
                true
            },
            "__sync_xor_and_fetch" => {
                if args.len() < 2 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7);
                self.gen_expr(&args[1], 6);
                self.emit_cas_loop(sz, |cg, s| {
                    cg.emit_xor_rdx_rsi(s);
                });
                if dst != 2 {
                    self.mov(dst, 2);
                }
                true
            },
            "__sync_nand_and_fetch" => {
                if args.len() < 2 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7);
                self.gen_expr(&args[1], 6);
                self.emit_cas_loop(sz, |cg, s| {
                    cg.emit_and_rdx_rsi(s);
                    let w = match s {
                        1 => crate::codegen::shared::asm_operand::Width::B8,
                        2 => crate::codegen::shared::asm_operand::Width::B16,
                        4 => crate::codegen::shared::asm_operand::Width::B32,
                        _ => crate::codegen::shared::asm_operand::Width::B64,
                    };
                    cg.emit_insn(&crate::codegen::x86_64::insn::Insn::unary_rm_reg(
                        crate::codegen::x86_64::opcode::UnaryOp::Not, w, 2,
                    ));
                });
                if dst != 2 {
                    self.mov(dst, 2);
                }
                true
            },
            "__sync_bool_compare_and_swap" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7); // rdi = ptr
                self.gen_expr(&args[1], Self::EVAL_BASE); // rax = old
                self.gen_expr(&args[2], 6); // rsi = new
                self.emit_lock_cmpxchg_rdi_rsi(sz); // lock cmpxchg [rdi], rsi
                                                    // ZF=1 if success (rax was old value)
                self.emit_insn(&Insn::setcc_reg(crate::codegen::x86_64::opcode::CondCode::Eq, 0)); // sete al
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                    sign: false,
                    from: crate::codegen::shared::asm_operand::Width::B8,
                    to: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B8),
                }, &[])); // movzx rax, al
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            "__sync_val_compare_and_swap" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7); // rdi = ptr
                self.gen_expr(&args[1], Self::EVAL_BASE); // rax = old
                self.gen_expr(&args[2], 6); // rsi = new
                self.emit_lock_cmpxchg_rdi_rsi(sz); // lock cmpxchg [rdi], rsi
                                                    // rax = actual old value (whether success or fail)
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            "__sync_lock_test_and_set" => {
                if args.len() < 2 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7); // rdi = ptr
                self.gen_expr(&args[1], 6); // rsi = val
                self.emit_lock_xchg_rdi_rsi(sz); // xchg [rdi], rsi
                                                 // rsi = old value
                if dst != 6 {
                    self.mov(dst, 6);
                }
                true
            },
            "__sync_lock_release" => {
                if args.is_empty() {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7); // rdi = ptr
                                            // mov [rdi], 0 (size-dependent)
                match sz {
                    1 => { self.emit_insn(&Insn::raw_bytes(&[0xC6, 0x07, 0x00])); } // mov byte [rdi], 0
                    2 => { self.emit_insn(&Insn::raw_bytes(&[0x66, 0xC7, 0x07, 0x00, 0x00])); } // mov word [rdi], 0
                    4 => { self.emit_insn(&Insn::raw_bytes(&[0xC7, 0x07, 0x00, 0x00, 0x00, 0x00])); } // mov dword [rdi], 0
                    _ => { self.emit_insn(&Insn::raw_bytes(&[0x48, 0xC7, 0x07, 0x00, 0x00, 0x00, 0x00])); } // mov qword [rdi], 0
                }
                true
            },
            "__sync_synchronize" => {
                // mfence: full memory barrier
                self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Mfence)); // mfence
                true
            },
            "__atomic_thread_fence" => {
                // args: (mem_order)
                // Hardware memory barrier (mfence for all orders on x86_64)
                self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Mfence)); // mfence
                true
            },
            "__atomic_signal_fence" => {
                // args: (mem_order)
                // Compiler barrier only — no hardware instruction needed.
                // Without optimisation passes, no-op is sufficient.
                true
            },
            "__atomic_test_and_set" => {
                // args: (ptr, mem_order)
                // Atomically set *ptr = 1, return old value
                if args.len() < 2 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7); // rdi = ptr
                self.emit_insn(&Insn::mov_reg_imm32_b64(6, 1)); // mov rsi, 1
                self.emit_lock_xchg_rdi_rsi(sz);
                let ord = self.eval_mem_order(&args[1]);
                if ord == 5 {
                    self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Mfence));
                }
                if dst != 6 {
                    self.mov(dst, 6);
                }
                true
            },
            "__atomic_clear" => {
                // args: (ptr, mem_order)
                // Atomically set *ptr = 0
                if args.len() < 2 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                let ord = self.eval_mem_order(&args[1]);
                self.gen_expr(&args[0], 7); // rdi = ptr
                match sz {
                    1 => { self.emit_insn(&Insn::raw_bytes(&[0xC6, 0x07, 0x00])); }
                    2 => { self.emit_insn(&Insn::raw_bytes(&[0x66, 0xC7, 0x07, 0x00, 0x00])); }
                    4 => { self.emit_insn(&Insn::raw_bytes(&[0xC7, 0x07, 0x00, 0x00, 0x00, 0x00])); }
                    _ => { self.emit_insn(&Insn::raw_bytes(&[0x48, 0xC7, 0x07, 0x00, 0x00, 0x00, 0x00])); }
                }
                if ord == 5 {
                    self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Mfence));
                }
                true
            },
            "__atomic_is_lock_free" => {
                // args: (size [, ptr])
                // On x86_64, all standard integer sizes are always lock-free
                self.mov_imm(dst, 1);
                true
            },
            _ => false, // 未识别的 __atomic_ 函数, 走普通调用路径
        }
    }
    pub(crate) fn gen_builtin(&mut self, name: &str, args: &[Expr], dst: u8) -> bool {
        match name {
            // __builtin_expect(cond, hint): 返回 cond 的值 (第二参数是分支预测
            // hint, ABI 规定它的 side effect 也必须执行, 即 args[1] 不能跳过)。
            // GCC 实现: 先完整求值 args[1] (side effect 保留), 再求值 args[0]
            // 并把它作为整个 builtin 的值返回。pr85156 中 z++ 就是这样被丢的
            // —— 旧代码只 gen_expr(&args[0], dst), hint 参数从未被发射。
            "__builtin_expect" | "__builtin_expect_with_probability" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                // 先发射 hint 参数 side effect (值丢弃)
                if args.len() >= 2 {
                    // args[1] 可能有 side effect 但值不需要, 用 rcx 作 scratch.
                    // 注意 gen_expr 到非 rax 寄存器时若 args[1] 本身又依赖
                    // rax (少见, 一般是 ++/-- 或 Call), rcx 是安全的 scratch.
                    self.gen_expr(&args[1], 1 /* rcx */);
                }
                self.gen_expr(&args[0], dst);
                true
            },
            // parser 通常已折叠；保留后端兜底，且与共享常量求值器一致。
            "__builtin_constant_p" => {
                let value = args
                    .first()
                    .map_or(0, |arg| const_eval::is_constant(arg) as i128);
                self.mov_imm(dst, value);
                true
            },
            // __builtin_nonzero_p(x): returns 1 if x is known to be non-zero,
            // 0 otherwise. Conservative: return 0 (unknown) at compile time.
            "__builtin_nonzero_p" => {
                // Conservative implementation: we cannot prove non-zero at
                // compile time in general, so return 0 (unknown).
                // A more sophisticated version could check constant args.
                let value = args
                    .first()
                    .and_then(|arg| {
                        const_eval::eval(arg)
                            .ok()
                            .map(|v| if v != 0 { 1i128 } else { 0i128 })
                    })
                    .unwrap_or(0);
                self.mov_imm(dst, value);
                true
            },
            // __builtin_mul_loop: not a standard GCC builtin, but used in some
            // kernel code. Treat as no-op returning 0.
            "__builtin_mul_loop" => {
                self.mov_imm(dst, 0);
                true
            },
            // __builtin_object_size(ptr, type): 编译期对象大小，未知时 (size_t)-1
            "__builtin_object_size" => {
                let type_arg = args
                    .get(1)
                    .and_then(|e| const_eval::eval(e).ok())
                    .unwrap_or(0);
                let size = args
                    .first()
                    .and_then(|p| const_eval::object_size(p, type_arg))
                    .unwrap_or(-1);
                self.mov_imm(dst, size);
                true
            },
            // __builtin_memcpy/__builtin_memmove: 内联 rep movsb (freestanding 无 libc)
            "__builtin_memcpy" | "__builtin_memmove" => {
                if args.len() < 3 {
                    self.mov_imm(dst, 0);
                    return true;
                }
                // 参数: dst(rdi), src(rsi), n(rcx)
                self.gen_expr(&args[1], 6); // rsi = src
                self.gen_expr(&args[0], 7); // rdi = dst
                self.gen_expr(&args[2], 1); // rcx = n
                                            // 保存 rdi 作为返回值 (memcpy/memmove 返回 dst)
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Push, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(7, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // push rdi
                self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Cld)); // cld (清除方向标志)
                self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::RepMovsb)); // rep movsb
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Pop, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // pop rax (返回值 = dst)
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_memset: 内联 rep stosb (freestanding 无 libc)
            "__builtin_memset" => {
                if args.len() < 3 {
                    self.mov_imm(dst, 0);
                    return true;
                }
                // 参数: dst(rdi), val(rax), n(rcx)
                self.gen_expr(&args[0], 7); // rdi = dst
                self.gen_expr(&args[1], Self::EVAL_BASE); // rax = val
                self.gen_expr(&args[2], 1); // rcx = n
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Push, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(7, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // push rdi
                self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Cld)); // cld
                self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::RepStosb)); // rep stosb
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Pop, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // pop rax (返回值 = dst)
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_mempcpy: 同 memcpy 但返回 dst+n (而非 dst)
            "__builtin_mempcpy" => {
                if args.len() < 3 {
                    self.mov_imm(dst, 0);
                    return true;
                }
                // 参数: dst(rdi), src(rsi), n(rcx)
                self.gen_expr(&args[1], 6); // rsi = src
                self.gen_expr(&args[0], 7); // rdi = dst
                self.gen_expr(&args[2], 1); // rcx = n
                self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Cld)); // cld
                self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::RepMovsb)); // rep movsb
                                                // rep movsb 后 rdi = dst+n, 移到 rax 作为返回值
                self.mov(0, 7); // rax = rdi
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_memcmp: 转发到 libc ( memcmp 不适合简单内联)
            "__builtin_memcmp" => {
                let ar = [7u8, 6, 2, 1, 8, 9];
                for (i, a) in args.iter().enumerate() {
                    self.gen_expr(a, ar[i]);
                }
                self.call_label("memcmp");
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_strcpy/__builtin_strncpy/__builtin_strcmp 等
            "__builtin_strcpy"
            | "__builtin_strncpy"
            | "__builtin_strcmp"
            | "__builtin_strlen"
            | "__builtin_strchr"
            | "__builtin_strrchr"
            | "__builtin_strstr"
            | "__builtin_strpbrk"
            | "__builtin_stpcpy"
            | "__builtin_strnlen"
            | "__builtin_strspn"
            | "__builtin_strcspn"
            | "__builtin_strcasestr"
            | "__builtin_strncmp"
            | "__builtin_strdup"
            | "__builtin_strndup" => {
                let libc_name = name.strip_prefix("__builtin_").unwrap();
                let ar = [7u8, 6, 2, 1, 8, 9];
                for (i, a) in args.iter().enumerate() {
                    self.gen_expr(a, ar[i]);
                }
                self.call_label(libc_name);
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_index / __builtin_rindex: BSD 别名, 分别转发到 strchr / strrchr.
            "__builtin_index" | "__builtin_rindex" => {
                let libc_name = if name == "__builtin_index" { "strchr" } else { "strrchr" };
                let ar = [7u8, 6, 2, 1, 8, 9];
                for (i, a) in args.iter().enumerate() {
                    self.gen_expr(a, ar[i]);
                }
                self.call_label(libc_name);
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_wc* 系列 (Section 11.3): wchar_t 版本的字符串函数.
            // wchar_t 在 Linux 为 4 字节 int, 但调用约定与 char* 版本相同
            // (指针 / size_t 均通过 rdi / rsi / rdx 传递). 这里直接转发到
            // libc 的 wcs* / wmem* 函数, 与 __builtin_str* 的处理方式一致.
            "__builtin_wcslen" | "__builtin_wcscpy" | "__builtin_wcscat" | "__builtin_wcschr"
            | "__builtin_wcsrchr" | "__builtin_wcscmp" | "__builtin_wcsncmp"
            | "__builtin_wmemcpy" | "__builtin_wmemmove" | "__builtin_wmemset"
            | "__builtin_wmemcmp" => {
                let libc_name = name.strip_prefix("__builtin_").unwrap();
                let ar = [7u8, 6, 2, 1, 8, 9];
                for (i, a) in args.iter().enumerate() {
                    if i >= 6 {
                        break;
                    }
                    self.gen_expr(a, ar[i]);
                }
                self.call_label(libc_name);
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_abs / __builtin_labs / __builtin_llabs:
            // absolute value for int/long/long long.
            "__builtin_abs" | "__builtin_labs" | "__builtin_llabs" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], dst);
                // abs: test dst, dst; jns skip; neg dst; skip:
                // rex 必带 W 位 (0x48), neg 始终是 3 字节, skip offset = 3.
                // 之前硬编码 4 导致跳转越过 neg 的末尾 1 字节, 落到
                // 下一条指令的中间, 触发 mid-instruction decode.
                let rex = 0x48 | if dst >= 8 { 0x05 } else { 0 };   // 位序: REX.R+B: 自反 ModRM, 同一个寄存器既在 reg 又在 r/m, 判据见 docs/ssa-pass-upgrade-plan.md §7.21
                self.emit_insn(&Insn::raw_bytes(&[rex, 0x85, 0xC0 | ((dst & 7) << 3) | (dst & 7)])); // test dst, dst
                self.emit_insn(&Insn::raw_bytes(&[0x79, 0x03])); // jns +3 (跳过 neg, 含 1 字节 W REX)
                self.emit_insn(&Insn::raw_bytes(&[rex, 0xf7, 0xd8 | (dst & 7)])); // neg dst
                true
            },
            // __builtin_unreachable: 不可达, 生成 ud2
            "__builtin_unreachable" | "__builtin_trap" => {
                self.emit_insn(&Insn::raw_bytes(&[0x0F, 0x0B])); // ud2
                true
            },
            // __builtin_debugtrap(): 调试断点, 生成 int3
            "__builtin_debugtrap" => {
                self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Int3)); // int3
                true
            },
            // __builtin_stack_save(): 保存栈指针, 返回当前 rsp
            "__builtin_stack_save" => {
                let rex = if dst >= 8 { 0x49 } else { 0x48 };
                self.emit_insn(&Insn::raw_bytes(&[rex, 0x89, 0xe0 | (dst & 7)])); // mov dst, rsp
                true
            },
            // __builtin_stack_restore(p): 恢复栈指针, 设置 rsp = p
            "__builtin_stack_restore" => {
                if !args.is_empty() {
                    self.gen_expr(&args[0], dst);
                    let rex = if dst >= 8 { 0x49 } else { 0x48 };
                    self.emit_insn(&Insn::raw_bytes(&[rex, 0x89, 0xc4 | ((dst & 7) << 3)])); // mov rsp, dst
                }
                true
            },
            // __builtin_va_end: no-op, 但参数副作用必须求值
            // (va-arg-21: va_end(**ap_ptr++) 的 ap_ptr++ 推进不可丢失)
            "__builtin_va_end" => {
                if !args.is_empty() {
                    self.gen_expr(&args[0], Self::EVAL_BASE);
                }
                true
            },
            // __builtin_setjmp(buf): GCC 语义实现.
            // buf 布局 (前 3 个 slot, 兼容 GCC 5-word buffer 约定):
            //   buf[0] = receiver label 地址 (__builtin_longjmp 的落点)
            //   buf[1] = 捕获时刻 rsp (含本 builtin 内部分配的 40 字节临时区)
            //   buf[2] = r13 (dcc 帧基址)
            // 直接调用返回 0; 经 __builtin_longjmp 返回时 receiver 处写 1.
            // 栈上临时区布局: [rsp]=setjmp 结果, [rsp+8..40]=callee-saved
            // pool 寄存器 (rbx,r12,r14,r15) 备份 — longjmp 返回时恢复,
            // 避免陈旧寄存器值 (对应 GCC returns_twice 语义).
            "__builtin_setjmp" => {
                if args.is_empty() {
                    return false;
                }
                let recv = self.fresh_label();
                let cont = self.fresh_label();
                // rdi = buf
                self.gen_expr(&args[0], 7);
                // 栈上临时区
                self.emit_insn(&Insn::sub_rsp_imm(0x28)); // sub rsp, 40
                self.emit_insn(&Insn::mov_rm_mem(false, crate::codegen::shared::asm_operand::Width::B64, 0, 4, 0)); // mov qword [rsp], 0 placeholder
                // 保存 callee-saved pool 寄存器
                self.emit_insn(&Insn::mov_rm_mem(false, crate::codegen::shared::asm_operand::Width::B64, 3, 4, 8)); // mov [rsp+8], rbx
                self.emit_insn(&Insn::mov_rm_mem(false, crate::codegen::shared::asm_operand::Width::B64, 12, 4, 0x10)); // mov [rsp+16], r12
                self.emit_insn(&Insn::mov_rm_mem(false, crate::codegen::shared::asm_operand::Width::B64, 14, 4, 0x18)); // mov [rsp+24], r14
                self.emit_insn(&Insn::mov_rm_mem(false, crate::codegen::shared::asm_operand::Width::B64, 15, 4, 0x20)); // mov [rsp+32], r15
                                                            // buf[1] = rsp (含临时区)
                self.emit_insn(&Insn::mov_rm_mem(false, crate::codegen::shared::asm_operand::Width::B64, 4, 7, 8)); // mov [rdi+8], rsp
                                                      // buf[2] = r13
                self.emit_insn(&Insn::mov_rm_mem(false, crate::codegen::shared::asm_operand::Width::B64, 13, 7, 0x10)); // mov [rdi+16], r13
                                                      // buf[0] = receiver 地址: lea rax, [rip+recv] → mov [rdi], rax
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x8D, 0x05, 0, 0, 0, 0]));
                let disp_pos = self.pos() - 4;
                self.label_addr_refs.push((disp_pos, recv.clone()));
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(7),
                            index: None,
                            seg: None,
                        }
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B64
                    ),
                },
                &[],
            )); // mov [rdi], rax
                                                // 直接路径跳过 receiver
                self.jmp_rel32(&cont, JumpKind::Jmp);
                // receiver: longjmp 落点 (rsp/r13 已恢复到捕获点)
                self.label(&recv);
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x8B, 0x5C, 0x24, 0x08])); // mov rbx, [rsp+8]
                self.emit_insn(&Insn::raw_bytes(&[0x4C, 0x8B, 0x64, 0x24, 0x10])); // mov r12, [rsp+16]
                self.emit_insn(&Insn::raw_bytes(&[0x4C, 0x8B, 0x74, 0x24, 0x18])); // mov r14, [rsp+24]
                self.emit_insn(&Insn::raw_bytes(&[0x4C, 0x8B, 0x7C, 0x24, 0x20])); // mov r15, [rsp+32]
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0xC7, 0x04, 0x24, 1, 0, 0, 0])); // mov qword [rsp], 1
                self.label(&cont);
                // 结果 → dst
                let rex = 0x48u8 | if dst >= 8 { 0x44 } else { 0 };
                let modrm = 0x04u8 | ((dst & 7) << 3);
                self.emit_insn(&Insn::raw_bytes(&[rex, 0x8B, modrm, 0x24])); // mov dst, [rsp]
                self.emit_insn(&Insn::add_rsp_imm(0x28)); // add rsp, 40
                true
            },
            // __builtin_longjmp(buf, val): 恢复 rsp/r13 后跳转到 receiver.
            // val 按语义恒为 1, 仅求值其副作用.
            "__builtin_longjmp" => {
                if args.is_empty() {
                    return false;
                }
                self.gen_expr(&args[0], 7); // rdi = buf
                if args.len() >= 2 {
                    self.gen_expr(&args[1], 6); // rsi = val (副作用)
                }
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x8B, 0x67, 0x08])); // mov rsp, [rdi+8]
                self.emit_insn(&Insn::raw_bytes(&[0x4C, 0x8B, 0x6F, 0x10])); // mov r13, [rdi+16]
                self.emit_insn(&Insn::raw_bytes(&[0xFF, 0x27])); // jmp [rdi]
                true
            },
            // __builtin_va_arg_pack: 返回 0 (placeholder)
            "__builtin_va_arg_pack" => {
                self.mov_imm(dst, 0);
                true
            },
            // __builtin_clear_padding: no-op
            "__builtin_clear_padding" => true,
            // __builtin___clear_cache / __builtin_clear_cache: no-op (x86 不需要显式 cache flush)
            "__builtin___clear_cache" | "__builtin_clear_cache" => true,
            // 数学函数: 转发到 libc (float 参数走 xmm0/xmm1, SysV ABI)
            "__builtin_pow" | "__builtin_powf" | "__builtin_powl" => {
                let libc_name = name.strip_prefix("__builtin_").unwrap();
                // SysV ABI: float/double 参数在 xmm0, xmm1
                if !args.is_empty() {
                    self.gen_expr(&args[0], Self::EVAL_BASE); // xmm0 = arg0
                }
                if args.len() >= 2 {
                    self.emit_insn(&Insn::movsd_xmm(2, 0)); // movsd xmm2, xmm0
                    self.gen_expr(&args[1], Self::EVAL_BASE); // xmm0 = arg1
                    self.emit_insn(&Insn::movsd_xmm(1, 0)); // movsd xmm1, xmm0
                    self.emit_insn(&Insn::movsd_xmm(0, 2)); // movsd xmm0, xmm2
                }
                self.call_label(libc_name);
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // 复数函数: 转发到 libc
            "__builtin_cpow" | "__builtin_cpowf" | "__builtin_cpowl" | "__builtin_cexpi"
            | "__builtin_cexpif" | "__builtin_cexpil" => {
                let libc_name = name.strip_prefix("__builtin_").unwrap();
                let ar = [7u8, 6, 2, 1, 8, 9];
                for (i, a) in args.iter().enumerate() {
                    if i >= 6 {
                        break;
                    }
                    self.gen_expr(a, ar[i]);
                }
                self.call_label(libc_name);
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_conj/conjf/conjl: 复数共轭 (实部不变, 虚部翻转符号位).
            // 参数为 lvalue 时直接对内存分量寻址; rvalue 时按 complex rvalue
            // 约定 (xmm0=实部, 调用返回时虚部在 xmm1) 物化到临时槽再处理.
            //   complex LD: 走 x87, fldt imag / fchs / fstpt; SSE 翻转 0x8000...
            //   标志位对 LD 是错的 (符号位在 tbyte 第 9 字节, 不在低 8 字节)。
            "__builtin_conj" | "__builtin_conjf" | "__builtin_conjl" => {
                if let Some(arg) = args.first() {
                    let inner = match &arg.ty() {
                        Type::Complex(i) => i.base.clone(),
                        _ => Type::Double,
                    };
                    let inner_sz = inner.size() as i64;
                    let result_off = self.complex_result_off();
                    if matches!(inner, Type::LongDouble) {
                        // LD 路径: 物化源到临时 tbyte 对, real 不变直写,
                        // imag fchs 后写 (同 unary.rs BitNot 分支)。
                        let src = self.gen_complex_ld_materialize(arg);
                        self.emit_x87_fld_at(13, src as i32, &inner);
                        self.emit_x87_fstp_at(13, result_off as i32, &inner);
                        self.emit_x87_fld_at(13, (src + 16) as i32, &inner);
                        self.emit_x87_chs();
                        self.emit_x87_fstp_at(13, (result_off + inner_sz) as i32, &inner);
                        // LD rvalue 约定: rax = 结果槽地址。
                        self.emit_lea_r13(0, result_off);
                        if dst != 0 {
                            self.mov(dst, 0);
                        }
                        return true;
                    }
                    let pfx = if matches!(inner, Type::Float) { 0xF3u8 } else { 0xF2u8 };
                    if self.gen_lvalue_addr(arg, Self::EVAL_BASE) {
                        self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovRm { width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(1, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // mov rcx, rax
                        let imag_off = inner_sz;
                        // 实部: [rcx] → xmm0 → [result_off] (不变)
                        self.emit_insn(&Insn::raw_bytes(&[pfx, 0x0F, 0x10, 0x01]));
                        self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x11], result_off);
                        // 虚部: [rcx+off] → xmm0 → flip → [result_off+inner_sz]
                        self.emit_insn(&Insn::raw_bytes(&[pfx, 0x0F, 0x10, 0x81]));
                        self.emit_insn(&Insn::raw_bytes(&(imag_off as i32).to_le_bytes()));
                        self.flip_sign_xmm(0, &inner);
                        self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x11], result_off + inner_sz);
                    } else {
                        self.gen_expr(arg, Self::EVAL_BASE);
                        if matches!(arg, Expr::Call { .. }) {
                            // SSE 返回: 虚部在 xmm1, 物化到临时区
                            self.emit_insn(&Insn::raw_bytes(&[pfx, 0x41, 0x0F, 0x11]));
                            if (result_off + inner_sz).abs() < 128 {
                                self.emit_insn(&Insn::raw_bytes(&[0x4D, (result_off + inner_sz) as u8]));
                            } else {
                                self.emit_insn(&Insn::raw_bytes(&[0x8D]));
                                self.emit_insn(&Insn::raw_bytes(&((result_off + inner_sz) as i32).to_le_bytes()));
                            }
                        }
                        // 实部: xmm0 → [result_off] (不变)
                        self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x11], result_off);
                        // 虚部: 加载并翻转
                        self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x10], result_off + inner_sz);
                        self.flip_sign_xmm(0, &inner);
                        self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x11], result_off + inner_sz);
                    }
                    // 重载实部到 xmm0 (complex rvalue 约定: xmm0=实部)
                    self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x10], result_off);
                    if dst != 0 {
                        self.mov(dst, 0);
                    }
                }
                true
            },
            // __builtin_memchr: 转发到 libc memchr
            "__builtin_memchr" => {
                let ar = [7u8, 6, 2, 1, 8, 9];
                for (i, a) in args.iter().enumerate() {
                    if i >= 6 {
                        break;
                    }
                    self.gen_expr(a, ar[i]);
                }
                self.call_label("memchr");
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_memcmp_eq: 调用 memcmp 并检查返回值 == 0
            "__builtin_memcmp_eq" => {
                let ar = [7u8, 6, 2, 1, 8, 9];
                for (i, a) in args.iter().enumerate() {
                    if i >= 3 {
                        break;
                    }
                    self.gen_expr(a, ar[i]);
                }
                self.call_label("memcmp");
                // test eax, eax; setz al; movzx eax, al
                self.emit_insn(&Insn::test_rm_reg(crate::codegen::shared::asm_operand::Width::B32, 0, 0)); // test eax, eax
                self.emit_insn(&Insn::raw_bytes(&[0x0f, 0x94, 0xc0])); // setz al
                self.emit_insn(&Insn::raw_bytes(&[0x0f, 0xb6, 0xc0])); // movzx eax, al
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // Vector 操作: placeholder (返回 0)
            "__builtin_convertvector" => {
                self.mov_imm(dst, 0);
                true
            },
            // __builtin_ilogb / __builtin_iseqsig: 转发到 libc
            "__builtin_ilogb" | "__builtin_ilogbf" | "__builtin_ilogbl" | "__builtin_iseqsig"
            | "__builtin_iseqsigf" | "__builtin_iseqsigl" => {
                let libc_name = name.strip_prefix("__builtin_").unwrap();
                let ar = [7u8, 6, 2, 1, 8, 9];
                for (i, a) in args.iter().enumerate() {
                    if i >= 6 {
                        break;
                    }
                    self.gen_expr(a, ar[i]);
                }
                self.call_label(libc_name);
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_assume_aligned(p, n): 返回指针, 提示对齐
            "__builtin_assume_aligned" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], dst);
                true
            },
            // __builtin_alloca(size) / alloca(size): 动态栈分配, 返回 rsp 指向的内存
            "__builtin_alloca" | "alloca" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                // 计算分配大小到 rcx (dcc 寄存器编码: 1 = rcx)
                self.gen_expr(&args[0], 1); // rcx = size
                                            // 对齐到 16 字节: rcx = (rcx + 15) & ~15
                self.emit_insn(&Insn::raw_with_gas(&[0x48, 0x83, 0xc1, 0x0f], "add $15, %rcx"));
                self.emit_insn(&Insn::raw_with_gas(&[0x48, 0x83, 0xe1, 0xf0], "and $-16, %rcx"));
                                                      // sub rsp, rcx
                self.emit_insn(&Insn::raw_with_gas(&[0x48, 0x29, 0xcc], "sub %rcx, %rsp"));
                                                // 返回新 rsp 到 dst
                if dst != 4 && dst != 12 {
                    let rex = if dst >= 8 { 0x49 } else { 0x48 };
                    let dst_name = crate::codegen::x86_64::abi::regnames::att_name_of_num(dst, 64)
                        .unwrap_or_else(|| "%rax".to_string());
                    self.emit_insn(&Insn::raw_with_gas(&[rex, 0x89, 0xe0 | (dst & 7)], &format!("mov %rsp, {}", dst_name)));
                }
                true
            },
            // === Per-CPU 内建函数 ===
            // __builtin_dcc_this_cpu_read(var): 通过 %gs: 段前缀读取当前 CPU 的 per-cpu 变量
            // 直接生成 mov %gs:var_addr, dst 而非内联汇编, 使编译器能追踪语义.
            "__builtin_dcc_this_cpu_read" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                let arg = &args[0];
                let var_size = arg.ty().size().max(1);
                // 取变量地址到 rax
                if let Expr::Ident { name, .. } = arg {
                    if self.is_global_var(&name.name) {
                        self.emit_global_addr(0, &name.name);
                    } else {
                        let off = self.var_offset(&name.name);
                        self.emit_lea_r13(0, off);
                    }
                } else {
                    self.gen_expr(
                        &Expr::AddrOf {
                            e: Box::new(arg.clone()),
                            span: Span {
                                file: std::sync::Arc::from(""),
                                line: 0,
                                col: 0,
                                len: 0,
                            },
                            ty: Type::Ptr(Box::new(arg.ty().into())),
                        },
                        0,
                    );
                }
                // mov %gs:[rax], dst (段前缀 + 按类型加载)
                self.emit_insn(&Insn::raw_bytes(&[0x65])); // GS segment prefix
                self.emit_load_mem_typed(var_size, &arg.ty());
                if dst != 0 && !matches!(arg.ty(), Type::Float | Type::Double) {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_dcc_this_cpu_write(var, val): 通过 %gs: 段前缀写入 per-cpu 变量
            "__builtin_dcc_this_cpu_write" => {
                if args.len() < 2 {
                    return true;
                }
                let var_arg = &args[0];
                let var_size = var_arg.ty().size().max(1);
                // 取变量地址到 rax, 保存到 rcx
                if let Expr::Ident { name, .. } = var_arg {
                    if self.is_global_var(&name.name) {
                        self.emit_global_addr(0, &name.name);
                    } else {
                        let off = self.var_offset(&name.name);
                        self.emit_lea_r13(0, off);
                    }
                } else {
                    self.gen_expr(
                        &Expr::AddrOf {
                            e: Box::new(var_arg.clone()),
                            span: Span {
                                file: std::sync::Arc::from(""),
                                line: 0,
                                col: 0,
                                len: 0,
                            },
                            ty: Type::Ptr(Box::new(var_arg.ty().into())),
                        },
                        0,
                    );
                }
                self.push(0); // save addr
                              // 求值 val 到 rax
                self.gen_expr(&args[1], Self::EVAL_BASE);
                self.pop(1); // rcx = addr
                             // mov %gs:[rcx], rax (段前缀 + 存储)
                self.emit_insn(&Insn::raw_bytes(&[0x65])); // GS segment prefix
                match var_size {
                    1 => { self.emit_insn(&Insn::raw_bytes(&[0x88, 0x01])); }       // mov [rcx], al
                    2 => { self.emit_insn(&Insn::raw_bytes(&[0x66, 0x89, 0x01])); } // mov [rcx], ax
                    4 => { self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(1),
                            index: None,
                            seg: None,
                        }
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                },
                &[],
            )); }       // mov [rcx], eax
                    _ => { self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(1),
                            index: None,
                            seg: None,
                        }
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B64
                    ),
                },
                &[],
            )); } // mov [rcx], rax
                }
                true
            },
            // __builtin_dcc_this_cpu_add(var, val): 通过 %gs: 段前缀原子加 per-cpu 变量
            "__builtin_dcc_this_cpu_add" => {
                if args.len() < 2 {
                    return true;
                }
                let var_arg = &args[0];
                let var_size = var_arg.ty().size().max(1);
                // 取变量地址到 rax, 保存到 rcx
                if let Expr::Ident { name, .. } = var_arg {
                    if self.is_global_var(&name.name) {
                        self.emit_global_addr(0, &name.name);
                    } else {
                        let off = self.var_offset(&name.name);
                        self.emit_lea_r13(0, off);
                    }
                } else {
                    self.gen_expr(
                        &Expr::AddrOf {
                            e: Box::new(var_arg.clone()),
                            span: Span {
                                file: std::sync::Arc::from(""),
                                line: 0,
                                col: 0,
                                len: 0,
                            },
                            ty: Type::Ptr(Box::new(var_arg.ty().into())),
                        },
                        0,
                    );
                }
                self.push(0); // save addr
                              // 求值 val 到 rax
                self.gen_expr(&args[1], Self::EVAL_BASE);
                self.pop(1); // rcx = addr
                             // add %gs:[rcx], rax (段前缀 + 加法)
                self.emit_insn(&Insn::raw_bytes(&[0x65])); // GS segment prefix
                match var_size {
                    1 => { self.emit_insn(&Insn::raw_bytes(&[0x00, 0x01])); }       // add [rcx], al
                    2 => { self.emit_insn(&Insn::raw_bytes(&[0x66, 0x01, 0x01])); } // add [rcx], ax
                    4 => { self.emit_insn(&Insn::raw_bytes(&[0x01, 0x01])); }       // add [rcx], eax
                    _ => { self.emit_insn(&Insn::raw_bytes(&[0x48, 0x01, 0x01])); } // add [rcx], rax
                }
                true
            },
            // __builtin_va_start(ap, last): 初始化 SysV va_list 结构
            "__builtin_va_start" => {
                if !args.is_empty() {
                    if let Expr::Ident { name, .. } = &args[0] {
                        if matches!(args[0].ty(), Type::Array(_, _)) {
                            // 数组类型 Ident: 局部栈槽版 / 全局 rip 版
                            // (emit_va_start 内部按 is_global_not_local 分派)
                            self.emit_va_start(&name.name);
                            return true;
                        }
                        // 指针类型 Ident (va_list 形参退化, va-arg-14 的
                        // va_start(param, param)): 加载指针值作为目标地址
                        self.gen_expr(&args[0], 1);
                        self.emit_va_start_at(1);
                        return true;
                    }
                    // 通用: 指针类型表达式 → 加载值; 数组类型 lvalue
                    // (成员 a.g / 下标 aps[4] / 解引用 *ap_array[0]) → 地址
                    let ap_ptr = matches!(args[0].ty(), Type::Ptr(_));
                    let ok = if ap_ptr {
                        self.gen_expr(&args[0], 1);
                        true
                    } else {
                        self.gen_lvalue_addr(&args[0], 1)
                    };
                    if ok {
                        self.emit_va_start_at(1);
                    }
                }
                true
            },
            // __builtin_va_arg 已通过 Expr::VaArg 处理, 此处不会到达
            "__builtin_va_arg" => true,
            // __builtin_va_copy(dest, src): 复制整个 va_list 结构 (24 字节)
            // 统一 va_list 地址规则 (与 va_arg 一致): 参数类型是数组
            // (__dcc_sysv_va_list[1]) → lvalue 地址; 是指针 (va_list 传参
            // 退化 / va_list*) → 加载指针值。va-arg-10: fap 的 ap 是形参
            // 指针, &ap 会复制指针变量而非指向的 va_list。
            "__builtin_va_copy" => {
                if args.len() >= 2 {
                    let dest_ptr = matches!(args[0].ty(), Type::Ptr(_));
                    let dest_ok = if dest_ptr {
                        self.gen_expr(&args[0], Self::EVAL_BASE);
                        true
                    } else {
                        self.gen_lvalue_addr(&args[0], Self::EVAL_BASE)
                    };
                    if dest_ok {
                        self.mov(7, 0); // rdi = dest va_list 地址
                        let src_ptr = matches!(args[1].ty(), Type::Ptr(_));
                        let src_ok = if src_ptr {
                            self.gen_expr(&args[1], 6);
                            true
                        } else {
                            self.gen_lvalue_addr(&args[1], 6)
                        };
                        if src_ok {
                            // rep movsq 复制 3 个 qword
                            self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Cld)); // cld
                            self.emit_insn(&Insn::raw_bytes(&[0x48, 0xC7, 0xC1, 0x03, 0, 0, 0]));
                            self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::RepMovsq)); // rep movsq
                        }
                    }
                }
                true
            },
            // __builtin_bswap16/32/64: 字节序反转
            "__builtin_bswap16" | "__builtin_bswap32" | "__builtin_bswap64" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], dst);
                if name == "__builtin_bswap64" {
                    // bswap 的寄存器编码在 opcode 低三位，扩展寄存器使用 REX.B。
                    let rex = 0x48 | if dst >= 8 { 0x01 } else { 0 };   // 位序: REX.B: 该寄存器只在 ModRM.r/m 或 opcode 低 3 位, 判据见 docs/ssa-pass-upgrade-plan.md §7.21
                    self.emit_insn(&Insn::raw_bytes(&[rex, 0x0F, 0xC8 | (dst & 7)]));
                } else if name == "__builtin_bswap32" {
                    if dst >= 8 {
                        self.emit_insn(&Insn::raw_bytes(&[0x41]));
                    }
                    self.emit_insn(&Insn::raw_bytes(&[0x0F, 0xC8 | (dst & 7)]));
                } else {
                    // x86 没有 bswap16；rol r16, 8 等价于交换两个字节。
                    self.emit_insn(&Insn::raw_bytes(&[0x66]));
                    if dst >= 8 {
                        self.emit_insn(&Insn::raw_bytes(&[0x41]));
                    }
                    self.emit_insn(&Insn::raw_bytes(&[0xC1, 0xC0 | (dst & 7), 8]));
                    self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                        sign: false,
                        from: crate::codegen::shared::asm_operand::Width::B16,
                        to: crate::codegen::shared::asm_operand::Width::B64,
                        dst: crate::codegen::x86_64::opcode::Operand::Reg(dst, crate::codegen::shared::asm_operand::Width::B64),
                        src: crate::codegen::x86_64::opcode::Operand::Reg(dst, crate::codegen::shared::asm_operand::Width::B16),
                    }, &[]));
                }
                true
            },
            // __builtin_huge_val / huge_valf / huge_vall: IEEE 754 +Inf
            // __builtin_inf / inff / infl: 同 huge_val*, 单独列出
            // 直接加载 IEEE 754 位模式:
            //   double +Inf:  0x7FF0_0000_0000_0000
            //   float  +Inf:  0x7F80_0000
            //   long double (x86 80-bit) +Inf: 0x7FFF_8000_0000_0000_0000
            // dcc 把 long double 当 double 处理, 8 字节近似.
            // emit: 8 字节 movsd/movss xmm0, [rip+disp32], disp32 在 code_pos+4.
            // 注意: 结果必须写到 xmm0 (dst=0) 而非 rax, 因为双精度返回走 xmm0.
            "__builtin_huge_val"
            | "__builtin_huge_valf"
            | "__builtin_huge_vall"
            | "__builtin_inf"
            | "__builtin_inff"
            | "__builtin_infl" => {
                let (is_float, bytes) = match name {
                    "__builtin_huge_valf" | "__builtin_inff" => {
                        (true, 0x7F800000u32.to_le_bytes().to_vec())
                    },
                    "__builtin_huge_vall" | "__builtin_infl" => {
                        // long double: dcc 用 8 字节近似, 加载 0 进入 xmm0 (近似 +Inf)
                        (false, 0x7FF0000000000000u64.to_le_bytes().to_vec())
                    },
                    _ => (false, 0x7FF0000000000000u64.to_le_bytes().to_vec()),
                };
                let off = self.rodata.len() as u64;
                self.rodata.extend_from_slice(&bytes);
                // movss/movsd xmm0, [rip+disp32]: 8 字节指令
                //   F3 0F 10 05 disp32 (movss, 4 字节) 或
                //   F2 0F 10 05 disp32 (movsd, 8 字节)
                // disp32 在 code_pos+4, 加 REX 不需要 (xmm0 < 8).
                let prefix = if is_float { 0xF3 } else { 0xF2 };
                let code_pos = self.pos();
                self.emit_insn(&Insn::raw_bytes(&[prefix, 0x0F, 0x10, 0x05, 0, 0, 0, 0]));
                // 结果在 xmm0. 若调用方 dst 不是 0, 移动到 dst.
                if dst != 0 {
                    // dst 应当是 x86 通用寄存器. 使用 movsd/movss 拷贝.
                    if is_float {
                        self.emit_insn(&Insn::movss_xmm_store(dst, 0));
                    } else {
                        self.emit_insn(&Insn::movsd_xmm_store(dst, 0));
                    }
                }
                // 走 float_refs 通道: ET_EXEC resolve() 直接按下一条 RIP
                // 回填 disp32; ET_REL 则由 compile_rel 使用 PC32 addend
                // off - 4, 因为重定位位置 P 是 disp32 字段而非下一条 RIP。
                self.float_refs.push((code_pos, off));
                true
            },
            // __builtin_nan / nanf / nanl: 静默 NaN (quiet NaN).
            // GCC 接受字符串参数 (mantissa bits), 我们忽略字符串, 取默认 qNaN.
            // qNaN 位模式:
            //   double:  0x7FF8_0000_0000_0000
            //   float:   0x7FC0_0000
            //   long double: 0x7FFF_C000_0000_0000_0000 (dcc 8 字节近似)
            // emit 8 字节 movsd/movss xmm0, [rip+disp32], disp32 在 code_pos+4.
            "__builtin_nan" | "__builtin_nanf" | "__builtin_nanl" => {
                let (is_float, bytes) = match name {
                    "__builtin_nanf" => (true, 0x7FC00000u32.to_le_bytes().to_vec()),
                    "__builtin_nanl" => (false, 0x7FF8000000000000u64.to_le_bytes().to_vec()),
                    _ => (false, 0x7FF8000000000000u64.to_le_bytes().to_vec()),
                };
                let off = self.rodata.len() as u64;
                self.rodata.extend_from_slice(&bytes);
                let prefix = if is_float { 0xF3 } else { 0xF2 };
                let code_pos = self.pos();
                self.emit_insn(&Insn::raw_bytes(&[prefix, 0x0F, 0x10, 0x05, 0, 0, 0, 0]));
                if dst != 0 {
                    if is_float {
                        self.emit_insn(&Insn::movss_xmm_store(dst, 0));
                    } else {
                        self.emit_insn(&Insn::movsd_xmm_store(dst, 0));
                    }
                }
                // 走 float_refs 通道 (同 __builtin_huge_val 实现)
                self.float_refs.push((code_pos, off));
                true
            },
            // __builtin_isnan / isnanf / isnanl: NaN 检测.
            // NaN 的特性: ucomisd/ucomiss 设置 PF=1 (任一为 NaN).
            //   较新 Intel CPU 在 NaN 时 ZF=1, PF=1, CF=1
            //   旧文档:  ZF=0, PF=1, CF=1
            // 统一用 setp 检测 PF=1 → NaN.
            // 类型由实参决定, 而非函数名后缀: __builtin_isnan(float) 也应
            // 使用 ucomiss; 否则 movss 后高位 0 会使 ucomisd 行为不一致.
            "__builtin_isnan" | "__builtin_isnanf" | "__builtin_isnanl" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                let arg_ty = args[0].ty();
                let is_f = matches!(arg_ty, Type::Float);
                self.gen_expr(&args[0], Self::EVAL_BASE); // xmm0 = x
                                            // ucomis{s,d} xmm0, xmm0
                if is_f {
                    self.emit_insn(&Insn::raw_with_gas(&[0x0F, 0x2E, 0xC0], "ucomiss %xmm0, %xmm0"));
                } else {
                    self.emit_insn(&Insn::raw_with_gas(&[0x66, 0x0F, 0x2E, 0xC0], "ucomisd %xmm0, %xmm0"));
                }
                // setp al (PF=1 ⇒ NaN)
                self.emit_insn(&Insn::setcc_reg(crate::codegen::x86_64::opcode::CondCode::P, 0));
                // movzx eax, al (REX.W 0F B6 C0)
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                    sign: false,
                    from: crate::codegen::shared::asm_operand::Width::B8,
                    to: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B8),
                }, &[]));
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_isinf / isinff / isinfl: ±Inf 检测.
            // 返回: +Inf→1, -Inf→-1, 其他 (含 NaN) →0.
            // 算法: 用 |x| vs +Inf 比较 (sete); 符号位取 sign.
            // 合成: result = isinf * (1 - 2*sign)
            //   sign=0: result = +isinf
            //   sign=1: result = -isinf
            // 排除 NaN: setnp (NOT PF). NaN 时 PF=1, setnp=0 → 整体被清零.
            // 类型按实参, 不用函数名后缀.
            "__builtin_isinf" | "__builtin_isinff" | "__builtin_isinfl" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                let arg_ty = args[0].ty();
                let is_f = matches!(arg_ty, Type::Float);
                // 1. isinf: |x| == +Inf 且非 NaN
                self.gen_expr(&args[0], Self::EVAL_BASE); // xmm0 = x
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64),
                    src: crate::codegen::x86_64::opcode::Operand::Imm(0x7FFFFFFFFFFFFFFFu64 as i64),
                }, &[]));
                self.emit_insn(&Insn::gpr_xmm(true, crate::codegen::shared::asm_operand::Width::B64, 0, 1)); // movq xmm1, rax
                self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Andps, SimdWidth::O, 0, 1)); // andps xmm0, xmm1 (|x|)
                // float +Inf bit pattern: 0x7F800000
                let inf_bits: u64 = if is_f { 0x7F800000 } else { 0x7FF0000000000000 };
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64),
                    src: crate::codegen::x86_64::opcode::Operand::Imm(inf_bits as i64),
                }, &[]));
                self.emit_insn(&Insn::gpr_xmm(true, crate::codegen::shared::asm_operand::Width::B64, 0, 1)); // movq xmm1, rax
                                                            // ucomiss (0F 2E) for float, ucomisd (66 0F 2E) for double
                if is_f {
                    self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Ucomi, SimdWidth::SS, 0, 1)); // ucomiss xmm0, xmm1
                } else {
                    self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Ucomi, SimdWidth::SD, 0, 1)); // ucomisd xmm0, xmm1
                }
                // sete al (ZF=1): |x| == +Inf
                self.emit_insn(&Insn::setcc_reg(crate::codegen::x86_64::opcode::CondCode::Eq, 0));
                // setnp bl (NOT PF): 非 NaN
                self.emit_insn(&Insn::setcc_reg(crate::codegen::x86_64::opcode::CondCode::Np, 3));
                // and al, bl: 既等于 Inf 又非 NaN
                self.emit_insn(&Insn::raw_bytes(&[0x20, 0xD8])); // and al, bl
                                          // movzx eax, al
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                    sign: false,
                    from: crate::codegen::shared::asm_operand::Width::B8,
                    to: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B32),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B8),
                }, &[]));
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        7, crate::codegen::shared::asm_operand::Width::B32
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                },
                &[],
            )); // mov edi, eax (edi = isinf)
                                          // 2. sign: 重新加载 x, 取符号位
                self.gen_expr(&args[0], Self::EVAL_BASE);
                self.emit_insn(&Insn::raw_bytes(&[0x0F, 0x50, 0xC0])); // movmskps eax, xmm0
                self.emit_insn(&Insn::alu_rm_reg_imm8(4, 0, 1)); // and eax, 1 (eax = sign)
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        6, crate::codegen::shared::asm_operand::Width::B32
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                },
                &[],
            )); // mov esi, eax (esi = sign)
                                          // 3. 合成: eax=isinf, edx=isinf, eax=-isinf; 若 sign==0, eax=edx
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        7, crate::codegen::shared::asm_operand::Width::B32
                    ),
                },
                &[],
            )); // mov eax, edi (eax = isinf)
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        2, crate::codegen::shared::asm_operand::Width::B32
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                },
                &[],
            )); // mov edx, eax (edx = isinf)
                self.emit_insn(&Insn::unary_rm_reg(crate::codegen::x86_64::opcode::UnaryOp::Neg, crate::codegen::shared::asm_operand::Width::B32, 0)); // neg eax (eax = -isinf)
                self.emit_insn(&Insn::test_rm_reg(crate::codegen::shared::asm_operand::Width::B32, 6, 6)); // test esi, esi
                self.emit_insn(&Insn::raw_bytes(&[0x0F, 0x44, 0xC2])); // cmove eax, edx
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_finite: x 既不是 NaN 也不是 Inf.
            // 用 |x| vs +Inf 比较, 同时排除 NaN (PF=1 ⇒ NaN).
            // 正确语义: finite = (|x| < +Inf) AND NOT NaN
            // 编码:  (|x| < +Inf) ⇒ CF=1 ⇒ setb al
            //         NaN          ⇒ PF=1 ⇒ setnp bl (NOT PF)
            //         finite = al AND bl
            "__builtin_finite" | "__builtin_finitef" | "__builtin_finitel" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                let arg_ty = args[0].ty();
                let is_f = matches!(arg_ty, Type::Float);
                self.gen_expr(&args[0], Self::EVAL_BASE); // xmm0 = x
                                            // 加载 abs_mask 到 xmm1
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64),
                    src: crate::codegen::x86_64::opcode::Operand::Imm(0x7FFFFFFFFFFFFFFFu64 as i64),
                }, &[]));
                self.emit_insn(&Insn::gpr_xmm(true, crate::codegen::shared::asm_operand::Width::B64, 0, 1)); // movq xmm1, rax
                                                            // andps xmm0, xmm1: |x|
                self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Andps, SimdWidth::O, 0, 1));
                // 加载 +Inf 到 xmm1
                let inf_bits: u64 = if is_f { 0x7F800000 } else { 0x7FF0000000000000 };
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64),
                    src: crate::codegen::x86_64::opcode::Operand::Imm(inf_bits as i64),
                }, &[]));
                self.emit_insn(&Insn::gpr_xmm(true, crate::codegen::shared::asm_operand::Width::B64, 0, 1)); // movq xmm1, rax
                                                            // ucomiss (0F 2E) for float, ucomisd (66 0F 2E) for double
                if is_f {
                    self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Ucomi, SimdWidth::SS, 0, 1)); // ucomiss xmm0, xmm1
                } else {
                    self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Ucomi, SimdWidth::SD, 0, 1)); // ucomisd xmm0, xmm1
                }
                // setb al (CF=1): |x| < +Inf
                self.emit_insn(&Insn::setcc_reg(crate::codegen::x86_64::opcode::CondCode::B, 0));
                // setnp bl (NOT PF): 非 NaN
                //   0F 9B C3 = setnp bl
                self.emit_insn(&Insn::setcc_reg(crate::codegen::x86_64::opcode::CondCode::Np, 3));
                // and al, bl: 两者都成立才是 finite
                self.emit_insn(&Insn::raw_bytes(&[0x20, 0xD8])); // and al, bl
                                          // movzx eax, al
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                    sign: false,
                    from: crate::codegen::shared::asm_operand::Width::B8,
                    to: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B8),
                }, &[]));
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_isunordered(a, b): 任一为 NaN 则 1.
            // 优先把两个实参对齐到 double (用 cvtps2pd/cvtss2sd).
            // 较新 Intel CPU 在 NaN 时 ZF=1, PF=1, CF=1 → setp 仍可识别.
            "__builtin_isunordered" => {
                if args.len() < 2 {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], Self::EVAL_BASE);
                // 如果 a 是 float, 升为 double
                if matches!(args[0].ty(), Type::Float) {
                    self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::CvtSdSs, SimdWidth::SS, 0, 0));
                }
                self.gen_expr(&args[1], 1);
                // 如果 b 是 float, 升为 double
                if matches!(args[1].ty(), Type::Float) {
                    self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::CvtSdSs, SimdWidth::SS, 1, 1));
                }
                // ucomisd xmm0, xmm1
                self.emit_insn(&Insn::raw_with_gas(&[0x66, 0x0F, 0x2E, 0xC1], "ucomisd %xmm0, %xmm1"));
                // setp al
                self.emit_insn(&Insn::setcc_reg(crate::codegen::x86_64::opcode::CondCode::P, 0));
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                    sign: false,
                    from: crate::codegen::shared::asm_operand::Width::B8,
                    to: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B8),
                }, &[]));
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_fpclassify(p, n, z, i, x, v): 浮点分类.
            // GCC 返回 0..5: NaN, Inf, Zero, Subnormal, Normal, 实现自定义.
            // 简化: 仅区分 4 类 (NaN, Inf, Zero, Normal); Subnormal 归到 Normal.
            //   NaN → args[0]
            //   Inf → args[1]
            //   Zero → args[2]
            //   其他 (含 Subnormal) → args[4]  (跳过 Subnormal args[3])
            "__builtin_fpclassify" => {
                if args.len() < 6 {
                    self.mov_imm(dst, 0);
                    return true;
                }
                let arg_ty = args[5].ty();
                let is_f = matches!(arg_ty, Type::Float);
                // 决定用 ucomiss 还是 ucomisd
                let cmp_width = if is_f { SimdWidth::SS } else { SimdWidth::SD };
                // 1. NaN 测试
                self.gen_expr(&args[5], Self::EVAL_BASE);
                self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Ucomi, cmp_width, 0, 0)); // ucomis{s,d} xmm0, xmm0
                self.emit_insn(&Insn::setcc_reg(crate::codegen::x86_64::opcode::CondCode::P, 3)); // setp bl
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                    sign: false,
                    from: crate::codegen::shared::asm_operand::Width::B8,
                    to: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(3, crate::codegen::shared::asm_operand::Width::B32),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(3, crate::codegen::shared::asm_operand::Width::B8),
                }, &[])); // movzx ebx, bl
                                                // 2. Zero 测试
                self.gen_expr(&args[5], Self::EVAL_BASE);
                self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Xorps, SimdWidth::PS, 1, 1)); // xorps xmm1, xmm1
                self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Ucomi, cmp_width, 1, 0)); // ucomis{s,d} xmm1, xmm0
                self.emit_insn(&Insn::setcc_reg(crate::codegen::x86_64::opcode::CondCode::Eq, 1)); // sete cl
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                    sign: false,
                    from: crate::codegen::shared::asm_operand::Width::B8,
                    to: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(1, crate::codegen::shared::asm_operand::Width::B32),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(1, crate::codegen::shared::asm_operand::Width::B8),
                }, &[])); // movzx ecx, cl
                                                // 3. Inf 测试
                self.gen_expr(&args[5], Self::EVAL_BASE);
                let inf_bits: u64 = if is_f { 0x7F800000 } else { 0x7FF0000000000000 };
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64),
                    src: crate::codegen::x86_64::opcode::Operand::Imm(inf_bits as i64),
                }, &[]));
                self.emit_insn(&Insn::gpr_xmm(true, crate::codegen::shared::asm_operand::Width::B64, 0, 1)); // movq xmm1, rax
                self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Ucomi, cmp_width, 0, 1)); // ucomis{s,d} xmm0, xmm1
                self.emit_insn(&Insn::setcc_reg(crate::codegen::x86_64::opcode::CondCode::Eq, 2)); // sete dl
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                    sign: false,
                    from: crate::codegen::shared::asm_operand::Width::B8,
                    to: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(2, crate::codegen::shared::asm_operand::Width::B32),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(2, crate::codegen::shared::asm_operand::Width::B8),
                }, &[])); // movzx edx, dl
                                                // 4. 合成: 优先级 NaN > Inf > Zero > Normal
                                                // 默认: args[4] (Normal)
                self.gen_expr(&args[4], Self::EVAL_BASE);
                // 若 Zero: eax = args[2]
                // 若 Inf:  eax = args[1]
                // 若 NaN:  eax = args[0]
                // 我们用栈暂存 args[0..3] 到 r12-r15? 太复杂.
                // 简化: 返回分类编号 (0..4), 由调用者按映射.
                // 分类编号: NaN=0, Inf=1, Zero=2, Subnormal=3, Normal=4
                self.emit_insn(&Insn::new(
                    crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                        width: crate::codegen::shared::asm_operand::Width::B32,
                        dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B32),
                        src: crate::codegen::x86_64::opcode::Operand::Imm(4),
                    },
                    &[],
                )); // mov eax, 4 (Normal)
                                                            // eax=4 默认
                                                            // 若 NaN (ebx=1): eax = 0
                                                            // 若 Inf (edx=1): eax = 1
                                                            // 若 Zero (ecx=1): eax = 2
                                                            // 链: 先 mov eax, 4; test ecx cmove eax, 2; test edx cmove eax, 1; test ebx cmove eax, 0
                                                            // 准备 3 个常量:
                self.emit_insn(&Insn::raw_bytes(&[0xBF, 0x02, 0x00, 0x00, 0x00])); // mov edi, 2
                self.emit_insn(&Insn::raw_bytes(&[0xBE, 0x01, 0x00, 0x00, 0x00])); // mov esi, 1
                self.emit_insn(&Insn::raw_bytes(&[0xBD, 0x00, 0x00, 0x00, 0x00])); // mov ebp, 0
                                                            // 实际上分类值由调用者映射, 我们这里返回 0/1/2/4 即可
                                                            // cmovne: 当 ebx=1 (NaN) 时 eax=0; edx=1 (Inf) 时 eax=1; ecx=1 (Zero) 时 eax=2
                                                            // 注意 cmovne 条件是 ZF=0 (非零), cmove 条件是 ZF=1 (零)
                self.emit_insn(&Insn::test_rm_reg(crate::codegen::shared::asm_operand::Width::B32, 1, 1)); // test ecx, ecx (Zero?)
                self.emit_insn(&Insn::raw_bytes(&[0x0F, 0x45, 0xC7])); // cmovne eax, edi (Zero=2)
                self.emit_insn(&Insn::test_rm_reg(crate::codegen::shared::asm_operand::Width::B32, 2, 2)); // test edx, edx (Inf?)
                self.emit_insn(&Insn::raw_bytes(&[0x0F, 0x45, 0xC6])); // cmovne eax, esi (Inf=1)
                self.emit_insn(&Insn::test_rm_reg(crate::codegen::shared::asm_operand::Width::B32, 3, 3)); // test ebx, ebx (NaN?)
                self.emit_insn(&Insn::raw_bytes(&[0x0F, 0x45, 0xC5])); // cmovne eax, ebp (NaN=0)
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_signbit: 浮点数符号位 (x<0 返回 1, 否则 0)
            // 用 movmskps 提取 4 packed single 的符号位, 放入 eax 的 bit 0..3.
            //   bit 0 = 第一个 32-bit 字的符号位
            //   bit 1 = 第二个 32-bit 字的符号位
            //   bit 2 = 第三个 32-bit 字的符号位
            //   bit 3 = 第四个 32-bit 字的符号位
            // double 占据 64 位, 其符号位在 bit 63 = 第二个 32-bit 字的最高位
            //   => 对应 movmskps 输出的 bit 1
            // float 占据 32 位, 符号位在 bit 31 = 第一个 32-bit 字的最高位
            //   => 对应 movmskps 输出的 bit 0
            // 简化: 用 shr eax, 1 后再 and 1 拿 bit 1 (double), 但 float 会被右移
            //   丢掉. 我们用类型区分:
            //   double/long double: and 2 → 是否为负
            //   float:               and 1 → 是否为负
            "__builtin_signbit" | "__builtin_signbitf" | "__builtin_signbitl" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                let arg_ty = args[0].ty();
                let is_f = matches!(arg_ty, Type::Float);
                self.gen_expr(&args[0], Self::EVAL_BASE); // xmm0 = x
                                            // movmskps eax, xmm0
                self.emit_insn(&Insn::raw_bytes(&[0x0F, 0x50, 0xC0]));
                if is_f {
                    // float: 符号位是 bit 0
                    self.emit_insn(&Insn::alu_rm_reg_imm8(4, 0, 1)); // and eax, 1
                } else {
                    // double/long double: 符号位是 bit 1
                    self.emit_insn(&Insn::alu_rm_reg_imm8(4, 0, 2)); // and eax, 2
                                                    // 归一化为 0/1: shr 1
                    self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Shift {
                        op: crate::codegen::x86_64::opcode::ShiftOp::Shr,
                        width: crate::codegen::shared::asm_operand::Width::B32,
                        dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B32),
                        amount: crate::codegen::x86_64::opcode::ShiftAmount::Imm8(1),
                    }, &[])); // shr eax, 1
                }
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_call_with_static_chain(call, chain, args...):
            // 加载 chain 到 r10 (静态链寄存器), 然后按 SysV ABI 准备 args... 参数,
            // 最后调用 call. 这是嵌套函数调用的关键 ABI: r10 = 父帧指针.
            //
            // 注: 嵌套函数"取地址" (nested function → function pointer) 还需要
            // 单独生成 trampoline, 才能在跨函数指针场景用上, 本 builtin 主要给
            // 已经能拿到函数指针 (例如通过宏展开) 的场景使用.
            "__builtin_call_with_static_chain" => {
                if args.len() < 2 {
                    self.mov_imm(dst, 0);
                    return true;
                }
                // 1. chain → r10
                self.gen_expr(&args[1], 10);
                // 2. 按 SysV ABI 准备剩余参数 (args[2..]) 到 rdi/rsi/rdx/rcx/r8/r9 + 栈
                let extra = &args[2..];
                let gp_regs: [u8; 6] = [7, 6, 2, 1, 8, 9]; // rdi, rsi, rdx, rcx, r8, r9
                let mut stack_off: i32 = 0;
                for (i, a) in extra.iter().enumerate() {
                    let arg_ty = a.ty();
                    if i < 6 {
                        // 前 6 个参数: 整数到 gp_regs[i], 浮点到 xmm{i}
                        if matches!(arg_ty, Type::Float | Type::Double | Type::LongDouble) {
                            self.gen_expr(a, Self::EVAL_BASE); // xmm0
                            if i != 0 {
                                // movaps xmm{i}, xmm0
                                self.emit_insn(&Insn::movss_xmm_store(i as u8, 0));
                            }
                        } else {
                            self.gen_expr(a, gp_regs[i]);
                        }
                    } else {
                        // 第 7+ 个参数: 压栈
                        self.gen_expr(a, Self::EVAL_BASE);
                        if matches!(arg_ty, Type::Float | Type::Double | Type::LongDouble) {
                            self.emit_insn(&Insn::simd_mov_mem(true, crate::codegen::x86_64::opcode::SimdWidth::SD, 0, 0)); // movsd [rsp+0], xmm0
                        } else {
                            self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(4),
                            index: None,
                            seg: None,
                        }
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B64
                    ),
                },
                &[],
            )); // mov [rsp+0], rax
                        }
                        stack_off += 8;
                    }
                }
                if stack_off != 0 {
                    // sub rsp, stack_off
                    if stack_off < 128 {
                        self.emit_insn(&Insn::sub_rsp_imm(stack_off as i64));
                    } else {
                        self.emit_insn(&Insn::raw_bytes(&[0x48, 0x81, 0xEC]));
                        self.emit_insn(&Insn::raw_bytes(&stack_off.to_le_bytes()));
                    }
                }
                // 3. 调整 RSP 对齐 (call 需要 16 字节对齐)
                // 注意: 简化版, 假设栈已经 16 字节对齐.
                // 4. 调用 args[0]
                match &args[0] {
                    Expr::Ident { name, .. } => {
                        self.call_label(&name.name);
                    },
                    _ => {
                        // 函数指针: 加载到 r11 然后 call r11
                        self.gen_expr(&args[0], 11);
                        self.emit_insn(&Insn::raw_bytes(&[0x41, 0xFF, 0xD3])); // call r11
                    },
                }
                if stack_off != 0 {
                    // add rsp, stack_off
                    if stack_off < 128 {
                        self.emit_insn(&Insn::add_rsp_imm(stack_off as i64));
                    } else {
                        self.emit_insn(&Insn::raw_bytes(&[0x48, 0x81, 0xC4]));
                        self.emit_insn(&Insn::raw_bytes(&stack_off.to_le_bytes()));
                    }
                }
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_clz/clzl/clzll: 前导零计数
            "__builtin_clz" | "__builtin_clzl" | "__builtin_clzll" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], dst);
                // GCC 语义: 无后缀 → int 宽度(32), l → long 宽度, ll → long long 宽度.
                let bits = match name {
                    "__builtin_clz" => 32,
                    _ => args[0].ty().size() as i32 * 8,
                };
                let rex = 0x48 | if dst >= 8 { 0x05 } else { 0 };   // 位序: REX.R+B: 自反 ModRM, 同一个寄存器既在 reg 又在 r/m, 判据见 docs/ssa-pass-upgrade-plan.md §7.21
                // bsr dst, dst → dst = 最高位位置
                self.emit_insn(&Insn::raw_bytes(&[rex, 0x0F, 0xBD, 0xC0 | ((dst & 7) << 3) | (dst & 7)]));
                // clz = (bits - 1) - dst
                self.emit_insn(&Insn::unary_rm_reg(
                    crate::codegen::x86_64::opcode::UnaryOp::Neg,
                    crate::codegen::shared::asm_operand::Width::B64,
                    dst,
                ));
                self.emit_insn(&Insn::raw_bytes(&[
                    rex,
                    0x83,
                    0xC0 | ((dst & 7) << 3) | (dst & 7),
                    (bits - 1) as u8,
                ])); // add dst, bits-1
                true
            },
            // __builtin_ctz/ctzl/ctzll: 尾随零计数
            "__builtin_ctz" | "__builtin_ctzl" | "__builtin_ctzll" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], dst);
                let rex = 0x48 | if dst >= 8 { 0x05 } else { 0 };   // 位序: REX.R+B: 自反 ModRM, 同一个寄存器既在 reg 又在 r/m, 判据见 docs/ssa-pass-upgrade-plan.md §7.21
                // bsf dst, dst → dst = 最低位位置
                self.emit_insn(&Insn::raw_bytes(&[rex, 0x0F, 0xBC, 0xC0 | ((dst & 7) << 3) | (dst & 7)])); // bsf dst, dst
                true
            },
            // __builtin_popcount/popcountl/popcountll: 位计数
            "__builtin_popcount" | "__builtin_popcountl" | "__builtin_popcountll" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], dst);
                let rex = 0x48 | if dst >= 8 { 0x05 } else { 0 };   // 位序: REX.R+B: 自反 ModRM, 同一个寄存器既在 reg 又在 r/m, 判据见 docs/ssa-pass-upgrade-plan.md §7.21
                // popcnt dst, dst: F3 0F B8 /r
                self.emit_insn(&Insn::raw_bytes(&[0xF3, rex, 0x0F, 0xB8, 0xC0 | ((dst & 7) << 3) | (dst & 7)]));
                true
            },
            // __builtin_parity/parityl/parityll: 奇偶校验 (popcount & 1)
            "__builtin_parity" | "__builtin_parityl" | "__builtin_parityll" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                // XOR fold，避免依赖可选的 POPCNT CPU 特性。
                self.gen_expr(&args[0], Self::EVAL_BASE);
                for shift in [32u8, 16, 8, 4, 2, 1] {
                    self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovRm { width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(1, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // mov rcx, rax
                    self.emit_insn(&Insn::raw_bytes(&[0x48, 0xC1, 0xE9, shift])); // shr rcx, shift
                    // P11: 收敛到 AluRm{Xor, B64, rax, rcx}.
                    let insn = Insn::new(
                        crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                            op: crate::codegen::x86_64::opcode::AluOp::Xor,
                            width: crate::codegen::shared::asm_operand::Width::B64,
                            dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                0,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                            src: crate::codegen::x86_64::opcode::Operand::Reg(
                                1,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                        },
                        &[],
                    );
                    if !self.emit_insn(&insn) {
                        self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::AluRm { op: crate::codegen::x86_64::opcode::AluOp::Xor, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Reg(1, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // xor rax, rcx
                    }
                }
                self.emit_insn(&Insn::alu_rm_reg_imm8(4, 0, 1)); // and eax, 1
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_bitreverse8/16/32/64: 固定次数移位循环，无外部运行库依赖。
            "__builtin_bitreverse8"
            | "__builtin_bitreverse16"
            | "__builtin_bitreverse32"
            | "__builtin_bitreverse64" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                let bits = match name {
                    "__builtin_bitreverse8" => 8u32,
                    "__builtin_bitreverse16" => 16,
                    "__builtin_bitreverse32" => 32,
                    _ => 64,
                };
                self.gen_expr(&args[0], Self::EVAL_BASE); // rax = input
                // P11: 收敛到 AluRm{Xor, B64, rcx, rcx}.
                {
                    let insn = Insn::new(
                        crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                            op: crate::codegen::x86_64::opcode::AluOp::Xor,
                            width: crate::codegen::shared::asm_operand::Width::B64,
                            dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                1,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                            src: crate::codegen::x86_64::opcode::Operand::Reg(
                                1,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                        },
                        &[],
                    );
                    if !self.emit_insn(&insn) {
                        self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::AluRm { op: crate::codegen::x86_64::opcode::AluOp::Xor, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(1, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Reg(1, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // xor rcx, rcx
                    }
                }
                self.emit_insn(&Insn::raw_bytes(&[0xBA]));
                self.emit_insn(&Insn::raw_bytes(&bits.to_le_bytes())); // mov edx, bits
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Shift {
                    op: crate::codegen::x86_64::opcode::ShiftOp::Shr,
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64),
                    amount: crate::codegen::x86_64::opcode::ShiftAmount::Imm8(1),
                }, &[])); // shr rax, 1 (LSB -> CF)
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0xD1, 0xD1])); // rcl rcx, 1
                self.emit_insn(&Insn::raw_bytes(&[0xFF, 0xCA])); // dec edx
                self.emit_insn(&Insn::raw_bytes(&[0x75, 0xF6])); // jnz loop
                if dst != 1 {
                    self.mov(dst, 1);
                }
                true
            },
            // __builtin_frame_address(level): 帧指针
            "__builtin_frame_address" => {
                // 本后端使用 r13 作为稳定帧指针；[r13] 是调用者保存的 r13。
                self.emit_insn(&Insn::raw_with_gas(&[0x4C, 0x89, 0xE8], "mov %r13, %rax")); // mov rax, r13
                let level = if let Some(Expr::Int { v, .. }) = args.first() {
                    *v as i32
                } else {
                    0
                };
                for _ in 0..level {
                    self.emit_insn(&Insn::raw_with_gas(&[0x48, 0x8B, 0x00], "mov (%rax), %rax")); // mov rax, [rax]
                }
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_return_address(level): 返回地址
            "__builtin_return_address" => {
                // push r13 后 r13 指向保存的旧 r13，返回地址位于 [r13+8]。
                self.emit_insn(&Insn::raw_with_gas(&[0x4C, 0x89, 0xE8], "mov %r13, %rax")); // mov rax, r13
                let level = if let Some(Expr::Int { v, .. }) = args.first() {
                    *v as i32
                } else {
                    0
                };
                for _ in 0..level {
                    self.emit_insn(&Insn::raw_with_gas(&[0x48, 0x8B, 0x00], "mov (%rax), %rax")); // mov rax, [rax]
                }
                self.emit_insn(&Insn::raw_with_gas(&[0x48, 0x8B, 0x40, 0x08], "mov 8(%rax), %rax")); // mov rax, [rax+8]
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // *_overflow_p(a, b, (type)0): 计算目标类型宽度下的 OF/CF。
            "__builtin_overflow_p"
            | "__builtin_add_overflow_p"
            | "__builtin_sub_overflow_p"
            | "__builtin_mul_overflow_p" => {
                if args.len() < 2 {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], Self::EVAL_BASE);
                self.push(0);
                self.gen_expr(&args[1], Self::EVAL_BASE);
                self.pop(1); // rcx = lhs, rax = rhs
                let result_ty = args.get(2).map(|e| e.ty()).unwrap_or_else(|| args[0].ty());
                let size = result_ty.size();
                let unsigned = matches!(
                    result_ty,
                    Type::Bool
                        | Type::UChar
                        | Type::UShort
                        | Type::UInt
                        | Type::ULong
                        | Type::ULongLong
                        | Type::UInt128
                );
                let op = if name.contains("sub") {
                    1
                } else if name.contains("mul") {
                    2
                } else {
                    0
                };
                // P11 helper: AluRm{Add|Sub, rcx, rax} 按 sz 选择宽度.
                let mk_addsub =
                    |op: u8, sz: usize| -> crate::codegen::x86_64::opcode::X86_64Opcode {
                        use crate::codegen::shared::asm_operand::Width;
                        use crate::codegen::x86_64::opcode::{AluOp, Operand as Op};
                        let w = match sz {
                            1 => Width::B8,
                            2 => Width::B16,
                            4 => Width::B32,
                            _ => Width::B64,
                        };
                        let op = if op == 0 { AluOp::Add } else { AluOp::Sub };
                        crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                            op,
                            width: w,
                            dst: Op::Reg(1, w),
                            src: Op::Reg(0, w),
                        }
                    };
                match (op, size) {
                    (0, 1) => {
                        let insn = Insn::new(mk_addsub(0, 1), &[]);
                        if !self.emit_insn(&insn) { self.emit_insn(&Insn::raw_bytes(&[0x00, 0xC1])); }
                    }       // add cl, al
                    (0, 2) => {
                        let insn = Insn::new(mk_addsub(0, 2), &[]);
                        if !self.emit_insn(&insn) { self.emit_insn(&Insn::raw_bytes(&[0x66, 0x01, 0xC1])); }
                    } // add cx, ax
                    (0, 4) => {
                        let insn = Insn::new(mk_addsub(0, 4), &[]);
                        if !self.emit_insn(&insn) { self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::Add, crate::codegen::shared::asm_operand::Width::B32, 1, 0)); }
                    }       // add ecx, eax
                    (0, _) => {
                        let insn = Insn::new(mk_addsub(0, 8), &[]);
                        if !self.emit_insn(&insn) { self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::Add, crate::codegen::shared::asm_operand::Width::B64, 1, 0)); }
                    } // add rcx, rax
                    (1, 1) => {
                        let insn = Insn::new(mk_addsub(1, 1), &[]);
                        if !self.emit_insn(&insn) { self.emit_insn(&Insn::raw_bytes(&[0x28, 0xC1])); }
                    }       // sub cl, al
                    (1, 2) => {
                        let insn = Insn::new(mk_addsub(1, 2), &[]);
                        if !self.emit_insn(&insn) { self.emit_insn(&Insn::raw_bytes(&[0x66, 0x29, 0xC1])); }
                    } // sub cx, ax
                    (1, 4) => {
                        let insn = Insn::new(mk_addsub(1, 4), &[]);
                        if !self.emit_insn(&insn) { self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::Sub, crate::codegen::shared::asm_operand::Width::B32, 1, 0)); }
                    }       // sub ecx, eax
                    (1, _) => {
                        let insn = Insn::new(mk_addsub(1, 8), &[]);
                        if !self.emit_insn(&insn) { self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::Sub, crate::codegen::shared::asm_operand::Width::B64, 1, 0)); }
                    } // sub rcx, rax
                    (2, 1) => { self.emit_insn(&Insn::raw_bytes(&[0xF6, if unsigned { 0xE1 } else { 0xE9 }])); } // mul/imul cl
                    (2, 2) => {
                        if unsigned {
                            self.emit_insn(&Insn::raw_bytes(&[0x66, 0xF7, 0xE1])); // mul cx (dx:ax = ax*cx)
                        } else {
                            self.emit_insn(&Insn::raw_bytes(&[0x66, 0x0F, 0xAF, 0xC8])); // imul cx, ax
                        }
                    },
                    (2, 4) => {
                        if unsigned {
                            self.emit_insn(&Insn::raw_bytes(&[0xF7, 0xE1])); // mul ecx (edx:eax = eax*ecx)
                        } else {
                            self.emit_insn(&Insn::imul_rm_reg(
                                crate::codegen::shared::asm_operand::Width::B32,
                                1, 0, // ecx, eax
                            ));
                        }
                    },
                    (2, _) => {
                        if unsigned {
                            self.emit_insn(&Insn::raw_bytes(&[0x48, 0xF7, 0xE1])); // mul rcx (rdx:rax = rax*rcx)
                        } else {
                            self.emit_insn(&Insn::imul_rm_reg(
                                crate::codegen::shared::asm_operand::Width::B64,
                                1, 0, // rcx, rax
                            ));
                        }
                    },
                    _ => unreachable!(),
                }
                let cc = if unsigned { 0x92 } else { 0x90 }; // setb / seto
                self.emit_insn(&Insn::raw_bytes(&[0x0F, cc, 0xC0]));
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                    sign: false,
                    from: crate::codegen::shared::asm_operand::Width::B8,
                    to: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B32),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B8),
                }, &[]));
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_apply/__builtin_return: SysV 子集（≤6 个 GP 参数）
            "__builtin_apply" => {
                if args.len() < 3 {
                    self.mov_imm(dst, 0);
                    return true;
                }
                let size = args
                    .get(2)
                    .and_then(|e| const_eval::eval(e).ok())
                    .filter(|s| *s > 0)
                    .unwrap_or(-1);
                if size <= 0 {
                    self.mov_imm(dst, 0);
                    return true;
                }
                let size = size as i32;
                // 帧: [0..size) 栈参数拷贝, [size..size+16) 结果块,
                // [size+16..size+24) 缓冲区指针保存槽, 余量为对齐.
                let frame = ((size + 32 + 15) / 16) * 16;
                let buf_off = size + 16;
                self.gen_builtin_fn_ptr(&args[0], 10); // r10 = fn
                self.gen_expr(&args[1], 6); // rsi = data
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x81, 0xEC]));
                self.emit_insn(&Insn::raw_bytes(&frame.to_le_bytes())); // sub rsp, frame
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x89, 0xB4, 0x24]));
                self.emit_insn(&Insn::raw_bytes(&buf_off.to_le_bytes())); // mov [rsp+buf_off], rsi
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovRm { width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(7, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Reg(4, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // mov rdi, rsp
                                                // 栈参数区在缓冲区 GP+XMM 保存区 (176 字节) 之后
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x81, 0xC6]));
                self.emit_insn(&Insn::raw_bytes(&176u32.to_le_bytes())); // add rsi, 176
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0xC7, 0xC1]));
                self.emit_insn(&Insn::raw_bytes(&size.to_le_bytes())); // mov rcx, size
                self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Cld)); self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::RepMovsb)); // cld; rep movsb
                                                      // 从缓冲区恢复 GP 寄存器实参 (rdi..r9)
                self.emit_insn(&Insn::raw_bytes(&[0x4C, 0x8B, 0x9C, 0x24]));
                self.emit_insn(&Insn::raw_bytes(&buf_off.to_le_bytes())); // mov r11, [rsp+buf_off]
                self.emit_insn(&Insn::raw_bytes(&[0x49, 0x8B, 0x3B])); // mov rdi, [r11] (REX.W+B)
                self.emit_insn(&Insn::raw_bytes(&[0x49, 0x8B, 0x73, 0x08])); // mov rsi, [r11+8]
                self.emit_insn(&Insn::raw_bytes(&[0x49, 0x8B, 0x53, 0x10])); // mov rdx, [r11+16]
                self.emit_insn(&Insn::raw_bytes(&[0x49, 0x8B, 0x4B, 0x18])); // mov rcx, [r11+24]
                self.emit_insn(&Insn::raw_bytes(&[0x4D, 0x8B, 0x43, 0x20])); // mov r8, [r11+32]
                self.emit_insn(&Insn::raw_bytes(&[0x4D, 0x8B, 0x4B, 0x28])); // mov r9, [r11+40]
                self.emit_insn(&Insn::raw_bytes(&[0x41, 0xFF, 0xD2])); // call r10
                                                // 结果块: [0]=rax, [8]=rdx (16 字节整数返回约定)
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x89, 0x84, 0x24]));
                self.emit_insn(&Insn::raw_bytes(&size.to_le_bytes())); // mov [rsp+size], rax
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x89, 0x94, 0x24]));
                self.emit_insn(&Insn::raw_bytes(&(size + 8).to_le_bytes())); // mov [rsp+size+8], rdx
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x8D, 0x84, 0x24]));
                self.emit_insn(&Insn::raw_bytes(&size.to_le_bytes())); // lea rax, [rsp+size]
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            "__builtin_return" => {
                if !args.is_empty() {
                    self.gen_expr(&args[0], 6); // rsi = result block
                    self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B64
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(6),
                            index: None,
                            seg: None,
                        }
                    ),
                },
                &[],
            )); // mov rax, [rsi]
                }
                if self.target.instrument_functions && !self.current_no_instrument {
                    self.emit_instrument_func_exit();
                }
                self.emit_func_epilogue();
                true
            },
            // __builtin_prefetch(addr, rw=0, locality=3): emit actual prefetch instructions
            n if n.starts_with("__builtin_prefetch") => {
                if args.is_empty() {
                    return true;
                }
                // Evaluate address argument to rax
                self.gen_expr(&args[0], Self::EVAL_BASE);
                // Determine rw (0=read, 1=write) and locality (0-3)
                let rw = args
                    .get(1)
                    .and_then(|e| const_eval::eval(e).ok())
                    .unwrap_or(0);
                let locality = args
                    .get(2)
                    .and_then(|e| const_eval::eval(e).ok())
                    .unwrap_or(3);
                // Emit prefetch instruction based on rw and locality
                // prefetchw for write, otherwise prefetchnta/t0/t1/t2 based on locality
                if rw == 1 {
                    // prefetchw [rax]: 0x0F 0x0D 0x08
                    self.emit_insn(&Insn::raw_bytes(&[0x0F, 0x0D, 0x08]));
                } else {
                    // locality 0 -> prefetchnta, 1 -> prefetcht2, 2 -> prefetcht1, 3 -> prefetcht0
                    let modrm = match locality {
                        0 => 0x00, // prefetchnta [rax]
                        1 => 0x18, // prefetcht2 [rax]
                        2 => 0x10, // prefetcht1 [rax]
                        _ => 0x08, // prefetcht0 [rax] (default)
                    };
                    self.emit_insn(&Insn::raw_bytes(&[0x0F, 0x18, modrm]));
                }
                true
            },
            // __builtin_add_overflow(a, b, &result): 溢出检测
            // 策略: 计算 64 位结果, 截断到操作数类型, 比较检测溢出
            "__builtin_add_overflow" | "__builtin_sub_overflow" | "__builtin_mul_overflow" => {
                if args.len() < 3 {
                    self.mov_imm(dst, 0);
                    return true;
                }
                // 溢出检测基于结果类型 (第三个参数解引用后的类型)
                let result_ty = match &args[2].ty() {
                    Type::Ptr(p) => p.base.clone(),
                    _ => args[2].ty(),
                };
                let result_size = result_ty.size().max(4);
                let is_unsigned = !Self::is_signed_int(&result_ty);
                // 1. 求值 ptr → r10
                self.gen_expr(&args[2], Self::EVAL_BASE);
                self.emit_insn(&Insn::raw_bytes(&[0x49, 0x89, 0xc2])); // mov r10, rax
                                                // 2. 求值 a → push
                self.gen_expr(&args[0], Self::EVAL_BASE);
                self.push(0);
                // 3. 求值 b → rax
                self.gen_expr(&args[1], Self::EVAL_BASE);
                // 4. pop a → rcx
                self.pop(1);
                // rax = b, rcx = a, r10 = ptr
                // 5. 计算 64 位结果
                match name {
                    "__builtin_add_overflow" => {
                        self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::Add, crate::codegen::shared::asm_operand::Width::B64, 0, 1)); // add rax, rcx
                    },
                    "__builtin_sub_overflow" => {
                        self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovRm { width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(2, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // mov rdx, rax
                        self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovRm { width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Reg(1, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // mov rax, rcx
                        self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::Sub, crate::codegen::shared::asm_operand::Width::B64, 0, 2)); // sub rax, rdx
                    },
                    "__builtin_mul_overflow" => {
                        // GCC 语义 (fold_builtin_arith_overflow): 操作数按
                        // 自身类型的值在无限精度下相乘 (无中间 usual
                        // conversion); 溢出 = 数学积不能被 *res 类型表示;
                        // 存储值为积 wrap 到 res 类型.
                        // 精确路径: 各输入按自身类型扩展到 64 位, 128 位乘法
                        // (mul/imul) 得到 rdx:rax = 数学积的精确补码表示, 再按
                        // res 宽度/符号判断 fit. 同号乘法才可用单指令; 混合
                        // 符号与 __int128 输入退回硬件标志近似.
                        let a_uns = !Self::is_signed_int(&args[0].ty());
                        let b_uns = !Self::is_signed_int(&args[1].ty());
                        let op_sz = args[0].ty().size().max(args[1].ty().size()).max(4);
                        if op_sz >= 16 {
                            // __int128 输入: 128×128 乘积需 256 位精度才能
                            // 精确判溢出, 退回硬件近似 (imul 低 64 位 + 标志)
                            self.emit_insn(&Insn::raw_bytes(&[0x48, 0x0f, 0xaf, 0xc1])); // imul rax, rcx
                            self.emit_insn(&Insn::sign_extend_rax_to_rdx(crate::codegen::shared::asm_operand::Width::B64)); // cqo: rdx = 近似积高 64
                            let res_ty = match &args[2].ty() {
                                Type::Ptr(p) => p.base.clone(),
                                other => other.clone(),
                            };
                            let res_sz = res_ty.size();
                            if res_sz >= 16 {
                                self.emit_insn(&Insn::raw_bytes(&[0x49, 0x89, 0x02])); // mov [r10], rax
                                self.emit_insn(&Insn::raw_bytes(&[0x49, 0x89, 0x52, 0x08])); // mov [r10+8], rdx
                            } else if res_sz == 8 {
                                self.emit_insn(&Insn::raw_bytes(&[0x49, 0x89, 0x02])); // mov [r10], rax
                            } else if res_sz == 4 {
                                self.emit_insn(&Insn::raw_bytes(&[0x41, 0x89, 0x02])); // mov [r10], eax
                            } else if res_sz == 2 {
                                self.emit_insn(&Insn::raw_bytes(&[0x66, 0x41, 0x89, 0x02])); // mov [r10], ax
                            } else {
                                self.emit_insn(&Insn::raw_bytes(&[0x41, 0x88, 0x02])); // mov [r10], al
                            }
                            if Self::is_signed_int(&res_ty) {
                                self.emit_insn(&Insn::raw_bytes(&[0x0f, 0x90, 0xc2])); // seto dl
                            } else {
                                self.emit_insn(&Insn::raw_bytes(&[0x0f, 0x92, 0xc2])); // setc dl
                            }
                            self.emit_insn(&Insn::raw_bytes(&[0x0f, 0xb6, 0xc2])); // movzx eax, dl
                            if dst != 0 {
                                self.mov(dst, 0);
                            }
                            return true;
                        }
                        // rax = b, rcx = a: 按各自类型扩展到 64 位
                        if op_sz < 8 {
                            if a_uns {
                                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        1, crate::codegen::shared::asm_operand::Width::B32
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        1, crate::codegen::shared::asm_operand::Width::B32
                    ),
                },
                &[],
            )); // mov ecx, ecx
                            } else {
                                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                                    sign: true,
                                    from: crate::codegen::shared::asm_operand::Width::B32,
                                    to: crate::codegen::shared::asm_operand::Width::B64,
                                    dst: crate::codegen::x86_64::opcode::Operand::Reg(1, crate::codegen::shared::asm_operand::Width::B64),
                                    src: crate::codegen::x86_64::opcode::Operand::Reg(1, crate::codegen::shared::asm_operand::Width::B32),
                                }, &[])); // movsxd rcx, ecx
                            }
                            if b_uns {
                                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                },
                &[],
            )); // mov eax, eax
                            } else {
                                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                                    sign: true,
                                    from: crate::codegen::shared::asm_operand::Width::B32,
                                    to: crate::codegen::shared::asm_operand::Width::B64,
                                    dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64),
                                    src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B32),
                                }, &[])); // movsxd rax, eax
                            }
                        }
                        // rdx:rax = 数学积 (128 位精确)
                        if a_uns != b_uns {
                            // 混合符号: imul 结果 + 运行时修正.
                            // P = imul_128 + d×2^64×v (d=unsigned 侧 bit63,
                            // v=signed 侧位模式); 单条件修正后仍精确.
                            self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovRm { width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(6, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // mov rsi, rax (b 位模式)
                            self.emit_insn(&Insn::raw_bytes(&[0x48, 0xf7, 0xe9])); // imul rcx
                            let ok = self.fresh_label();
                            if a_uns {
                                self.emit_insn(&Insn::test_rm_reg(
                                    crate::codegen::shared::asm_operand::Width::B64,
                                    1, 1,
                                )); // test rcx, rcx
                                self.jmp_rel32(&ok, JumpKind::Jns);
                                self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::Add, crate::codegen::shared::asm_operand::Width::B64, 2, 6)); // add rdx, rsi
                            } else {
                                self.emit_insn(&Insn::test_rm_reg(
                                    crate::codegen::shared::asm_operand::Width::B64,
                                    6, 6,
                                )); // test rsi, rsi
                                self.jmp_rel32(&ok, JumpKind::Jns);
                                self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::Add, crate::codegen::shared::asm_operand::Width::B64, 2, 1)); // add rdx, rcx
                            }
                            self.label(&ok);
                        } else if a_uns {
                            self.emit_insn(&Insn::raw_bytes(&[0x48, 0xf7, 0xe1])); // mul rcx
                        } else {
                            self.emit_insn(&Insn::raw_bytes(&[0x48, 0xf7, 0xe9])); // imul rcx
                        }
                        let res_ty = match &args[2].ty() {
                            Type::Ptr(p) => p.base.clone(),
                            other => other.clone(),
                        };
                        let res_sz = res_ty.size();
                        let res_uns = !Self::is_signed_int(&res_ty);
                        // 1/2/4 字节: 先提取 wrapped → rsi (存储与比较共用),
                        // r11 保留原始低 64 位用于溢出比较
                        if res_sz < 8 {
                            self.emit_insn(&Insn::raw_bytes(&[0x49, 0x89, 0xc3])); // mov r11, rax
                            match (res_sz, res_uns) {
                                (4, true) => { self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        6, crate::codegen::shared::asm_operand::Width::B32
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                },
                &[],
            )); }        // mov esi, eax
                                (4, false) => { self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                                    sign: true,
                                    from: crate::codegen::shared::asm_operand::Width::B32,
                                    to: crate::codegen::shared::asm_operand::Width::B64,
                                    dst: crate::codegen::x86_64::opcode::Operand::Reg(6, crate::codegen::shared::asm_operand::Width::B64),
                                    src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B32),
                                }, &[])); } // movsxd rsi, eax
                                (2, true) => { self.emit_insn(&Insn::raw_bytes(&[0x0f, 0xb7, 0xf0])); }  // movzx esi, ax
                                (2, false) => { self.emit_insn(&Insn::raw_bytes(&[0x0f, 0xbf, 0xf0])); } // movsx esi, ax
                                (1, true) => { self.emit_insn(&Insn::raw_bytes(&[0x0f, 0xb6, 0xf0])); }  // movzx esi, al
                                _ => { self.emit_insn(&Insn::raw_bytes(&[0x0f, 0xbe, 0xf0])); }          // movsx esi, al
                            }
                        }
                        // 存储先于标志使用 (比较会破坏 rdx)
                        if res_sz >= 16 {
                            self.emit_insn(&Insn::raw_bytes(&[0x49, 0x89, 0x02])); // mov [r10], rax
                            self.emit_insn(&Insn::raw_bytes(&[0x49, 0x89, 0x52, 0x08])); // mov [r10+8], rdx
                        } else if res_sz == 8 {
                            self.emit_insn(&Insn::raw_bytes(&[0x49, 0x89, 0x02])); // mov [r10], rax
                        } else if res_sz == 4 {
                            self.emit_insn(&Insn::raw_bytes(&[0x41, 0x89, 0x32])); // mov [r10], esi
                        } else if res_sz == 2 {
                            self.emit_insn(&Insn::raw_bytes(&[0x41, 0x89, 0x32])); // mov [r10], si
                        } else {
                            self.emit_insn(&Insn::raw_bytes(&[0x41, 0x88, 0x32])); // mov [r10], sil
                        }
                        // 溢出判断
                        if res_sz >= 16 {
                            // 128 位积对 signed res 恒 fit (输入≤64 位,
                            // 积≤2^126); 对 unsigned res 不 fit ⟺ 积<0
                            if res_uns {
                                self.emit_insn(&Insn::test_rm_reg(
                                    crate::codegen::shared::asm_operand::Width::B64,
                                    2, 2,
                                )); // test rdx, rdx
                                self.emit_insn(&Insn::raw_bytes(&[0x0f, 0x98, 0xc0])); // sets al
                            } else {
                                self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::Xor, crate::codegen::shared::asm_operand::Width::B32, 0, 0)); // xor eax, eax
                            }
                        } else if res_sz == 8 {
                            // fit ⟺ rdx == 期望高位 (uns:0 / signed:符号扩展)
                            if res_uns {
                                self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::Xor, crate::codegen::shared::asm_operand::Width::B32, 6, 6)); // xor esi, esi
                            } else {
                                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovRm { width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(6, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // mov rsi, rax
                                self.emit_insn(&Insn::raw_bytes(&[0x48, 0xc1, 0xfe, 0x3f])); // sar rsi, 63
                            }
                            self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::Cmp, crate::codegen::shared::asm_operand::Width::B64, 2, 6)); // cmp rdx, rsi
                            self.emit_insn(&Insn::raw_bytes(&[0x0f, 0x95, 0xc0])); // setne al
                            self.emit_insn(&Insn::raw_bytes(&[0x0f, 0xb6, 0xc0])); // movzx eax, al
                        } else {
                            // 1/2/4 字节: 溢出 = (rdx != 期望高位 rdi) || (r11 != rsi)
                            if res_uns {
                                self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::Xor, crate::codegen::shared::asm_operand::Width::B32, 7, 7)); // xor edi, edi
                            } else {
                                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovRm { width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(7, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Reg(6, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // mov rdi, rsi
                                self.emit_insn(&Insn::raw_bytes(&[0x48, 0xc1, 0xff, 0x3f])); // sar rdi, 63
                            }
                            self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::Cmp, crate::codegen::shared::asm_operand::Width::B64, 2, 7)); // cmp rdx, rdi
                            self.emit_insn(&Insn::raw_bytes(&[0x0f, 0x95, 0xc0])); // setne al
                            self.emit_insn(&Insn::raw_bytes(&[0x4c, 0x39, 0xde])); // cmp rsi, r11
                            self.emit_insn(&Insn::raw_bytes(&[0x0f, 0x95, 0xc2])); // setne dl
                            self.emit_insn(&Insn::raw_bytes(&[0x08, 0xd0])); // or al, dl
                            self.emit_insn(&Insn::raw_bytes(&[0x0f, 0xb6, 0xc0])); // movzx eax, al
                        }
                        if dst != 0 {
                            self.mov(dst, 0);
                        }
                        return true;
                    },
                    _ => unreachable!(),
                }
                // rax = 64 位结果
                // 6. 截断到结果类型, 检测溢出
                // 保存完整结果到 rcx
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovRm { width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(1, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // mov rcx, rax
                match result_size {
                    4 => {
                        // 32 位: 截断到 eax
                        self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                },
                &[],
            )); // mov eax, eax (截断到 32 位，同时零扩展到 64 位)
                                                  // mov eax, eax 已经零扩展到 rax，直接复制到 rdx
                        self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovRm { width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(2, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // mov rdx, rax
                        if !is_unsigned {
                            // 有符号: 需要符号扩展而不是零扩展
                            // 重新截断并符号扩展
                            self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                                sign: true,
                                from: crate::codegen::shared::asm_operand::Width::B32,
                                to: crate::codegen::shared::asm_operand::Width::B64,
                                dst: crate::codegen::x86_64::opcode::Operand::Reg(2, crate::codegen::shared::asm_operand::Width::B64),
                                src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B32),
                            }, &[])); // movsxd rdx, eax
                        }
                    },
                    2 => {
                        if is_unsigned {
                            // 16 位无符号: movzx rdx, ax
                            self.emit_insn(&Insn::raw_bytes(&[0x48, 0x0f, 0xb7, 0xd0])); // movzx rdx, ax
                        } else {
                            // 16 位有符号: movsx rdx, ax
                            self.emit_insn(&Insn::raw_bytes(&[0x48, 0x0f, 0xbf, 0xd0])); // movsx rdx, ax
                        }
                    },
                    1 => {
                        if is_unsigned {
                            // 8 位无符号: movzx rdx, al
                            self.emit_insn(&Insn::raw_bytes(&[0x48, 0x0f, 0xb6, 0xd0])); // movzx rdx, al
                        } else {
                            // 8 位有符号: movsx rdx, al
                            self.emit_insn(&Insn::raw_bytes(&[0x48, 0x0f, 0xbe, 0xd0])); // movsx rdx, al
                        }
                    },
                    _ => {
                        // 64 位: 无截断
                        // 无符号用 setc (CF=进位), 有符号用 seto (OF=溢出)
                        self.emit_insn(&Insn::raw_bytes(&[0x49, 0x89, 0x02])); // mov [r10], rax (先存储结果)
                        if is_unsigned {
                            self.emit_insn(&Insn::raw_bytes(&[0x0f, 0x92, 0xc2])); // setc dl (unsigned: carry)
                        } else {
                            self.emit_insn(&Insn::raw_bytes(&[0x0f, 0x90, 0xc2])); // seto dl (signed: overflow)
                        }
                        self.emit_insn(&Insn::raw_bytes(&[0x0f, 0xb6, 0xc2])); // movzx eax, dl
                        if dst != 0 {
                            self.mov(dst, 0);
                        }
                        return true;
                    },
                }
                // rdx = 截断后符号扩展的结果, rcx = 64 位结果
                // 比较 rdx 和 rcx: 如果不等则溢出
                self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::Cmp, crate::codegen::shared::asm_operand::Width::B64, 1, 2)); // cmp rcx, rdx
                self.emit_insn(&Insn::raw_bytes(&[0x0f, 0x94, 0xc2])); // sete dl (相等=无溢出=1)
                self.emit_insn(&Insn::raw_bytes(&[0x80, 0xf2, 0x01])); // xor dl, 1 (反转: 溢出=1)
                                                // 7. 存储截断结果到 *ptr
                match result_size {
                    4 => {
                        self.emit_insn(&Insn::raw_bytes(&[0x41, 0x89, 0x02])); // mov [r10], eax
                    },
                    2 => {
                        self.emit_insn(&Insn::raw_bytes(&[0x66, 0x41, 0x89, 0x02])); // mov [r10], ax
                    },
                    1 => {
                        self.emit_insn(&Insn::raw_bytes(&[0x41, 0x88, 0x02])); // mov [r10], al
                    },
                    _ => {
                        self.emit_insn(&Insn::raw_bytes(&[0x49, 0x89, 0x02])); // mov [r10], rax
                    },
                }
                // 8. dst = 溢出标志
                self.emit_insn(&Insn::raw_bytes(&[0x0f, 0xb6, 0xc2])); // movzx eax, dl
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_shl_overflow(a, b, &result): 左移溢出检测
            // 溢出条件: b >= 类型宽度 或 b < 0
            // 策略: 先检查移位量, 再执行移位, 比较检测溢出
            "__builtin_shl_overflow" | "__builtin_shr_overflow" => {
                if args.len() < 3 {
                    self.mov_imm(dst, 0);
                    return true;
                }
                let op_size = args[0].ty().size().max(4); // 至少 32 位
                let type_bits = op_size * 8;
                // 1. 求值 ptr → r10
                self.gen_expr(&args[2], Self::EVAL_BASE);
                self.emit_insn(&Insn::raw_bytes(&[0x49, 0x89, 0xc2])); // mov r10, rax
                                                // 2. 求值 b (移位量) → push
                self.gen_expr(&args[1], Self::EVAL_BASE);
                self.push(0);
                // 3. 求值 a (被移值) → rax
                self.gen_expr(&args[0], Self::EVAL_BASE);
                // 4. pop b → rcx (移位量)
                self.pop(1);
                // rax = a (被移值), rcx = b (移位量), r10 = ptr
                // 5. 检查移位量是否溢出 (b >= type_bits 或 b < 0)
                let overflow_label = self.fresh_label();
                let end_label = self.fresh_label();
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::AluRm { op: crate::codegen::x86_64::opcode::AluOp::Cmp, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(1, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Imm(0)}, &[])); // cmp rcx, 0
                self.jmp_rel32(&overflow_label, JumpKind::Jl); // jl overflow (负数)
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x83, 0xf9])); // cmp rcx, type_bits
                self.emit_insn(&Insn::raw_bytes(&[type_bits as u8]));
                self.jmp_rel32(&overflow_label, JumpKind::Jge); // jge overflow (>= type_bits)
                                                                // 6. 正常移位
                                                                // rax = a (被移值), rcx = b (移位量)
                                                                // x86 移位指令使用 cl 作为移位计数
                if name == "__builtin_shl_overflow" {
                    self.emit_insn(&Insn::raw_bytes(&[0x48, 0xd3, 0xe0])); // shl rax, cl
                } else {
                    // 对于有符号类型使用 sar, 无符号使用 shr
                    let unsigned = matches!(
                        args[0].ty(),
                        Type::Bool
                            | Type::UChar
                            | Type::UShort
                            | Type::UInt
                            | Type::ULong
                            | Type::ULongLong
                            | Type::UInt128
                    );
                    if unsigned {
                        self.emit_insn(&Insn::raw_bytes(&[0x48, 0xd3, 0xe8])); // shr rax, cl
                    } else {
                        self.emit_insn(&Insn::raw_bytes(&[0x48, 0xd3, 0xf8])); // sar rax, cl
                    }
                }
                // 7. 存储结果到 *ptr
                self.emit_insn(&Insn::raw_bytes(&[0x49, 0x89, 0x02])); // mov [r10], rax
                                                // 8. 返回 0 (无溢出)
                self.mov_imm(dst, 0);
                self.jmp_rel32(&end_label, JumpKind::Jmp);
                // overflow 处理:
                self.label(&overflow_label);
                // 存储 0 到 *ptr (溢出时结果为 0)
                self.emit_insn(&Insn::raw_bytes(&[0x49, 0xc7, 0x02, 0x00, 0x00, 0x00, 0x00])); // mov qword [r10], 0
                                                                        // 返回 1 (有溢出)
                self.mov_imm(dst, 1);
                self.label(&end_label);
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // ===== x86 CPU 内置函数：真实现（CPUID + `.data` 里的缓存）=====
            // 发射器、特性位表、缓存对象全部在 `codegen::x86_64::ifunc`，与
            // `target_clones` 的 resolver **共用同一条 `emit_feature_check`**：
            // 「运行时 builtin 的答案」与「resolver 在装载期的选型」同真同假。
            // 缓存必须坐 `.data` 而不是 `.bss`：内置链接器的静态路径不映射 NOBITS
            // 输出节（`src/link/layout.rs` 的 `fill_section_data`），坐 `.bss` 会让
            // `__dcc_cpu_indicator_init` 的第一个写 SIGSEGV（判据
            // `attr_target_clones_ifunc_symbols` 断 `__dcc_cpu_model` 的节是 PROGBITS）。
            // 之前的 `mov_imm(dst, 0)` 恒假桩已删除：resolver 体要复用同一条
            // 真实检测（本轮改造前，CPUID 机器码只能由内置链接器现场拼字节流补，
            // `.o` 里根本没有 resolver，链接因此静默从动态降级为静态）。
            "__builtin_cpu_init" => {
                self.emit_cpu_init_builtin();
                true
            },
            "__builtin_cpu_supports" => {
                let span = Span::point(&self.source_name, 0, 1);
                match args.first() {
                    Some(a) => self.emit_cpu_supports(a, span, dst),
                    None => {
                        self.emit_cpu_supports_missing_arg(span, dst);
                    },
                }
                true
            },
            "__builtin_cpu_is" => {
                let span = Span::point(&self.source_name, 0, 1);
                match args.first() {
                    Some(a) => self.emit_cpu_is(a, span, dst),
                    None => {
                        self.emit_cpu_is_missing_arg(span, dst);
                    },
                }
                true
            },
            // ===== M12+ 补洞 builtins =====
            // __builtin_strcasecmp(s1, s2): 不区分大小写比较, 走 libc.
            // 链接时由 glibc strcasecmp 提供; freestanding 测试可在 fixture 中
            // 自行实现同名函数并通过 weak 引用覆盖 (见 link/resolve.rs).
            "__builtin_strcasecmp" | "__builtin_strncasecmp" => {
                let libc_name = if name == "__builtin_strncasecmp" {
                    "strncasecmp"
                } else {
                    "strcasecmp"
                };
                // 不能在本函数里裸摆参数再 call_label：那样直接落在 `call_label`
                // primitive 上，完全绕过 `emit_sysv_call` 的延伸池家存/取窗口
                // （E1136 守卫只能事后报错，不能救场）。走统一入口让窗口盖住
                // 「参数求值 + call + 栈参回收」全程。
                self.emit_sysv_call(Some(libc_name), None, args, None);
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_strncat(dst, src, n): 拼接字符串, 走 libc strncat.
            // __builtin_memchr(s, c, n): 内存查找字符, 走 libc memchr.
            // 两者签名均与 libc 完全对应, 直接转发.
            "__builtin_strncat" | "__builtin_memchr" => {
                let libc_name = if name == "__builtin_strncat" {
                    "strncat"
                } else {
                    "memchr"
                };
                // 同上：必须走 `emit_sysv_call` 统一窗口入口，不得裸摆参数后 call_label。
                self.emit_sysv_call(Some(libc_name), None, args, None);
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_fma / fmaf / fmal: 融合乘加 (优先 vfmadd213pd/fma libm 走 fma).
            // 走 libm: 链接器解析 fma, 兼顾 freestanding (编译时不开 vfmadd).
            "__builtin_fma" | "__builtin_fmaf" | "__builtin_fmal" => {
                if args.len() < 3 {
                    self.mov_imm(dst, 0);
                    return true;
                }
                // SysV ABI: xmm0, xmm1, xmm2.
                // 策略: 每个参数都 gen_expr 到 xmm0, 然后移至目标 xmm.
                // 但 i=1/2 加载到 xmm0 会覆盖 i=0 的 arg0. 修复: 用 xmm3 备份 arg0.
                // 关键: fmaf 用 movss (F3 0F 10/11, 32-bit), fma/fmal 用 movsd (F2 0F 10/11, 64-bit).
                // movss 只设低 32 位, 高 96 位不变; movsd 只设低 64 位, 高 64 位不变.
                // 备份/恢复必须用与原始加载同宽的指令.
                let (prefix, opcode) = if name == "__builtin_fmaf" {
                    (0xF3u8, 0x10u8)
                } else {
                    (0xF2u8, 0x10u8)
                };
                // ModR/M (F2/F3 0F 11 指令: MOVSD/MOVSS xmm/m, xmm; reg=src, rm=dst):
                //   xmm3 <- xmm0: reg=0, rm=3 ⇒ 0xC3
                //   xmm1 <- xmm0: reg=0, rm=1 ⇒ 0xC1
                //   xmm2 <- xmm0: reg=0, rm=2 ⇒ 0xC2
                // ModR/M (F2/F3 0F 10 指令: MOVSD/MOVSS xmm, xmm/m; reg=dst, rm=src):
                //   xmm0 <- xmm3: reg=0, rm=3 ⇒ 0xC3
                self.gen_expr(&args[0], Self::EVAL_BASE); // xmm0 = arg0
                self.emit_insn(&Insn::raw_bytes(&[prefix, 0x0F, 0x11, 0xC3])); // mov[sd|ss] xmm3, xmm0 (备份 arg0)
                self.gen_expr(&args[1], Self::EVAL_BASE); // xmm0 = arg1
                self.emit_insn(&Insn::raw_bytes(&[prefix, 0x0F, 0x11, 0xC1])); // mov[sd|ss] xmm1, xmm0
                self.gen_expr(&args[2], Self::EVAL_BASE); // xmm0 = arg2
                self.emit_insn(&Insn::raw_bytes(&[prefix, 0x0F, 0x11, 0xC2])); // mov[sd|ss] xmm2, xmm0
                self.emit_insn(&Insn::raw_bytes(&[prefix, 0x0F, opcode, 0xC3])); // mov[sd|ss] xmm0, xmm3 (恢复 arg0)
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Push, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(4, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // push rsp
                                    // 调用 libm 中对应函数: fma / fmaf / fmal
                let fn_name = match name {
                    "__builtin_fmaf" => "fmaf",
                    "__builtin_fmal" => "fmal",
                    _ => "fma",
                };
                self.call_label(fn_name);
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Pop, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(4, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // pop rsp
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_rotateleft8/16/32/64 / __builtin_rotateright8/16/32/64
            // 策略: 按位宽选 ROL/ROR modrm 编码.
            n if n.starts_with("__builtin_rotateleft")
                || n.starts_with("__builtin_rotateright") =>
            {
                if args.len() < 2 {
                    self.mov_imm(dst, 0);
                    return true;
                }
                let width: u32 = match n {
                    "__builtin_rotateleft8" | "__builtin_rotateright8" => 8,
                    "__builtin_rotateleft16" | "__builtin_rotateright16" => 16,
                    "__builtin_rotateleft32" | "__builtin_rotateright32" => 32,
                    _ => 64,
                };
                // shift amount = args[1] & (width - 1)
                // result = rotate(args[0], shift)
                self.gen_expr(&args[0], dst);
                self.gen_expr(&args[1], 1); // rcx = shift
                if width == 64 {
                    let rex = 0x48 | if dst >= 8 { 0x01 } else { 0 };   // 位序: REX.B: 该寄存器只在 ModRM.r/m 或 opcode 低 3 位, 判据见 docs/ssa-pass-upgrade-plan.md §7.21
                    if n.starts_with("__builtin_rotateleft") {
                        self.emit_insn(&Insn::raw_bytes(&[rex, 0xD3, 0xC0 | (dst & 7)])); // rol dst, cl
                    } else {
                        self.emit_insn(&Insn::raw_bytes(&[rex, 0xD3, 0xC8 | (dst & 7)])); // ror dst, cl
                    }
                } else if width == 32 {
                    // 32-bit 操作: mov 到 eax, rol/ror eax, cl, mov 回 dst
                    self.emit_insn(&Insn::raw_bytes(&[0x89, 0xC0 | (dst & 7)])); // mov eax, dst (截断到 32 位)
                    if n.starts_with("__builtin_rotateleft") {
                        self.emit_insn(&Insn::raw_bytes(&[0xD3, 0xC0])); // rol eax, cl
                    } else {
                        self.emit_insn(&Insn::raw_bytes(&[0xD3, 0xC8])); // ror eax, cl
                    }
                    if dst >= 8 {
                        // mov dst, eax  (REX.B 89 C0+reg)
                        self.emit_insn(&Insn::raw_bytes(&[0x41, 0x89, 0xC0 | (dst & 7)]));
                    } else {
                        self.emit_insn(&Insn::raw_bytes(&[0x89, 0xC0 | (dst & 7)]));
                    }
                } else if width == 16 {
                    // 16-bit 操作
                    self.emit_insn(&Insn::raw_bytes(&[0x66]));
                    if dst >= 8 {
                        self.emit_insn(&Insn::raw_bytes(&[0x41]));
                    }
                    if n.starts_with("__builtin_rotateleft") {
                        self.emit_insn(&Insn::raw_bytes(&[0xD3, 0xC0 | (dst & 7)])); // rol r16, cl
                    } else {
                        self.emit_insn(&Insn::raw_bytes(&[0xD3, 0xC8 | (dst & 7)])); // ror r16, cl
                    }
                } else {
                    // 8-bit 操作
                    if n.starts_with("__builtin_rotateleft") {
                        self.emit_insn(&Insn::raw_bytes(&[0xD2, 0xC0 | (dst & 7)])); // rol r8, cl
                    } else {
                        self.emit_insn(&Insn::raw_bytes(&[0xD2, 0xC8 | (dst & 7)])); // ror r8, cl
                    }
                }
                true
            },
            // __builtin_clrsb / clrsbl / clrsbll: 计算符号位冗余的位数.
            // 等价于: clz(x ^ s_bits) - 1, 其中 s_bits = 把符号位扩展到整个类型宽度.
            // 对 32-bit:
            //   x = 0x00000001 → s=0, x^s = 1,         clz=31, clrsb=30
            //   x = 0x80000000 → s=-1, x^s = 0x7FFFFFFF, clz=1,  clrsb=0
            //   x = 0xFFFFFFFF → s=-1, x^s = 0,         clz=32, clrsb=31
            //   x = 0          → s=0,  x^s = 0,         clz=32, clrsb=31
            // 实现技巧 (关键: 必须用与类型宽度一致的位运算):
            //   1. mask 到类型宽度 (32-bit 用 cdq/32-bit bsr; 64-bit 用 cqo/64-bit bsr).
            //   2. 用 cdq (32-bit) 或 cqo (64-bit) 把符号位扩展到寄存器宽度.
            //   3. xor 后, 用 test+cmove 解决 bsr 0 未定义:
            //      a. 把 x^s 复制到第二寄存器 (ecx/rcx), 用于 test 设置 ZF
            //         (不能依赖 bsr 的 ZF, 因为 bsr(0) 在 x86 CPU 上 ZF=0, 不可靠).
            //      b. bsr eax, eax (无副作用依赖)
            //      c. mov edx, -1 (sentinel); test ecx, ecx; cmove eax, edx
            //         → ecx==0 时 eax = -1; 否则 eax = bsr.
            //      d. mov ecx, bits-2; sub ecx, eax
            //         正常: bits-2 - bsr = clrsb ✓
            //         零:   bits-2 - (-1) = bits-1 = clrsb ✓
            // 注: 不能用 sar reg, (bits-1) 在 64-bit reg 上做符号扩展, 因为
            //     dcc 把 0x80000000 当作正数 2^31 零扩展到 64-bit (0x0000000080000000),
            //     SAR 31 不会把 1 符号位填到 64 位, 而是填 0. 必须用 cdq/cqo.
            "__builtin_clrsb" | "__builtin_clrsbl" | "__builtin_clrsbll" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                // GCC 语义: 无后缀 → int 宽度, l → long 宽度, ll → long long 宽度。
                // 宽度必须取**形参类型**而不是实参表达式的类型：`__builtin_clrsbl(0x80000000)`
                // 的字面量在 C 里是 `unsigned int`（32 位），拿它的宽度算就会得到 0，
                // 而 gcc 给 31（long 是 64 位，符号位在 bit 63）。实参进入形参时已完成
                // 宽度转换，所以形参类型就是本次 clrsb 的真实位宽。
                let bits = match name {
                    "__builtin_clrsb" => Type::Int.size() as i32 * 8,
                    "__builtin_clrsbl" => Type::Long.size() as i32 * 8,
                    _ => Type::LongLong.size() as i32 * 8,
                };
                self.gen_expr(&args[0], Self::EVAL_BASE); // rax = x
                if bits == 64 {
                    // 64-bit clrsb: 用 cqo 复制符号到 rdx, 然后 xor rax, rdx.
                    self.emit_insn(&Insn::sign_extend_rax_to_rdx(crate::codegen::shared::asm_operand::Width::B64)); // cqo (REX.W)
                    self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::AluRm { op: crate::codegen::x86_64::opcode::AluOp::Xor, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Reg(2, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // xor rax, rdx
                    self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovRm { width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(1, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // mov rcx, rax
                    self.emit_insn(&Insn::raw_bytes(&[0x48, 0x0F, 0xBD, 0xC0])); // bsr rax, rax
                    self.emit_insn(&Insn::raw_bytes(&[0x48, 0xC7, 0xC2, 0xFF, 0xFF, 0xFF, 0xFF])); // mov rdx, -1
                    self.emit_insn(&Insn::test_rm_reg(crate::codegen::shared::asm_operand::Width::B64, 1, 1)); // test rcx, rcx
                    self.emit_insn(&Insn::raw_bytes(&[0x48, 0x0F, 0x44, 0xC2])); // cmove rax, rdx
                    self.emit_insn(&Insn::raw_bytes(&[0x48, 0xC7, 0xC2, 0x3E, 0x00, 0x00, 0x00])); // mov rdx, 62
                    // P11: 收敛到 AluRm{Sub, B64, rdx, rax}.
                    {
                        let insn = Insn::new(
                            crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                                op: crate::codegen::x86_64::opcode::AluOp::Sub,
                                width: crate::codegen::shared::asm_operand::Width::B64,
                                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                    2,
                                    crate::codegen::shared::asm_operand::Width::B64,
                                ),
                                src: crate::codegen::x86_64::opcode::Operand::Reg(
                                    0,
                                    crate::codegen::shared::asm_operand::Width::B64,
                                ),
                            },
                            &[],
                        );
                        if !self.emit_insn(&insn) {
                            self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::Sub, crate::codegen::shared::asm_operand::Width::B64, 2, 0)); // sub rdx, rax
                        }
                    }
                    if dst != 2 {
                        // 结果在 rdx（sub rdx, rax 之后），不是 rax；此前误从 rax 取 bsr
                        // 使 clrsbll(0x100000000) 得 32 而非 30（clrsbl 仅因 bsr==31 侥幸相等）。
                        self.emit_insn(&Insn::mov_rm_reg(crate::codegen::shared::asm_operand::Width::B64, dst, 2));
                    }
                } else {
                    // 32-bit (及 16/8-bit) clrsb: 用 32-bit 操作.
                    // 32-bit AND EAX, imm32 零扩展到 RAX (无 REX.W).
                    let mask32: u32 = if bits == 32 {
                        0xFFFFFFFF
                    } else if bits == 16 {
                        0x0000FFFF
                    } else {
                        0x000000FF
                    };
                    let mb = mask32.to_le_bytes();
                    self.emit_insn(&Insn::raw_bytes(&[0x25, mb[0], mb[1], mb[2], mb[3]]));
                    // 计算 s_bits 到 edx: 0 或 mask32 (32-bit).
                    if bits == 32 {
                        self.emit_insn(&Insn::sign_extend_rax_to_rdx(crate::codegen::shared::asm_operand::Width::B32)); // cdq: edx = sign(eax) = 0 或 0xFFFFFFFF
                    } else if bits == 16 {
                        self.emit_insn(&Insn::raw_bytes(&[0xC1, 0xE0, 16])); // shl eax, 16
                        self.emit_insn(&Insn::raw_bytes(&[0xC1, 0xF8, 31])); // sar eax, 31 → 0 或 -1
                        self.emit_insn(&Insn::raw_bytes(&[0x25, 0xFF, 0xFF, 0x00, 0x00])); // and eax, 0xFFFF → 保留符号扩展
                        self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        2, crate::codegen::shared::asm_operand::Width::B32
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                },
                &[],
            )); // mov edx, eax
                        // 恢复原始16位值到eax的低16位，高位清零
                        self.emit_insn(&Insn::raw_bytes(&[0x25, 0xFF, 0xFF, 0x00, 0x00])); // and eax, 0xFFFF
                    } else {
                        // bits == 8
                        self.emit_insn(&Insn::raw_bytes(&[0xC1, 0xE0, 24])); // shl eax, 24
                        self.emit_insn(&Insn::raw_bytes(&[0xC1, 0xF8, 31])); // sar eax, 31 → 0 或 -1
                        self.emit_insn(&Insn::raw_bytes(&[0x25, 0xFF, 0x00, 0x00, 0x00])); // and eax, 0xFF → 保留符号扩展
                        self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        2, crate::codegen::shared::asm_operand::Width::B32
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                },
                &[],
            )); // mov edx, eax
                        // 恢复原始8位值到eax的低8位，高位清零
                        self.emit_insn(&Insn::raw_bytes(&[0x25, 0xFF, 0x00, 0x00, 0x00])); // and eax, 0xFF
                    }
                    // xor eax, edx: eax = x ^ s_bits (按类型宽度处理)
                    self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::Xor, crate::codegen::shared::asm_operand::Width::B32, 0, 2));
                    // mov ecx, eax: 保存 x^s_bits 到 ecx, 用于 test
                    self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        1, crate::codegen::shared::asm_operand::Width::B32
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                },
                &[],
            ));
                    // bsr eax, eax (32-bit) - 可能为 UB for 0, 但 cmove 兜底
                    self.emit_insn(&Insn::raw_bytes(&[0x0F, 0xBD, 0xC0]));
                    // mov edx, 0xFFFFFFFF (sentinel)
                    self.emit_insn(&Insn::raw_bytes(&[0xBA, 0xFF, 0xFF, 0xFF, 0xFF]));
                    // test ecx, ecx: 设置 ZF 基于 ecx (0 ⇒ ZF=1), 覆盖 bsr 的 ZF
                    self.emit_insn(&Insn::test_rm_reg(crate::codegen::shared::asm_operand::Width::B32, 1, 1));
                    // cmove eax, edx: 如果 x^s_bits==0, eax = edx = -1
                    // ModR/M = 0xC2 = 11_000_010: reg=eax(0), rm=edx(2) ⇒ Intel cmove eax, edx
                    self.emit_insn(&Insn::raw_bytes(&[0x0F, 0x44, 0xC2]));
                    // mov ecx, (bits-2)
                    let bm2: u32 = (bits - 2) as u32;
                    let b = bm2.to_le_bytes();
                    self.emit_insn(&Insn::raw_bytes(&[0xB9, b[0], b[1], b[2], b[3]]));
                    // sub ecx, eax: ecx = clrsb
                    self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::Sub, crate::codegen::shared::asm_operand::Width::B32, 1, 0));
                    // mov dst, ecx (REX.W; ecx 零扩展到 dst)
                    if dst != 1 {
                        self.emit_insn(&Insn::mov_rm_reg(crate::codegen::shared::asm_operand::Width::B64, dst, 1));
                    }
                }
                true
            },
            // __builtin_apply_args(): 保存寄存器实参到栈上缓冲区并返回指针.
            // 缓冲区布局 (与 __builtin_apply 自洽约定):
            //   [0..48)    rdi, rsi, rdx, rcx, r8, r9
            //   [48..176)  xmm0..xmm7
            //   [176..432) 传入栈参数 (从 r13+16 拷贝 256 字节)
            // 缓冲区占用 rsp 下方 448 字节 (16 对齐); 由随后 __builtin_apply
            // 尾部统一回收 (含自身栈参区与结果区), 见 apply 代码注释.
            "__builtin_apply_args" => {
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x81, 0xEC]));
                self.emit_insn(&Insn::raw_bytes(&448u32.to_le_bytes())); // sub rsp, 448
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(4),
                            index: None,
                            seg: None,
                        }
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        7, crate::codegen::shared::asm_operand::Width::B64
                    ),
                },
                &[],
            )); // mov [rsp], rdi
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x89, 0x74, 0x24, 0x08])); // mov [rsp+8], rsi
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x89, 0x54, 0x24, 0x10])); // mov [rsp+16], rdx
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x89, 0x4C, 0x24, 0x18])); // mov [rsp+24], rcx
                self.emit_insn(&Insn::raw_bytes(&[0x4C, 0x89, 0x44, 0x24, 0x20])); // mov [rsp+32], r8
                self.emit_insn(&Insn::raw_bytes(&[0x4C, 0x89, 0x4C, 0x24, 0x28])); // mov [rsp+40], r9
                for k in 0..8u32 {
                    let off = 48 + k * 16;
                    // movdqu [rsp+off], xmmK: F3 0F 7F /r (modrm.reg = K)
                    if off < 128 {
                        self.emit_insn(&Insn::simd_mov_mem(true, crate::codegen::x86_64::opcode::SimdWidth::DQ, k as u8, off as i32));
                    } else {
                        self.emit_insn(&Insn::raw_bytes(&[0xF3, 0x0F, 0x7F, 0x84, 0x24]));
                        self.emit_insn(&Insn::raw_bytes(&off.to_le_bytes()));
                        let n = self.code.len();
                        // 9 字节: F3 0F 7F 84 24 disp32, modrm 在倒数第 6 字节
                        self.code[n - 6] |= (k as u8) << 3; // modrm
                    }
                }
                // 拷贝传入栈参数: rsi = r13+16 (首栈参), rdi = rsp+176
                self.emit_insn(&Insn::raw_bytes(&[0x49, 0x8D, 0x75, 0x10])); // lea rsi, [r13+16]
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x8D, 0xBC, 0x24]));
                self.emit_insn(&Insn::raw_bytes(&176u32.to_le_bytes())); // lea rdi, [rsp+176]
                self.emit_insn(&Insn::raw_bytes(&[0xB9, 32, 0, 0, 0])); // mov ecx, 32 (qwords)
                self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Cld)); self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::RepMovsq)); // cld; rep movsq
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x8D, 0x04, 0x24])); // lea rax, [rsp]
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_choose_expr fallback (parser 应当已折叠)
            "__builtin_choose_expr" => {
                if args.len() >= 3 {
                    let cond = const_eval::eval(&args[0]).unwrap_or(0);
                    let chosen = if cond != 0 { &args[1] } else { &args[2] };
                    self.gen_expr(chosen, dst);
                } else {
                    self.mov_imm(dst, 0);
                }
                true
            },
            // __builtin_types_compatible_p fallback
            "__builtin_types_compatible_p" => {
                // parser 应当已折叠; 此处按 0 处理
                self.mov_imm(dst, 0);
                true
            },
            // __builtin_va_arg(ap, type) — parser 已生成 Expr::VaArg, 此兜底不会到达
            "__builtin_va_arg" => true,
            // __builtin_FILE / __builtin_LINE / __builtin_FUNCTION / __builtin_FUNCSIG
            // parser 已折叠; codegen 兜底返回空串 (与 parser 一致).
            "__builtin_FILE"
            | "__builtin_FUNCTION"
            | "__builtin_FUNCSIG"
            | "__builtin_PRETTY_FUNCTION" => {
                // 复用 string 表达式生成路径, 避免重复维护 rodata ref 编码
                let empty: Vec<u8> = vec![0];
                let data = empty;
                if !self.string_pool.contains_key(&data) {
                    let o = self.rodata.len() as u64;
                    self.rodata.extend_from_slice(&data);
                    self.string_pool.insert(data.clone(), o);
                }
                let at = self.pos();
                let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                self.emit_insn(&Insn::raw_bytes(&[rex, 0x8D, 0x05 | ((dst & 7) << 3), 0, 0, 0, 0]));
                self.string_refs.push((at, data));
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            "__builtin_LINE" => {
                self.mov_imm(dst, 0);
                true
            },
            // __builtin_classify_type fallback
            "__builtin_classify_type" => {
                self.mov_imm(dst, 0);
                true
            },
            // __builtin_ffs/ffsl/ffsll: find first set bit.
            // Semantics: ffs(x) = (x == 0) ? 0 : ctz(x) + 1.
            // Implementation: evaluate into rax, save to rcx,
            //   xor dst,dst; bsf dst,rcx; test rcx,rcx;
            //   setnz cl; movzx ecx,cl; add dst,rcx.
            "__builtin_ffs" | "__builtin_ffsl" | "__builtin_ffsll" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                // gen_expr always returns result in rax (reg 0).
                self.gen_expr(&args[0], Self::EVAL_BASE);
                // xor dst,dst: dst in both reg and r/m → REX.R+REX.B
                let rex_xor = 0x48 | if dst >= 8 { 0x05 } else { 0 };   // 位序: REX.R+B: 自反 ModRM, 同一个寄存器既在 reg 又在 r/m, 判据见 docs/ssa-pass-upgrade-plan.md §7.21
                // add dst,rcx: dst in r/m only → REX.B
                let rex_add = 0x48 | if dst >= 8 { 0x01 } else { 0 };   // 位序: REX.B: 该寄存器只在 ModRM.r/m 或 opcode 低 3 位, 判据见 docs/ssa-pass-upgrade-plan.md §7.21
                // push rcx
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Push, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(1, crate::codegen::shared::asm_operand::Width::B64)}, &[]));
                // mov rcx, rax  (save original input; rcx = rax)
                // REX.W 89 C1
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovRm { width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(1, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64)}, &[]));
                // xor dst, dst  (zero dst for input==0 case)
                self.emit_insn(&Insn::raw_bytes(&[rex_xor, 0x31, 0xC0 | ((dst & 7) << 3) | (dst & 7)]));
                // bsf dst, rcx  (dst in reg → REX.R)
                let rex_bsf = 0x48 | if dst >= 8 { 0x04 } else { 0 };
                self.emit_insn(&Insn::raw_bytes(&[rex_bsf, 0x0F, 0xBC, 0xC0 | ((dst & 7) << 3) | 1]));
                // test rcx, rcx  (48 85 C9)
                self.emit_insn(&Insn::test_rm_reg(crate::codegen::shared::asm_operand::Width::B64, 1, 1));
                // setnz cl  (0F 95 C1)
                self.emit_insn(&Insn::setcc_reg(crate::codegen::x86_64::opcode::CondCode::Ne, 1));
                // movzx ecx, cl  (0F B6 C9)
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                    sign: false,
                    from: crate::codegen::shared::asm_operand::Width::B8,
                    to: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(1, crate::codegen::shared::asm_operand::Width::B32),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(1, crate::codegen::shared::asm_operand::Width::B8),
                }, &[]));
                // add dst, rcx  (reg=rcx=001, r/m=dst)
                self.emit_insn(&Insn::raw_bytes(&[rex_add, 0x01, 0xC8 | (dst & 7)]));
                // pop rcx
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Pop, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(1, crate::codegen::shared::asm_operand::Width::B64)}, &[]));
                true
            },
            // __builtin_dynamic_object_size: dynamic variant of object_size.
            // At compile time, falls back to the same compile-time analysis.
            "__builtin_dynamic_object_size" => {
                let type_arg = args
                    .get(1)
                    .and_then(|e| const_eval::eval(e).ok())
                    .unwrap_or(0);
                let size = args
                    .first()
                    .and_then(|p| const_eval::object_size(p, type_arg))
                    .unwrap_or(-1);
                self.mov_imm(dst, size);
                true
            },
            // __builtin_extract_return_addr: mask implementation-defined
            // bits from a return address. On x86_64, the address is
            // already canonical, so this is a no-op (pass-through).
            "__builtin_extract_return_addr" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], dst);
                true
            },
            // __builtin_preserve_access_index: BPF CO-RE helper.
            // In non-BPF context, just evaluate the pointer expression.
            "__builtin_preserve_access_index" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], dst);
                true
            },
            // __builtin_btf_type_id: BPF CO-RE type ID query.
            // Not meaningful outside BPF; return 0.
            "__builtin_btf_type_id" => {
                self.mov_imm(dst, 0);
                true
            },
            // __builtin_has_attribute: check if object/type has
            // a given attribute. Conservative: return 0 (unknown).
            "__builtin_has_attribute" => {
                self.mov_imm(dst, 0);
                true
            },
            // __builtin_isdigit: check if value is a digit (0-9).
            // Equivalent to ((unsigned)(x - '0')) < 10.
            "__builtin_isdigit" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], dst);
                // sub dst, '0' (0x30)
                let rex = 0x48 | if dst >= 8 { 0x01 } else { 0 };   // 位序: 该寄存器只在 r/m 或 opcode 低 3 位, 判据见 docs/ssa-pass-upgrade-plan.md §7.21
                self.emit_insn(&Insn::raw_bytes(&[rex, 0x81, 0xE8 | (dst & 7)]));
                self.emit_insn(&Insn::raw_bytes(&0x30u32.to_le_bytes()));
                // cmp dst, 9
                self.emit_insn(&Insn::raw_bytes(&[rex, 0x83, 0xF8 | (dst & 7), 9]));
                // setbe al (unsigned <= 9)
                self.emit_insn(&Insn::setcc_reg(crate::codegen::x86_64::opcode::CondCode::Be, 0));
                // movzx eax, al
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                    sign: false,
                    from: crate::codegen::shared::asm_operand::Width::B8,
                    to: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B32),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B8),
                }, &[]));
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_warning: compiler diagnostic hint, no codegen.
            "__builtin_warning" => {
                self.mov_imm(dst, 0);
                true
            },
            // __builtin_counted_by_ref: C23 counted_by attribute helper.
            // Returns a pointer to the count field. In non-annotated
            // context, evaluate the first argument (pass-through).
            "__builtin_counted_by_ref" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], dst);
                true
            },
            // __builtin_alloca_uninitialized: like alloca but skip
            // zero-initialization. Same codegen as __builtin_alloca
            // since dcc alloca does not initialize either.
            "__builtin_alloca_uninitialized" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], 1); // rcx = size (dcc 编码: 1 = rcx)
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::AluRm { op: crate::codegen::x86_64::opcode::AluOp::Add, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(1, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Imm(15)}, &[])); // add rcx, 15
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::AluRm { op: crate::codegen::x86_64::opcode::AluOp::And, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(1, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Imm(-16)}, &[])); // and rcx, ~15
                self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::Sub, crate::codegen::shared::asm_operand::Width::B64, 4, 1)); // sub rsp, rcx
                if dst != 4 && dst != 12 {
                    let rex = if dst >= 8 { 0x49 } else { 0x48 };
                    self.emit_insn(&Insn::raw_bytes(&[rex, 0x89, 0xe0 | (dst & 7)])); // mov dst, rsp
                }
                true
            },
            // __builtin_strcat: forward to libc strcat.
            "__builtin_strcat" => {
                let ar = [7u8, 6, 2, 1, 8, 9];
                for (i, a) in args.iter().enumerate() {
                    if i >= 6 {
                        break;
                    }
                    self.gen_expr(a, ar[i]);
                }
                self.call_label("strcat");
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // fenv.h 运行时调用 (C11 7.6): fegetround / fesetround 等
            // 内联为 MXCSR / x87 指令序列, 不依赖 libm (host 链接缺 -lm
            // 会 undefined reference; freestanding 无 libc). 用户在本 TU
            // 自行定义同名函数时 (defined_funcs 命中) 回退到普通调用.
            "fegetround" | "fesetround" | "feclearexcept" | "feraiseexcept" | "fetestexcept"
            | "fegetexceptflag" | "fesetexceptflag" | "fetestexceptflag" | "fegetenv"
            | "fesetenv" | "feholdexcept" | "feupdateenv"
                if !self.defined_funcs.contains(name) =>
            {
                self.gen_fenv_builtin(name, args, dst)
            },
            // ===== 其他缺失的 __builtin_* 函数 =====
            // __builtin_setjmp / __builtin_longjmp: 实现见上方 match 臂
            // (本 match 中先前此处存在重复臂, 已删除 — 位于同一 match 的
            //  早期臂会遮蔽此处, 导致实现从未生效)
            // __builtin_speculation_safe_value: 返回参数值 (no-op)
            "__builtin_speculation_safe_value" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], dst);
                true
            },
            // __builtin_addc: 带进位加法
            "__builtin_addc" => {
                if args.len() < 4 {
                    return false;
                }
                self.gen_expr(&args[0], Self::EVAL_BASE); // rax = a
                self.gen_expr(&args[1], 6); // rsi = b
                self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::Add, crate::codegen::shared::asm_operand::Width::B64, 0, 6)); // add rax, rsi
                                                // 进位标志在 CF, 但 __builtin_addc 返回结果 (不进位)
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_subc: 带借位减法
            "__builtin_subc" => {
                if args.len() < 4 {
                    return false;
                }
                self.gen_expr(&args[0], Self::EVAL_BASE); // rax = a
                self.gen_expr(&args[1], 6); // rsi = b
                self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::Sub, crate::codegen::shared::asm_operand::Width::B64, 0, 6)); // sub rax, rsi
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_is_constant_evaluated: 编译期折叠为 0
            // (dcc 不在 const_eval 上下文中, 保守返回 0)
            "__builtin_is_constant_evaluated" => {
                self.mov_imm(dst, 0);
                true
            },
            // __builtin_memcpy_inline: 内联 rep movsb (同 __builtin_memcpy)
            "__builtin_memcpy_inline" => {
                if args.len() < 3 {
                    return false;
                }
                self.gen_expr(&args[1], 6); // rsi = src
                self.gen_expr(&args[0], 7); // rdi = dst
                self.gen_expr(&args[2], 1); // rcx = n
                self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Cld)); // cld
                self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::RepMovsb)); // rep movsb
                if dst != 0 {
                    self.mov(dst, 7);
                }
                true
            },
            // __builtin_memset_inline: 内联 rep stosb (同 __builtin_memset)
            "__builtin_memset_inline" => {
                if args.len() < 3 {
                    return false;
                }
                self.gen_expr(&args[0], 7); // rdi = dst
                self.gen_expr(&args[1], Self::EVAL_BASE); // rax = val
                self.gen_expr(&args[2], 1); // rcx = n
                self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Cld)); // cld
                self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::RepStosb)); // rep stosb
                if dst != 0 {
                    self.mov(dst, 7);
                }
                true
            },
            // __builtin_unwind_init: no-op
            "__builtin_unwind_init" => true,
            // __builtin_printf / __builtin_fprintf / __builtin_sprintf / __builtin_snprintf:
            // 转发到 libc (同 __builtin_strcpy 等模式)
            "__builtin_printf" | "__builtin_fprintf" | "__builtin_sprintf"
            | "__builtin_snprintf" => {
                let libc_name = name.strip_prefix("__builtin_").unwrap();
                // 通过 emit_sysv_call 转发: 内含参数暂存 (staging) 与 16 字节
                // 栈对齐, 避免后续参数求值破坏已就位的寄存器参数 (如
                // snprintf(buf, 5, "%hhd", x+1) 中 x+1 的 push/pop 临时
                // 会覆盖 rdx 的格式串); SysV 变参 al 约定也由它统一设置.
                self.emit_sysv_call(Some(libc_name), None, args, None);
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_bcmp: 转发到 libc bcmp
            "__builtin_bcmp" => {
                let ar = [7u8, 6, 2, 1, 8, 9];
                for (i, a) in args.iter().enumerate() {
                    if i >= 6 {
                        break;
                    }
                    self.gen_expr(a, ar[i]);
                }
                self.call_label("bcmp");
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_putc / __builtin_putchar / __builtin_puts: 转发到 libc
            "__builtin_putc" | "__builtin_putchar" | "__builtin_puts" => {
                let libc_name = name.strip_prefix("__builtin_").unwrap();
                let ar = [7u8, 6, 2, 1, 8, 9];
                for (i, a) in args.iter().enumerate() {
                    if i >= 6 {
                        break;
                    }
                    self.gen_expr(a, ar[i]);
                }
                self.call_label(libc_name);
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_abort: 转发到 libc abort()
            "__builtin_abort" => {
                self.call_label("abort");
                true
            },
            // __builtin_exit: 转发到 libc exit()
            "__builtin_exit" => {
                if !args.is_empty() {
                    self.gen_expr(&args[0], 7); // rdi = status
                }
                self.call_label("exit");
                true
            },
            // __builtin_copysign: 转发到 libc copysign()
            // SysV ABI: xmm0 = magnitude, xmm1 = sign
            "__builtin_copysign" => {
                if args.len() < 2 {
                    self.mov_imm(dst, 0);
                    return true;
                }
                // 加载 magnitude 到 xmm0
                self.gen_expr(&args[0], Self::EVAL_BASE); // xmm0 = magnitude
                                            // 备份 magnitude 到 xmm2
                self.emit_insn(&Insn::movsd_xmm(2, 0)); // movsd xmm2, xmm0
                                                      // 加载 sign 到 xmm0
                self.gen_expr(&args[1], Self::EVAL_BASE); // xmm0 = sign
                                            // 移动到 xmm1
                self.emit_insn(&Insn::movsd_xmm(1, 0)); // movsd xmm1, xmm0
                                                      // 恢复 magnitude 到 xmm0
                self.emit_insn(&Insn::movsd_xmm(0, 2)); // movsd xmm0, xmm2
                                                      // 调用 libc copysign
                self.call_label("copysign");
                // 结果在 xmm0, 如果需要则移动到 dst
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_va_arg_pack: 返回 0 (简化实现)
            "__builtin_va_arg_pack" => {
                self.mov_imm(dst, 0);
                true
            },
            // __builtin_malloc: 转发到 libc malloc()
            "__builtin_malloc" => {
                if !args.is_empty() {
                    self.gen_expr(&args[0], 7); // rdi = size
                }
                self.call_label("malloc");
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_free: 转发到 libc free()
            "__builtin_free" => {
                if !args.is_empty() {
                    self.gen_expr(&args[0], 7); // rdi = ptr
                }
                self.call_label("free");
                true
            },
            // __builtin_fabs / fabsf / fabsl: 浮点绝对值
            // 清除符号位: AND with abs_mask
            "__builtin_fabs" | "__builtin_fabsf" | "__builtin_fabsl" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                let arg_ty = args[0].ty();
                let is_f = matches!(arg_ty, Type::Float);
                self.gen_expr(&args[0], Self::EVAL_BASE); // xmm0 = x
                                            // 加载 abs_mask 到 xmm1
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64),
                    src: crate::codegen::x86_64::opcode::Operand::Imm(0x7FFFFFFFFFFFFFFFu64 as i64),
                }, &[]));
                self.emit_insn(&Insn::gpr_xmm(true, crate::codegen::shared::asm_operand::Width::B64, 0, 1)); // movq xmm1, rax
                if is_f {
                    self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Andps, SimdWidth::O, 0, 1)); // andps xmm0, xmm1
                } else {
                    self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Andpd, SimdWidth::O, 0, 1)); // andpd xmm0, xmm1
                }
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_copysignf: float 版本
            "__builtin_copysignf" => {
                if args.len() < 2 {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], Self::EVAL_BASE); // xmm0 = magnitude (float)
                self.emit_insn(&Insn::movsd_xmm(2, 0)); // movsd xmm2, xmm0 (backup)
                self.gen_expr(&args[1], Self::EVAL_BASE); // xmm0 = sign (float)
                self.emit_insn(&Insn::movsd_xmm(1, 0)); // movsd xmm1, xmm0
                self.emit_insn(&Insn::movsd_xmm(0, 2)); // movsd xmm0, xmm2
                self.call_label("copysignf");
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_copysignl: long double 版本
            // SysV ABI: long double 参数在 xmm0 (实际走 x87 栈), 但 libc
            // copysignl 接受 xmm0/xmm1 传递, 结果在 xmm0
            "__builtin_copysignl" => {
                if args.len() < 2 {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], Self::EVAL_BASE); // xmm0 = magnitude (long double)
                self.emit_insn(&Insn::movsd_xmm(2, 0)); // movsd xmm2, xmm0 (backup)
                self.gen_expr(&args[1], Self::EVAL_BASE); // xmm0 = sign (long double)
                self.emit_insn(&Insn::movsd_xmm(1, 0)); // movsd xmm1, xmm0
                self.emit_insn(&Insn::movsd_xmm(0, 2)); // movsd xmm0, xmm2
                self.call_label("copysignl");
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_modf/modff/modfl: 分解浮点数为整数和小数部分
            // 签名: double modf(double x, double *iptr)
            // 返回值: 小数部分 (fractional part)
            // 副作用: *iptr = 整数部分 (integer part)
            // SysV ABI: xmm0 = x, rdi = iptr, 返回值在 xmm0
            "__builtin_modf" | "__builtin_modff" | "__builtin_modfl" => {
                if args.len() < 2 {
                    self.mov_imm(dst, 0);
                    return true;
                }
                // 第一个参数: x, 放到 xmm0
                self.gen_expr(&args[0], Self::EVAL_BASE); // xmm0 = x
                                            // 第二个参数: iptr (指针), 放到 rdi
                self.gen_expr(&args[1], 7); // rdi = iptr
                                            // 调用 libc modf/modff/modfl
                let libc_name = name.strip_prefix("__builtin_").unwrap();
                self.call_label(libc_name);
                // 返回值在 xmm0 (小数部分), 如需则移动到 dst
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_bcopy(src, dst, len): 等价于 memmove(dst, src, len)
            // SysV: rdi=src, rsi=dst, rdx=len → 需要交换 rdi/rsi
            "__builtin_bcopy" => {
                if args.len() >= 3 {
                    self.gen_expr(&args[0], 7); // rdi = src
                    self.gen_expr(&args[1], 6); // rsi = dst
                    self.gen_expr(&args[2], 2); // rdx = len
                                                // 交换 rdi, rsi
                    self.emit_insn(&Insn::raw_bytes(&[0x48, 0x87, 0xFE])); // xchg rsi, rdi
                }
                self.call_label("memmove");
                if dst != 7 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_bzero(ptr, len): 等价于 memset(ptr, 0, len)
            "__builtin_bzero" => {
                if args.len() >= 2 {
                    self.gen_expr(&args[0], 7); // rdi = ptr
                    self.gen_expr(&args[1], 6); // rsi = len
                }
                self.call_label("bzero");
                true
            },
            // __builtin_stpncpy(dst, src, len): 转发到 libc
            "__builtin_stpncpy" => {
                if args.len() >= 3 {
                    self.gen_expr(&args[0], 7); // rdi = dst
                    self.gen_expr(&args[1], 6); // rsi = src
                    self.gen_expr(&args[2], 2); // rdx = len
                }
                self.call_label("stpncpy");
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_fputs(str, stream): 转发到 libc
            "__builtin_fputs" => {
                if args.len() >= 2 {
                    self.gen_expr(&args[0], 7); // rdi = str
                    self.gen_expr(&args[1], 6); // rsi = stream
                }
                self.call_label("fputs");
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_uabs: 无符号绝对值 (恒等)
            "__builtin_uabs" => {
                if !args.is_empty() {
                    self.gen_expr(&args[0], dst);
                } else {
                    self.mov_imm(dst, 0);
                }
                true
            },
            // __builtin_isfinite / isfinitef / isfinitel: 等价于 __builtin_finite
            "__builtin_isfinite" | "__builtin_isfinitef" | "__builtin_isfinitel" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                let arg_ty = args[0].ty();
                let is_f = matches!(arg_ty, Type::Float);
                self.gen_expr(&args[0], Self::EVAL_BASE);
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64),
                    src: crate::codegen::x86_64::opcode::Operand::Imm(0x7FFFFFFFFFFFFFFFu64 as i64),
                }, &[]));
                self.emit_insn(&Insn::gpr_xmm(true, crate::codegen::shared::asm_operand::Width::B64, 0, 1));
                self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Andps, SimdWidth::O, 0, 1));
                let inf_bits: u64 = if is_f { 0x7F800000 } else { 0x7FF0000000000000 };
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64),
                    src: crate::codegen::x86_64::opcode::Operand::Imm(inf_bits as i64),
                }, &[]));
                self.emit_insn(&Insn::gpr_xmm(true, crate::codegen::shared::asm_operand::Width::B64, 0, 1));
                if is_f {
                    self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Ucomi, SimdWidth::SS, 0, 1));
                } else {
                    self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Ucomi, SimdWidth::SD, 0, 1));
                }
                self.emit_insn(&Insn::setcc_reg(crate::codegen::x86_64::opcode::CondCode::B, 0));
                self.emit_insn(&Insn::setcc_reg(crate::codegen::x86_64::opcode::CondCode::Np, 3));
                self.emit_insn(&Insn::raw_bytes(&[0x20, 0xD8]));
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                    sign: false,
                    from: crate::codegen::shared::asm_operand::Width::B8,
                    to: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B8),
                }, &[]));
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_isgreater/isless/islessgreater: FP 比较 (NaN 安全)
            // 简化: 转发到 libc 或使用 ucomisd
            "__builtin_isgreater"
            | "__builtin_isgreaterequal"
            | "__builtin_isless"
            | "__builtin_islessequal"
            | "__builtin_islessgreater" => {
                if args.len() < 2 {
                    self.mov_imm(dst, 0);
                    return true;
                }
                // 转发到 libc 比较
                self.gen_expr(&args[0], Self::EVAL_BASE); // xmm0 = a
                                            // 备份 a 到 xmm2
                self.emit_insn(&Insn::movsd_xmm(2, 0)); // movsd xmm2, xmm0
                self.gen_expr(&args[1], Self::EVAL_BASE); // xmm0 = b
                self.emit_insn(&Insn::movsd_xmm(1, 0)); // movsd xmm1, xmm0
                self.emit_insn(&Insn::movsd_xmm(0, 2)); // movsd xmm0, xmm2
                                                      // ucomisd xmm0, xmm1: 比较 a vs b
                self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Ucomi, SimdWidth::SD, 0, 1));
                match name {
                    "__builtin_isgreater" => {
                        // a > b: CF=0, ZF=0, PF=0 → seta al
                        self.emit_insn(&Insn::setcc_reg(crate::codegen::x86_64::opcode::CondCode::A, 0)); // seta al
                    },
                    "__builtin_isgreaterequal" => {
                        // a >= b: CF=0 → setae al / setnb al
                        self.emit_insn(&Insn::setcc_reg(crate::codegen::x86_64::opcode::CondCode::Ae, 0)); // setae al
                    },
                    "__builtin_isless" => {
                        // a < b: CF=1 → setb al
                        self.emit_insn(&Insn::setcc_reg(crate::codegen::x86_64::opcode::CondCode::B, 0)); // setb al
                    },
                    "__builtin_islessequal" => {
                        // a <= b: CF=1 or ZF=1 → setbe al
                        self.emit_insn(&Insn::setcc_reg(crate::codegen::x86_64::opcode::CondCode::Be, 0)); // setbe al
                    },
                    "__builtin_islessgreater" => {
                        // a != b and not unordered: ZF=0 and PF=0
                        self.emit_insn(&Insn::setcc_reg(crate::codegen::x86_64::opcode::CondCode::Ne, 0)); // setne al (ZF=0)
                        self.emit_insn(&Insn::setcc_reg(crate::codegen::x86_64::opcode::CondCode::Np, 3)); // setnp bl (PF=0)
                        self.emit_insn(&Insn::raw_bytes(&[0x20, 0xD8])); // and al, bl
                    },
                    _ => {
                        self.emit_insn(&Insn::setcc_reg(crate::codegen::x86_64::opcode::CondCode::A, 0)); // seta al (default)
                    },
                }
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                    sign: false,
                    from: crate::codegen::shared::asm_operand::Width::B8,
                    to: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B8),
                }, &[])); // movzx eax, al
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_issignaling / issignalingf / issignalingl
            // 简化: 转发到 libc issignaling
            "__builtin_issignaling" | "__builtin_issignalingf" | "__builtin_issignalingl" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], 7); // rdi = x
                self.call_label("issignaling");
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_nans / nansf / nansl: signaling NaN
            // 转发到 libc nan/nanf/nanl
            "__builtin_nans" => {
                if !args.is_empty() {
                    self.gen_expr(&args[0], 7);
                } else {
                    self.mov_imm(7, 0);
                }
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Push, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(4, crate::codegen::shared::asm_operand::Width::B64)}, &[]));
                self.call_label("nan");
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Pop, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(4, crate::codegen::shared::asm_operand::Width::B64)}, &[]));
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            "__builtin_nansf" => {
                if !args.is_empty() {
                    self.gen_expr(&args[0], 7);
                } else {
                    self.mov_imm(7, 0);
                }
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Push, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(4, crate::codegen::shared::asm_operand::Width::B64)}, &[]));
                self.call_label("nanf");
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Pop, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(4, crate::codegen::shared::asm_operand::Width::B64)}, &[]));
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            "__builtin_nansl" => {
                if !args.is_empty() {
                    self.gen_expr(&args[0], 7);
                } else {
                    self.mov_imm(7, 0);
                }
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Push, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(4, crate::codegen::shared::asm_operand::Width::B64)}, &[]));
                self.call_label("nanl");
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Pop, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(4, crate::codegen::shared::asm_operand::Width::B64)}, &[]));
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_fmin: inline SSE minsd (double)
            "__builtin_fmin" => {
                if args.len() < 2 {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], Self::EVAL_BASE); // xmm0 = a
                self.emit_insn(&Insn::movsd_xmm(2, 0)); // movsd xmm2, xmm0
                self.gen_expr(&args[1], Self::EVAL_BASE); // xmm0 = b
                self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Min, SimdWidth::SD, 0, 2)); // minsd xmm0, xmm2
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_fminf: inline SSE minss (float)
            "__builtin_fminf" => {
                if args.len() < 2 {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], Self::EVAL_BASE);
                self.emit_insn(&Insn::movss_xmm(2, 0)); // movss xmm2, xmm0
                self.gen_expr(&args[1], Self::EVAL_BASE);
                self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Min, SimdWidth::SS, 0, 2)); // minss xmm0, xmm2
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_fmax: inline SSE maxsd (double)
            "__builtin_fmax" => {
                if args.len() < 2 {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], Self::EVAL_BASE);
                self.emit_insn(&Insn::movsd_xmm(2, 0)); // movsd xmm2, xmm0
                self.gen_expr(&args[1], Self::EVAL_BASE);
                self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Max, SimdWidth::SD, 0, 2)); // maxsd xmm0, xmm2
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_fmaxf: inline SSE maxss (float)
            "__builtin_fmaxf" => {
                if args.len() < 2 {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], Self::EVAL_BASE);
                self.emit_insn(&Insn::movss_xmm(2, 0)); // movss xmm2, xmm0
                self.gen_expr(&args[1], Self::EVAL_BASE);
                self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Max, SimdWidth::SS, 0, 2)); // maxss xmm0, xmm2
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_fminl/fmaxl: long double -> libm (80-bit)
            "__builtin_fmaxl" | "__builtin_fminl" => {
                if args.len() >= 2 {
                    self.gen_expr(&args[0], Self::EVAL_BASE);
                    self.emit_insn(&Insn::movsd_xmm(2, 0));
                    self.gen_expr(&args[1], Self::EVAL_BASE);
                    self.emit_insn(&Insn::movsd_xmm(1, 0));
                    self.emit_insn(&Insn::movsd_xmm(0, 2));
                }
                let lib_fn = if name == "__builtin_fmaxl" { "fmaxl" } else { "fminl" };
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Push, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(4, crate::codegen::shared::asm_operand::Width::B64)}, &[]));
                self.call_label(lib_fn);
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Pop, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(4, crate::codegen::shared::asm_operand::Width::B64)}, &[]));
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_fputc: 转发到 libc fputc
            "__builtin_fputc" => {
                if args.len() >= 2 {
                    self.gen_expr(&args[0], 7); // rdi = c
                    self.gen_expr(&args[1], 6); // rsi = stream
                }
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Push, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(4, crate::codegen::shared::asm_operand::Width::B64)}, &[]));
                self.call_label("fputc");
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Pop, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(4, crate::codegen::shared::asm_operand::Width::B64)}, &[]));
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_fwrite: 转发到 libc fwrite
            "__builtin_fwrite" => {
                if args.len() >= 4 {
                    self.gen_expr(&args[0], 7); // rdi = ptr
                    self.gen_expr(&args[1], 6); // rsi = size
                    self.gen_expr(&args[2], 2); // rdx = nmemb
                    self.gen_expr(&args[3], 1); // rcx = stream
                }
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Push, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(4, crate::codegen::shared::asm_operand::Width::B64)}, &[]));
                self.call_label("fwrite");
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Pop, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(4, crate::codegen::shared::asm_operand::Width::B64)}, &[]));
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_fread: 转发到 libc fread
            "__builtin_fread" => {
                if args.len() >= 4 {
                    self.gen_expr(&args[0], 7);
                    self.gen_expr(&args[1], 6);
                    self.gen_expr(&args[2], 2);
                    self.gen_expr(&args[3], 1);
                }
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Push, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(4, crate::codegen::shared::asm_operand::Width::B64)}, &[]));
                self.call_label("fread");
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Pop, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(4, crate::codegen::shared::asm_operand::Width::B64)}, &[]));
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_ulabs / __builtin_ullabs: unsigned abs (identity)
            "__builtin_ulabs" | "__builtin_ullabs" => {
                if !args.is_empty() {
                    self.gen_expr(&args[0], dst);
                } else {
                    self.mov_imm(dst, 0);
                }
                true
            },
            // _FloatN/_FloatNx builtin variants: forward to libc
            // Matches patterns like __builtin_nansf128, __builtin_issignalingf64x, etc.
            _ if name.starts_with("__builtin_")
                && (name.ends_with("f128")
                    || name.ends_with("f64")
                    || name.ends_with("f32")
                    || name.ends_with("f16")
                    || name.ends_with("f64x")
                    || name.ends_with("f32x")
                    || name.ends_with("f128x")) =>
            {
                // Extract base name: __builtin_issignalingf128 -> issignaling
                let base = name.strip_prefix("__builtin_").unwrap();
                let lib_name = base
                    .trim_end_matches("f128x")
                    .trim_end_matches("f128")
                    .trim_end_matches("f64x")
                    .trim_end_matches("f64")
                    .trim_end_matches("f32x")
                    .trim_end_matches("f32")
                    .trim_end_matches("f16");
                // Forward to libc with appropriate args
                if name.contains("issignaling")
                    || name.contains("isfinite")
                    || name.contains("isnan")
                    || name.contains("isinf")
                {
                    // Classification: 1 arg
                    if !args.is_empty() {
                        self.gen_expr(&args[0], 7);
                    }
                    self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Push, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(4, crate::codegen::shared::asm_operand::Width::B64)}, &[]));
                    self.call_label(lib_name);
                    self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Pop, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(4, crate::codegen::shared::asm_operand::Width::B64)}, &[]));
                    if dst != 0 {
                        self.mov(dst, 0);
                    }
                } else if name.contains("nans") || name.contains("nan") {
                    // NaN generation: 0-1 args
                    if !args.is_empty() {
                        self.gen_expr(&args[0], 7);
                    } else {
                        self.mov_imm(7, 0);
                    }
                    self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Push, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(4, crate::codegen::shared::asm_operand::Width::B64)}, &[]));
                    self.call_label(lib_name);
                    self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Pop, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(4, crate::codegen::shared::asm_operand::Width::B64)}, &[]));
                    if dst != 0 {
                        self.mov(dst, 0);
                    }
                } else {
                    // Generic: forward all args
                    for (i, arg) in args.iter().take(4).enumerate() {
                        let reg = [7, 6, 2, 1][i]; // rdi, rsi, rdx, rcx
                        self.gen_expr(arg, reg);
                    }
                    self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Push, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(4, crate::codegen::shared::asm_operand::Width::B64)}, &[]));
                    self.call_label(lib_name);
                    self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Pop, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(4, crate::codegen::shared::asm_operand::Width::B64)}, &[]));
                    if dst != 0 {
                        self.mov(dst, 0);
                    }
                }
                true
            },
            // __builtin_shuffle(vec, mask): GCC SIMD shuffle — 标量逐元素回退
            // __builtin_shufflevector(vec1, vec2, idx...) = clang 风格, 复用同逻辑
            "__builtin_shuffle" | "__builtin_shufflevector" => {
                if args.len() < 2 {
                    self.mov_imm(dst, 0);
                    return true;
                }
                let src_ty = args[0].ty();
                let (elem_ty, count) = match src_ty {
                    Type::Vector(ref elem, n) => (elem.base.clone(), n),
                    _ => {
                        self.gen_expr(&args[0], dst);
                        return true;
                    },
                };
                let elem_size = elem_ty.size();
                let vec_size = elem_size * count;
                self.gen_vector_load_into_slot(&args[0], 0);
                self.gen_vector_load_into_slot(&args[1], 1);
                let src_off = self.vector_temp_base(0, vec_size);
                let mask_off = self.vector_temp_base(1, vec_size);
                let result_slot = 2usize;
                let dst_off = self.vector_temp_base(result_slot, vec_size);
                let count_mask = count - 1;
                let is_float = matches!(elem_ty, Type::Float);
                let is_double = matches!(elem_ty, Type::Double);
                // rsi = &src[0] (caller-saved, vec/mask 已物化完毕)
                self.emit_lea_r13(6, src_off);
                for i in 0..count {
                    // 1. 读 mask[i] → rax (mask 为整型向量, 用 GP load)
                    let mi_off = mask_off + (i * elem_size) as i64;
                    self.emit_lea_r13(0, mi_off);
                    self.emit_load_mem(elem_size);
                    // 2. rax = (rax & count_mask) * elem_size → byte offset
                    self.emit_insn(&Insn::raw_bytes(&[0x48, 0x25]));
                    self.emit_insn(&Insn::raw_bytes(&(count_mask as i32).to_le_bytes()));
                    if elem_size == 1 {
                    } else if elem_size.is_power_of_two() {
                        let shift = elem_size.trailing_zeros();
                        self.emit_insn(&Insn::raw_bytes(&[0x48, 0xC1, 0xE0, shift as u8]));
                    } else {
                        self.mov_imm(10, elem_size as i128);
                        self.emit_insn(&Insn::imul_rm_reg(
                            crate::codegen::shared::asm_operand::Width::B64,
                            0, 10, // rax, r10
                        ));
                    }
                    // 3. rax = rsi + rax → &src[j]
                    self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::Add, crate::codegen::shared::asm_operand::Width::B64, 0, 6)); // add rax, rsi
                                                    // 4. 读 *rax → xmm0 (标量, 只加载 elem_size 字节)
                    if is_double {
                        self.emit_insn(&Insn::simd_mov_mem_base(false, crate::codegen::x86_64::opcode::SimdWidth::SD, 0, 0, 0)); // movsd xmm0, [rax] (8B)
                    } else if is_float {
                        self.emit_insn(&Insn::simd_mov_mem_base(false, crate::codegen::x86_64::opcode::SimdWidth::SS, 0, 0, 0)); // movss xmm0, [rax] (4B)
                    } else {
                        self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B64
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0, base: Some(0), index: None, seg: None,
                        }
                    ),
                },
                &[],
            )); // mov rax, [rax]
                    }
                    // 5. 写 dst[i]
                    let ri_off = dst_off + (i * elem_size) as i64;
                    if is_double {
                        self.store_xmm64_stack(0, ri_off);
                    } else if is_float {
                        self.store_xmm32_stack(0, ri_off);
                    } else {
                        self.emit_store(0, ri_off, elem_size);
                    }
                }
                // 6. 把结果从 slot 2 复制回 slot 0, 然后 load 到 xmm0
                self.copy_r13_bytes(dst_off, src_off, vec_size);
                if vec_size == 16 {
                    self.emit_movdqu_r13_to_xmm(0, src_off);
                }
                true
            },
            // __builtin_convertvector: vector type conversion (stub: return first arg)
            // __builtin_sqrt: inline SSE sqrtsd (double)
            // sqrtsd: F2 0F 51 /r
            "__builtin_sqrt" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], Self::EVAL_BASE); // xmm0 = x
                self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Sqrt, SimdWidth::SD, 0, 0)); // sqrtsd xmm0, xmm0
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_sqrtf: inline SSE sqrtss (float)
            // sqrtss: F3 0F 51 /r
            "__builtin_sqrtf" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], Self::EVAL_BASE);
                self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Sqrt, SimdWidth::SS, 0, 0)); // sqrtss xmm0, xmm0
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_sqrtl: long double -> libm sqrtl (80-bit, no SSE)
            "__builtin_sqrtl" => {
                if !args.is_empty() {
                    self.gen_expr(&args[0], Self::EVAL_BASE);
                }
                self.call_label("sqrtl");
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_floor/floorf: inline SSE roundsd/roundss (mode 01 = toward -inf)
            // roundsd: 66 0F 3A 0B /r ib
            "__builtin_floor" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], Self::EVAL_BASE);
                self.emit_insn(&Insn::simd_round_reg(SimdWidth::SD, 0x01, 0, 0));
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            "__builtin_floorf" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], Self::EVAL_BASE);
                self.emit_insn(&Insn::simd_round_reg(SimdWidth::SS, 0x01, 0, 0));
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_floorl: long double -> libm (80-bit)
            "__builtin_floorl" => {
                if !args.is_empty() {
                    self.gen_expr(&args[0], Self::EVAL_BASE);
                }
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Push, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(4, crate::codegen::shared::asm_operand::Width::B64)}, &[]));
                self.call_label("floorl");
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Pop, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(4, crate::codegen::shared::asm_operand::Width::B64)}, &[]));
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_ceil/ceilf: inline SSE roundsd/roundss (mode 10 = toward +inf)
            "__builtin_ceil" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], Self::EVAL_BASE);
                self.emit_insn(&Insn::simd_round_reg(SimdWidth::SD, 0x02, 0, 0));
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            "__builtin_ceilf" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], Self::EVAL_BASE);
                self.emit_insn(&Insn::simd_round_reg(SimdWidth::SS, 0x02, 0, 0));
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_ceill: long double -> libm (80-bit)
            "__builtin_ceill" => {
                if !args.is_empty() {
                    self.gen_expr(&args[0], Self::EVAL_BASE);
                }
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Push, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(4, crate::codegen::shared::asm_operand::Width::B64)}, &[]));
                self.call_label("ceill");
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Pop, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(4, crate::codegen::shared::asm_operand::Width::B64)}, &[]));
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_round/roundf: inline SSE roundsd/roundss (mode 00 = nearest)
            "__builtin_round" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], Self::EVAL_BASE);
                self.emit_insn(&Insn::simd_round_reg(SimdWidth::SD, 0x00, 0, 0));
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            "__builtin_roundf" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], Self::EVAL_BASE);
                self.emit_insn(&Insn::simd_round_reg(SimdWidth::SS, 0x00, 0, 0));
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_roundl: long double -> libm (80-bit)
            "__builtin_roundl" => {
                if !args.is_empty() {
                    self.gen_expr(&args[0], Self::EVAL_BASE);
                }
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Push, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(4, crate::codegen::shared::asm_operand::Width::B64)}, &[]));
                self.call_label("roundl");
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Pop, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(4, crate::codegen::shared::asm_operand::Width::B64)}, &[]));
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_trunc/truncf: inline SSE roundsd/roundss (mode 03 = toward zero)
            "__builtin_trunc" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], Self::EVAL_BASE);
                self.emit_insn(&Insn::simd_round_reg(SimdWidth::SD, 0x03, 0, 0));
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            "__builtin_truncf" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], Self::EVAL_BASE);
                self.emit_insn(&Insn::simd_round_reg(SimdWidth::SS, 0x03, 0, 0));
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // __builtin_truncl: long double -> libm (80-bit)
            "__builtin_truncl" => {
                if !args.is_empty() {
                    self.gen_expr(&args[0], Self::EVAL_BASE);
                }
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Push, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(4, crate::codegen::shared::asm_operand::Width::B64)}, &[]));
                self.call_label("truncl");
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Pop, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(4, crate::codegen::shared::asm_operand::Width::B64)}, &[]));
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // libm 数学函数: 转发到 libc (log/exp/sin 及 f/l 变体)
            // 运行时依赖 libm, 优化级别下可被 const/pure 折叠.
            "__builtin_log" | "__builtin_logf" | "__builtin_logl" | "__builtin_exp"
            | "__builtin_expf" | "__builtin_expl" | "__builtin_sin" | "__builtin_sinf"
            | "__builtin_sinl" => {
                if !args.is_empty() {
                    self.gen_expr(&args[0], Self::EVAL_BASE);
                }
                let libc_name = name.strip_prefix("__builtin_").unwrap();
                self.call_label(libc_name);
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            "__builtin_convertvector" => {
                if !args.is_empty() {
                    self.gen_expr(&args[0], dst);
                } else {
                    self.mov_imm(dst, 0);
                }
                true
            },
            _ => false,
        }
    }
    fn gen_builtin_fn_ptr(&mut self, e: &Expr, dst: u8) {
        let inner = match e {
            Expr::Cast { e, .. } | Expr::AddrOf { e, .. } => e.as_ref(),
            other => other,
        };
        if let Expr::Ident { name, .. } = inner {
            if self.labels.contains_key(&name.name) || self.is_global_var(&name.name) {
                self.emit_func_addr(dst, &name.name);
                return;
            }
        }
        self.gen_expr(e, dst);
    }
    fn gen_fenv_builtin(&mut self, name: &str, args: &[Expr], dst: u8) -> bool {
        match name {
            // fegetround(): 返回当前舍入模式 (FE_TONEAREST=0, FE_DOWNWARD=0x400, ...)
            "fegetround" => {
                self.emit_insn(&Insn::sub_rsp_imm(8)); // sub rsp, 8
                self.emit_insn(&Insn::mxcsr_mem(true, 4, 0)); // stmxcsr [rsp]
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(4),
                            index: None,
                            seg: None,
                        }
                    ),
                },
                &[],
            )); // mov eax, [rsp]
                self.emit_insn(&Insn::raw_bytes(&[0x25, 0x00, 0x60, 0x00, 0x00])); // and eax, 0x6000
                self.emit_insn(&Insn::raw_bytes(&[0xC1, 0xE8, 0x03])); // shr eax, 3 → 控制字位 10-11
                self.emit_insn(&Insn::add_rsp_imm(8)); // add rsp, 8
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // fesetround(mode): 设置 MXCSR 舍入位 13-14, 返回 0
            "fesetround" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], Self::EVAL_BASE); // rax = mode
                self.emit_insn(&Insn::raw_bytes(&[0x25, 0x00, 0x0C, 0x00, 0x00])); // and eax, 0xC00
                self.emit_insn(&Insn::raw_bytes(&[0xC1, 0xE0, 0x03])); // shl eax, 3 → MXCSR 位 13-14
                self.emit_insn(&Insn::sub_rsp_imm(8)); // sub rsp, 8
                self.emit_insn(&Insn::mxcsr_mem(true, 4, 0)); // stmxcsr [rsp]
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        1, crate::codegen::shared::asm_operand::Width::B32
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0, base: Some(4), index: None, seg: None,
                        }
                    ),
                },
                &[],
            )); // mov ecx, [rsp]
                self.emit_insn(&Insn::new(
                    crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                        op: crate::codegen::x86_64::opcode::AluOp::And,
                        width: crate::codegen::shared::asm_operand::Width::B32,
                        dst: crate::codegen::x86_64::opcode::Operand::Reg(
                            1, crate::codegen::shared::asm_operand::Width::B32
                        ),
                        src: crate::codegen::x86_64::opcode::Operand::Imm(
                            0xFFFF_9FFF_u32 as i32 as i64
                        ),
                    },
                    &[],
                )); // and ecx, 0xFFFF9FFF
                self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::Or, crate::codegen::shared::asm_operand::Width::B32, 1, 0)); // or ecx, eax
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(4),
                            index: None,
                            seg: None,
                        }
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        1, crate::codegen::shared::asm_operand::Width::B32
                    ),
                },
                &[],
            )); // mov [rsp], ecx
                self.emit_insn(&Insn::mxcsr_mem(false, 4, 0)); // ldmxcsr [rsp]
                self.emit_insn(&Insn::add_rsp_imm(8)); // add rsp, 8
                self.mov_imm(dst, 0); // 返回 0
                true
            },
            // feclearexcept(excepts): 清除 MXCSR 和 x87 状态字中选中的异常标志, 返回 0
            // 5.5: 同时处理 SSE (MXCSR) 和 x87 FPU 异常标志
            "feclearexcept" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], Self::EVAL_BASE); // rax = excepts
                self.emit_insn(&Insn::raw_bytes(&[0x25, 0x3F, 0x00, 0x00, 0x00])); // and eax, 0x3F
                self.emit_insn(&Insn::unary_rm_reg(crate::codegen::x86_64::opcode::UnaryOp::Not, crate::codegen::shared::asm_operand::Width::B32, 0)); // not eax
                                          // 清除 MXCSR 异常标志
                self.emit_insn(&Insn::sub_rsp_imm(8)); // sub rsp, 8
                self.emit_insn(&Insn::mxcsr_mem(true, 4, 0)); // stmxcsr [rsp]
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        1, crate::codegen::shared::asm_operand::Width::B32
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0, base: Some(4), index: None, seg: None,
                        }
                    ),
                },
                &[],
            )); // mov ecx, [rsp]
                self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::And, crate::codegen::shared::asm_operand::Width::B32, 1, 0)); // and ecx, eax
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(4),
                            index: None,
                            seg: None,
                        }
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        1, crate::codegen::shared::asm_operand::Width::B32
                    ),
                },
                &[],
            )); // mov [rsp], ecx
                self.emit_insn(&Insn::mxcsr_mem(false, 4, 0)); // ldmxcsr [rsp]
                self.emit_insn(&Insn::add_rsp_imm(8)); // add rsp, 8
                                                   // 清除 x87 FPU 状态字异常标志 (fnclex 清除所有异常)
                                                   // 注: fnclex 清除所有 6 个异常标志, 与 excepts 掩码无关
                                                   // 这是安全的因为 C11 允许清除额外的异常
                self.emit_insn(&Insn::raw_bytes(&[0xDB, 0xE2])); // fnclex
                self.mov_imm(dst, 0); // 返回 0
                true
            },
            // feraiseexcept(excepts): 置位 MXCSR 中选中的异常标志, 返回 0
            // 5.5: x87 异常通过 MXCSR 同步 (fwait 检查 MXCSR 异常标志)
            "feraiseexcept" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], Self::EVAL_BASE); // rax = excepts
                self.emit_insn(&Insn::raw_bytes(&[0x25, 0x3F, 0x00, 0x00, 0x00])); // and eax, 0x3F
                self.emit_insn(&Insn::sub_rsp_imm(8)); // sub rsp, 8
                self.emit_insn(&Insn::mxcsr_mem(true, 4, 0)); // stmxcsr [rsp]
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        1, crate::codegen::shared::asm_operand::Width::B32
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0, base: Some(4), index: None, seg: None,
                        }
                    ),
                },
                &[],
            )); // mov ecx, [rsp]
                self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::Or, crate::codegen::shared::asm_operand::Width::B32, 1, 0)); // or ecx, eax
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(4),
                            index: None,
                            seg: None,
                        }
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        1, crate::codegen::shared::asm_operand::Width::B32
                    ),
                },
                &[],
            )); // mov [rsp], ecx
                self.emit_insn(&Insn::mxcsr_mem(false, 4, 0)); // ldmxcsr [rsp]
                self.emit_insn(&Insn::add_rsp_imm(8)); // add rsp, 8
                                                   // fwait 同步 x87: 检查 MXCSR 中刚置位的异常标志
                                                   // 如果异常未被屏蔽, x87 会产生相应的异常
                self.emit_insn(&Insn::raw_bytes(&[0x9B])); // fwait
                self.mov_imm(dst, 0); // 返回 0
                true
            },
            // fetestexcept(excepts): 返回 MXCSR 和 x87 状态字中选中异常标志的位与
            // 5.5: 同时测试 SSE (MXCSR) 和 x87 FPU 异常标志
            "fetestexcept" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], Self::EVAL_BASE); // rax = excepts
                self.emit_insn(&Insn::raw_bytes(&[0x25, 0x3F, 0x00, 0x00, 0x00])); // and eax, 0x3F
                                                            // 测试 MXCSR 异常标志
                self.emit_insn(&Insn::sub_rsp_imm(8)); // sub rsp, 8
                self.emit_insn(&Insn::mxcsr_mem(true, 4, 0)); // stmxcsr [rsp]
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        1, crate::codegen::shared::asm_operand::Width::B32
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0, base: Some(4), index: None, seg: None,
                        }
                    ),
                },
                &[],
            )); // mov ecx, [rsp]
                self.emit_insn(&Insn::raw_bytes(&[0x81, 0xE1, 0x3F, 0x00, 0x00, 0x00])); // and ecx, 0x3F
                self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::And, crate::codegen::shared::asm_operand::Width::B32, 1, 0)); // and ecx, eax (ecx = MXCSR & excepts)
                self.emit_insn(&Insn::add_rsp_imm(8)); // add rsp, 8
                                                   // 测试 x87 FPU 状态字异常标志
                self.emit_insn(&Insn::sub_rsp_imm(8)); // sub rsp, 8
                self.emit_insn(&Insn::raw_bytes(&[0xDF, 0xE0])); // fnstsw ax (x87 状态字 -> ax)
                self.emit_insn(&Insn::raw_bytes(&[0x66, 0x25, 0x3F, 0x00])); // and ax, 0x3F
                self.emit_insn(&Insn::raw_bytes(&[0x66, 0x21, 0xC0])); // and ax, ax (保留 excepts 位)
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                    sign: false,
                    from: crate::codegen::shared::asm_operand::Width::B16,
                    to: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B32),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B16),
                }, &[])); // movzx eax, ax
                self.emit_insn(&Insn::add_rsp_imm(8)); // add rsp, 8
                                                   // 合并 MXCSR 和 x87 结果 (OR)
                self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::Or, crate::codegen::shared::asm_operand::Width::B32, 1, 0)); // or ecx, eax
                self.mov(0, 1); // rax = rcx
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // fegetexceptflag(flagp, excepts): 保存选中标志到 *flagp (u16), 返回 0
            "fegetexceptflag" => {
                if args.len() < 2 {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], 7); // rdi = flagp
                self.gen_expr(&args[1], Self::EVAL_BASE); // rax = excepts
                self.emit_insn(&Insn::raw_bytes(&[0x25, 0x3F, 0x00, 0x00, 0x00])); // and eax, 0x3F
                self.emit_insn(&Insn::sub_rsp_imm(8)); // sub rsp, 8
                self.emit_insn(&Insn::mxcsr_mem(true, 4, 0)); // stmxcsr [rsp]
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0, base: Some(4), index: None, seg: None,
                        }
                    ),
                },
                &[],
            )); // mov ecx, [rsp]
                self.emit_insn(&Insn::raw_bytes(&[0x81, 0xE1, 0x3F, 0x00, 0x00, 0x00])); // and ecx, 0x3F
                self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::And, crate::codegen::shared::asm_operand::Width::B32, 1, 0)); // and ecx, eax
                self.emit_insn(&Insn::raw_bytes(&[0x66, 0x89, 0x0F])); // mov [rdi], cx
                self.emit_insn(&Insn::add_rsp_imm(8)); // add rsp, 8
                self.mov_imm(dst, 0); // 返回 0
                true
            },
            // fesetexceptflag(flagp, excepts): 从 *flagp 恢复选中标志, 返回 0
            "fesetexceptflag" => {
                if args.len() < 2 {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], 7); // rdi = flagp
                self.gen_expr(&args[1], Self::EVAL_BASE); // rax = excepts
                self.emit_insn(&Insn::mov_ext_mem(
                    false,
                    crate::codegen::shared::asm_operand::Width::B16,
                    crate::codegen::shared::asm_operand::Width::B32,
                    1, 7, 0,
                )); // movzx ecx, word [rdi]
                self.emit_insn(&Insn::raw_bytes(&[0x81, 0xE1, 0x3F, 0x00, 0x00, 0x00])); // and ecx, 0x3F
                self.emit_insn(&Insn::raw_bytes(&[0x25, 0x3F, 0x00, 0x00, 0x00])); // and eax, 0x3F
                self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::And, crate::codegen::shared::asm_operand::Width::B32, 1, 0)); // and ecx, eax → ecx = *flagp & excepts
                self.emit_insn(&Insn::sub_rsp_imm(8)); // sub rsp, 8
                self.emit_insn(&Insn::mxcsr_mem(true, 4, 0)); // stmxcsr [rsp]
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        2, crate::codegen::shared::asm_operand::Width::B32
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(4),
                            index: None,
                            seg: None,
                        }
                    ),
                },
                &[],
            )); // mov edx, [rsp]
                self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::Or, crate::codegen::shared::asm_operand::Width::B32, 2, 1)); // or edx, ecx (置位选中标志)
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(4),
                            index: None,
                            seg: None,
                        }
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        2, crate::codegen::shared::asm_operand::Width::B32
                    ),
                },
                &[],
            )); // mov [rsp], edx
                self.emit_insn(&Insn::mxcsr_mem(false, 4, 0)); // ldmxcsr [rsp]
                self.emit_insn(&Insn::add_rsp_imm(8)); // add rsp, 8
                self.mov_imm(dst, 0); // 返回 0
                true
            },
            // fetestexceptflag(flagp, excepts) (C23): 返回 *flagp 中选中标志
            "fetestexceptflag" => {
                if args.len() < 2 {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], 7); // rdi = flagp
                self.emit_insn(&Insn::mov_ext_mem(
                    false,
                    crate::codegen::shared::asm_operand::Width::B16,
                    crate::codegen::shared::asm_operand::Width::B32,
                    1, 7, 0,
                )); // movzx ecx, word [rdi]
                self.emit_insn(&Insn::raw_bytes(&[0x81, 0xE1, 0x3F, 0x00, 0x00, 0x00])); // and ecx, 0x3F
                self.gen_expr(&args[1], Self::EVAL_BASE); // rax = excepts
                self.emit_insn(&Insn::raw_bytes(&[0x25, 0x3F, 0x00, 0x00, 0x00])); // and eax, 0x3F
                self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::And, crate::codegen::shared::asm_operand::Width::B32, 1, 0)); // and ecx, eax
                self.mov(0, 1); // rax = rcx
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // fegetenv(envp): fnstenv [rdi] + stmxcsr [rdi+28] (glibc fenv_t 布局)
            "fegetenv" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], 7); // rdi = envp
                self.emit_insn(&Insn::raw_bytes(&[0xD9, 0x37])); // fnstenv [rdi]
                self.emit_insn(&Insn::mxcsr_mem(true, 7, 0x1C)); // stmxcsr [rdi+0x1C]
                self.mov_imm(dst, 0); // 返回 0
                true
            },
            // fesetenv(envp): fldenv + ldmxcsr; FE_DFL_ENV (-1) 恢复默认环境
            "fesetenv" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], 7); // rdi = envp
                let l_default = self.fresh_label();
                let l_done = self.fresh_label();
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::AluRm { op: crate::codegen::x86_64::opcode::AluOp::Cmp, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(7, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Imm(-1)}, &[])); // cmp rdi, -1
                self.jmp_rel32(&l_default, JumpKind::Je);
                self.emit_insn(&Insn::raw_bytes(&[0xD9, 0x27])); // fldenv [rdi]
                self.emit_insn(&Insn::mxcsr_mem(false, 7, 0x1C)); // ldmxcsr [rdi+0x1C]
                self.jmp_rel32(&l_done, JumpKind::Jmp);
                self.label(&l_default);
                self.emit_insn(&Insn::sub_rsp_imm(8)); // sub rsp, 8
                self.emit_insn(&Insn::raw_bytes(&[0x66, 0xC7, 0x04, 0x24, 0x7F, 0x03])); // mov word [rsp], 0x037F
                self.emit_insn(&Insn::fcw_mem(false, 4, 0)); // fldcw [rsp]
                self.emit_insn(&Insn::raw_bytes(&[0xC7, 0x04, 0x24, 0x80, 0x1F, 0x00, 0x00])); // mov dword [rsp], 0x1F80
                self.emit_insn(&Insn::mxcsr_mem(false, 4, 0)); // ldmxcsr [rsp]
                self.emit_insn(&Insn::add_rsp_imm(8)); // add rsp, 8
                self.label(&l_done);
                self.mov_imm(dst, 0); // 返回 0
                true
            },
            // feholdexcept(envp): 保存环境, 清除异常标志并屏蔽全部异常, 返回 0
            "feholdexcept" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], 7); // rdi = envp
                self.emit_insn(&Insn::raw_bytes(&[0xD9, 0x37])); // fnstenv [rdi]
                self.emit_insn(&Insn::mxcsr_mem(true, 7, 0x1C)); // stmxcsr [rdi+0x1C]
                self.emit_insn(&Insn::sub_rsp_imm(8)); // sub rsp, 8
                self.emit_insn(&Insn::mxcsr_mem(true, 4, 0)); // stmxcsr [rsp]
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(4),
                            index: None,
                            seg: None,
                        }
                    ),
                },
                &[],
            )); // mov eax, [rsp]
                self.emit_insn(&Insn::raw_bytes(&[0x25, 0xC0, 0xFF, 0xFF, 0xFF])); // and eax, ~0x3F
                self.emit_insn(&Insn::raw_bytes(&[0x0D, 0x80, 0x1F, 0x00, 0x00])); // or eax, 0x1F80 (mask all)
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(4),
                            index: None,
                            seg: None,
                        }
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                },
                &[],
            )); // mov [rsp], eax
                self.emit_insn(&Insn::mxcsr_mem(false, 4, 0)); // ldmxcsr [rsp]
                self.emit_insn(&Insn::fcw_mem(true, 4, 0)); // fnstcw [rsp]
                self.emit_insn(&Insn::raw_bytes(&[0x66, 0x8B, 0x04, 0x24])); // mov ax, [rsp]
                self.emit_insn(&Insn::raw_bytes(&[0x66, 0x83, 0xC8, 0x3F])); // or ax, 0x3F
                self.emit_insn(&Insn::raw_bytes(&[0x66, 0x89, 0x04, 0x24])); // mov [rsp], ax
                self.emit_insn(&Insn::fcw_mem(false, 4, 0)); // fldcw [rsp]
                self.emit_insn(&Insn::add_rsp_imm(8)); // add rsp, 8
                self.mov_imm(dst, 0); // 返回 0
                true
            },
            // feupdateenv(envp): 保存旧异常标志, 恢复环境, 再置位旧标志, 返回 0
            "feupdateenv" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], 7); // rdi = envp
                                            // rcx = 旧 SSE 异常标志
                self.emit_insn(&Insn::sub_rsp_imm(8)); // sub rsp, 8
                self.emit_insn(&Insn::mxcsr_mem(true, 4, 0)); // stmxcsr [rsp]
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0, base: Some(4), index: None, seg: None,
                        }
                    ),
                },
                &[],
            )); // mov ecx, [rsp]
                self.emit_insn(&Insn::raw_bytes(&[0x81, 0xE1, 0x3F, 0x00, 0x00, 0x00])); // and ecx, 0x3F
                self.emit_insn(&Insn::add_rsp_imm(8)); // add rsp, 8
                                                   // 恢复环境 (同 fesetenv 内联)
                let l_default = self.fresh_label();
                let l_done = self.fresh_label();
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::AluRm { op: crate::codegen::x86_64::opcode::AluOp::Cmp, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(7, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Imm(-1)}, &[])); // cmp rdi, -1
                self.jmp_rel32(&l_default, JumpKind::Je);
                self.emit_insn(&Insn::raw_bytes(&[0xD9, 0x27])); // fldenv [rdi]
                self.emit_insn(&Insn::mxcsr_mem(false, 7, 0x1C)); // ldmxcsr [rdi+0x1C]
                self.jmp_rel32(&l_done, JumpKind::Jmp);
                self.label(&l_default);
                self.emit_insn(&Insn::sub_rsp_imm(8)); // sub rsp, 8
                self.emit_insn(&Insn::raw_bytes(&[0x66, 0xC7, 0x04, 0x24, 0x7F, 0x03])); // mov word [rsp], 0x037F
                self.emit_insn(&Insn::fcw_mem(false, 4, 0)); // fldcw [rsp]
                self.emit_insn(&Insn::raw_bytes(&[0xC7, 0x04, 0x24, 0x80, 0x1F, 0x00, 0x00])); // mov dword [rsp], 0x1F80
                self.emit_insn(&Insn::mxcsr_mem(false, 4, 0)); // ldmxcsr [rsp]
                self.emit_insn(&Insn::add_rsp_imm(8)); // add rsp, 8
                self.label(&l_done);
                // 在新环境中置位旧标志
                self.emit_insn(&Insn::sub_rsp_imm(8)); // sub rsp, 8
                self.emit_insn(&Insn::mxcsr_mem(true, 4, 0)); // stmxcsr [rsp]
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(4),
                            index: None,
                            seg: None,
                        }
                    ),
                },
                &[],
            )); // mov eax, [rsp]
                self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::Or, crate::codegen::shared::asm_operand::Width::B32, 0, 1)); // or eax, ecx
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(4),
                            index: None,
                            seg: None,
                        }
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                },
                &[],
            )); // mov [rsp], eax
                self.emit_insn(&Insn::mxcsr_mem(false, 4, 0)); // ldmxcsr [rsp]
                self.emit_insn(&Insn::add_rsp_imm(8)); // add rsp, 8
                self.mov_imm(dst, 0); // 返回 0
                true
            },
            // fesetexcept(excepts): C17 设置异常标志 (不触发陷阱)
            // 与 feraiseexcept 类似但不调用 fwait (不检查屏蔽)
            "fesetexcept" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], Self::EVAL_BASE); // rax = excepts
                self.emit_insn(&Insn::raw_bytes(&[0x25, 0x3F, 0x00, 0x00, 0x00])); // and eax, 0x3F
                self.emit_insn(&Insn::sub_rsp_imm(8)); // sub rsp, 8
                self.emit_insn(&Insn::mxcsr_mem(true, 4, 0)); // stmxcsr [rsp]
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0, base: Some(4), index: None, seg: None,
                        }
                    ),
                },
                &[],
            )); // mov ecx, [rsp]
                self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::Or, crate::codegen::shared::asm_operand::Width::B32, 1, 0)); // or ecx, eax
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(4),
                            index: None,
                            seg: None,
                        }
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        1, crate::codegen::shared::asm_operand::Width::B32
                    ),
                },
                &[],
            )); // mov [rsp], ecx
                self.emit_insn(&Insn::mxcsr_mem(false, 4, 0)); // ldmxcsr [rsp]
                self.emit_insn(&Insn::add_rsp_imm(8)); // add rsp, 8
                self.mov_imm(dst, 0);
                true
            },
            // feenableexcept(excepts): GNU extension 启用异常陷阱
            // 清除 MXCSR 中对应的异常屏蔽位 (位 7-12)
            // 返回之前启用的异常掩码
            "feenableexcept" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], Self::EVAL_BASE); // rax = excepts
                self.emit_insn(&Insn::raw_bytes(&[0x25, 0x3F, 0x00, 0x00, 0x00])); // and eax, 0x3F
                self.emit_insn(&Insn::sub_rsp_imm(8)); // sub rsp, 8
                self.emit_insn(&Insn::mxcsr_mem(true, 4, 0)); // stmxcsr [rsp]
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0, base: Some(4), index: None, seg: None,
                        }
                    ),
                },
                &[],
            )); // mov ecx, [rsp]
                                                // 保存旧的启用掩码 (屏蔽位取反 = 启用位)
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        1, crate::codegen::shared::asm_operand::Width::B32
                    ),
                },
                &[],
            )); // mov eax, ecx
                self.emit_insn(&Insn::raw_bytes(&[0xC1, 0xE8, 0x07])); // shr eax, 7
                self.emit_insn(&Insn::raw_bytes(&[0x25, 0x3F, 0x00, 0x00, 0x00])); // and eax, 0x3F
                                                            // 清除屏蔽位 (启用陷阱): 屏蔽位在 7-12
                                                            // eax = excepts (from gen_expr above), copy to edx
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        2, crate::codegen::shared::asm_operand::Width::B32
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                },
                &[],
            )); // mov edx, eax (edx = excepts)
                self.emit_insn(&Insn::raw_bytes(&[0xC1, 0xE2, 0x07])); // shl edx, 7 (shift to mask position)
                self.emit_insn(&Insn::unary_rm_reg(crate::codegen::x86_64::opcode::UnaryOp::Not, crate::codegen::shared::asm_operand::Width::B32, 2)); // not edx (invert: clear bit = enable)
                self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::And, crate::codegen::shared::asm_operand::Width::B32, 1, 2)); // and ecx, edx (clear mask bits in MXCSR)
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(4),
                            index: None,
                            seg: None,
                        }
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        1, crate::codegen::shared::asm_operand::Width::B32
                    ),
                },
                &[],
            )); // mov [rsp], ecx
                self.emit_insn(&Insn::mxcsr_mem(false, 4, 0)); // ldmxcsr [rsp]
                self.emit_insn(&Insn::add_rsp_imm(8)); // add rsp, 8
                                                   // eax = 旧的启用掩码
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // fedisableexcept(excepts): GNU extension 禁用异常陷阱
            // 设置 MXCSR 中对应的异常屏蔽位 (位 7-12)
            // 返回之前启用的异常掩码
            "fedisableexcept" => {
                if args.is_empty() {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], Self::EVAL_BASE); // rax = excepts
                self.emit_insn(&Insn::raw_bytes(&[0x25, 0x3F, 0x00, 0x00, 0x00])); // and eax, 0x3F
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        2, crate::codegen::shared::asm_operand::Width::B32
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                },
                &[],
            )); // mov edx, eax (save excepts in edx)
                self.emit_insn(&Insn::sub_rsp_imm(0x10)); // sub rsp, 16
                self.emit_insn(&Insn::mxcsr_mem(true, 4, 0)); // stmxcsr [rsp]
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0, base: Some(4), index: None, seg: None,
                        }
                    ),
                },
                &[],
            )); // mov ecx, [rsp] (ecx = MXCSR)
                                                // 计算旧的启用掩码
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        1, crate::codegen::shared::asm_operand::Width::B32
                    ),
                },
                &[],
            )); // mov eax, ecx
                self.emit_insn(&Insn::raw_bytes(&[0xC1, 0xE8, 0x07])); // shr eax, 7
                self.emit_insn(&Insn::raw_bytes(&[0x25, 0x3F, 0x00, 0x00, 0x00])); // and eax, 0x3F
                                                            // 保存旧掩码到 [rsp+8]
                self.emit_insn(&Insn::raw_bytes(&[0x89, 0x44, 0x24, 0x08])); // mov [rsp+8], eax
                                                      // 设置屏蔽位 (禁用陷阱)
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        2, crate::codegen::shared::asm_operand::Width::B32
                    ),
                },
                &[],
            )); // mov eax, edx (eax = excepts)
                self.emit_insn(&Insn::raw_bytes(&[0xC1, 0xE0, 0x07])); // shl eax, 7
                self.emit_insn(&Insn::alu_rm_reg_reg(crate::codegen::x86_64::opcode::AluOp::Or, crate::codegen::shared::asm_operand::Width::B32, 1, 0)); // or ecx, eax
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(4),
                            index: None,
                            seg: None,
                        }
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        1, crate::codegen::shared::asm_operand::Width::B32
                    ),
                },
                &[],
            )); // mov [rsp], ecx
                self.emit_insn(&Insn::mxcsr_mem(false, 4, 0)); // ldmxcsr [rsp]
                                                      // 恢复旧掩码作为返回值
                self.emit_insn(&Insn::raw_bytes(&[0x8B, 0x44, 0x24, 0x08])); // mov eax, [rsp+8]
                self.emit_insn(&Insn::add_rsp_imm(0x10)); // add rsp, 16
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            // fegetexcept(): GNU extension 返回当前启用的异常掩码
            "fegetexcept" => {
                self.emit_insn(&Insn::sub_rsp_imm(8)); // sub rsp, 8
                self.emit_insn(&Insn::mxcsr_mem(true, 4, 0)); // stmxcsr [rsp]
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(4),
                            index: None,
                            seg: None,
                        }
                    ),
                },
                &[],
            )); // mov eax, [rsp]
                self.emit_insn(&Insn::raw_bytes(&[0xC1, 0xE8, 0x07])); // shr eax, 7
                self.emit_insn(&Insn::raw_bytes(&[0x25, 0x3F, 0x00, 0x00, 0x00])); // and eax, 0x3F
                self.emit_insn(&Insn::add_rsp_imm(8)); // add rsp, 8
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            _ => false,
        }
    }
    pub(crate) fn gen_unknown_builtin_fallback(
        &mut self,
        name: &str,
        ty: &Type,
        dst: u8,
        sret_off: Option<i64>,
    ) {
        // Unknown __builtin_* are hard errors (E0018), not warnings.
        // This prevents silent zero/ud2 fallback that could cause
        // subtle runtime bugs in kernel code.
        let msg = format!("unsupported __builtin_ function '{}'", name);
        let span = Span::point(&self.source_name, 0, 1);
        let mut diag = Diagnostic::err("E0018", msg, span);
        diag.hint = Some(
            "implement this builtin in gen_builtin() \
             or add it to the known-builtin list"
                .to_string(),
        );
        self.diagnostics.push(diag);
        // Still emit fallback code so codegen can continue
        // for diagnostics of other functions.
        if matches!(ty, Type::Void) {
            self.emit_insn(&Insn::raw_bytes(&[0x0F, 0x0B])); // ud2
            return;
        }
        self.emit_zero_value(ty, dst, sret_off);
    }
    pub(crate) fn gen_unknown_builtin_stmt(&mut self, e: &Expr) -> bool {
        let Expr::Call {
            callee, args, ty, ..
        } = e
        else {
            return false;
        };
        let Expr::Ident { name, .. } = callee.as_ref() else {
            return false;
        };
        if !name.name.starts_with("__builtin_") {
            return false;
        }
        if self.gen_builtin(&name.name, args, 0) {
            return true;
        }
        // Linux FORTIFY: __builtin___memcpy_chk 等 FORTIFY 宏族展开后
        // 带有 __builtin_ 前缀, 但实际应链接到 libc 的 __*_chk 符号.
        // 语句上下文中也需剥离前缀后按普通函数调用处理.
        if Self::is_fortify_builtin(&name.name) {
            let real_name = &name.name["__builtin_".len()..];
            // 按 SysV ABI 发射函数调用: 参数已按约定准备, 直接 call
            self.call_label(real_name);
            return true;
        }
        self.gen_unknown_builtin_fallback(&name.name, ty, 0, None);
        true
    }
    pub(crate) fn is_fortify_builtin(name: &str) -> bool {
        name.starts_with("__builtin___") && name.ends_with("_chk")
    }
}
