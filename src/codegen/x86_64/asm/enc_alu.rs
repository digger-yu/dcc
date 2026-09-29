//! asm/enc_alu.rs —— 自历史完整实现（git 28a9063 单文件 Codegen）按职责簇恢复的模块。
//! 仅移动/可见性调整，不改语义（backend_refactor_plan §3.1 原则 1）。
#![allow(clippy::all)]
#![allow(unused_imports)]
use crate::ast::*;
use crate::codegen::shared::elf as shared_elf;
use crate::codegen::shared::elf::*;
use crate::codegen::shared::*;
use crate::codegen::x86_64::asm::*;
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
    pub(crate) fn emit_asm_alu(&mut self, src: &AsmOperand, dst: &AsmOperand, op: u8, bits: u8) {
        let w = bits == 64;
        if Self::asm_operands_have_non_gpr_regs(&[src, dst]) {
            self.emit_unsupported_asm_operands("ALU");
            return;
        }
        // 段前缀 mem: emit 段前缀 + 普通 mem ALU
        if let AsmOperand::SegMem {
            seg,
            disp,
            base,
            index,
            scale,
        } = src
        {
            let (_code, pfx) = Self::seg_meta(*seg);
            self.emit(&[pfx]);
            if let AsmOperand::Reg(d, _) = dst {
                if bits == 16 {
                    self.emit(&[0x66]);
                }
                self.emit(&[Self::rex(w, *d, index.unwrap_or(0), *base), 0x03 + op * 8]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            }
            return;
        }
        if let AsmOperand::SegMem {
            seg,
            disp,
            base,
            index,
            scale,
        } = dst
        {
            let (_code, pfx) = Self::seg_meta(*seg);
            self.emit(&[pfx]);
            if let AsmOperand::Reg(s, _) = src {
                if bits == 16 {
                    self.emit(&[0x66]);
                }
                self.emit(&[Self::rex(w, *s, index.unwrap_or(0), *base), 0x01 + op * 8]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            }
            return;
        }
        match (src, dst) {
            (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) => {
                if bits == 8 {
                    if *s >= 4 || *d >= 4 {
                        self.emit(&[Self::rex(false, *s, 0, *d)]);
                    }
                    self.emit(&[(op * 8), Self::modrm_reg_reg(*s, *d)]);
                } else {
                    if bits == 16 {
                        self.emit(&[0x66]);
                    }
                    self.emit(&[
                        Self::rex(w, *s, 0, *d),
                        0x01 + op * 8,
                        Self::modrm_reg_reg(*s, *d),
                    ]);
                }
            },
            (AsmOperand::Imm(v), AsmOperand::Reg(d, _)) => {
                if bits == 16 {
                    self.emit(&[0x66]);
                }
                if bits != 8 {
                    self.emit(&[Self::rex(w, 0, 0, *d)]);
                }
                if bits == 8 && *v >= 0 && *v <= 255 {
                    // 8 位 ALU: 80 /0 ib, 接受 0..255 的字节值
                    self.emit(&[0x80 + op * 8, Self::modrm_reg_reg(op, *d), *v as u8]);
                } else if *v >= -128 && *v <= 127 {
                    // 8/16 位: 80 /0 ib
                    if bits == 8 {
                        self.emit(&[0x80 + op * 8, Self::modrm_reg_reg(op, *d), *v as u8]);
                    } else {
                        self.emit(&[0x83, Self::modrm_reg_reg(op, *d), *v as u8]);
                    }
                } else if bits == 8 {
                    // 8 位但 imm 越界: 报错
                    self.asm_reject(&format!("8-bit ALU immediate out of range ({})", v));
                    self.emit(&[0x90]);
                } else if *v >= i32::MIN as i64 && *v <= i32::MAX as i64 {
                    self.emit(&[0x81, Self::modrm_reg_reg(op, *d)]);
                    self.emit(&(*v as i32).to_le_bytes());
                } else {
                    // 64 位立即数: movabs rax, imm; op rax, rax (无 op reg, reg 模式用 addq)
                    self.emit(&[0x48, 0xB8]);
                    self.emit(&v.to_le_bytes());
                    if *d != 0 {
                        // mov d, rax
                        if bits == 64 {
                            self.emit(&[
                                Self::rex(true, 0, 0, *d),
                                0x89,
                                Self::modrm_reg_reg(0, *d),
                            ]);
                        }
                        // 现在 rax 是 imm; dst 是 d: op d, rax
                        self.emit(&[
                            Self::rex(w, *d, 0, 0),
                            0x01 + op * 8,
                            Self::modrm_reg_reg(*d, 0),
                        ]);
                    } else {
                        // op rax, rax
                        self.emit(&[
                            Self::rex(w, 0, 0, 0),
                            0x01 + op * 8,
                            Self::modrm_reg_reg(0, 0),
                        ]);
                    }
                }
            },
            (
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
                AsmOperand::Reg(d, _),
            ) => {
                if bits == 16 {
                    self.emit(&[0x66]);
                }
                self.emit(&[Self::rex(w, *d, index.unwrap_or(0), *base)]);
                self.emit(&[0x03 + op * 8]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            },
            (
                AsmOperand::Reg(s, _),
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
            ) => {
                if bits == 16 {
                    self.emit(&[0x66]);
                }
                self.emit(&[Self::rex(w, *s, index.unwrap_or(0), *base)]);
                self.emit(&[0x01 + op * 8]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            },
            (
                AsmOperand::Imm(v),
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
            ) => {
                if bits == 16 {
                    self.emit(&[0x66]);
                }
                self.emit(&[Self::rex(w, 0, index.unwrap_or(0), *base)]);
                if *v >= -128 && *v <= 127 && op == 7 {
                    self.emit(&[0x83]);
                    self.emit_asm_modrm_mem_only(7, *disp, *base, *index, *scale);
                    self.emit(&[*v as u8]);
                } else if bits == 8 {
                    self.emit(&[0x80 + op * 8]);
                    self.emit_asm_modrm_mem_only(op, *disp, *base, *index, *scale);
                    self.emit(&[*v as u8]);
                } else {
                    self.emit(&[0x81]);
                    self.emit_asm_modrm_mem_only(op, *disp, *base, *index, *scale);
                    self.emit(&(*v as i32).to_le_bytes());
                }
            },
            _ => {
                // SymDisp (symbolic RIP-relative) handling
                if let (AsmOperand::SymDisp(sym, _), AsmOperand::Reg(d, _)) = (src, dst) {
                    if bits == 16 {
                        self.emit(&[0x66]);
                    }
                    self.emit(&[Self::rex(w, *d, 0, 0), 0x03 + op * 8]);
                    self.emit_asm_modrm_mem_only(*d, 0, ASM_BASE_RIP, None, 1);
                    let pos = self.pos() - 4;
                    self.pending_data_rels.push((pos, sym.clone()));
                } else if let (AsmOperand::Reg(s, _), AsmOperand::SymDisp(sym, _)) = (src, dst) {
                    if bits == 16 {
                        self.emit(&[0x66]);
                    }
                    self.emit(&[Self::rex(w, *s, 0, 0), 0x01 + op * 8]);
                    self.emit_asm_modrm_mem_only(*s, 0, ASM_BASE_RIP, None, 1);
                    let pos = self.pos() - 4;
                    self.pending_data_rels.push((pos, sym.clone()));
                } else if let (AsmOperand::Imm(v), AsmOperand::SymDisp(sym, _)) = (src, dst) {
                    if bits == 16 {
                        self.emit(&[0x66]);
                    }
                    self.emit(&[Self::rex(w, 0, 0, 0)]);
                    if *v >= -128 && *v <= 127 {
                        self.emit(&[0x83]);
                        self.emit_asm_modrm_mem_only(op, 0, ASM_BASE_RIP, None, 1);
                        self.emit(&[*v as u8]);
                    } else {
                        self.emit(&[0x81]);
                        self.emit_asm_modrm_mem_only(op, 0, ASM_BASE_RIP, None, 1);
                        self.emit(&(*v as i32).to_le_bytes());
                    }
                    let pos = self.pos() - 4 - if *v >= -128 && *v <= 127 { 1 } else { 4 };
                    self.pending_data_rels.push((pos, sym.clone()));
                } else {
                    self.asm_reject("unsupported ALU operand pattern");
                }
            },
        }
    }
    pub(crate) fn emit_asm_bit(
        &mut self,
        src: &AsmOperand,
        dst: &AsmOperand,
        opcode: u8,
        bits: u8,
    ) {
        let w = bits == 64;
        if bits == 16 {
            self.emit(&[0x66]);
        }
        match (src, dst) {
            (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) => {
                self.emit(&[
                    Self::rex(w, *s, 0, *d),
                    0x0F,
                    opcode,
                    Self::modrm_reg_reg(*s, *d),
                ]);
            },
            (AsmOperand::Imm(v), AsmOperand::Reg(d, _)) => {
                let ext = match opcode {
                    0xA3 => 4,
                    0xAB => 5,
                    0xB3 => 6,
                    0xBB => 7,
                    _ => 4,
                };
                self.emit(&[
                    Self::rex(w, 0, 0, *d),
                    0x0F,
                    0xBA,
                    Self::modrm_reg_reg(ext, *d),
                    *v as u8,
                ]);
            },
            (
                AsmOperand::Imm(v),
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
            ) => {
                let ext = match opcode {
                    0xA3 => 4,
                    0xAB => 5,
                    0xB3 => 6,
                    0xBB => 7,
                    _ => 4,
                };
                self.emit(&[Self::rex(w, 0, index.unwrap_or(0), *base), 0x0F, 0xBA]);
                self.emit_asm_modrm_mem_only(ext, *disp, *base, *index, *scale);
                self.emit(&[*v as u8]);
            },
            (
                AsmOperand::Reg(s, _),
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
            ) => {
                self.emit(&[Self::rex(w, *s, index.unwrap_or(0), *base), 0x0F, opcode]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            },
            // SegMem dst: 发射段前缀后走普通 mem 路径
            (
                AsmOperand::Imm(v),
                AsmOperand::SegMem {
                    seg,
                    disp,
                    base,
                    index,
                    scale,
                },
            ) => {
                let (_code, pfx) = Self::seg_meta(*seg);
                self.emit(&[pfx, Self::rex(w, 0, index.unwrap_or(0), *base), 0x0F, 0xBA]);
                let ext = match opcode {
                    0xA3 => 4,
                    0xAB => 5,
                    0xB3 => 6,
                    0xBB => 7,
                    _ => 4,
                };
                self.emit_asm_modrm_mem_only(ext, *disp, *base, *index, *scale);
                self.emit(&[*v as u8]);
            },
            (
                AsmOperand::Reg(s, _),
                AsmOperand::SegMem {
                    seg,
                    disp,
                    base,
                    index,
                    scale,
                },
            ) => {
                let (_code, pfx) = Self::seg_meta(*seg);
                self.emit(&[
                    pfx,
                    Self::rex(w, *s, index.unwrap_or(0), *base),
                    0x0F,
                    opcode,
                ]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            },
            _ => {
                self.asm_reject("unsupported bit-test operand pattern");
            },
        }
    }
    pub(crate) fn emit_asm_bswap(&mut self, op: &AsmOperand, bits: u8) {
        if let AsmOperand::Reg(r, _) = op {
            if bits == 64 {
                if *r >= 8 {
                    self.emit(&[0x49]);
                } else {
                    self.emit(&[0x48]);
                }
                self.emit(&[0x0F, 0xC8 + (r & 7)]);
            } else if bits == 32 {
                if *r >= 8 {
                    self.emit(&[0x41]);
                }
                self.emit(&[0x0F, 0xC8 + (r & 7)]);
            } else if bits == 16 {
                self.asm_reject("bswap on 16-bit register not supported");
            } else {
                if *r >= 8 {
                    self.emit(&[0x41]);
                }
                self.emit(&[0x0F, 0xC8 + (r & 7)]);
            }
        } else {
            self.asm_reject("unsupported bswap operand pattern");
        }
    }
    pub(crate) fn emit_asm_cmov(&mut self, src: &AsmOperand, dst: &AsmOperand, cc: u8, bits: u8) {
        let w = bits == 64;
        if bits == 16 {
            self.emit(&[0x66]);
        }
        match (src, dst) {
            (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) => {
                self.emit(&[
                    Self::rex(w, *d, 0, *s),
                    0x0F,
                    cc,
                    Self::modrm_reg_reg(*d, *s),
                ]);
            },
            (
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
                AsmOperand::Reg(d, _),
            ) => {
                self.emit(&[Self::rex(w, *d, index.unwrap_or(0), *base), 0x0F, cc]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            },
            (
                AsmOperand::Reg(s, _),
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
            ) => {
                self.emit(&[Self::rex(w, *s, index.unwrap_or(0), *base), 0x0F, cc]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            },
            _ => {
                self.asm_reject("unsupported cmov operand pattern");
            },
        }
    }
    pub(crate) fn emit_asm_cmpxchg(&mut self, src: &AsmOperand, dst: &AsmOperand, bits: u8) {
        let w = bits == 64;
        // cmpxchg 编码: 8位 0F B0, 16位 66 0F B1, 32/64位 0F B1
        let opcode = if bits == 8 { 0xB0 } else { 0xB1 };
        // 段前缀 mem
        if let AsmOperand::SegMem {
            seg,
            disp,
            base,
            index,
            scale,
        } = dst
        {
            let (_code, pfx) = Self::seg_meta(*seg);
            self.emit(&[pfx]);
            if let AsmOperand::Reg(s, _) = src {
                if bits == 16 {
                    self.emit(&[0x66]);
                }
                self.emit(&[Self::rex(w, *s, index.unwrap_or(0), *base), 0x0F, opcode]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            }
            return;
        }
        match (src, dst) {
            (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) => {
                self.emit(&[
                    Self::rex(w, *s, 0, *d),
                    0x0F,
                    opcode,
                    Self::modrm_reg_reg(*s, *d),
                ]);
            },
            (
                AsmOperand::Reg(s, _),
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
            ) => {
                if bits == 16 {
                    self.emit(&[0x66]);
                }
                self.emit(&[Self::rex(w, *s, index.unwrap_or(0), *base), 0x0F, opcode]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            },
            (
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
                AsmOperand::Reg(d, _),
            ) => {
                if bits == 16 {
                    self.emit(&[0x66]);
                }
                self.emit(&[Self::rex(w, *d, index.unwrap_or(0), *base), 0x0F, opcode]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            },
            (AsmOperand::Reg(s, _), AsmOperand::SymDisp(sym, disp)) => {
                // cmpxchg reg, sym(%rip): 0F B1 /r with RIP-relative memory
                self.emit(&[Self::rex(w, *s, 0, 0), 0x0F, opcode]);
                self.emit_asm_modrm_mem_only(*s, *disp, ASM_BASE_RIP, None, 1);
                let pos = self.pos() - 4;
                self.pending_code_rels.push(PendingCodeRel {
                    offset: pos,
                    symbol: sym.clone(),
                    rel_type: R_X86_64_PC32,
                    addend: -4,
                });
            },
            _ => {
                self.asm_reject("unsupported cmpxchg operand pattern");
            },
        }
    }
    pub(crate) fn emit_asm_cmpxchg16b(&mut self, op: &AsmOperand) {
        match op {
            AsmOperand::Mem {
                disp,
                base,
                index,
                scale,
            } => {
                self.emit(&[Self::rex(false, 0, index.unwrap_or(0), *base), 0x0F, 0xC7]);
                self.emit_asm_modrm_mem_only(1, *disp, *base, *index, *scale);
            },
            AsmOperand::SegMem {
                seg,
                disp,
                base,
                index,
                scale,
            } => {
                let (_code, pfx) = Self::seg_meta(*seg);
                self.emit(&[
                    pfx,
                    Self::rex(false, 0, index.unwrap_or(0), *base),
                    0x0F,
                    0xC7,
                ]);
                self.emit_asm_modrm_mem_only(1, *disp, *base, *index, *scale);
            },
            _ => {
                self.asm_reject("cmpxchg16b requires a memory operand");
            },
        }
    }
    pub(crate) fn emit_asm_cmpxchg8b(&mut self, op: &AsmOperand) {
        match op {
            AsmOperand::Mem {
                disp,
                base,
                index,
                scale,
            } => {
                let rex = Self::rex(false, 0, index.unwrap_or(0), *base);
                if rex != 0x40 {
                    self.emit(&[rex]);
                }
                self.emit(&[0x0F, 0xC7]);
                self.emit_asm_modrm_mem_only(1, *disp, *base, *index, *scale);
            },
            AsmOperand::SegMem {
                seg,
                disp,
                base,
                index,
                scale,
            } => {
                let (_code, pfx) = Self::seg_meta(*seg);
                let rex = Self::rex(false, 0, index.unwrap_or(0), *base);
                self.emit(&[pfx]);
                if rex != 0x40 {
                    self.emit(&[rex]);
                }
                self.emit(&[0x0F, 0xC7]);
                self.emit_asm_modrm_mem_only(1, *disp, *base, *index, *scale);
            },
            AsmOperand::SymDisp(sym, _) => {
                // RIP-relative: 0F C7 /1 mod=00 r/m=101 + disp32
                self.emit(&[0x0F, 0xC7, 0x0D, 0, 0, 0, 0]);
                let pos = self.pos() - 4;
                self.pending_data_rels.push((pos, sym.clone()));
            },
            _ => {
                self.asm_reject("cmpxchg8b requires a memory operand");
            },
        }
    }
    pub(crate) fn emit_asm_imul2(&mut self, src: &AsmOperand, dst: &AsmOperand) {
        match (src, dst) {
            (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) => {
                self.emit(&[
                    Self::rex(true, *d, 0, *s),
                    0x0F,
                    0xAF,
                    Self::modrm_reg_reg(*d, *s),
                ]);
            },
            (
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
                AsmOperand::Reg(d, _),
            ) => {
                self.emit(&[Self::rex(true, *d, index.unwrap_or(0), *base), 0x0F, 0xAF]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            },
            (
                AsmOperand::Reg(s, _),
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
            ) => {
                self.emit(&[Self::rex(true, *s, index.unwrap_or(0), *base), 0x0F, 0xAF]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            },
            (AsmOperand::SymDisp(sym, _), AsmOperand::Reg(d, _)) => {
                // imul src=*(sym(%rip)), dst=%reg: 0F AF /r
                let rex = Self::rex(true, *d, 0, 0);
                let modrm = 0x05 | ((d & 7) << 3);
                self.emit(&[rex, 0x0F, 0xAF, modrm, 0, 0, 0, 0]);
                let pos = self.pos() - 4;
                self.pending_data_rels.push((pos, sym.clone()));
            },
            (AsmOperand::Reg(s, _), AsmOperand::SymDisp(sym, _)) => {
                // imul src=%reg, dst=sym(%rip): 0F AF /r — %reg is modrm.reg, sym is modrm.r/m
                let rex = Self::rex(true, *s, 0, 0);
                let modrm = 0x05 | ((s & 7) << 3);
                self.emit(&[rex, 0x0F, 0xAF, modrm, 0, 0, 0, 0]);
                let pos = self.pos() - 4;
                self.pending_data_rels.push((pos, sym.clone()));
            },
            _ => {
                self.asm_reject("unsupported imul2 operand pattern");
            },
        }
    }
    pub(crate) fn emit_asm_imul3(
        &mut self,
        src1: &AsmOperand,
        src2: &AsmOperand,
        dst: &AsmOperand,
    ) {
        // AT&T: imul $imm, src, dst  =>  src1=$imm, src2=src(reg/mem), dst=reg
        if let (AsmOperand::Imm(v), AsmOperand::Reg(d, _)) = (src1, dst) {
            match src2 {
                AsmOperand::Reg(s, _) => {
                    if *v >= -128 && *v <= 127 {
                        self.emit(&[
                            Self::rex(true, *d, 0, *s),
                            0x6B,
                            Self::modrm_reg_reg(*d, *s),
                            *v as u8,
                        ]);
                    } else {
                        self.emit(&[
                            Self::rex(true, *d, 0, *s),
                            0x69,
                            Self::modrm_reg_reg(*d, *s),
                        ]);
                        self.emit(&(*v as i32).to_le_bytes());
                    }
                },
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                } => {
                    if *v >= -128 && *v <= 127 {
                        self.emit(&[Self::rex(true, *d, index.unwrap_or(0), *base), 0x6B]);
                        self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
                        self.emit(&[*v as u8]);
                    } else {
                        self.emit(&[Self::rex(true, *d, index.unwrap_or(0), *base), 0x69]);
                        self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
                        self.emit(&(*v as i32).to_le_bytes());
                    }
                },
                AsmOperand::SymDisp(sym, _) => {
                    // imul $imm, sym(%rip), %reg: 6B /r ib / 69 /r id
                    let rex = Self::rex(true, *d, 0, 0);
                    let modrm = 0x05 | ((d & 7) << 3);
                    if *v >= -128 && *v <= 127 {
                        self.emit(&[rex, 0x6B, modrm, 0, 0, 0, 0, *v as u8]);
                        let pos = self.pos() - 5;
                        self.pending_data_rels.push((pos, sym.clone()));
                    } else {
                        self.emit(&[rex, 0x69, modrm, 0, 0, 0, 0]);
                        let pos = self.pos() - 4;
                        self.pending_data_rels.push((pos, sym.clone()));
                        self.emit(&(*v as i32).to_le_bytes());
                    }
                },
                _ => {
                    self.asm_reject("unsupported imul3 src operand pattern");
                },
            }
        } else {
            self.asm_reject("unsupported imul3 operand pattern");
        }
    }
    pub(crate) fn emit_asm_popcnt(&mut self, src: &AsmOperand, dst: &AsmOperand, bits: u8) {
        let w = bits == 64;
        if let AsmOperand::SegMem {
            seg,
            disp,
            base,
            index,
            scale,
        } = src
        {
            let (_code, pfx) = Self::seg_meta(*seg);
            self.emit(&[pfx]);
            if let AsmOperand::Reg(d, _) = dst {
                if bits == 16 {
                    self.emit(&[0x66]);
                }
                self.emit(&[0xF3]);
                self.emit(&[Self::rex(w, *d, index.unwrap_or(0), *base), 0x0F, 0xB8]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            }
            return;
        }
        match (src, dst) {
            (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) => {
                let r = Self::rex(w, *d, 0, *s);
                self.emit(&[0xF3]);
                if r != 0x40 {
                    self.emit(&[r]);
                }
                self.emit(&[0x0F, 0xB8, Self::modrm_reg_reg(*d, *s)]);
            },
            (
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
                AsmOperand::Reg(d, _),
            ) => {
                if bits == 16 {
                    self.emit(&[0x66]);
                }
                let r = Self::rex(w, *d, index.unwrap_or(0), *base);
                self.emit(&[0xF3]);
                if r != 0x40 {
                    self.emit(&[r]);
                }
                self.emit(&[0x0F, 0xB8]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            },
            _ => {
                self.asm_reject("unsupported popcnt operand pattern");
            },
        }
    }
    pub(crate) fn emit_asm_rotate(&mut self, src: &AsmOperand, dst: &AsmOperand, op: u8, bits: u8) {
        self.emit_asm_shift(src, dst, op, bits);
    }
    pub(crate) fn emit_asm_setcc(&mut self, op: &AsmOperand, cc: u8) {
        match op {
            AsmOperand::Reg(r, sz) => {
                // 8 位寄存器编码: ah/bh/ch/dh 是 4..7 物理槽, 但 r/m 字段也用 4..7
                // REX 无效编码 0x40 用于 ah/bh/ch/dh 旧语法; 用 REX 后 ah 不可达
                // 这里统一按 r/m 字段 0..7 处理
                if *r >= 4 && *r < 8 && *sz == 8 {
                    // 高 8 位寄存器: 不发 REX, 直接编码
                    self.emit(&[0x0F, cc, 0xC0 | (r & 7)]);
                } else {
                    if *r >= 8 {
                        self.emit(&[0x41]);
                    }
                    self.emit(&[0x0F, cc, 0xC0 | (r & 7)]);
                }
            },
            AsmOperand::Mem {
                disp,
                base,
                index,
                scale,
            } => {
                self.emit(&[Self::rex(false, 0, index.unwrap_or(0), *base), 0x0F, cc]);
                self.emit_asm_modrm_mem_only(0, *disp, *base, *index, *scale);
            },
            AsmOperand::SegMem {
                seg,
                disp,
                base,
                index,
                scale,
            } => {
                let (_code, pfx) = Self::seg_meta(*seg);
                self.emit(&[pfx]);
                self.emit(&[Self::rex(false, 0, index.unwrap_or(0), *base), 0x0F, cc]);
                self.emit_asm_modrm_mem_only(0, *disp, *base, *index, *scale);
            },
            _ => {
                self.asm_reject("unsupported setcc operand pattern");
            },
        }
    }
    pub(crate) fn emit_asm_shift(&mut self, src: &AsmOperand, dst: &AsmOperand, op: u8, bits: u8) {
        let w = bits == 64;
        // 段前缀 mem: 段前缀 + 普通 mem shift
        if let AsmOperand::SegMem {
            seg,
            disp,
            base,
            index,
            scale,
        } = dst
        {
            let (_code, pfx) = Self::seg_meta(*seg);
            self.emit(&[pfx]);
            match src {
                AsmOperand::Imm(v) => {
                    if bits == 16 {
                        self.emit(&[0x66]);
                    }
                    self.emit(&[Self::rex(w, 0, index.unwrap_or(0), *base)]);
                    if *v == 1 {
                        self.emit(&[0xD1]);
                        self.emit_asm_modrm_mem_only(op, *disp, *base, *index, *scale);
                    } else {
                        self.emit(&[0xC1]);
                        self.emit_asm_modrm_mem_only(op, *disp, *base, *index, *scale);
                        self.emit(&[*v as u8]);
                    }
                },
                AsmOperand::Reg(c, _) if *c == 1 => {
                    if bits == 16 {
                        self.emit(&[0x66]);
                    }
                    self.emit(&[Self::rex(w, 0, index.unwrap_or(0), *base), 0xD3]);
                    self.emit_asm_modrm_mem_only(op, *disp, *base, *index, *scale);
                },
                _ => {
                    self.asm_reject("shift SegMem: only $imm or %cl supported");
                },
            }
            return;
        }
        match (src, dst) {
            (AsmOperand::Imm(v), AsmOperand::Reg(d, _)) => {
                if bits == 16 {
                    self.emit(&[0x66]);
                }
                self.emit(&[Self::rex(w, 0, 0, *d)]);
                if *v == 1 {
                    self.emit(&[0xD1, Self::modrm_reg_reg(op, *d)]);
                } else {
                    self.emit(&[0xC1, Self::modrm_reg_reg(op, *d), *v as u8]);
                }
            },
            // 兼容: shift $cl 实际语义是 shl/sar/shr/r 任意 reg 作为 count 来源
            // 但 x86 仅支持 %cl; 这里扩展为任意 GP (mov 到 cl 后 D3 /4)
            (AsmOperand::Reg(c, _), AsmOperand::Reg(d, _)) => {
                if bits == 16 {
                    self.emit(&[0x66]);
                }
                if *c != 1 {
                    // 任意 reg: 先 mov cl, c 再 D3 /4
                    if *c >= 8 {
                        self.emit(&[0x49]);
                    } else {
                        self.emit(&[0x48]);
                    }
                    self.emit(&[0x89, Self::modrm_reg_reg(*c, 1)]);
                }
                self.emit(&[Self::rex(w, 0, 0, *d), 0xD3, Self::modrm_reg_reg(op, *d)]);
            },
            (
                AsmOperand::Imm(v),
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
            ) => {
                if bits == 16 {
                    self.emit(&[0x66]);
                }
                self.emit(&[Self::rex(w, 0, index.unwrap_or(0), *base)]);
                if *v == 1 {
                    self.emit(&[0xD1]);
                    self.emit_asm_modrm_mem_only(op, *disp, *base, *index, *scale);
                } else {
                    self.emit(&[0xC1]);
                    self.emit_asm_modrm_mem_only(op, *disp, *base, *index, *scale);
                    self.emit(&[*v as u8]);
                }
            },
            (
                AsmOperand::Reg(c, _),
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
            ) if *c == 1 => {
                if bits == 16 {
                    self.emit(&[0x66]);
                }
                self.emit(&[Self::rex(w, 0, index.unwrap_or(0), *base), 0xD3]);
                self.emit_asm_modrm_mem_only(op, *disp, *base, *index, *scale);
            },
            (
                AsmOperand::Reg(c, _),
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
            ) => {
                // 任意 reg: 先 mov cl, c 再 D3
                if bits == 16 {
                    self.emit(&[0x66]);
                }
                if *c >= 8 {
                    self.emit(&[0x49]);
                } else {
                    self.emit(&[0x48]);
                }
                self.emit(&[0x89, Self::modrm_reg_reg(*c, 1)]);
                self.emit(&[Self::rex(w, 0, index.unwrap_or(0), *base), 0xD3]);
                self.emit_asm_modrm_mem_only(op, *disp, *base, *index, *scale);
            },
            _ => {
                self.asm_reject("unsupported shift operand pattern");
            },
        }
    }
    pub(crate) fn emit_asm_shld_shrd(
        &mut self,
        src: &AsmOperand,
        dst: &AsmOperand,
        count: &AsmOperand,
        is_shld: bool,
        bits: u8,
    ) {
        let w = bits == 64;
        let op = if is_shld { 0xA5u8 } else { 0xADu8 };
        let op_imm = if is_shld { 0xA4u8 } else { 0xACu8 };
        if bits == 16 {
            self.emit(&[0x66]);
        }
        match (src, dst, count) {
            // shld %cl, %src, %dst: src=Reg(%cl), dst=Reg(src), count=Reg(dst)
            (AsmOperand::Reg(c, _), AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) if *c == 1 => {
                self.emit(&[
                    Self::rex(w, *s, 0, *d),
                    0x0F,
                    op,
                    Self::modrm_reg_reg(*s, *d),
                ]);
            },
            // shld $imm, %src, %dst: src=Imm(count), dst=Reg(src), count=Reg(dst)
            (AsmOperand::Imm(v), AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) => {
                self.emit(&[
                    Self::rex(w, *s, 0, *d),
                    0x0F,
                    op_imm,
                    Self::modrm_reg_reg(*s, *d),
                    *v as u8,
                ]);
            },
            // shldq $imm, %src, (%mem): src=Imm(count), dst=Reg(src), count=Mem(dst)
            (
                AsmOperand::Imm(v),
                AsmOperand::Reg(s, _),
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
            ) => {
                self.emit(&[Self::rex(w, *s, index.unwrap_or(0), *base), 0x0F, op_imm]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
                self.emit(&[*v as u8]);
            },
            // shldq %cl, %src, (%mem): src=Reg(%cl), dst=Reg(src), count=Mem(dst)
            (
                AsmOperand::Reg(c, _),
                AsmOperand::Reg(s, _),
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
            ) if *c == 1 => {
                self.emit(&[Self::rex(w, *s, index.unwrap_or(0), *base), 0x0F, op]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            },
            _ => {
                self.asm_reject("unsupported shld/shrd operand pattern");
            },
        }
    }
    pub(crate) fn emit_asm_test(&mut self, src: &AsmOperand, dst: &AsmOperand, bits: u8) {
        let w = bits == 64;
        // 段前缀 mem: 段前缀 + 普通 mem test
        if let AsmOperand::SegMem {
            seg,
            disp,
            base,
            index,
            scale,
        } = src
        {
            let (_code, pfx) = Self::seg_meta(*seg);
            self.emit(&[pfx]);
            if let AsmOperand::Reg(d, _) = dst {
                if bits == 16 {
                    self.emit(&[0x66]);
                }
                self.emit(&[Self::rex(w, *d, index.unwrap_or(0), *base), 0x85]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            }
            return;
        }
        if let AsmOperand::SegMem {
            seg,
            disp,
            base,
            index,
            scale,
        } = dst
        {
            let (_code, pfx) = Self::seg_meta(*seg);
            self.emit(&[pfx]);
            if let AsmOperand::Reg(s, _) = src {
                if bits == 16 {
                    self.emit(&[0x66]);
                }
                self.emit(&[Self::rex(w, *s, index.unwrap_or(0), *base), 0x85]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            }
            return;
        }
        match (src, dst) {
            (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) => {
                if bits == 16 {
                    self.emit(&[0x66]);
                }
                self.emit(&[Self::rex(w, *s, 0, *d), 0x85, Self::modrm_reg_reg(*s, *d)]);
            },
            (AsmOperand::Imm(v), AsmOperand::Reg(d, _)) => {
                if bits == 16 {
                    self.emit(&[0x66]);
                }
                self.emit(&[Self::rex(w, 0, 0, *d)]);
                if bits == 8 {
                    if *v >= 0 && *v <= 255 {
                        self.emit(&[0xF6, Self::modrm_reg_reg(0, *d), *v as u8]);
                    } else if *v >= -128 && *v <= -1 {
                        // 负值: 0xF6 /0 ib 接受 unsigned 8-bit, 负值截断为 u8
                        self.emit(&[0xF6, Self::modrm_reg_reg(0, *d), *v as u8]);
                    } else {
                        self.asm_reject(&format!("8-bit test immediate out of range ({})", v));
                        self.emit(&[0x90]);
                    }
                } else if *v >= -128 && *v <= 127 && (*d & 7) == 0 {
                    // test $imm8, %al/ax/eax/rax 优化形式: A8 ib / 66 A9 iw / A9 id / 48 A9 id
                    let opcode = if bits == 16 { 0x66A9u16 } else { 0xA9u16 };
                    if bits == 16 {
                        self.emit(&[0x66]);
                        self.emit(&[0xA9]);
                        self.emit(&(*v as i16).to_le_bytes());
                    } else if *v >= i8::MIN as i64 && *v <= i8::MAX as i64 {
                        self.emit(&[0xA8, *v as u8]);
                    } else if bits == 32 {
                        self.emit(&[0xA9]);
                        self.emit(&(*v as i32).to_le_bytes());
                    } else {
                        // 64 位 + 大 imm
                        if *v >= i32::MIN as i64 && *v <= i32::MAX as i64 {
                            self.emit(&[0xA9]);
                            self.emit(&(*v as i32).to_le_bytes());
                        } else {
                            // 64 位 imm: 走通用 modrm 形式
                            self.emit(&[0xF7, Self::modrm_reg_reg(0, *d)]);
                            self.emit(&v.to_le_bytes());
                        }
                    }
                    let _ = opcode;
                } else if bits == 16 {
                    self.emit(&[0xF7, Self::modrm_reg_reg(0, *d)]);
                    self.emit(&(*v as i16).to_le_bytes());
                } else if *v >= i32::MIN as i64 && *v <= i32::MAX as i64 {
                    self.emit(&[0xF7, Self::modrm_reg_reg(0, *d)]);
                    self.emit(&(*v as i32).to_le_bytes());
                } else {
                    // 64 位 imm 走 movabs + test
                    self.emit(&[0x48, 0xB8]);
                    self.emit(&v.to_le_bytes());
                    self.emit(&[Self::rex(w, 0, 0, 0), 0x85, Self::modrm_reg_reg(0, *d)]);
                }
            },
            (
                AsmOperand::Reg(s, _),
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
            ) => {
                if bits == 16 {
                    self.emit(&[0x66]);
                }
                self.emit(&[Self::rex(w, *s, index.unwrap_or(0), *base), 0x85]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            },
            (
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
                AsmOperand::Reg(d, _),
            ) => {
                if bits == 16 {
                    self.emit(&[0x66]);
                }
                self.emit(&[Self::rex(w, *d, index.unwrap_or(0), *base), 0x85]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            },
            (
                AsmOperand::Imm(v),
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
            ) => {
                if bits == 16 {
                    self.emit(&[0x66]);
                }
                self.emit(&[Self::rex(w, 0, index.unwrap_or(0), *base), 0xF7]);
                self.emit_asm_modrm_mem_only(0, *disp, *base, *index, *scale);
                if bits == 8 {
                    self.emit(&[*v as u8]);
                } else if *v >= i32::MIN as i64 && *v <= i32::MAX as i64 {
                    self.emit(&(*v as i32).to_le_bytes());
                } else {
                    self.asm_reject(&format!("32/64-bit test mem immediate out of range ({})", v));
                    self.emit(&[0x90]);
                }
            },
            _ => {
                self.asm_reject("unsupported test operand pattern");
            },
        }
    }
    pub(crate) fn emit_asm_unary_rm(&mut self, op: &AsmOperand, ext: u8, bits: u8) {
        let w = bits == 64;
        // 操作码组: INC/DEC (ext 0/1) 用 0xFE (8位) / 0xFF (16+位);
        // 其他单操作数组 (NOT/NEG/MUL/IMUL/DIV/IDIV, ext 2-7) 用 0xF6 (8位) / 0xF7 (16+位)
        let opcode = if bits == 8 {
            if ext <= 1 {
                0xFE
            } else {
                0xF6
            }
        } else {
            if ext <= 1 {
                0xFF
            } else {
                0xF7
            }
        };
        match op {
            AsmOperand::Reg(r, _) => {
                self.emit(&[Self::rex(w, 0, 0, *r), opcode, Self::modrm_reg_reg(ext, *r)]);
            },
            AsmOperand::Mem {
                disp,
                base,
                index,
                scale,
            } => {
                if bits == 16 {
                    self.emit(&[0x66]);
                }
                self.emit(&[Self::rex(w, 0, index.unwrap_or(0), *base), opcode]);
                self.emit_asm_modrm_mem_only(ext, *disp, *base, *index, *scale);
            },
            AsmOperand::SymDisp(sym, _) => {
                // Symbolic RIP-relative: inc/dec/not/neg sym(%rip)
                if bits == 16 {
                    self.emit(&[0x66]);
                }
                self.emit(&[Self::rex(w, 0, 0, 0), opcode]);
                self.emit_asm_modrm_mem_only(ext, 0, ASM_BASE_RIP, None, 1);
                let pos = self.pos() - 4;
                self.pending_data_rels.push((pos, sym.clone()));
            },
            _ => {
                self.asm_reject("unsupported unary operand pattern");
            },
        }
    }
    pub(crate) fn emit_asm_xadd(&mut self, src: &AsmOperand, dst: &AsmOperand, bits: u8) {
        let w = bits == 64;
        // xadd 编码: 8位 0F C0, 16位 66 0F C1, 32/64位 0F C1
        let opcode = if bits == 8 { 0xC0 } else { 0xC1 };
        if let AsmOperand::SegMem {
            seg,
            disp,
            base,
            index,
            scale,
        } = dst
        {
            let (_code, pfx) = Self::seg_meta(*seg);
            self.emit(&[pfx]);
            if let AsmOperand::Reg(s, _) = src {
                if bits == 16 {
                    self.emit(&[0x66]);
                }
                self.emit(&[Self::rex(w, *s, index.unwrap_or(0), *base), 0x0F, opcode]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            }
            return;
        }
        match (src, dst) {
            (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) => {
                self.emit(&[
                    Self::rex(w, *s, 0, *d),
                    0x0F,
                    opcode,
                    Self::modrm_reg_reg(*s, *d),
                ]);
            },
            (
                AsmOperand::Reg(s, _),
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
            ) => {
                if bits == 16 {
                    self.emit(&[0x66]);
                }
                self.emit(&[Self::rex(w, *s, index.unwrap_or(0), *base), 0x0F, opcode]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            },
            _ => {
                self.asm_reject("unsupported xadd operand pattern");
            },
        }
    }
    pub(crate) fn emit_asm_xchg(&mut self, src: &AsmOperand, dst: &AsmOperand, bits: u8) {
        let w = bits == 64;
        if Self::asm_operands_have_non_gpr_regs(&[src, dst]) {
            self.emit_unsupported_asm_operands("xchg");
            return;
        }
        // xchg 编码: 8位 86 /r, 16位 66 86 /r, 32位 87 /r, 64位 REX.W 87 /r
        let opcode = if bits == 8 { 0x86 } else { 0x87 };
        // 段前缀 mem
        if let AsmOperand::SegMem {
            seg,
            disp,
            base,
            index,
            scale,
        } = dst
        {
            let (_code, pfx) = Self::seg_meta(*seg);
            self.emit(&[pfx]);
            if let AsmOperand::Reg(s, _) = src {
                if bits == 16 {
                    self.emit(&[0x66]);
                }
                self.emit(&[Self::rex(w, *s, index.unwrap_or(0), *base), opcode]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            }
            return;
        }
        if let (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) = (src, dst) {
            if bits >= 32 {
                if *s == 0 {
                    // xchg eax, reg 快捷编码 90+rd
                    if *d >= 8 {
                        self.emit(&[0x41]);
                    }
                    self.emit(&[0x90 + (d & 7)]);
                } else if *d == 0 {
                    if *s >= 8 {
                        self.emit(&[0x41]);
                    }
                    self.emit(&[0x90 + (s & 7)]);
                } else {
                    self.emit(&[Self::rex(w, *s, 0, *d), opcode, Self::modrm_reg_reg(*s, *d)]);
                }
            } else {
                if bits == 16 {
                    self.emit(&[0x66]);
                }
                self.emit(&[Self::rex(w, *s, 0, *d), opcode, Self::modrm_reg_reg(*s, *d)]);
            }
        } else if let (
            AsmOperand::Reg(s, _),
            AsmOperand::Mem {
                disp,
                base,
                index,
                scale,
            },
        ) = (src, dst)
        {
            if bits == 16 {
                self.emit(&[0x66]);
            }
            self.emit(&[Self::rex(w, *s, index.unwrap_or(0), *base), opcode]);
            self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
        } else {
            self.asm_reject("unsupported xchg operand pattern");
        }
    }
    pub(crate) fn parse_cmov_cond(cond: &str) -> (Option<u8>, u8) {
        // 剥离可选宽度后缀 w/l/q
        let (cc_str, bits) = if let Some(stripped) = cond.strip_suffix('w') {
            (stripped, 16u8)
        } else if let Some(stripped) = cond.strip_suffix('l') {
            (stripped, 32u8)
        } else if let Some(stripped) = cond.strip_suffix('q') {
            (stripped, 64u8)
        } else {
            (cond, 64u8)
        };
        let cc = match cc_str {
            "z" | "ze" | "e" => 0x44,
            "nz" | "ne" => 0x45,
            "a" | "nbe" => 0x47,
            "ae" | "nb" | "nc" => 0x43,
            "b" | "c" | "nae" => 0x42,
            "be" | "na" => 0x46,
            "l" | "nge" => 0x4C,
            "le" | "ng" => 0x4E,
            "g" | "nle" => 0x4F,
            "ge" | "nl" => 0x4D,
            "s" => 0x48,
            "ns" => 0x49,
            "o" => 0x40,
            "no" => 0x41,
            "p" | "pe" => 0x4A,
            "np" | "po" => 0x4B,
            _ => return (None, bits),
        };
        (Some(cc), bits)
    }
}
