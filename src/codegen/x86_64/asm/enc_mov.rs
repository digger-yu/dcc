//! asm/enc_mov.rs —— 自历史完整实现（git 28a9063 单文件 Codegen）按职责簇恢复的模块。
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
    pub(crate) fn emit_asm_lea(&mut self, src: &AsmOperand, dst: &AsmOperand, bits: u8) {
        if Self::asm_operand_has_non_gpr_reg(dst) {
            self.emit_unsupported_asm_operands("lea");
            return;
        }
        if let (
            AsmOperand::Mem {
                disp,
                base,
                index,
                scale,
            },
            AsmOperand::Reg(d, _),
        ) = (src, dst)
        {
            let w = bits == 64;
            if *base == ASM_BASE_RIP {
                if bits == 16 {
                    self.emit(&[0x66]);
                }
                self.emit(&[Self::rex(w, *d, 0, 0), 0x8D]);
                self.emit_asm_modrm_mem_only(*d, *disp, ASM_BASE_RIP, None, 1);
                return;
            }
            self.emit(&[Self::rex(w, *d, index.unwrap_or(0), *base), 0x8D]);
            let has_sib = index.is_some() || (base & 7) == 4;
            let need_disp = *disp != 0 || (base & 7) == 5;
            let mut modrm = (d & 7) << 3;
            if !need_disp && *disp == 0 {
                modrm |= 0x00;
            } else if *disp >= -128 && *disp <= 127 {
                modrm |= 0x40;
            } else {
                modrm |= 0x80;
            }
            if has_sib {
                self.emit(&[modrm | 0x04]);
                let mut sib = *base & 7;
                if let Some(idx) = index {
                    sib |= (idx & 7) << 3;
                    let sc = match scale {
                        2 => 1,
                        4 => 2,
                        8 => 3,
                        _ => 0,
                    };
                    sib |= sc << 6;
                } else {
                    sib |= 4 << 3;
                }
                self.emit(&[sib]);
            } else {
                self.emit(&[modrm | (base & 7)]);
            }
            if need_disp {
                if *disp >= -128 && *disp <= 127 {
                    self.emit(&[*disp as u8]);
                } else {
                    self.emit(&disp.to_le_bytes());
                }
            }
        } else if let (AsmOperand::SymDisp(sym, _), AsmOperand::Reg(d, _)) = (src, dst) {
            // lea sym(%rip), %reg: 8D /r mod=00 r/m=101 + disp32 (reloc)
            let w = bits == 64;
            self.emit(&[Self::rex(w, *d, 0, 0), 0x8D]);
            self.emit_asm_modrm_mem_only(*d, 0, ASM_BASE_RIP, None, 1);
            let pos = self.pos() - 4;
            self.pending_data_rels.push((pos, sym.clone()));
        } else {
            self.asm_reject("unsupported lea operand pattern");
        }
    }
    fn emit_asm_load(
        &mut self,
        dst: u8,
        disp: i32,
        base: u8,
        index: Option<u8>,
        scale: u8,
        bits: u8,
    ) {
        let w = bits == 64;
        // 8 位装载必须走 `8A /r`（mov r8, r/m8）。写成 8B 会把 1 字节读做成 4 字节读，
        // 把高 3 字节的相邻数据当成操作数（与存回侧同一个越界类缺陷，见 emit_asm_store）。
        let load_op = if bits == 8 { 0x8A } else { 0x8B };
        if base == ASM_BASE_RIP {
            if bits == 16 {
                self.emit(&[0x66]);
            }
            self.emit(&[Self::rex(w, dst, 0, 0), load_op]);
            self.emit_asm_modrm_mem_only(dst, disp, ASM_BASE_RIP, None, 1);
            return;
        }
        let has_sib = index.is_some() || (base & 7) == 4; // rsp/r12 需要 SIB
        let need_disp = disp != 0 || (base & 7) == 5; // rbp/r13 强制 disp8
        if bits == 16 {
            self.emit(&[0x66]);
        }
        self.emit(&[Self::rex(w, dst, index.unwrap_or(0), base)]);
        self.emit(&[load_op]);
        let mut modrm = (dst & 7) << 3;
        if !need_disp && disp == 0 {
            modrm |= 0x00;
        }
        // mod=00
        else if (-128..=127).contains(&disp) {
            modrm |= 0x40;
        }
        // mod=01
        else {
            modrm |= 0x80;
        } // mod=10
        if has_sib {
            self.emit(&[modrm | 0x04]); // rm=100 (SIB follows)
            let mut sib = base & 7;
            if let Some(idx) = index {
                sib |= (idx & 7) << 3;
                let sc = match scale {
                    2 => 1,
                    4 => 2,
                    8 => 3,
                    _ => 0,
                };
                sib |= sc << 6;
            } else {
                sib |= 4 << 3; // idx=100 (no index)
            }
            self.emit(&[sib]);
        } else {
            self.emit(&[modrm | (base & 7)]);
        }
        if need_disp {
            if (-128..=127).contains(&disp) {
                self.emit(&[disp as u8]);
            } else {
                self.emit(&disp.to_le_bytes());
            }
        }
    }
    fn emit_asm_load_segmem(
        &mut self,
        disp: i32,
        base: u8,
        index: Option<u8>,
        scale: u8,
        bits: u8,
        dst: &AsmOperand,
        opcode: u8,
    ) {
        let w = bits == 64;
        if let AsmOperand::Reg(d, _) = dst {
            self.emit_asm_load(*d, disp, base, index, scale, bits);
            // opcode 已被 emit_asm_load 内部固定为 0x8B; 此参数保留用于扩展
            let _ = opcode;
        } else if bits == 16 {
            self.emit(&[0x66]);
            self.emit(&[Self::rex(w, 0, index.unwrap_or(0), base)]);
            self.emit(&[opcode]);
            // dst 必须是 reg 才能形成完整 modrm
        }
    }
    pub(crate) fn emit_asm_modrm_mem(
        &mut self,
        opcode: u8,
        reg: u8,
        disp: i32,
        base: u8,
        index: Option<u8>,
        scale: u8,
        need_rex: bool,
    ) {
        let has_sib = index.is_some() || (base & 7) == 4;
        let need_disp = disp != 0 || (base & 7) == 5;
        if need_rex {
            self.emit(&[Self::rex(false, reg, index.unwrap_or(0), base)]);
        }
        self.emit(&[opcode]);
        let mut modrm = (reg & 7) << 3;
        if !need_disp && disp == 0 {
            modrm |= 0x00;
        } else if (-128..=127).contains(&disp) {
            modrm |= 0x40;
        } else {
            modrm |= 0x80;
        }
        if has_sib {
            self.emit(&[modrm | 0x04]);
            let mut sib = base & 7;
            if let Some(idx) = index {
                sib |= (idx & 7) << 3;
                let sc = match scale {
                    2 => 1,
                    4 => 2,
                    8 => 3,
                    _ => 0,
                };
                sib |= sc << 6;
            } else {
                sib |= 4 << 3;
            }
            self.emit(&[sib]);
        } else {
            self.emit(&[modrm | (base & 7)]);
        }
        if need_disp {
            if (-128..=127).contains(&disp) {
                self.emit(&[disp as u8]);
            } else {
                self.emit(&disp.to_le_bytes());
            }
        }
    }
    pub(crate) fn emit_asm_modrm_mem_only(
        &mut self,
        reg: u8,
        disp: i32,
        base: u8,
        index: Option<u8>,
        scale: u8,
    ) {
        if base == ASM_BASE_RIP {
            if let Some(idx) = index {
                // [rip+disp+idx*scale]: modrm=0x04 (SIB), sib=scale:idx:5
                let modrm = ((reg & 7) << 3) | 0x04;
                let sc = match scale {
                    2 => 1,
                    4 => 2,
                    8 => 3,
                    _ => 0,
                };
                let sib = (sc << 6) | ((idx & 7) << 3) | 5;
                self.emit(&[modrm, sib]);
                self.emit(&disp.to_le_bytes());
            } else {
                // [rip+disp]: modrm=0x05 (mod=00, rm=101)
                let modrm = ((reg & 7) << 3) | 0x05;
                self.emit(&[modrm]);
                self.emit(&disp.to_le_bytes());
            }
            return;
        }
        let has_sib = index.is_some() || (base & 7) == 4;
        let need_disp = disp != 0 || (base & 7) == 5;
        let mut modrm = (reg & 7) << 3;
        if !need_disp && disp == 0 {
            modrm |= 0x00;
        } else if (-128..=127).contains(&disp) {
            modrm |= 0x40;
        } else {
            modrm |= 0x80;
        }
        if has_sib {
            self.emit(&[modrm | 0x04]);
            let mut sib = base & 7;
            if let Some(idx) = index {
                sib |= (idx & 7) << 3;
                let sc = match scale {
                    2 => 1,
                    4 => 2,
                    8 => 3,
                    _ => 0,
                };
                sib |= sc << 6;
            } else {
                sib |= 4 << 3;
            }
            self.emit(&[sib]);
        } else {
            self.emit(&[modrm | (base & 7)]);
        }
        if need_disp {
            if (-128..=127).contains(&disp) {
                self.emit(&[disp as u8]);
            } else {
                self.emit(&disp.to_le_bytes());
            }
        } else if has_sib && (base & 7) == 5 {
            // base=rbp + index, no disp: 强制 disp8=0 (mod=01, disp8=0)
            self.emit(&[0x00]);
        }
    }
    pub(crate) fn emit_asm_mov(&mut self, src: &AsmOperand, dst: &AsmOperand, bits: u8) {
        self.emit_asm_mov_inner(src, dst, bits, false);
    }
    pub(crate) fn emit_asm_movbe(&mut self, src: &AsmOperand, dst: &AsmOperand, bits: u8) {
        let w = bits == 64;
        let is16 = bits == 16;
        match (src, dst) {
            // Load: mem → reg
            (
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
                AsmOperand::Reg(d, _),
            ) => {
                if is16 {
                    self.emit(&[0x66]);
                }
                let r = Self::rex(w, *d, index.unwrap_or(0), *base);
                if r != 0x40 {
                    self.emit(&[r]);
                }
                self.emit(&[0x0F, 0x38, 0xF0]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            },
            // Store: reg → mem
            (
                AsmOperand::Reg(s, _),
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
            ) => {
                if is16 {
                    self.emit(&[0x66]);
                }
                let r = Self::rex(w, *s, index.unwrap_or(0), *base);
                if r != 0x40 {
                    self.emit(&[r]);
                }
                self.emit(&[0x0F, 0x38, 0xF1]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            },
            // Load: SegMem → reg
            (
                AsmOperand::SegMem {
                    seg,
                    disp,
                    base,
                    index,
                    scale,
                },
                AsmOperand::Reg(d, _),
            ) => {
                let (_code, pfx) = Self::seg_meta(*seg);
                self.emit(&[pfx]);
                if is16 {
                    self.emit(&[0x66]);
                }
                self.emit(&[
                    Self::rex(w, *d, index.unwrap_or(0), *base),
                    0x0F,
                    0x38,
                    0xF0,
                ]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            },
            // Store: reg → SegMem
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
                self.emit(&[pfx]);
                if is16 {
                    self.emit(&[0x66]);
                }
                self.emit(&[
                    Self::rex(w, *s, index.unwrap_or(0), *base),
                    0x0F,
                    0x38,
                    0xF1,
                ]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            },
            // Load: SymDisp → reg (RIP-relative)
            (AsmOperand::SymDisp(sym, _), AsmOperand::Reg(d, _)) => {
                if is16 {
                    self.emit(&[0x66]);
                }
                let r = Self::rex(w, *d, 0, 0);
                if r != 0x40 {
                    self.emit(&[r]);
                }
                self.emit(&[0x0F, 0x38, 0xF0, 0x05 | ((d & 7) << 3), 0, 0, 0, 0]);
                let pos = self.pos() - 4;
                self.pending_data_rels.push((pos, sym.clone()));
            },
            _ => {
                self.asm_reject("unsupported movbe operand pattern");
            },
        }
    }
    pub(crate) fn emit_asm_movd(&mut self, src: &AsmOperand, dst: &AsmOperand) {
        self.emit_asm_mov_inner(src, dst, 32, true);
    }
    fn emit_asm_mov_inner(
        &mut self,
        src: &AsmOperand,
        dst: &AsmOperand,
        bits: u8,
        allow_xmm: bool,
    ) {
        let w = bits == 64;
        if Self::asm_operands_have_non_gpr_regs(&[src, dst]) {
            let special_mov = matches!(
                (src, dst),
                (AsmOperand::Reg(_, ASM_REG_MMX), _)
                    | (_, AsmOperand::Reg(_, ASM_REG_MMX))
                    | (AsmOperand::Reg(_, ASM_REG_CR), AsmOperand::Reg(_, _))
                    | (AsmOperand::Reg(_, _), AsmOperand::Reg(_, ASM_REG_CR))
                    | (AsmOperand::Reg(_, ASM_REG_DR), AsmOperand::Reg(_, _))
                    | (AsmOperand::Reg(_, _), AsmOperand::Reg(_, ASM_REG_DR))
            );
            let xmm_movd = allow_xmm
                && matches!(
                    (src, dst),
                    (AsmOperand::Reg(_, ASM_REG_XMM), AsmOperand::Reg(_, sz))
                        | (AsmOperand::Reg(_, sz), AsmOperand::Reg(_, ASM_REG_XMM))
                        if Self::asm_reg_is_gpr(*sz)
                );
            if !special_mov && !xmm_movd {
                self.emit_unsupported_asm_operands("mov");
                return;
            }
        }
        // 段寄存器读写: 0x8C /r (mov r/m16, Sreg) / 0x8E /r (mov Sreg, r/m16)
        if let (AsmOperand::Reg(s, _), AsmOperand::Seg(seg)) = (src, dst) {
            if bits == 64 {
                // mov %reg, %segreg : 8E /r
                let rex = Self::rex(false, *seg & 7, 0, *s);
                if *s >= 8 || *seg >= 8 {
                    self.emit(&[rex]);
                }
                self.emit(&[0x8E, Self::modrm_reg_reg(*seg, *s)]);
            } else {
                if bits == 16 {
                    self.emit(&[0x66]);
                }
                self.emit(&[
                    Self::rex(false, *seg & 7, 0, *s),
                    0x8E,
                    Self::modrm_reg_reg(*seg, *s),
                ]);
            }
            return;
        }
        if let (AsmOperand::Seg(seg), AsmOperand::Reg(d, _)) = (src, dst) {
            if bits == 64 {
                // mov %segreg, %reg : 8C /r
                let rex = Self::rex(false, *d, 0, *seg & 7);
                if *d >= 8 || *seg >= 8 {
                    self.emit(&[rex]);
                }
                self.emit(&[0x8C, Self::modrm_reg_reg(*d, *seg)]);
            } else {
                if bits == 16 {
                    self.emit(&[0x66]);
                }
                self.emit(&[
                    Self::rex(false, *d, 0, *seg & 7),
                    0x8C,
                    Self::modrm_reg_reg(*d, *seg),
                ]);
            }
            return;
        }
        // 控制寄存器: mov %crN, %reg (0F 20 /r) / mov %reg, %crN (0F 22 /r)
        if let (AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (src, dst) {
            if *ss == ASM_REG_CR {
                self.emit(&[
                    Self::rex(false, *d, 0, *s),
                    0x0F,
                    0x20,
                    Self::modrm_reg_reg(*d, *s),
                ]);
                return;
            }
            if *ds == ASM_REG_CR {
                self.emit(&[
                    Self::rex(false, *s, 0, *d),
                    0x0F,
                    0x22,
                    Self::modrm_reg_reg(*s, *d),
                ]);
                return;
            }
            // 调试寄存器: mov %dbN, %reg (0F 21 /r) / mov %reg, %dbN (0F 23 /r)
            if *ss == ASM_REG_DR {
                self.emit(&[
                    Self::rex(false, *d, 0, *s),
                    0x0F,
                    0x21,
                    Self::modrm_reg_reg(*d, *s),
                ]);
                return;
            }
            if *ds == ASM_REG_DR {
                self.emit(&[
                    Self::rex(false, *s, 0, *d),
                    0x0F,
                    0x23,
                    Self::modrm_reg_reg(*s, *d),
                ]);
                return;
            }
        }
        // MMX 寄存器 mov: movq mm, mm/m64 (0F 6F) / movq mm/m64, mm (0F 7F)
        // movd mm, r/m32 (0F 6E) / movd r/m32, mm (0F 7E)
        if let (AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (src, dst) {
            if *ss == ASM_REG_MMX || *ds == ASM_REG_MMX {
                if *ds == ASM_REG_MMX {
                    // dst is MMX: movq/movd mm, src (0F 6F / 0F 6E)
                    let op = if bits <= 32 { 0x6E } else { 0x6F };
                    self.emit(&[0x0F, op, Self::modrm_reg_reg(*d, *s)]);
                } else {
                    // src is MMX, dst is GP: movq/movd dst, mm (0F 7F / 0F 7E)
                    let op = if bits <= 32 { 0x7E } else { 0x7F };
                    self.emit(&[0x0F, op, Self::modrm_reg_reg(*s, *d)]);
                }
                return;
            }
        }
        if let (
            AsmOperand::Mem {
                disp,
                base,
                index,
                scale,
            },
            AsmOperand::Reg(d, ds),
        ) = (src, dst)
        {
            if *ds == ASM_REG_MMX {
                // movq/movd mm, [mem] (0F 6F / 0F 6E)
                let op = if bits <= 32 { 0x6E } else { 0x6F };
                self.emit(&[0x0F, op]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
                return;
            }
        }
        if let (
            AsmOperand::Reg(s, ss),
            AsmOperand::Mem {
                disp,
                base,
                index,
                scale,
            },
        ) = (src, dst)
        {
            if *ss == ASM_REG_MMX {
                // movq/movd [mem], mm (0F 7F / 0F 7E)
                let op = if bits <= 32 { 0x7E } else { 0x7F };
                self.emit(&[0x0F, op]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
                return;
            }
        }
        // 段前缀 mem: 发射段前缀 + 普通 mem 路径
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
            self.emit_asm_load_segmem(*disp, *base, *index, *scale, bits, dst, 0x8B);
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
            self.emit_asm_store_segmem(*disp, *base, *index, *scale, bits, src, 0x89);
            return;
        }
        // sym(%rip) → mov [rip+sym], reg
        if let (AsmOperand::SymDisp(sym, _), AsmOperand::Reg(d, _)) = (src, dst) {
            // 全局变量 RIP-relative 加载: 与 emit_global_load 相同编码
            let rex = if *d >= 8 { 0x4C } else { 0x48 };
            let modrm = 0x05 | ((d & 7) << 3);
            if bits == 8 {
                self.emit(&[Self::rex(false, *d, 0, 0), 0x0F, 0xB6, modrm, 0, 0, 0, 0]);
            } else if bits == 16 {
                self.emit(&[
                    0x66,
                    Self::rex(false, *d, 0, 0),
                    0x0F,
                    0xB7,
                    modrm,
                    0,
                    0,
                    0,
                    0,
                ]);
            } else if bits == 32 {
                self.emit(&[Self::rex(false, *d, 0, 0), 0x8B, modrm, 0, 0, 0, 0]);
            } else {
                self.emit(&[rex, 0x8B, modrm, 0, 0, 0, 0]);
            }
            let pos = self.pos() - 4;
            self.pending_data_rels.push((pos, sym.clone()));
            return;
        }
        // mov reg, sym(%rip)
        if let (AsmOperand::Reg(s, _), AsmOperand::SymDisp(sym, _)) = (src, dst) {
            // 64 位默认: mov [rip+sym], reg
            let rex = if *s >= 8 { 0x4C } else { 0x48 };
            let modrm = 0x05 | ((s & 7) << 3);
            if bits == 8 {
                self.emit(&[Self::rex(false, *s, 0, 0), 0x88, modrm, 0, 0, 0, 0]);
            } else if bits == 16 {
                self.emit(&[0x66, Self::rex(false, *s, 0, 0), 0x89, modrm, 0, 0, 0, 0]);
            } else if bits == 32 {
                self.emit(&[Self::rex(false, *s, 0, 0), 0x89, modrm, 0, 0, 0, 0]);
            } else {
                self.emit(&[rex, 0x89, modrm, 0, 0, 0, 0]);
            }
            let pos = self.pos() - 4;
            self.pending_data_rels.push((pos, sym.clone()));
            return;
        }
        match (src, dst) {
            // reg → reg
            (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) => {
                if bits == 8 {
                    // mov r8, r8: 88 /r
                    if *s >= 4 || *d >= 4 {
                        self.emit(&[Self::rex(false, *s, 0, *d)]);
                    }
                    self.emit(&[0x88, Self::modrm_reg_reg(*s, *d)]);
                } else {
                    if bits == 16 {
                        self.emit(&[0x66]);
                    }
                    self.emit_rex_if_used(w, *s, 0, *d, false);
                    self.emit(&[0x89, Self::modrm_reg_reg(*s, *d)]);
                }
            },
            // imm → reg
            (AsmOperand::Imm(v), AsmOperand::Reg(d, _)) => {
                if bits == 8 {
                    // `B0+rd ib`: 寄存器号在 **opcode 低 3 位**，扩展它的是 REX.B；而
                    // %spl/%bpl/%sil/%dil (4..7) 只需要「存在 REX」。旧代码一个前缀都不发，
                    // 于是 `movb $65, %sil` 被编成 `mov $65,%dh`（写进 %rsi 的 8..15 位，
                    // 内存目标从未被写）。两者也不能合并：为 4..7 置 B 会搬到 %r12b..%r15b。
                    if *d >= 4 {
                        self.emit(&[0x40 | if *d >= 8 { 0x01 } else { 0 }]);
                    }
                    self.emit(&[0xB0 + (d & 7), *v as u8]);
                } else if *v >= i32::MIN as i64 && *v <= i32::MAX as i64 {
                    if bits == 64 {
                        // 64 位目标 + 32 位立即数只能走 `REX.W + C7 /0 id`（符号扩展）。
                        // `B8+rd imm32` 不带 REX.W，那是 32 位写：高 32 位被清零，
                        // 于是 `movq $-1, %rax` 得到 0xFFFFFFFF 而不是 -1。
                        self.emit(&[
                            0x48 | if *d >= 8 { 0x01 } else { 0 },
                            0xC7,
                            Self::modrm_reg_reg(0, *d),
                        ]);
                        self.emit(&(*v as i32).to_le_bytes());
                    } else {
                        // 16 位: 0x66 前缀须在 REX 之前
                        if bits == 16 {
                            self.emit(&[0x66]);
                        }
                        if *d >= 8 {
                            self.emit(&[0x41]);
                        }
                        self.emit(&[0xB8 + (d & 7)]);
                        if bits == 16 {
                            self.emit(&(*v as i16).to_le_bytes());
                        } else {
                            self.emit(&(*v as i32).to_le_bytes());
                        }
                    }
                } else {
                    // movabs reg, imm64
                    if *d >= 8 {
                        self.emit(&[0x49]);
                    } else {
                        self.emit(&[0x48]);
                    }
                    self.emit(&[0xB8 + (d & 7)]);
                    self.emit(&v.to_le_bytes());
                }
            },
            // mem → reg
            (
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
                AsmOperand::Reg(d, _),
            ) => {
                self.emit_asm_load(*d, *disp, *base, *index, *scale, bits);
            },
            // reg → mem
            (
                AsmOperand::Reg(s, _),
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
            ) => {
                self.emit_asm_store(*s, *disp, *base, *index, *scale, bits);
            },
            // imm → mem
            (
                AsmOperand::Imm(v),
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
            ) => {
                // mov [mem], imm: C7 /0 id (16/32/64 位) 或 C6 /0 ib (8 位)。
                // 目的在 **ModRM.r/m**：base/index 只要有一个 >=8 就必须发 REX。
                // 旧代码把 need_rex 硬编码成 false（所以下面永远传 false），实测
                // `movb $77, -0x40(%r13)` 被编成 `-0x40(%rbp)` —— 写进了别人的栈槽，
                // 长度完全一样，因此既不崩溃也没有任何诊断。16 位那一支还同时
                // 漏了 0x66（于是按 32 位存）。
                let need_rex = *base >= 8 || index.map_or(false, |i| i >= 8);
                if bits == 8 {
                    if need_rex {
                        self.emit(&[Self::rex(false, 0, index.unwrap_or(0), *base)]);
                    }
                    self.emit_asm_modrm_mem(0xC6, 0, *disp, *base, *index, *scale, false);
                    self.emit(&[*v as u8]);
                } else {
                    if bits == 16 {
                        // legacy 前缀必须在 REX 之前
                        self.emit(&[0x66]);
                    }
                    if bits == 64 || need_rex {
                        self.emit(&[Self::rex(bits == 64, 0, index.unwrap_or(0), *base)]);
                    }
                    self.emit_asm_modrm_mem(0xC7, 0, *disp, *base, *index, *scale, false);
                    self.emit(&(*v as i32).to_le_bytes());
                }
            },
            // imm64 → mem (内核 movabs 内存目标)
            // 支持模式: mov imm, mem 走 REX.W + C7 /0 id (无法表示 imm64)
            // 走两步: movabs reg, imm; mov [mem], reg
            (
                AsmOperand::Imm(v),
                AsmOperand::SegMem {
                    disp,
                    base,
                    index,
                    scale,
                    ..
                },
            )
            | (
                AsmOperand::Imm(v),
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
            ) => {
                // 先 movabs 到 rax, 再 mov [mem], rax
                if bits == 64 && (*v > i32::MAX as i64 || *v < i32::MIN as i64) {
                    // movabs %rax, imm64
                    self.emit(&[0x48, 0xB8]);
                    self.emit(&v.to_le_bytes());
                    self.emit_asm_store(0, *disp, *base, *index, *scale, 64);
                } else {
                    // 32-bit 立即数
                    if bits != 16 && bits != 8 {
                        self.emit(&[Self::rex(false, 0, index.unwrap_or(0), *base)]);
                    }
                    self.emit_asm_modrm_mem(0xC7, 0, *disp, *base, *index, *scale, false);
                    self.emit(&(*v as i32).to_le_bytes());
                }
            },
            _ => {
                self.asm_reject("unsupported mov operand pattern");
            },
        }
    }
    pub(crate) fn emit_asm_movnt(&mut self, src: &AsmOperand, dst: &AsmOperand, bits: u8) {
        let w = bits == 64;
        let opcode = if bits == 64 { 0xC7u8 } else { 0xC3u8 };
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
                self.emit(&[Self::rex(w, *s, index.unwrap_or(0), *base), 0x0F, opcode]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            }
            return;
        }
        if let AsmOperand::SymDisp(sym, _) = dst {
            if let AsmOperand::Reg(s, _) = src {
                let rex = if *s >= 8 { 0x4C } else { 0x48 };
                let modrm = 0x05 | ((s & 7) << 3);
                if bits == 8 {
                    self.emit(&[Self::rex(false, *s, 0, 0), 0x88, modrm, 0, 0, 0, 0]);
                } else if bits == 16 {
                    self.emit(&[0x66, Self::rex(false, *s, 0, 0), 0x89, modrm, 0, 0, 0, 0]);
                } else if bits == 32 {
                    self.emit(&[Self::rex(false, *s, 0, 0), 0x89, modrm, 0, 0, 0, 0]);
                } else {
                    self.emit(&[rex, 0x89, modrm, 0, 0, 0, 0]);
                }
                let pos = self.pos() - 4;
                self.pending_data_rels.push((pos, sym.clone()));
                return;
            }
        }
        if let (
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
            self.emit(&[Self::rex(w, *s, index.unwrap_or(0), *base), 0x0F, opcode]);
            self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
        } else {
            self.asm_reject("unsupported movnt operand pattern");
        }
    }
    pub(crate) fn emit_asm_movsx(
        &mut self,
        src: &AsmOperand,
        dst: &AsmOperand,
        src_bits: u8,
        dst_bits: u8,
    ) {
        let w = dst_bits == 64;
        // 段前缀 mem
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
                if dst_bits == 16 {
                    self.emit(&[0x66]);
                }
                self.emit(&[Self::rex(w, *d, index.unwrap_or(0), *base), 0x0F]);
                let op = match src_bits {
                    8 => 0xBEu8,
                    16 => 0xBF,
                    32 => 0x63, // movsxd
                    _ => 0xBE,
                };
                self.emit(&[op]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            }
            return;
        }
        match (src, dst) {
            (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) => {
                self.emit(&[Self::rex(w, *d, 0, *s)]);
                match src_bits {
                    8 => self.emit(&[0x0F, 0xBE, Self::modrm_reg_reg(*d, *s)]),
                    16 => self.emit(&[0x0F, 0xBF, Self::modrm_reg_reg(*d, *s)]),
                    32 => self.emit(&[0x63, Self::modrm_reg_reg(*d, *s)]),
                    _ => {
                        self.asm_reject("unsupported movsx source size");
                    },
                };
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
                if dst_bits == 16 {
                    self.emit(&[0x66]);
                }
                self.emit(&[Self::rex(w, *d, index.unwrap_or(0), *base), 0x0F]);
                let op = match src_bits {
                    8 => 0xBEu8,
                    16 => 0xBF,
                    32 => 0x63,
                    _ => 0xBE,
                };
                self.emit(&[op]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            },
            (AsmOperand::Imm(v), AsmOperand::Reg(d, _)) => {
                // movsx $imm, %reg: 走 mov reg, imm
                if src_bits == 8 {
                    if *d < 8 {
                        self.emit(&[0xB0 + (d & 7), *v as u8]);
                    } else {
                        self.emit(&[0x41, 0xB0 + (d & 7), *v as u8]);
                    }
                } else if src_bits == 16 {
                    if dst_bits == 16 {
                        if *d < 8 {
                            self.emit(&[0x66, 0xB8 + (d & 7)]);
                        } else {
                            self.emit(&[0x66, 0x41, 0xB8 + (d & 7)]);
                        }
                    } else {
                        // movsx $imm16 → mov reg, imm
                        if *d < 8 {
                            self.emit(&[0xB8 + (d & 7)]);
                        } else {
                            self.emit(&[0x41, 0xB8 + (d & 7)]);
                        }
                    }
                    self.emit(&(*v as i16).to_le_bytes());
                } else if src_bits == 32 {
                    if dst_bits == 32 {
                        if *d < 8 {
                            self.emit(&[0xB8 + (d & 7)]);
                        } else {
                            self.emit(&[0x41, 0xB8 + (d & 7)]);
                        }
                    } else {
                        if *d < 8 {
                            self.emit(&[0x48, 0xB8 + (d & 7)]);
                        } else {
                            self.emit(&[0x49, 0xB8 + (d & 7)]);
                        }
                    }
                    self.emit(&(*v as i32).to_le_bytes());
                } else {
                    self.asm_reject("movsx $imm with unsupported size");
                }
            },
            (AsmOperand::SymDisp(sym, _), AsmOperand::Reg(d, _)) => {
                // movsx sym(%rip), %reg: 0F BE/BF /r or 63 /r (movsxd)
                let rex = Self::rex(w, *d, 0, 0);
                let op = match src_bits {
                    8 => 0xBEu8,
                    16 => 0xBF,
                    32 => 0x63,
                    _ => 0xBE,
                };
                let modrm = 0x05 | ((d & 7) << 3);
                if src_bits == 32 {
                    // movsxd: 63 /r (no 0F prefix)
                    self.emit(&[rex, op, modrm, 0, 0, 0, 0]);
                } else {
                    self.emit(&[rex, 0x0F, op, modrm, 0, 0, 0, 0]);
                }
                let pos = self.pos() - 4;
                self.pending_data_rels.push((pos, sym.clone()));
            },
            _ => {
                self.asm_reject("unsupported movsx operand pattern");
            },
        }
    }
    pub(crate) fn emit_asm_movzx(
        &mut self,
        src: &AsmOperand,
        dst: &AsmOperand,
        src_bits: u8,
        dst_bits: u8,
    ) {
        let w = dst_bits == 64;
        // 段前缀 mem: 段前缀 + 普通 mem movzx
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
                if dst_bits == 16 {
                    self.emit(&[0x66]);
                }
                self.emit(&[Self::rex(w, *d, index.unwrap_or(0), *base), 0x0F]);
                let op = if src_bits == 8 { 0xB6u8 } else { 0xB7 };
                self.emit(&[op]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            }
            return;
        }
        match (src, dst) {
            (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) => {
                self.emit(&[Self::rex(w, *d, 0, *s)]);
                match src_bits {
                    8 => self.emit(&[0x0F, 0xB6, Self::modrm_reg_reg(*d, *s)]),
                    16 => self.emit(&[0x0F, 0xB7, Self::modrm_reg_reg(*d, *s)]),
                    _ => {
                        self.asm_reject("unsupported movzx source size");
                    },
                };
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
                self.emit(&[Self::rex(w, *d, index.unwrap_or(0), *base), 0x0F]);
                let op = if src_bits == 8 { 0xB6u8 } else { 0xB7 };
                self.emit(&[op]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            },
            (AsmOperand::Imm(v), AsmOperand::Reg(d, _)) => {
                // movzx $imm, %reg: 走 mov reg, imm
                if src_bits == 8 {
                    if *d < 8 {
                        self.emit(&[0xB0 + (d & 7), *v as u8]);
                    } else {
                        self.emit(&[0x41, 0xB0 + (d & 7), *v as u8]);
                    }
                } else {
                    self.asm_reject("movzx $imm only supports 8-bit source");
                }
            },
            (AsmOperand::SymDisp(sym, _), AsmOperand::Reg(d, _)) => {
                // movzx sym(%rip), %reg: 0F B6/B7 /r
                let rex = Self::rex(w, *d, 0, 0);
                let op = if src_bits == 8 { 0xB6u8 } else { 0xB7 };
                let modrm = 0x05 | ((d & 7) << 3);
                self.emit(&[rex, 0x0F, op, modrm, 0, 0, 0, 0]);
                let pos = self.pos() - 4;
                self.pending_data_rels.push((pos, sym.clone()));
            },
            _ => {
                self.asm_reject("unsupported movzx operand pattern");
            },
        }
    }
    pub(crate) fn emit_asm_pop_sized(&mut self, op: &AsmOperand, _bits: u8) {
        match op {
            AsmOperand::Reg(r, _) => {
                if *r >= 8 {
                    self.emit(&[0x41]);
                }
                self.emit(&[0x58 + (r & 7)]);
            },
            AsmOperand::Seg(seg) => {
                // pop %cs/%ds/%es/%fs/%gs/%ss
                match *seg {
                    0 => self.emit(&[0x07]),       // pop %es
                    2 => self.emit(&[0x17]),       // pop %ss
                    3 => self.emit(&[0x1F]),       // pop %ds
                    4 => self.emit(&[0x0F, 0xA1]), // pop %fs
                    5 => self.emit(&[0x0F, 0xA9]), // pop %gs
                    _ => {
                        self.asm_reject(&format!("pop: unknown segment {}", seg));
                    },
                }
            },
            AsmOperand::Mem {
                disp,
                base,
                index,
                scale,
            } => {
                self.emit(&[Self::rex(false, 0, index.unwrap_or(0), *base), 0x8F]);
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
                self.emit(&[pfx, Self::rex(false, 0, index.unwrap_or(0), *base), 0x8F]);
                self.emit_asm_modrm_mem_only(0, *disp, *base, *index, *scale);
            },
            _ => {
                self.asm_reject("unsupported pop operand pattern");
            },
        }
    }
    pub(crate) fn emit_asm_push_sized(&mut self, op: &AsmOperand, _bits: u8) {
        match op {
            AsmOperand::Reg(r, _) => {
                if *r >= 8 {
                    self.emit(&[0x41]);
                }
                self.emit(&[0x50 + (r & 7)]);
            },
            AsmOperand::Seg(seg) => {
                // push %cs/%ds/%es/%fs/%gs/%ss: 0x0FA 段码 (CS=1, SS=2, DS=3, ES=0, FS=4, GS=5)
                // 标准形式 0x06 0x0E 0x16 0x1E (ES/CS/SS/DS), FS/GS 用 REX + opcode
                match *seg {
                    0 => self.emit(&[0x06]),       // push %es
                    1 => self.emit(&[0x0E]),       // push %cs
                    2 => self.emit(&[0x16]),       // push %ss
                    3 => self.emit(&[0x1E]),       // push %ds
                    4 => self.emit(&[0x0F, 0xA0]), // push %fs
                    5 => self.emit(&[0x0F, 0xA8]), // push %gs
                    _ => {
                        self.asm_reject(&format!("push: unknown segment {}", seg));
                    },
                }
            },
            AsmOperand::Imm(v) => {
                if *v >= -128 && *v <= 127 {
                    self.emit(&[0x6A, *v as u8]);
                } else if *v >= i32::MIN as i64 && *v <= i32::MAX as i64 {
                    self.emit(&[0x68]);
                    self.emit(&(*v as i32).to_le_bytes());
                } else {
                    // 64-bit imm: 必须走两步: movabs rax, imm; push rax
                    self.emit(&[0x48, 0xB8]);
                    self.emit(&v.to_le_bytes());
                    self.emit(&[0x50]); // push %rax
                }
            },
            AsmOperand::Sym(label) => {
                // push $label: 编码为 push imm32 + 重定位
                self.emit(&[0x68]);
                self.emit(&[0x00, 0x00, 0x00, 0x00]);
                let pos = self.pos() - 4;
                self.pending.push(PendingJump {
                    at: pos,
                    target: label.clone(),
                    kind: JumpKind::Jmp,
                    disp_offset: pos,
                    next_ip: 0,
                    is_push_imm: true,
                });
            },
            AsmOperand::Mem {
                disp,
                base,
                index,
                scale,
            } => {
                self.emit(&[Self::rex(false, 0, index.unwrap_or(0), *base), 0xFF]);
                self.emit_asm_modrm_mem_only(6, *disp, *base, *index, *scale);
            },
            AsmOperand::SegMem {
                seg,
                disp,
                base,
                index,
                scale,
            } => {
                let (_code, pfx) = Self::seg_meta(*seg);
                self.emit(&[pfx, Self::rex(false, 0, index.unwrap_or(0), *base), 0xFF]);
                self.emit_asm_modrm_mem_only(6, *disp, *base, *index, *scale);
            },
            AsmOperand::SymDisp(sym, _) => {
                // push sym(%rip): FF 35 disp32
                self.emit(&[Self::rex(false, 0, 0, 0), 0xFF, 0x35]);
                self.emit(&[0x00, 0x00, 0x00, 0x00]);
                let pos = self.pos() - 4;
                self.pending_data_rels.push((pos, sym.clone()));
            },
            _ => {
                self.asm_reject("unsupported push operand pattern");
            },
        }
    }
    fn emit_asm_store(
        &mut self,
        src: u8,
        disp: i32,
        base: u8,
        index: Option<u8>,
        scale: u8,
        bits: u8,
    ) {
        let w = bits == 64;
        // 8 位存回必须走 `88 /r`（mov r/m8, r8）。旧代码写死 0x89，实测 `"=m"(*p)`
        // （char *p）只该写 1 字节却写了 4 字节，越界盖掉相邻变量 —— 比错寄存器更坏。
        let store_op = if bits == 8 { 0x88 } else { 0x89 };
        if base == ASM_BASE_RIP {
            if bits == 16 {
                self.emit(&[0x66]);
            }
            self.emit(&[Self::rex(w, src, 0, 0), store_op]);
            self.emit_asm_modrm_mem_only(src, disp, ASM_BASE_RIP, None, 1);
            return;
        }
        let has_sib = index.is_some() || (base & 7) == 4;
        let need_disp = disp != 0 || (base & 7) == 5;
        if bits == 16 {
            self.emit(&[0x66]);
        }
        self.emit(&[Self::rex(w, src, index.unwrap_or(0), base)]);
        self.emit(&[store_op]);
        let mut modrm = (src & 7) << 3;
        if !need_disp && disp == 0 {
            modrm |= 0x00;
        } else if (-128..=127).contains(&disp) {
            modrm |= 0x40;
        } else {
            modrm |= 0x80;
        }
        if has_sib {
            self.emit(&[modrm | 0x04]);
            let mut sib = base & 7;
            if let Some(idx) = index {
                sib |= (idx & 7) << 3;
                let sc = match scale {
                    2 => 1,
                    4 => 2,
                    8 => 3,
                    _ => 0,
                };
                sib |= sc << 6;
            } else {
                sib |= 4 << 3;
            }
            self.emit(&[sib]);
        } else {
            self.emit(&[modrm | (base & 7)]);
        }
        if need_disp {
            if (-128..=127).contains(&disp) {
                self.emit(&[disp as u8]);
            } else {
                self.emit(&disp.to_le_bytes());
            }
        }
    }
    fn emit_asm_store_segmem(
        &mut self,
        disp: i32,
        base: u8,
        index: Option<u8>,
        scale: u8,
        bits: u8,
        src: &AsmOperand,
        opcode: u8,
    ) {
        let w = bits == 64;
        if let AsmOperand::Reg(s, _) = src {
            self.emit_asm_store(*s, disp, base, index, scale, bits);
            let _ = opcode;
        } else if bits == 16 {
            self.emit(&[0x66]);
            self.emit(&[Self::rex(w, 0, index.unwrap_or(0), base)]);
            self.emit(&[opcode]);
        }
    }
    pub(crate) fn emit_asm_sys_mem(&mut self, op: &AsmOperand, grp: u8, ext: u8) {
        if let AsmOperand::Mem {
            disp,
            base,
            index,
            scale,
        } = op
        {
            self.emit(&[Self::rex(false, 0, index.unwrap_or(0), *base), 0x0F, 0x01]);
            self.emit_asm_modrm_mem_only(ext, *disp, *base, *index, *scale);
        } else if let AsmOperand::SegMem {
            seg,
            disp,
            base,
            index,
            scale,
        } = op
        {
            let (_code, pfx) = Self::seg_meta(*seg);
            self.emit(&[
                pfx,
                Self::rex(false, 0, index.unwrap_or(0), *base),
                0x0F,
                0x01,
            ]);
            self.emit_asm_modrm_mem_only(ext, *disp, *base, *index, *scale);
        } else if let AsmOperand::SymDisp(sym, _) = op {
            // sys mem sym(%rip): 0F 01 /ext + disp32
            self.emit(&[0x0F, 0x01, 0x05 | ((ext & 7) << 3), 0, 0, 0, 0]);
            let pos = self.pos() - 4;
            self.pending_data_rels.push((pos, sym.clone()));
        } else {
            self.asm_reject("unsupported sys mem operand pattern");
        }
        let _ = grp;
    }
}
