//! asm/enc_ctrl.rs —— 自历史完整实现（git 28a9063 单文件 Codegen）按职责簇恢复的模块。
//! 仅移动/可见性调整，不改语义（backend_refactor_plan §3.1 原则 1）。
#![allow(clippy::all)]
#![allow(unused_imports)]
use crate::ast::*;
use crate::codegen::shared::elf as shared_elf;
use crate::codegen::shared::elf::*;
use crate::codegen::shared::*;
use crate::codegen::x86_64::asm::*;
use crate::codegen::x86_64::insn::Insn;
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
    pub(crate) fn emit_asm_call(&mut self, op: &AsmOperand) {
        match op {
            AsmOperand::Sym(label) => {
                self.call_label(label);
            },
            AsmOperand::SymDisp(label, _off) => {
                // call *sym@PLT: lea rax, [rip+label]; call *rax
                self.emit_insn(&Insn::raw(&[0x48, 0x8D, 0x05, 0, 0, 0, 0]));
                self.asm_rel32_symbol(label, 7);
                self.emit_insn(&Insn::raw(&[0xFF, 0xD0])); // call *%rax
            },
            AsmOperand::Reg(r, _) => {
                if *r >= 8 {
                    self.emit_insn(&Insn::raw(&[0x41]));
                }
                self.emit_insn(&Insn::raw(&[0xFF, 0xD0 | (r & 7)]));
            },
            AsmOperand::Mem {
                disp,
                base,
                index,
                scale,
            } => {
                self.emit_insn(&Insn::raw(&[Self::rex(false, 0, index.unwrap_or(0), *base)]));
                self.emit_asm_modrm_mem(0xFF, 2, *disp, *base, *index, *scale, false);
            },
            AsmOperand::SegMem {
                seg,
                disp,
                base,
                index,
                scale,
            } => {
                let (_code, pfx) = Self::seg_meta(*seg);
                self.emit_insn(&Insn::raw(&[pfx]));
                self.emit_insn(&Insn::raw(&[Self::rex(false, 0, index.unwrap_or(0), *base)]));
                self.emit_asm_modrm_mem(0xFF, 2, *disp, *base, *index, *scale, false);
            },
            AsmOperand::Imm(v) => {
                // 直接绝对 call: mov rax, imm; call *rax
                self.emit_insn(&Insn::raw(&[0x48, 0xB8]));
                self.emit_insn(&Insn::raw(&v.to_le_bytes()));
                self.emit_insn(&Insn::raw(&[0xFF, 0xD0]));
            },
            _ => {
                self.asm_reject("unsupported call operand pattern");
            },
        }
    }
    pub(crate) fn emit_asm_far_indirect(&mut self, op: &AsmOperand, is_call: bool, bits: u32) {
        let ext = if is_call { 3 } else { 5 };
        match op {
            AsmOperand::Mem {
                disp,
                base,
                index,
                scale,
            } => {
                let _ = bits;
                self.emit_insn(&Insn::raw(&[Self::rex(false, 0, index.unwrap_or(0), *base), 0xFF]));
                self.emit_asm_modrm_mem_only(ext, *disp, *base, *index, *scale);
            },
            _ => {
                if is_call {
                    self.asm_reject("lcall needs memory operand");
                } else {
                    self.asm_reject("ljmp needs memory operand");
                }
                self.emit_insn(&Insn::raw(&[0x90]));
            },
        }
    }
    pub(crate) fn emit_asm_in(&mut self, port: &AsmOperand, val: &AsmOperand, bits: u8) {
        match port {
            AsmOperand::Imm(p) if *p >= 0 && *p <= 0xFFFF => {
                if bits == 8 {
                    self.emit_insn(&Insn::raw(&[0xE4, *p as u8]));
                } else if bits == 16 {
                    self.emit_insn(&Insn::raw(&[0x66, 0xE5, *p as u8]));
                } else {
                    self.emit_insn(&Insn::raw(&[0xE5, *p as u8]));
                }
            },
            AsmOperand::Reg(2, _) => {
                // dx
                if bits == 8 {
                    self.emit_insn(&Insn::raw(&[0xEC]));
                } else if bits == 16 {
                    self.emit_insn(&Insn::raw(&[0x66, 0xED]));
                } else {
                    self.emit_insn(&Insn::raw(&[0xED]));
                }
            },
            AsmOperand::Mem {
                disp,
                base,
                index,
                scale,
            } => {
                // 内存间接端口: movzwl mem, %edx + in DX
                self.emit_insn(&Insn::raw(&[Self::rex(false, 0, index.unwrap_or(0), *base), 0x0F, 0xB7]));
                self.emit_asm_modrm_mem_only(2, *disp, *base, *index, *scale);
                if bits == 8 {
                    self.emit_insn(&Insn::raw(&[0xEC]));
                } else if bits == 16 {
                    self.emit_insn(&Insn::raw(&[0x66, 0xED]));
                } else {
                    self.emit_insn(&Insn::raw(&[0xED]));
                }
            },
            AsmOperand::SymDisp(sym, _) => {
                // 符号内存间接端口: movzwl sym(%rip), %edx + in DX
                self.emit_insn(&Insn::raw(&[0x0F, 0xB7, 0x15, 0, 0, 0, 0]));
                let pos = self.pos() - 4;
                self.pending_data_rels.push((pos, sym.clone()));
                if bits == 8 {
                    self.emit_insn(&Insn::raw(&[0xEC]));
                } else if bits == 16 {
                    self.emit_insn(&Insn::raw(&[0x66, 0xED]));
                } else {
                    self.emit_insn(&Insn::raw(&[0xED]));
                }
            },
            _ => {
                self.asm_reject("unsupported in port operand");
            },
        }
        let _ = val;
    }
    pub(crate) fn emit_asm_in_bare(&mut self, port: &AsmOperand, val: &AsmOperand) {
        self.emit_asm_in(port, val, 32);
    }
    pub(crate) fn emit_asm_int(&mut self, op: &AsmOperand) {
        if let AsmOperand::Imm(v) = op {
            self.emit_insn(&Insn::raw(&[0xCD, *v as u8]));
        } else {
            self.asm_reject("unsupported int operand pattern");
        }
    }
    pub(crate) fn emit_asm_jcc(&mut self, op: &AsmOperand, cc: u8) {
        match op {
            AsmOperand::Sym(label) => {
                // 发射条件跳转: 0F 8x disp32 (6 bytes)
                let at = self.pos();
                self.emit_insn(&Insn::raw(&[0x0F, cc, 0, 0, 0, 0]));
                // 映射 AT&T cc 字节到 JumpKind
                let kind = match cc {
                    0x84 => JumpKind::Je,
                    0x85 => JumpKind::Jne,
                    0x8C => JumpKind::Jl,
                    0x8E => JumpKind::Jle,
                    0x8F => JumpKind::Jg,
                    0x8D => JumpKind::Jge,
                    _ => JumpKind::Jne, // fallback
                };
                self.pending.push(PendingJump {
                    at,
                    target: label.clone(),
                    kind,
                    disp_offset: at + 2,
                    next_ip: at + 6,
                    is_push_imm: false,
                });
            },
            AsmOperand::Imm(v) => {
                // 短/近条件跳转到绝对地址: 0F 8x disp32
                let at = self.pos();
                self.emit_insn(&Insn::raw(&[0x0F, cc]));
                self.emit_insn(&Insn::raw(&(*v as i32).to_le_bytes()));
                let _ = at;
            },
            _ => {
                self.asm_reject("unsupported jcc operand pattern");
            },
        }
    }
    pub(crate) fn emit_asm_jmp(&mut self, op: &AsmOperand) {
        match op {
            AsmOperand::Sym(label) => {
                self.emit_insn(&Insn::raw(&[0xE9, 0, 0, 0, 0]));
                self.asm_rel32_symbol(label, 5);
            },
            AsmOperand::SymDisp(label, _off) => {
                // jmp *sym@PLT 或 jmp *sym: 走 RIP-relative 间接
                // 模拟方式: lea rax, [rip+label]; jmp *rax
                self.emit_insn(&Insn::raw(&[0x48, 0x8D, 0x05, 0, 0, 0, 0]));
                self.asm_rel32_symbol(label, 7);
                self.emit_insn(&Insn::raw(&[0xFF, 0xE0])); // jmp *%rax
            },
            AsmOperand::Reg(r, _) => {
                // jmp reg: FF /4
                if *r >= 8 {
                    self.emit_insn(&Insn::raw(&[0x41]));
                }
                self.emit_insn(&Insn::raw(&[0xFF, 0xE0 | (r & 7)]));
            },
            AsmOperand::Mem {
                disp,
                base,
                index,
                scale,
            } => {
                // jmp [mem]: FF /4
                self.emit_insn(&Insn::raw(&[Self::rex(false, 0, index.unwrap_or(0), *base)]));
                self.emit_asm_modrm_mem(0xFF, 4, *disp, *base, *index, *scale, false);
            },
            AsmOperand::SegMem {
                seg,
                disp,
                base,
                index,
                scale,
            } => {
                let (_code, pfx) = Self::seg_meta(*seg);
                self.emit_insn(&Insn::raw(&[pfx]));
                self.emit_insn(&Insn::raw(&[Self::rex(false, 0, index.unwrap_or(0), *base)]));
                self.emit_asm_modrm_mem(0xFF, 4, *disp, *base, *index, *scale, false);
            },
            AsmOperand::Imm(v) => {
                // 绝对远跳转: mov rax, imm; jmp *rax
                if *v >= i32::MIN as i64 && *v <= i32::MAX as i64 {
                    self.emit_insn(&Insn::raw(&[0xE9]));
                    self.emit_insn(&Insn::raw(&(*v as i32).to_le_bytes()));
                } else {
                    self.emit_insn(&Insn::raw(&[0x48, 0xB8]));
                    self.emit_insn(&Insn::raw(&v.to_le_bytes()));
                    self.emit_insn(&Insn::raw(&[0xFF, 0xE0]));
                }
            },
            _ => {
                self.asm_reject("unsupported jmp operand pattern");
            },
        }
    }
    pub(crate) fn emit_asm_out(&mut self, val: &AsmOperand, port: &AsmOperand, bits: u8) {
        match port {
            AsmOperand::Imm(p) if *p >= 0 && *p <= 0xFFFF => {
                if bits == 8 {
                    self.emit_insn(&Insn::raw(&[0xE6, *p as u8]));
                } else if bits == 16 {
                    self.emit_insn(&Insn::raw(&[0x66, 0xE7, *p as u8]));
                } else {
                    self.emit_insn(&Insn::raw(&[0xE7, *p as u8]));
                }
            },
            AsmOperand::Reg(2, _) => {
                // dx
                if bits == 8 {
                    self.emit_insn(&Insn::raw(&[0xEE]));
                } else if bits == 16 {
                    self.emit_insn(&Insn::raw(&[0x66, 0xEF]));
                } else {
                    self.emit_insn(&Insn::raw(&[0xEF]));
                }
            },
            AsmOperand::Mem {
                disp,
                base,
                index,
                scale,
            } => {
                // 内存间接端口: movzwl mem, %edx + out DX
                self.emit_insn(&Insn::raw(&[Self::rex(false, 0, index.unwrap_or(0), *base), 0x0F, 0xB7]));
                self.emit_asm_modrm_mem_only(2, *disp, *base, *index, *scale);
                if bits == 8 {
                    self.emit_insn(&Insn::raw(&[0xEE]));
                } else if bits == 16 {
                    self.emit_insn(&Insn::raw(&[0x66, 0xEF]));
                } else {
                    self.emit_insn(&Insn::raw(&[0xEF]));
                }
            },
            AsmOperand::SymDisp(sym, _) => {
                // 符号内存间接端口: movzwl sym(%rip), %edx + out DX
                self.emit_insn(&Insn::raw(&[0x0F, 0xB7, 0x15, 0, 0, 0, 0]));
                let pos = self.pos() - 4;
                self.pending_data_rels.push((pos, sym.clone()));
                if bits == 8 {
                    self.emit_insn(&Insn::raw(&[0xEE]));
                } else if bits == 16 {
                    self.emit_insn(&Insn::raw(&[0x66, 0xEF]));
                } else {
                    self.emit_insn(&Insn::raw(&[0xEF]));
                }
            },
            _ => {
                self.asm_reject("unsupported out port operand");
            },
        }
        let _ = val;
    }
    pub(crate) fn emit_asm_out_bare(&mut self, val: &AsmOperand, port: &AsmOperand) {
        self.emit_asm_out(val, port, 32);
    }
    pub(crate) fn jmp_external(&mut self, l: &str) {
        let at = self.pos();
        self.emit_insn(&Insn::raw(&[0xE9, 0, 0, 0, 0]));
        self.disasm_calls.insert(at + 1, l.to_string());
        if self.relocatable {
            self.pending_func_rels.push((at + 1, l.to_string()));
        } else {
            self.pending.push(PendingJump {
                at,
                target: l.into(),
                kind: JumpKind::Jmp,
                disp_offset: at + 1,
                next_ip: at + 5,
                is_push_imm: false,
            });
        }
    }
}
