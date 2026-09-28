//! asm/enc_misc.rs —— 自历史完整实现（git 28a9063 单文件 Codegen）按职责簇恢复的模块。
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
    pub(crate) fn emit_asm_clflush(&mut self, op: &AsmOperand, opt: bool, reg: u8) {
        let emit_mem = |c: &mut Self, disp: i32, base: u8, index: Option<u8>, scale: u8| {
            if opt {
                c.emit(&[0x66]);
            }
            c.emit(&[Self::rex(false, 0, index.unwrap_or(0), base), 0x0F, 0xAE]);
            c.emit_asm_modrm_mem_only(reg, disp, base, index, scale);
        };
        match op {
            AsmOperand::Mem {
                disp,
                base,
                index,
                scale,
            } => emit_mem(self, *disp, *base, *index, *scale),
            AsmOperand::SegMem {
                seg,
                disp,
                base,
                index,
                scale,
            } => {
                let (_code, pfx) = Self::seg_meta(*seg);
                self.emit(&[pfx]);
                emit_mem(self, *disp, *base, *index, *scale);
            },
            AsmOperand::SymDisp(sym, _) => {
                if opt {
                    self.emit(&[0x66]);
                }
                self.emit(&[0x0F, 0xAE, 0x3D, 0, 0, 0, 0]);
                let pos = self.pos() - 4;
                self.pending_data_rels.push((pos, sym.clone()));
            },
            _ => {
                self.asm_warn("unsupported clflush operand pattern");
            },
        }
    }
    pub(crate) fn emit_asm_fsgsbase(&mut self, op: &AsmOperand, ext: u8) {
        if let AsmOperand::Reg(r, _) = op {
            self.emit(&[0xF3]);
            self.emit(&[Self::rex(true, 0, 0, *r)]);
            self.emit(&[0x0F, 0xAE, ((ext & 7) << 3) | (r & 7)]);
        } else {
            self.asm_warn("fsgsbase requires a register operand");
        }
    }
    pub(crate) fn emit_asm_gdt(&mut self, op: &AsmOperand, ext: u8) {
        match op {
            AsmOperand::Mem {
                disp,
                base,
                index,
                scale,
            } => {
                self.emit(&[Self::rex(false, 0, index.unwrap_or(0), *base), 0x0F, 0x01]);
                self.emit_asm_modrm_mem_only(ext, *disp, *base, *index, *scale);
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
                    0x01,
                ]);
                self.emit_asm_modrm_mem_only(ext, *disp, *base, *index, *scale);
            },
            _ => {
                self.asm_warn("unsupported gdt operand pattern");
            },
        }
    }
    pub(crate) fn emit_asm_invpcid(&mut self, src: &AsmOperand, dst: &AsmOperand) {
        // AT&T invpcid desc, type — src=mem(desc), dst=reg(type)
        if let AsmOperand::Reg(d, _) = dst {
            match src {
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                } => {
                    self.emit(&[
                        0x66,
                        Self::rex(false, *d, index.unwrap_or(0), *base),
                        0x0F,
                        0x38,
                        0x82,
                    ]);
                    self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
                },
                AsmOperand::SegMem {
                    seg,
                    disp,
                    base,
                    index,
                    scale,
                } => {
                    let (_code, pfx) = Self::seg_meta(*seg);
                    self.emit(&[pfx, 0x66, Self::rex(false, *d, index.unwrap_or(0), *base)]);
                    self.emit(&[0x0F, 0x38, 0x82]);
                    self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
                },
                _ => {
                    self.asm_warn("invpcid needs memory operand");
                },
            }
        } else {
            self.asm_warn("invpcid needs register operand");
        }
    }
    pub(crate) fn emit_asm_prefetch(&mut self, op: &AsmOperand, hint: u8) {
        if let AsmOperand::Mem {
            disp,
            base,
            index,
            scale,
        } = op
        {
            self.emit(&[Self::rex(false, 0, index.unwrap_or(0), *base), 0x0F, 0x18]);
            self.emit_asm_modrm_mem_only(hint, *disp, *base, *index, *scale);
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
                0x18,
            ]);
            self.emit_asm_modrm_mem_only(hint, *disp, *base, *index, *scale);
        } else if let AsmOperand::SymDisp(sym, _) = op {
            // 全局符号: RIP-relative 0F 18 /hint
            self.emit(&[0x0F, 0x18, 0x05 | ((hint & 7) << 3), 0, 0, 0, 0]);
            let pos = self.pos() - 4;
            self.pending_data_rels.push((pos, sym.clone()));
        } else {
            self.asm_warn("unsupported prefetch operand pattern");
        }
    }
    pub(crate) fn emit_asm_rdpid(&mut self, op: &AsmOperand) {
        if let AsmOperand::Reg(r, _) = op {
            self.emit(&[0xF3]);
            if *r >= 8 {
                self.emit(&[0x41]);
            }
            self.emit(&[0x0F, 0xC7, 0xF8 | (r & 7)]);
        } else {
            self.asm_warn("rdpid requires a register operand");
        }
    }
}
