//! asm/enc_simd.rs —— 自历史完整实现（git 28a9063 单文件 Codegen）按职责簇恢复的模块。
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
    /// Emit SSE instruction header with correct prefix order.
    /// Order: legacy prefix (66/F2/F3 if any) -> REX (if any) -> opcode.
    /// `prefix` is 0x66/0xF2/0xF3/0x0F (0x0F means no legacy prefix).
    pub(crate) fn emit_sse_header(&mut self, prefix: u8, rex: u8) {
        if prefix == 0x66 || prefix == 0xF2 || prefix == 0xF3 {
            self.emit(&[prefix]);
        }
        if rex != 0 {
            self.emit(&[0x40 | rex]);
        }
        self.emit(&[0x0F]);
    }
    pub(crate) fn emit_asm_amx_3op(
        &mut self,
        dst: &AsmOperand,
        src1: &AsmOperand,
        src2: &AsmOperand,
        opcode: u8,
        pp: u8,
    ) {
        let (d, s1, s2) = match (dst, src1, src2) {
            (AsmOperand::Reg(d, ds), AsmOperand::Reg(s, ss), AsmOperand::Reg(t, ts))
                if *ds == ASM_REG_TMM && *ss == ASM_REG_TMM && *ts == ASM_REG_TMM =>
            {
                (*d, *s, *t)
            },
            _ => {
                self.asm_warn("AMX tdp* needs tmm regs");
                return;
            },
        };
        self.emit_vex4_prefix(d, s2, 2, 0, s1, 0, pp);
        self.emit(&[opcode, Self::modrm_reg_reg(d, s2)]);
    }
    pub(crate) fn emit_asm_evex_2op(
        &mut self,
        src: &AsmOperand,
        dst: &AsmOperand,
        l: u16,
        pp: u8,
        map: u8,
        opcode: u8,
        w: u8,
    ) {
        let l_bits: u8 = if l >= 512 {
            2u8
        } else if l >= 256 {
            1u8
        } else {
            0u8
        };
        match (src, dst) {
            (AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) if *ss >= 128 && *ds >= 128 => {
                self.emit_evex_prefix(*d, *s, map, w, 0, l_bits, pp, 0);
                self.emit(&[opcode, Self::modrm_reg_reg(*d, *s)]);
            },
            (
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
                AsmOperand::Reg(d, ds),
            ) if *ds >= 128 => {
                self.emit_evex_prefix(*d, *base, map, w, 0, l_bits, pp, 0);
                self.emit(&[opcode]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            },
            (
                AsmOperand::Reg(s, ss),
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
            ) if *ss >= 128 => {
                self.emit_evex_prefix(*s, *base, map, w, 0, l_bits, pp, 0);
                self.emit(&[opcode]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            },
            _ => {
                self.asm_warn("unsupported evex 2-op operand pattern");
            },
        }
    }
    pub(crate) fn emit_asm_evex_3op_reg(
        &mut self,
        dst: u8,
        src1: u8,
        src2: u8,
        l: u16,
        pp: u8,
        map: u8,
        opcode: u8,
        w: u8,
    ) {
        let l_bits: u8 = if l >= 512 {
            2u8
        } else if l >= 256 {
            1u8
        } else {
            0u8
        };
        self.emit_evex_prefix(dst, src2, map, w, src1, l_bits, pp, 0);
        self.emit(&[opcode, Self::modrm_reg_reg(dst, src2)]);
    }
    pub(crate) fn emit_asm_fma_3op(
        &mut self,
        src1: &AsmOperand,
        src2: &AsmOperand,
        dst: &AsmOperand,
        l: u8,
        pp: u8,
        opcode: u8,
        w: u8,
        _is_scalar: bool,
    ) {
        // AT&T: src1=ops[0] (Intel src2=modrm.r/m), src2=ops[1] (Intel src1=VEX.vvvv), dst=ops[2] (Intel dst=modrm.reg)
        let d = match dst {
            AsmOperand::Reg(d, ds) if *ds >= 128 => *d,
            _ => {
                self.asm_warn("unsupported FMA dst pattern");
                return;
            },
        };
        let s1 = match src2 {
            AsmOperand::Reg(s, ss) if *ss >= 128 => *s,
            _ => {
                self.asm_warn("unsupported FMA src1 pattern");
                return;
            },
        };
        // src1 (AT&T first) = Intel src2 = modrm.r/m
        match src1 {
            AsmOperand::Reg(s2, sz2) if *sz2 >= 128 => {
                // reg-reg-reg: all three are registers
                self.emit_vex4_prefix(d, *s2, 2, w, s1, l, pp);
                self.emit(&[opcode, Self::modrm_reg_reg(d, *s2)]);
            },
            AsmOperand::Mem {
                disp,
                base,
                index,
                scale,
            } => {
                // reg-reg-mem: memory is in AT&T first operand
                self.emit_vex4_prefix(d, *base, 2, w, s1, l, pp);
                self.emit(&[opcode]);
                self.emit_asm_modrm_mem_only(d, *disp, *base, *index, *scale);
            },
            AsmOperand::SegMem {
                seg,
                disp,
                base,
                index,
                scale,
            } => {
                // reg-reg-segmem: 段前缀 + VEX + modrm
                let (_code, pfx) = Self::seg_meta(*seg);
                self.emit(&[pfx]);
                self.emit_vex4_prefix(d, *base, 2, w, s1, l, pp);
                self.emit(&[opcode]);
                self.emit_asm_modrm_mem_only(d, *disp, *base, *index, *scale);
            },
            AsmOperand::SymDisp(sym, _) => {
                // reg-reg-sym: RIP-relative
                self.emit_vex4_prefix(d, 0, 2, w, s1, l, pp);
                self.emit(&[opcode, 0x05 | ((d & 7) << 3), 0, 0, 0, 0]);
                let pos = self.pos() - 4;
                self.pending_data_rels.push((pos, sym.clone()));
            },
            _ => {
                self.asm_warn("unsupported FMA src2 pattern");
            },
        }
    }
    pub(crate) fn emit_asm_fpu_load(
        &mut self,
        op: &AsmOperand,
        opcode: u8,
        ext: u8,
        has_0f: bool,
        rex_w: bool,
    ) {
        if let AsmOperand::Mem {
            disp,
            base,
            index,
            scale,
        } = op
        {
            self.emit(&[Self::rex(rex_w, 0, index.unwrap_or(0), *base)]);
            if has_0f {
                self.emit(&[0x0F]);
            }
            self.emit(&[opcode]);
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
            self.emit(&[pfx, Self::rex(false, 0, index.unwrap_or(0), *base)]);
            if has_0f {
                self.emit(&[0x0F]);
            }
            self.emit(&[opcode]);
            self.emit_asm_modrm_mem_only(ext, *disp, *base, *index, *scale);
        } else if let AsmOperand::SymDisp(sym, _) = op {
            // 全局变量 RIP-relative: mod=00 r/m=101
            self.emit(&[Self::rex(false, 0, 0, 0)]);
            if has_0f {
                self.emit(&[0x0F]);
            }
            self.emit(&[opcode, 0x05 | (ext << 3), 0, 0, 0, 0]);
            let pos = self.pos() - 4;
            self.pending_data_rels.push((pos, sym.clone()));
        } else {
            self.asm_warn("fpu_load only supports mem operand");
        }
    }
    pub(crate) fn emit_asm_mmx_2op(&mut self, src: &AsmOperand, dst: &AsmOperand, opcode: u8) {
        match (src, dst) {
            (AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds))
                if *ss == ASM_REG_MMX && *ds == ASM_REG_MMX =>
            {
                self.emit(&[0x0F, opcode, Self::modrm_reg_reg(*d, *s)]);
            },
            (
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
                AsmOperand::Reg(d, ds),
            ) if *ds == ASM_REG_MMX => {
                self.emit(&[0x0F, opcode]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            },
            _ => {
                self.asm_warn("unsupported MMX operand pattern");
            },
        }
    }
    pub(crate) fn emit_asm_pextr_imm8(
        &mut self,
        imm: i64,
        src_xmm: u8,
        dst: &AsmOperand,
        opcode: u8,
        w: u8,
    ) {
        match dst {
            AsmOperand::Reg(r, sz) if *sz < 128 => {
                self.emit_vex4_prefix(src_xmm, *r, 3, w, 0, 0, 1);
                self.emit(&[opcode, Self::modrm_reg_reg(src_xmm, *r), imm as u8]);
            },
            AsmOperand::Mem {
                disp,
                base,
                index,
                scale,
            } => {
                self.emit_vex4_prefix(src_xmm, *base, 3, w, 0, 0, 1);
                self.emit(&[opcode]);
                self.emit_asm_modrm_mem_only(src_xmm, *disp, *base, *index, *scale);
                self.emit(&[imm as u8]);
            },
            AsmOperand::SymDisp(sym, _) | AsmOperand::Sym(sym) => {
                // RIP-relative dest: mod=00 r/m=101
                self.emit_vex4_prefix(src_xmm, 0, 3, w, 0, 0, 1);
                self.emit(&[opcode, 0x05 | ((src_xmm & 7) << 3), 0, 0, 0, 0, imm as u8]);
                let pos = self.pos() - 5;
                self.pending_data_rels.push((pos, sym.clone()));
            },
            _ => {
                self.asm_warn("pextr dst must be gpr or mem");
            },
        }
    }
    pub(crate) fn emit_asm_pinsr_imm8(
        &mut self,
        imm: i64,
        src: &AsmOperand,
        dst_xmm: u8,
        opcode: u8,
        w: u8,
    ) {
        match src {
            AsmOperand::Reg(r, sz) if *sz < 128 => {
                self.emit_vex4_prefix(dst_xmm, *r, 3, w, 0, 0, 1);
                self.emit(&[opcode, Self::modrm_reg_reg(dst_xmm, *r), imm as u8]);
            },
            AsmOperand::Mem {
                disp,
                base,
                index,
                scale,
            } => {
                self.emit_vex4_prefix(dst_xmm, *base, 3, w, 0, 0, 1);
                self.emit(&[opcode]);
                self.emit_asm_modrm_mem_only(dst_xmm, *disp, *base, *index, *scale);
                self.emit(&[imm as u8]);
            },
            AsmOperand::SymDisp(sym, _) | AsmOperand::Sym(sym) => {
                // RIP-relative source: mod=00 r/m=101
                self.emit_vex4_prefix(dst_xmm, 0, 3, w, 0, 0, 1);
                self.emit(&[opcode, 0x05 | ((dst_xmm & 7) << 3), 0, 0, 0, 0, imm as u8]);
                let pos = self.pos() - 5;
                self.pending_data_rels.push((pos, sym.clone()));
            },
            _ => {
                self.asm_warn("pinsr src must be gpr or mem");
            },
        }
    }
    pub(crate) fn emit_asm_scan(
        &mut self,
        src: &AsmOperand,
        dst: &AsmOperand,
        opcode: u8,
        bits: u8,
    ) {
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
                self.emit(&[Self::rex(w, *d, index.unwrap_or(0), *base), 0x0F, opcode]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            }
            return;
        }
        match (src, dst) {
            (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) => {
                let r = Self::rex(w, *d, 0, *s);
                if r != 0x40 {
                    self.emit(&[r]);
                }
                self.emit(&[0x0F, opcode, Self::modrm_reg_reg(*d, *s)]);
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
                if r != 0x40 {
                    self.emit(&[r]);
                }
                self.emit(&[0x0F, opcode]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            },
            _ => {
                self.asm_warn("unsupported scan operand pattern");
            },
        }
    }
    pub(crate) fn emit_asm_sse_2op_imm_mem(
        &mut self,
        dst: u8,
        src: &AsmOperand,
        map: u8,
        w: u8,
        vvvv: u8,
        l: u8,
        pp: u8,
        opcode: u8,
        imm: i64,
    ) {
        match src {
            AsmOperand::Reg(s, ss) if *ss == 128 => {
                self.emit_vex4_prefix(dst, *s, map, w, vvvv, l, pp);
                self.emit(&[opcode, Self::modrm_reg_reg(dst, *s), imm as u8]);
            },
            AsmOperand::Mem {
                disp,
                base,
                index,
                scale,
            } => {
                self.emit_vex4_prefix(dst, *base, map, w, vvvv, l, pp);
                self.emit(&[opcode]);
                self.emit_asm_modrm_mem_only(dst, *disp, *base, *index, *scale);
                self.emit(&[imm as u8]);
            },
            AsmOperand::SymDisp(sym, _) | AsmOperand::Sym(sym) => {
                // RIP-relative source: mod=00 r/m=101
                if map == 1 {
                    self.emit_vex3_prefix(dst, 0, l, pp);
                } else {
                    self.emit_vex4_prefix(dst, 0, map, w, vvvv, l, pp);
                }
                self.emit(&[opcode, 0x05 | ((dst & 7) << 3), 0, 0, 0, 0, imm as u8]);
                let pos = self.pos() - 5;
                self.pending_data_rels.push((pos, sym.clone()));
            },
            _ => {
                self.asm_warn("src must be xmm or mem");
            },
        }
    }
    pub(crate) fn emit_asm_sse_arith(
        &mut self,
        src: &AsmOperand,
        dst: &AsmOperand,
        prefix: u8,
        opcode: u8,
    ) {
        let is_xmm = |_r: u8, sz: u8| sz == 128;
        // 段前缀 mem src: 段前缀 + 普通 mem sse arith
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
            if let AsmOperand::Reg(d, ds) = dst {
                if is_xmm(*d, *ds) {
                    let mut rex = 0u8;
                    if *d >= 8 {
                        rex |= 0x04;
                    }
                    if *base >= 8 {
                        rex |= 0x01;
                    }
                    if index.unwrap_or(0) >= 8 {
                        rex |= 0x02;
                    }
                    self.emit_sse_header(prefix, rex);
                    self.emit(&[opcode]);
                    self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
                }
            }
            return;
        }
        // 段前缀 mem dst
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
            if let AsmOperand::Reg(s, ss) = src {
                if is_xmm(*s, *ss) {
                    let mut rex = 0u8;
                    if *s >= 8 {
                        rex |= 0x04;
                    }
                    if *base >= 8 {
                        rex |= 0x01;
                    }
                    if index.unwrap_or(0) >= 8 {
                        rex |= 0x02;
                    }
                    self.emit_sse_header(prefix, rex);
                    self.emit(&[opcode]);
                    self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
                }
            }
            return;
        }
        match (src, dst) {
            (AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds))
                if is_xmm(*s, *ss) && is_xmm(*d, *ds) =>
            {
                let mut rex = 0u8;
                if *d >= 8 {
                    rex |= 0x04;
                }
                if *s >= 8 {
                    rex |= 0x01;
                }
                self.emit_sse_header(prefix, rex);
                self.emit(&[opcode, Self::modrm_reg_reg(*d, *s)]);
            },
            (
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
                AsmOperand::Reg(d, ds),
            ) if is_xmm(*d, *ds) => {
                let mut rex = 0u8;
                if *d >= 8 {
                    rex |= 0x04;
                }
                if *base >= 8 {
                    rex |= 0x01;
                }
                if index.unwrap_or(0) >= 8 {
                    rex |= 0x02;
                }
                self.emit_sse_header(prefix, rex);
                self.emit(&[opcode]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            },
            (
                AsmOperand::Reg(s, ss),
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
            ) if is_xmm(*s, *ss) => {
                let mut rex = 0u8;
                if *s >= 8 {
                    rex |= 0x04;
                }
                if *base >= 8 {
                    rex |= 0x01;
                }
                if index.unwrap_or(0) >= 8 {
                    rex |= 0x02;
                }
                self.emit_sse_header(prefix, rex);
                self.emit(&[opcode]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            },
            _ => {
                self.asm_warn("unsupported sse arith operand pattern");
            },
        }
    }
    pub(crate) fn emit_asm_sse_cmp_packed(
        &mut self,
        src: &AsmOperand,
        dst: &AsmOperand,
        imm_op: &AsmOperand,
        prefix: u8,
        opcode: u8,
    ) {
        let imm8 = match imm_op {
            AsmOperand::Imm(v) => *v as u8,
            _ => 0,
        };
        let is_xmm = |_r: u8, sz: u8| sz == 128;
        match (src, dst) {
            (AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds))
                if is_xmm(*s, *ss) && is_xmm(*d, *ds) =>
            {
                let mut rex = 0u8;
                if *d >= 8 {
                    rex |= 0x04;
                }
                if *s >= 8 {
                    rex |= 0x01;
                }
                self.emit_sse_header(prefix, rex);
                self.emit(&[opcode, Self::modrm_reg_reg(*d, *s), imm8]);
            },
            (
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
                AsmOperand::Reg(d, ds),
            ) if is_xmm(*d, *ds) => {
                let mut rex = 0u8;
                if *d >= 8 {
                    rex |= 0x04;
                }
                if *base >= 8 {
                    rex |= 0x01;
                }
                if index.unwrap_or(0) >= 8 {
                    rex |= 0x02;
                }
                self.emit_sse_header(prefix, rex);
                self.emit(&[opcode]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
                self.emit(&[imm8]);
            },
            _ => {
                self.asm_warn("unsupported sse cmp/shuffle operand pattern");
            },
        }
    }
    pub(crate) fn emit_asm_sse_mov(
        &mut self,
        src: &AsmOperand,
        dst: &AsmOperand,
        single: bool,
        to_xmm64: bool,
    ) {
        let prefix = if single && !to_xmm64 { 0xF3u8 } else { 0xF2u8 };
        let is_xmm = |_r: u8, sz: u8| sz == 128;
        match (src, dst) {
            (AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds))
                if is_xmm(*s, *ss) && is_xmm(*d, *ds) =>
            {
                let mut rex = 0u8;
                if *d >= 8 {
                    rex |= 0x04;
                }
                if *s >= 8 {
                    rex |= 0x01;
                }
                self.emit(&[prefix]);
                if rex != 0 {
                    self.emit(&[0x40 | rex]);
                }
                self.emit(&[0x0F, 0x10, Self::modrm_reg_reg(*d, *s)]);
            },
            (
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
                AsmOperand::Reg(d, ds),
            ) if is_xmm(*d, *ds) => {
                let mut rex = 0u8;
                if *d >= 8 {
                    rex |= 0x04;
                }
                if *base >= 8 {
                    rex |= 0x01;
                }
                if index.unwrap_or(0) >= 8 {
                    rex |= 0x02;
                }
                self.emit(&[prefix]);
                if rex != 0 {
                    self.emit(&[0x40 | rex]);
                }
                self.emit(&[0x0F, 0x10]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            },
            (
                AsmOperand::Reg(s, ss),
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
            ) if is_xmm(*s, *ss) => {
                let mut rex = 0u8;
                if *s >= 8 {
                    rex |= 0x04;
                }
                if *base >= 8 {
                    rex |= 0x01;
                }
                if index.unwrap_or(0) >= 8 {
                    rex |= 0x02;
                }
                self.emit(&[prefix]);
                if rex != 0 {
                    self.emit(&[0x40 | rex]);
                }
                self.emit(&[0x0F, 0x11]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            },
            _ => {
                self.asm_warn("unsupported sse mov operand pattern");
            },
        }
    }
    pub(crate) fn emit_asm_sse_mov_packed(
        &mut self,
        src: &AsmOperand,
        dst: &AsmOperand,
        prefix: u8,
        load_op: u8,
        store_op: u8,
    ) {
        let is_xmm = |_r: u8, sz: u8| sz == 128;
        match (src, dst) {
            // mem -> xmm (load)
            (
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
                AsmOperand::Reg(d, ds),
            ) if is_xmm(*d, *ds) => {
                let mut rex = 0u8;
                if *d >= 8 {
                    rex |= 0x04;
                }
                if *base >= 8 {
                    rex |= 0x01;
                }
                if index.unwrap_or(0) >= 8 {
                    rex |= 0x02;
                }
                self.emit_sse_header(prefix, rex);
                self.emit(&[load_op]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            },
            // xmm -> mem (store)
            (
                AsmOperand::Reg(s, ss),
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
            ) if is_xmm(*s, *ss) => {
                let mut rex = 0u8;
                if *s >= 8 {
                    rex |= 0x04;
                }
                if *base >= 8 {
                    rex |= 0x01;
                }
                if index.unwrap_or(0) >= 8 {
                    rex |= 0x02;
                }
                self.emit_sse_header(prefix, rex);
                self.emit(&[store_op]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            },
            // xmm -> xmm (reg-reg move, use load_op)
            (AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds))
                if is_xmm(*s, *ss) && is_xmm(*d, *ds) =>
            {
                let mut rex = 0u8;
                if *d >= 8 {
                    rex |= 0x04;
                }
                if *s >= 8 {
                    rex |= 0x01;
                }
                self.emit_sse_header(prefix, rex);
                self.emit(&[load_op, Self::modrm_reg_reg(*d, *s)]);
            },
            _ => {
                self.asm_warn("unsupported sse packed mov operand pattern");
            },
        }
    }
    pub(crate) fn emit_asm_tileloadd(&mut self, op0: &AsmOperand, op1: &AsmOperand) {
        let (tmm, disp, base, index, scale) = match (op0, op1) {
            (
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
                AsmOperand::Reg(d, sz),
            ) if *sz == ASM_REG_TMM => (*d, *disp, *base, *index, *scale),
            (
                AsmOperand::Reg(d, sz),
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
            ) if *sz == ASM_REG_TMM => (*d, *disp, *base, *index, *scale),
            _ => {
                self.asm_warn("tileloadd needs tmm and mem");
                return;
            },
        };
        self.emit(&[0xC4, 0xE2, 0x7B, 0x4B]);
        self.emit_asm_modrm_mem_only(tmm, disp, base, index, scale);
    }
    pub(crate) fn emit_asm_tilestored(&mut self, op0: &AsmOperand, op1: &AsmOperand) {
        let (tmm, disp, base, index, scale) = match (op0, op1) {
            (
                AsmOperand::Reg(s, sz),
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
            ) if *sz == ASM_REG_TMM => (*s, *disp, *base, *index, *scale),
            (
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
                AsmOperand::Reg(s, sz),
            ) if *sz == ASM_REG_TMM => (*s, *disp, *base, *index, *scale),
            _ => {
                self.asm_warn("tilestored needs tmm and mem");
                return;
            },
        };
        self.emit(&[0xC4, 0xE2, 0x7A, 0x4B]);
        self.emit_asm_modrm_mem_only(tmm, disp, base, index, scale);
    }
    pub(crate) fn emit_asm_vex_2op(
        &mut self,
        src: &AsmOperand,
        dst: &AsmOperand,
        l: u8,
        pp: u8,
        map: u8,
        opcode: u8,
        w: u8,
    ) {
        match (src, dst) {
            (AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) if *ss == 128 && *ds == 128 => {
                if map == 1 {
                    self.emit_vex3_prefix(*d, 0, l, pp);
                } else {
                    self.emit_vex4_prefix(*d, *s, map, w, 0, l, pp);
                }
                self.emit(&[opcode, Self::modrm_reg_reg(*d, *s)]);
            },
            (
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
                AsmOperand::Reg(d, ds),
            ) if *ds == 128 => {
                if map == 1 {
                    self.emit_vex3_prefix(*d, 0, l, pp);
                } else {
                    self.emit_vex4_prefix(*d, *base, map, w, 0, l, pp);
                }
                self.emit(&[opcode]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            },
            (
                AsmOperand::Reg(s, ss),
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
            ) if *ss == 128 => {
                if map == 1 {
                    self.emit_vex3_prefix(*s, 0, l, pp);
                } else {
                    self.emit_vex4_prefix(*s, *base, map, w, 0, l, pp);
                }
                self.emit(&[opcode]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            },
            (AsmOperand::SymDisp(sym, _), AsmOperand::Reg(d, ds)) if *ds == 128 => {
                // VEX RIP-relative load: 走 mod=00 r/m=101 + disp32 (与标量 mov 一致)
                if map == 1 {
                    self.emit_vex3_prefix(*d, 0, l, pp);
                } else {
                    self.emit_vex4_prefix(*d, 0, map, w, 0, l, pp);
                }
                self.emit(&[opcode, 0x05 | ((d & 7) << 3), 0, 0, 0, 0]);
                let pos = self.pos() - 4;
                self.pending_data_rels.push((pos, sym.clone()));
            },
            // SegMem → Reg: 段前缀 + VEX + mem
            (
                AsmOperand::SegMem {
                    seg,
                    disp,
                    base,
                    index,
                    scale,
                },
                AsmOperand::Reg(d, ds),
            ) if *ds == 128 => {
                let (_code, pfx) = Self::seg_meta(*seg);
                self.emit(&[pfx]);
                if map == 1 {
                    self.emit_vex3_prefix(*d, 0, l, pp);
                } else {
                    self.emit_vex4_prefix(*d, *base, map, w, 0, l, pp);
                }
                self.emit(&[opcode]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            },
            // Reg → SegMem: 段前缀 + VEX + mem
            (
                AsmOperand::Reg(s, ss),
                AsmOperand::SegMem {
                    seg,
                    disp,
                    base,
                    index,
                    scale,
                },
            ) if *ss == 128 => {
                let (_code, pfx) = Self::seg_meta(*seg);
                self.emit(&[pfx]);
                if map == 1 {
                    self.emit_vex3_prefix(*s, 0, l, pp);
                } else {
                    self.emit_vex4_prefix(*s, *base, map, w, 0, l, pp);
                }
                self.emit(&[opcode]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            },
            (AsmOperand::Reg(s, ss), AsmOperand::SymDisp(sym, _)) if *ss == 128 => {
                if map == 1 {
                    self.emit_vex3_prefix(*s, 0, l, pp);
                } else {
                    self.emit_vex4_prefix(*s, 0, map, w, 0, l, pp);
                }
                self.emit(&[opcode, 0x05 | ((s & 7) << 3), 0, 0, 0, 0]);
                let pos = self.pos() - 4;
                self.pending_data_rels.push((pos, sym.clone()));
            },
            // ldtilecfg / sttilecfg 形式: (None, Reg) 或 (Reg, None) — vvvv=0
            (AsmOperand::None, AsmOperand::Reg(d, ds)) if *ds >= 128 => {
                if map == 1 {
                    self.emit_vex3_prefix(*d, 0, l, pp);
                } else {
                    self.emit_vex4_prefix(*d, 0, map, w, 0, l, pp);
                }
                self.emit(&[opcode, Self::modrm_reg_reg(0, *d)]);
            },
            (AsmOperand::Reg(s, ss), AsmOperand::None) if *ss >= 128 => {
                if map == 1 {
                    self.emit_vex3_prefix(*s, 0, l, pp);
                } else {
                    self.emit_vex4_prefix(*s, 0, map, w, 0, l, pp);
                }
                self.emit(&[opcode, Self::modrm_reg_reg(0, *s)]);
            },
            // crc32 类 GPR-GPR / GPR-Mem: 无 VEX 前缀, 0F 38 F0/F1 + ModR/M
            // pp=0 → F2 (crc32b/crc32l/crc32q), pp=1 → 66 (crc32w); w=1 → REX.W (crc32q)
            (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) if pp <= 1 => {
                let prefix: u8 = if pp == 0 { 0xF2 } else { 0x66 };
                // mandatory prefix must precede REX
                self.emit(&[prefix]);
                if w == 1 {
                    self.emit(&[0x48]);
                } // REX.W
                self.emit(&[0x0F, 0x38, opcode, Self::modrm_reg_reg(*d, *s)]);
            },
            (
                AsmOperand::Reg(s, _),
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
            ) if pp <= 1 => {
                let prefix: u8 = if pp == 0 { 0xF2 } else { 0x66 };
                self.emit(&[prefix]);
                if w == 1 {
                    self.emit(&[0x48]);
                } // REX.W
                self.emit(&[0x0F, 0x38, opcode]);
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
            ) if pp <= 1 => {
                let prefix: u8 = if pp == 0 { 0xF2 } else { 0x66 };
                self.emit(&[prefix]);
                if w == 1 {
                    self.emit(&[0x48]);
                } // REX.W
                self.emit(&[0x0F, 0x38, opcode]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            },
            _ => {
                self.asm_warn(&format!(
                    "unsupported vex 2-op operand pattern (l:{}, pp:{}, map:{}, opcode:0x{:02x}, w:{}), src:{:?}, dst:{:?}",
                    l, pp, map, opcode, w, src, dst
                ));
                self.emit(&[0x90]);
            },
        }
    }
    pub(crate) fn emit_asm_vex_3op_imm8_check(
        &mut self,
        src1: &AsmOperand,
        src2: &AsmOperand,
        dst: &AsmOperand,
        pp: u8,
        opcode: u8,
        map: u8,
        _is_scalar: bool,
    ) {
        // AT&T: src1=ops[0] (Intel src2=modrm.r/m), src2=ops[1] (Intel src1=VEX.vvvv), dst=ops[2] (Intel dst=modrm.reg)
        let d = match dst {
            AsmOperand::Reg(d, ds) if *ds >= 128 => *d,
            _ => {
                self.asm_warn("unsupported VEX 3-op dst pattern");
                return;
            },
        };
        let s1 = match src2 {
            AsmOperand::Reg(s, ss) if *ss >= 128 => *s,
            _ => {
                self.asm_warn("unsupported VEX 3-op src1 pattern");
                return;
            },
        };
        match src1 {
            AsmOperand::Reg(s2, sz2) if *sz2 >= 128 => {
                self.emit_asm_vex_3op_reg(d, s1, *s2, 0, pp, map, opcode, 0);
            },
            AsmOperand::Mem {
                disp,
                base,
                index,
                scale,
            } => {
                self.emit_asm_vex_3op_mem(
                    d, s1, *disp, *base, *index, *scale, 0, pp, map, opcode, 0,
                );
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
                self.emit_asm_vex_3op_mem(
                    d, s1, *disp, *base, *index, *scale, 0, pp, map, opcode, 0,
                );
            },
            AsmOperand::SymDisp(sym, _) => {
                // VEX 3-op RIP-relative: mod=00 r/m=101 + disp32
                if map == 1 {
                    self.emit_vex3_prefix(d, s1, 0, pp);
                } else {
                    self.emit_vex4_prefix(d, 0, map, 0, s1, 0, pp);
                }
                self.emit(&[opcode, 0x05 | ((d & 7) << 3), 0, 0, 0, 0]);
                let pos = self.pos() - 4;
                self.pending_data_rels.push((pos, sym.clone()));
            },
            _ => {
                self.asm_warn("unsupported VEX 3-op src2 pattern");
            },
        }
    }
    fn emit_asm_vex_3op_mem(
        &mut self,
        dst: u8,
        src1: u8,
        disp: i32,
        base: u8,
        index: Option<u8>,
        scale: u8,
        l: u8,
        pp: u8,
        map: u8,
        opcode: u8,
        w: u8,
    ) {
        if map == 1 {
            self.emit_vex3_prefix(dst, src1, l, pp);
        } else {
            self.emit_vex4_prefix(dst, base, map, w, src1, l, pp);
        }
        self.emit(&[opcode]);
        self.emit_asm_modrm_mem_only(dst, disp, base, index, scale);
    }
    fn emit_asm_vex_3op_reg(
        &mut self,
        dst: u8,
        src1: u8,
        src2: u8,
        l: u8,
        pp: u8,
        map: u8,
        opcode: u8,
        w: u8,
    ) {
        if map == 1 {
            self.emit_vex3_prefix(dst, src1, l, pp);
        } else {
            self.emit_vex4_prefix(dst, src2, map, w, src1, l, pp);
        }
        self.emit(&[opcode, Self::modrm_reg_reg(dst, src2)]);
    }
    pub(crate) fn emit_asm_vmread(&mut self, src: &AsmOperand, dst: &AsmOperand) {
        if let (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) = (src, dst) {
            self.emit(&[
                Self::rex(true, *d, 0, *s),
                0x0F,
                0x78,
                Self::modrm_reg_reg(*d, *s),
            ]);
        } else {
            self.asm_warn("vmread only supports reg,reg operands");
        }
    }
    pub(crate) fn emit_asm_vmwrite(&mut self, src: &AsmOperand, dst: &AsmOperand) {
        if let (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) = (src, dst) {
            self.emit(&[
                Self::rex(true, *s, 0, *d),
                0x0F,
                0x79,
                Self::modrm_reg_reg(*s, *d),
            ]);
        } else {
            self.asm_warn("vmwrite only supports reg,reg operands");
        }
    }
    pub(crate) fn emit_asm_vpextr_imm8(
        &mut self,
        imm: i64,
        src_xmm: u8,
        dst: &AsmOperand,
        opcode: u8,
        w: u8,
    ) {
        match dst {
            AsmOperand::Reg(r, sz) if *sz < 128 => {
                self.emit_vex4_prefix(src_xmm, *r, 3, w, 0, 0, 1);
                self.emit(&[opcode, Self::modrm_reg_reg(src_xmm, *r), imm as u8]);
            },
            AsmOperand::Mem {
                disp,
                base,
                index,
                scale,
            } => {
                self.emit_vex4_prefix(src_xmm, *base, 3, w, 0, 0, 1);
                self.emit(&[opcode]);
                self.emit_asm_modrm_mem_only(src_xmm, *disp, *base, *index, *scale);
                self.emit(&[imm as u8]);
            },
            AsmOperand::SymDisp(sym, _) | AsmOperand::Sym(sym) => {
                // RIP-relative dest: mod=00 r/m=101
                self.emit_vex4_prefix(src_xmm, 0, 3, w, 0, 0, 1);
                self.emit(&[opcode, 0x05 | ((src_xmm & 7) << 3), 0, 0, 0, 0, imm as u8]);
                let pos = self.pos() - 5;
                self.pending_data_rels.push((pos, sym.clone()));
            },
            _ => {
                self.asm_warn("vpextr dst must be gpr or mem");
            },
        }
    }
    pub(crate) fn emit_asm_vpinsr_imm8(
        &mut self,
        imm: i64,
        src: &AsmOperand,
        vvvv: u8,
        dst: u8,
        opcode: u8,
        w: u8,
    ) {
        match src {
            AsmOperand::Reg(r, sz) if *sz < 128 => {
                self.emit_vex4_prefix(dst, *r, 3, w, vvvv, 0, 1);
                self.emit(&[opcode, Self::modrm_reg_reg(dst, *r), imm as u8]);
            },
            AsmOperand::Mem {
                disp,
                base,
                index,
                scale,
            } => {
                self.emit_vex4_prefix(dst, *base, 3, w, vvvv, 0, 1);
                self.emit(&[opcode]);
                self.emit_asm_modrm_mem_only(dst, *disp, *base, *index, *scale);
                self.emit(&[imm as u8]);
            },
            AsmOperand::SymDisp(sym, _) | AsmOperand::Sym(sym) => {
                // RIP-relative source: mod=00 r/m=101
                self.emit_vex4_prefix(dst, 0, 3, w, vvvv, 0, 1);
                self.emit(&[opcode, 0x05 | ((dst & 7) << 3), 0, 0, 0, 0, imm as u8]);
                let pos = self.pos() - 5;
                self.pending_data_rels.push((pos, sym.clone()));
            },
            _ => {
                self.asm_warn("vpinsr src must be gpr or mem");
            },
        }
    }
    pub(crate) fn emit_asm_x87_mem(&mut self, op: &AsmOperand, esc: u8, ext: u8, no_modrm: bool) {
        // x87 内存操作数编码: ESC opcode + ModRM (reg 字段 = ext, rm = 操作数)
        match op {
            AsmOperand::Reg(r, _) => {
                // 寄存器操作数: st(i) 或 %ax (fnstsw)
                if no_modrm {
                    // fnstsw %ax: DF E0
                    self.emit(&[esc, 0xE0]);
                } else {
                    // st(i) 操作数: 编码为 ESC C0+reg (mod=11, rm=reg)
                    self.emit(&[esc, 0xC0 | (ext << 3) | (r & 7)]);
                }
            },
            AsmOperand::Mem {
                disp,
                base,
                index,
                scale,
            } => {
                self.emit(&[esc]);
                self.emit_asm_modrm_mem_only(ext, *disp, *base, *index, *scale);
            },
            _ => {
                self.asm_warn("unsupported x87 operand pattern");
            },
        }
    }
    pub(crate) fn emit_asm_x87_st(
        &mut self,
        src: &AsmOperand,
        dst: &AsmOperand,
        rev_esc: u8,
        rev_ext: u8,
        fwd_esc: u8,
        fwd_ext: u8,
    ) {
        match (src, dst) {
            (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) => {
                // AT&T: fadd %st(1), %st  →  st = st + st(1)
                // dst 是 %st (0), src 是 %st(i)
                if *d == 0 {
                    // st, st(i) 形式: 使用 fwd 编码
                    self.emit(&[fwd_esc, 0xC0 | (fwd_ext << 3) | (s & 7)]);
                } else {
                    // st(i), st 形式: 使用 rev 编码
                    self.emit(&[rev_esc, 0xC0 | (rev_ext << 3) | (d & 7)]);
                }
            },
            _ => {
                self.asm_warn("unsupported x87 st operand pattern");
            },
        }
    }
}
