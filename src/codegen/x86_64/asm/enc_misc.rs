//! asm/enc_misc.rs —— 自历史完整实现（git 28a9063 单文件 Codegen）按职责簇恢复的模块。
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
    /// XSAVE / XRSTOR 家族：`0F AE /ext` 与 `0F C7 /ext`，内存操作数形式。
    ///
    /// AT&T 的 `mnemonic` + `mnemonic64` 两种写法都要认：内核
    /// `arch/x86/kernel/fpu/xstate.h` 把 `REX_SUFFIX` 在 x86-64 展开成 `"64"`，于是 ALTERNATIVE
    /// 的原文与替代码是 `xsave64`/`xsaveopt64`/`xsavec64`/`xsaves64`/`xrstor64`/`xrstors64`；
    /// `64` 后缀只多一个 REX.W（GAS 实测：`xsave64 (%rdx)` = `48 0f ae 32`，
    /// `xsave (%rax)` = `0f ae 20`）。没有这条编码时那一行会被静默丢弃，ALTERNATIVE 条目
    /// 的原文长/替换长都算成 0，`vmlinux.o` 在 objtool 那关报 `empty alternative entry`。
    pub(crate) fn emit_asm_xsave(&mut self, op: &AsmOperand, opcode: u8, ext: u8, rexw: bool) {
        let emit_mem = |c: &mut Self, disp: i32, base: u8, index: Option<u8>, scale: u8| {
            c.emit_insn(&Insn::raw(&[Self::rex(rexw, 0, index.unwrap_or(0), base), 0x0F, opcode]));
            c.emit_asm_modrm_mem_only(ext, disp, base, index, scale);
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
                self.emit_insn(&Insn::raw(&[pfx]));
                emit_mem(self, *disp, *base, *index, *scale);
            },
            AsmOperand::SymDisp(sym, _) => {
                // 符号地址形式（`xsave per_cpu_var`）走 RIP 相对：modrm 0x3D + disp32 重定位，
                // 与 `emit_asm_clflush` 的同一形状。
                if rexw {
                    self.emit_insn(&Insn::raw(&[0x48]));
                }
                self.emit_insn(&Insn::raw(&[0x0F, opcode, 0x3D, 0, 0, 0, 0]));
                let pos = self.pos() - 4;
                self.pending_data_rels.push((pos, sym.clone()));
            },
            _ => {
                self.asm_reject("xsave/xrstor family requires a memory operand");
            },
        }
    }

    /// x87 寄存器栈弹出算术 `<op>p`：`DE /ext`，modrm = `0xC0|(ext<<3)|i`。
    ///
    /// GAS 只认两种写法（实测）：不带操作数与 `%st, %st(i)`，默认 i=1；
    /// `%st(i), %st` 会被 GAS 直接拒 (`operand type mismatch`)。内核
    /// `arch/x86/kernel/fpu/bugs.c` 的 `fix_c11000_37_39` 写的就是 `fsubp %%st,%%st(1)`，
    /// 不认它时那一行被当成 0 字节，整段浮点 bug 修补循环少一条指令。
    pub(crate) fn emit_asm_x87_p(&mut self, ops: &[AsmOperand], ext: u8) {
        let mut idx = 1u8;
        for op in ops {
            match op {
                // `%st` 就是 st(0)，不改变默认的那个弹出对象；带号的才算。
                AsmOperand::Reg(r, _) => {
                    if *r != 0 {
                        idx = *r;
                    }
                },
                _ => {
                    self.asm_reject("x87 <op>p takes only %st register operands");
                    return;
                },
            }
        }
        self.emit_insn(&Insn::raw(&[0xDE, 0xC0 | ((ext & 7) << 3) | (idx & 7)]));
    }

    pub(crate) fn emit_asm_clflush(&mut self, op: &AsmOperand, opt: bool, reg: u8) {
        let emit_mem = |c: &mut Self, disp: i32, base: u8, index: Option<u8>, scale: u8| {
            if opt {
                c.emit_insn(&Insn::raw(&[0x66]));
            }
            c.emit_insn(&Insn::raw(&[Self::rex(false, 0, index.unwrap_or(0), base), 0x0F, 0xAE]));
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
                self.emit_insn(&Insn::raw(&[pfx]));
                emit_mem(self, *disp, *base, *index, *scale);
            },
            AsmOperand::SymDisp(sym, _) => {
                if opt {
                    self.emit_insn(&Insn::raw(&[0x66]));
                }
                self.emit_insn(&Insn::raw(&[0x0F, 0xAE, 0x3D, 0, 0, 0, 0]));
                let pos = self.pos() - 4;
                self.pending_data_rels.push((pos, sym.clone()));
            },
            _ => {
                self.asm_reject("unsupported clflush operand pattern");
            },
        }
    }
    pub(crate) fn emit_asm_fsgsbase(&mut self, op: &AsmOperand, ext: u8) {
        if let AsmOperand::Reg(r, _) = op {
            self.emit_insn(&Insn::raw(&[0xF3]));
            self.emit_insn(&Insn::raw(&[Self::rex(true, 0, 0, *r)]));
            self.emit_insn(&Insn::raw(&[0x0F, 0xAE, ((ext & 7) << 3) | (r & 7)]));
        } else {
            self.asm_reject("fsgsbase requires a register operand");
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
                self.emit_insn(&Insn::raw(&[
                    Self::rex(false, 0, index.unwrap_or(0), *base),
                    0x0F,
                    0x01,
                ]));
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
                self.emit_insn(&Insn::raw(&[
                    pfx,
                    Self::rex(false, 0, index.unwrap_or(0), *base),
                    0x0F,
                    0x01,
                ]));
                self.emit_asm_modrm_mem_only(ext, *disp, *base, *index, *scale);
            },
            _ => {
                self.asm_reject("unsupported gdt operand pattern");
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
                    self.emit_insn(&Insn::raw(&[
                        0x66,
                        Self::rex(false, *d, index.unwrap_or(0), *base),
                        0x0F,
                        0x38,
                        0x82,
                    ]));
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
                    self.emit_insn(&Insn::raw(&[
                        pfx,
                        0x66,
                        Self::rex(false, *d, index.unwrap_or(0), *base),
                    ]));
                    self.emit_insn(&Insn::raw(&[0x0F, 0x38, 0x82]));
                    self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
                },
                _ => {
                    self.asm_reject("invpcid needs memory operand");
                },
            }
        } else {
            self.asm_reject("invpcid needs register operand");
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
            self.emit_insn(&Insn::raw(&[
                Self::rex(false, 0, index.unwrap_or(0), *base),
                0x0F,
                0x18,
            ]));
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
            self.emit_insn(&Insn::raw(&[
                pfx,
                Self::rex(false, 0, index.unwrap_or(0), *base),
                0x0F,
                0x18,
            ]));
            self.emit_asm_modrm_mem_only(hint, *disp, *base, *index, *scale);
        } else if let AsmOperand::SymDisp(sym, _) = op {
            // 全局符号: RIP-relative 0F 18 /hint
            self.emit_insn(&Insn::raw(&[0x0F, 0x18, 0x05 | ((hint & 7) << 3), 0, 0, 0, 0]));
            let pos = self.pos() - 4;
            self.pending_data_rels.push((pos, sym.clone()));
        } else {
            self.asm_reject("unsupported prefetch operand pattern");
        }
    }
    pub(crate) fn emit_asm_rdpid(&mut self, op: &AsmOperand) {
        if let AsmOperand::Reg(r, _) = op {
            self.emit_insn(&Insn::raw(&[0xF3]));
            if *r >= 8 {
                self.emit_insn(&Insn::raw(&[0x41]));
            }
            self.emit_insn(&Insn::raw(&[0x0F, 0xC7, 0xF8 | (r & 7)]));
        } else {
            self.asm_reject("rdpid requires a register operand");
        }
    }
}
