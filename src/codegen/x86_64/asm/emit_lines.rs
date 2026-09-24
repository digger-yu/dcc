//! asm/emit_lines.rs —— 自历史完整实现（git 28a9063 单文件 Codegen）按职责簇恢复的模块。
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
    fn emit_asm_instr(&mut self, mnemonic: &str, ops: &[AsmOperand]) {
        match mnemonic {
            // 无操作数指令
            "nop" => self.emit(&[0x90]),
            // 裸段前缀作为独立指令 (旧式汇编风格)
            "ds" => self.emit(&[0x3E]),
            "es" => self.emit(&[0x26]),
            "cs" => self.emit(&[0x2E]),
            "ss" => self.emit(&[0x36]),
            "fs" => self.emit(&[0x64]),
            "gs" => self.emit(&[0x65]),
            "ret" | "retq" => self.emit_return_tail(),
            "leave" | "leaveq" => self.emit(&[0xC9]),
            "cdq" => self.emit(&[0x99]),
            "cdqe" => self.emit(&[0x48, 0x98]),
            "cqo" => self.emit(&[0x48, 0x99]),
            "cbtw" => self.emit(&[0x66, 0x98]),
            "cwtl" => self.emit(&[0x98]),
            "cltd" => self.emit(&[0x99]),
            "clac" => self.emit(&[0x0F, 0x01, 0xCA]),
            "stac" => self.emit(&[0x0F, 0x01, 0xCB]),
            "cli" => self.emit(&[0xFA]),
            "sti" => self.emit(&[0xFB]),
            "cld" => self.emit(&[0xFC]),
            "std" => self.emit(&[0xFD]),
            "hlt" => self.emit(&[0xF4]),
            "fwait" => self.emit(&[0x9B]),
            // x87 无操作数指令 (D9 编码空间)
            "fnop" => self.emit(&[0xD9, 0xD0]),
            "fchs" => self.emit(&[0xD9, 0xE0]),
            "fabs" => self.emit(&[0xD9, 0xE1]),
            "ftst" => self.emit(&[0xD9, 0xE4]),
            "fxam" => self.emit(&[0xD9, 0xE5]),
            "fld1" => self.emit(&[0xD9, 0xE8]),
            "fldl2t" => self.emit(&[0xD9, 0xE9]),
            "fldl2e" => self.emit(&[0xD9, 0xEA]),
            "fldpi" => self.emit(&[0xD9, 0xEB]),
            "fldlg2" => self.emit(&[0xD9, 0xEC]),
            "fldln2" => self.emit(&[0xD9, 0xED]),
            "fldz" => self.emit(&[0xD9, 0xEE]),
            "f2xm1" => self.emit(&[0xD9, 0xF0]),
            "fyl2x" => self.emit(&[0xD9, 0xF1]),
            "fptan" => self.emit(&[0xD9, 0xF2]),
            "fpatan" => self.emit(&[0xD9, 0xF3]),
            "fxtract" => self.emit(&[0xD9, 0xF4]),
            "fprem1" => self.emit(&[0xD9, 0xF5]),
            "fdecstp" => self.emit(&[0xD9, 0xF6]),
            "fincstp" => self.emit(&[0xD9, 0xF7]),
            "fprem" => self.emit(&[0xD9, 0xF8]),
            "fyl2xp1" => self.emit(&[0xD9, 0xF9]),
            "fsqrt" => self.emit(&[0xD9, 0xFA]),
            "fsincos" => self.emit(&[0xD9, 0xFB]),
            "frndint" => self.emit(&[0xD9, 0xFC]),
            "fscale" => self.emit(&[0xD9, 0xFD]),
            "fsin" => self.emit(&[0xD9, 0xFE]),
            "fcos" => self.emit(&[0xD9, 0xFF]),
            // x87 比较 (无操作数/栈顶)
            "fcom" => self.emit(&[0xD8, 0xD1]),
            "fcomp" => self.emit(&[0xD8, 0xD9]),
            "fcompp" => self.emit(&[0xDE, 0xD9]),
            "fucom" => self.emit(&[0xDD, 0xE1]),
            "fucomp" => self.emit(&[0xDD, 0xE9]),
            "fucomip" => self.emit(&[0xDF, 0xE9]),
            // x87 控制 (无操作数)
            "fninit" => self.emit(&[0xDB, 0xE3]),
            // x87 加载/存储 (单操作数: 内存或 st(i))
            "fld" | "flds" | "fldl" | "fldt" if ops.len() == 1 => {
                self.emit_asm_x87_mem(&ops[0], 0xD9, 0, false)
            },
            "fst" | "fsts" | "fstl" | "fstt" if ops.len() == 1 => {
                self.emit_asm_x87_mem(&ops[0], 0xD9, 2, false)
            },
            "fstp" | "fstps" | "fstpl" | "fstpt" if ops.len() == 1 => {
                self.emit_asm_x87_mem(&ops[0], 0xD9, 3, false)
            },
            "fldcw" if ops.len() == 1 => self.emit_asm_x87_mem(&ops[0], 0xD9, 5, false),
            "fnstcw" if ops.len() == 1 => self.emit_asm_x87_mem(&ops[0], 0xD9, 7, true),
            "fnstsw" if ops.len() == 1 => self.emit_asm_x87_mem(&ops[0], 0xDF, 7, false), // stsw 的编码: DF E0 (ax) 或 DD E0(mem)
            // 整数加载/存储
            "fild" | "fildl" | "filds" if ops.len() == 1 => {
                self.emit_asm_x87_mem(&ops[0], 0xDB, 0, false)
            },
            "fist" | "fistl" | "fists" if ops.len() == 1 => {
                self.emit_asm_x87_mem(&ops[0], 0xDB, 2, false)
            },
            "fistp" | "fistpl" | "fistps" | "fistpq" if ops.len() == 1 => {
                self.emit_asm_x87_mem(&ops[0], 0xDB, 3, false)
            },
            // BCD 加载/存储
            "fbld" if ops.len() == 1 => self.emit_asm_x87_mem(&ops[0], 0xDF, 4, false),
            "fbstp" if ops.len() == 1 => self.emit_asm_x87_mem(&ops[0], 0xDF, 6, false),
            // x87 算术 (单操作数: st(i) 或内存)
            "fadd" | "fadds" | "faddl" if ops.len() == 1 => {
                self.emit_asm_x87_mem(&ops[0], 0xD8, 0, false)
            },
            "fmul" | "fmuls" | "fmull" if ops.len() == 1 => {
                self.emit_asm_x87_mem(&ops[0], 0xD8, 1, false)
            },
            "fsub" | "fsubs" | "fsubl" if ops.len() == 1 => {
                self.emit_asm_x87_mem(&ops[0], 0xD8, 4, false)
            },
            "fsubr" | "fsubrs" | "fsubrl" if ops.len() == 1 => {
                self.emit_asm_x87_mem(&ops[0], 0xD8, 5, false)
            },
            "fdiv" | "fdivs" | "fdivl" if ops.len() == 1 => {
                self.emit_asm_x87_mem(&ops[0], 0xD8, 6, false)
            },
            "fdivr" | "fdivrs" | "fdivrl" if ops.len() == 1 => {
                self.emit_asm_x87_mem(&ops[0], 0xD8, 7, false)
            },
            // x87 双精度内存算术
            "fadd" | "fadds" | "faddl" if ops.len() == 2 => {
                self.emit_asm_x87_st(&ops[0], &ops[1], 0xDC, 0, 0xD8, 0)
            },
            "fmul" | "fmuls" | "fmull" if ops.len() == 2 => {
                self.emit_asm_x87_st(&ops[0], &ops[1], 0xDC, 1, 0xD8, 1)
            },
            "fsub" | "fsubs" | "fsubl" if ops.len() == 2 => {
                self.emit_asm_x87_st(&ops[0], &ops[1], 0xDC, 4, 0xD8, 4)
            },
            "fsubr" | "fsubrs" | "fsubrl" if ops.len() == 2 => {
                self.emit_asm_x87_st(&ops[0], &ops[1], 0xDC, 5, 0xD8, 5)
            },
            "fdiv" | "fdivs" | "fdivl" if ops.len() == 2 => {
                self.emit_asm_x87_st(&ops[0], &ops[1], 0xDC, 6, 0xD8, 6)
            },
            "fdivr" | "fdivrs" | "fdivrl" if ops.len() == 2 => {
                self.emit_asm_x87_st(&ops[0], &ops[1], 0xDC, 7, 0xD8, 7)
            },
            // x87 比较 (单操作数 st(i))
            "fcom" | "fcoms" | "fcoml" if ops.len() == 1 => {
                self.emit_asm_x87_mem(&ops[0], 0xD8, 2, false)
            },
            "fcomp" | "fcomps" | "fcompl" if ops.len() == 1 => {
                self.emit_asm_x87_mem(&ops[0], 0xD8, 3, false)
            },
            "pause" => self.emit(&[0xF3, 0x90]),
            "mfence" => self.emit(&[0x0F, 0xAE, 0xF0]),
            "lfence" => self.emit(&[0x0F, 0xAE, 0xE8]),
            "sfence" => self.emit(&[0x0F, 0xAE, 0xF8]),
            "syscall" => self.emit(&[0x0F, 0x05]),
            "sysenter" => self.emit(&[0x0F, 0x34]),
            "sysexit" => self.emit(&[0x0F, 0x35]),
            "rdtsc" => self.emit(&[0x0F, 0x31]),
            "rdpmc" => self.emit(&[0x0F, 0x33]),
            "xchg%eax,%eax" | "xchg %eax,%eax" => self.emit(&[0x90]), // nop alias
            "ud2" => self.emit(&[0x0F, 0x0B]),
            "clc" => self.emit(&[0xF8]),
            "stc" => self.emit(&[0xF9]),
            "cmc" => self.emit(&[0xF5]),
            "sahf" => self.emit(&[0x9E]),
            "lahf" => self.emit(&[0x9F]),

            // cpuid
            "cpuid" => self.emit(&[0x0F, 0xA2]),

            // 字符串操作
            "movsb" => self.emit(&[0xA4]),
            "movsw" => self.emit(&[0x66, 0xA5]),
            "movsl" | "movsd" => self.emit(&[0xA5]),
            "movsq" => self.emit(&[0x48, 0xA5]),
            "stosb" => self.emit(&[0xAA]),
            "stosw" => self.emit(&[0x66, 0xAB]),
            "stosl" | "stosd" => self.emit(&[0xAB]),
            "stosq" => self.emit(&[0x48, 0xAB]),
            "scasb" => self.emit(&[0xAE]),
            "cmpsb" => self.emit(&[0xA6]),

            // push/pop 寄存器
            "push" | "pushq" | "pushl" | "pushw" if ops.len() == 1 => {
                let sz = if mnemonic == "pushl" {
                    32
                } else if mnemonic == "pushw" {
                    16
                } else {
                    64
                };
                self.emit_asm_push_sized(&ops[0], sz);
            },
            "pop" | "popq" | "popl" | "popw" if ops.len() == 1 => {
                let sz = if mnemonic == "popl" {
                    32
                } else if mnemonic == "popw" {
                    16
                } else {
                    64
                };
                self.emit_asm_pop_sized(&ops[0], sz);
            },

            // 数据移动
            "mov" | "movq" if ops.len() == 2 => self.emit_asm_mov(&ops[0], &ops[1], 64),
            "movl" if ops.len() == 2 => self.emit_asm_mov(&ops[0], &ops[1], 32),
            "movw" if ops.len() == 2 => self.emit_asm_mov(&ops[0], &ops[1], 16),
            // movd: 32-bit 整数与 xmm 之间的传送 (66 0F 6E / 66 0F 7E)
            "movd" if ops.len() == 2 => self.emit_asm_movd(&ops[0], &ops[1]),
            "movb" if ops.len() == 2 => self.emit_asm_mov(&ops[0], &ops[1], 8),
            "movzbl" if ops.len() == 2 => self.emit_asm_movzx(&ops[0], &ops[1], 8, 32),
            "movzbq" if ops.len() == 2 => self.emit_asm_movzx(&ops[0], &ops[1], 8, 64),
            "movzwl" if ops.len() == 2 => self.emit_asm_movzx(&ops[0], &ops[1], 16, 32),
            "movzwq" if ops.len() == 2 => self.emit_asm_movzx(&ops[0], &ops[1], 16, 64),
            "movslq" if ops.len() == 2 => self.emit_asm_movsx(&ops[0], &ops[1], 32, 64),
            "movsbl" if ops.len() == 2 => self.emit_asm_movsx(&ops[0], &ops[1], 8, 32),
            "movsbq" if ops.len() == 2 => self.emit_asm_movsx(&ops[0], &ops[1], 8, 64),
            "movswl" if ops.len() == 2 => self.emit_asm_movsx(&ops[0], &ops[1], 16, 32),
            "lea" | "leaq" if ops.len() == 2 => self.emit_asm_lea(&ops[0], &ops[1], 64),
            "leal" if ops.len() == 2 => self.emit_asm_lea(&ops[0], &ops[1], 32),
            "xchg" | "xchgb" | "xchgw" | "xchgl" | "xchgq" if ops.len() == 2 => {
                let bits = if mnemonic == "xchgb" {
                    8
                } else if mnemonic == "xchgw" {
                    16
                } else if mnemonic == "xchgl" {
                    32
                } else {
                    64
                };
                self.emit_asm_xchg(&ops[0], &ops[1], bits);
            },
            // cmov 全系列条件码 + 可选宽度后缀 (w/l/q)
            m if m.starts_with("cmov") && ops.len() == 2 => {
                let cond = &m[4..];
                let (cc, bits) = Self::parse_cmov_cond(cond);
                if let Some(cc) = cc {
                    self.emit_asm_cmov(&ops[0], &ops[1], cc, bits);
                } else {
                    self.asm_warn(&format!("unsupported cmov condition '{}'", cond));
                }
            },

            // 算术
            "add" | "addq" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 0, 64),
            "addl" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 0, 32),
            "addw" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 0, 16),
            "addb" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 0, 8),
            "adc" | "adcq" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 2, 64),
            "adcl" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 2, 32),
            "adcb" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 2, 8),
            "sbb" | "sbbq" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 3, 64),
            "sbbl" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 3, 32),
            "sbbb" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 3, 8),
            "sub" | "subq" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 5, 64),
            "subl" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 5, 32),
            "subw" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 5, 16),
            "subb" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 5, 8),
            "and" | "andq" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 4, 64),
            "andl" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 4, 32),
            "andw" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 4, 16),
            "andb" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 4, 8),
            "or" | "orq" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 1, 64),
            "orl" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 1, 32),
            "orw" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 1, 16),
            "orb" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 1, 8),
            "xor" | "xorq" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 6, 64),
            "xorl" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 6, 32),
            "xorb" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 6, 8),
            "cmp" | "cmpq" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 7, 64),
            "cmpl" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 7, 32),
            "cmpw" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 7, 16),
            "cmpb" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 7, 8),
            "test" | "testq" if ops.len() == 2 => self.emit_asm_test(&ops[0], &ops[1], 64),
            "testl" if ops.len() == 2 => self.emit_asm_test(&ops[0], &ops[1], 32),
            "testb" if ops.len() == 2 => self.emit_asm_test(&ops[0], &ops[1], 8),
            "imul" if ops.len() == 2 => self.emit_asm_imul2(&ops[0], &ops[1]),
            "imul" if ops.len() == 3 => self.emit_asm_imul3(&ops[0], &ops[1], &ops[2]),
            "mul" | "mulq" if ops.len() == 1 => self.emit_asm_unary_rm(&ops[0], 4, 64),
            "mull" if ops.len() == 1 => self.emit_asm_unary_rm(&ops[0], 4, 32),
            "div" | "divq" if ops.len() == 1 => self.emit_asm_unary_rm(&ops[0], 6, 64),
            "divl" if ops.len() == 1 => self.emit_asm_unary_rm(&ops[0], 6, 32),
            "idiv" | "idivq" if ops.len() == 1 => self.emit_asm_unary_rm(&ops[0], 7, 64),
            "idivl" if ops.len() == 1 => self.emit_asm_unary_rm(&ops[0], 7, 32),
            "neg" | "negq" if ops.len() == 1 => self.emit_asm_unary_rm(&ops[0], 3, 64),
            "negl" if ops.len() == 1 => self.emit_asm_unary_rm(&ops[0], 3, 32),
            "not" | "notq" if ops.len() == 1 => self.emit_asm_unary_rm(&ops[0], 2, 64),
            "notl" if ops.len() == 1 => self.emit_asm_unary_rm(&ops[0], 2, 32),
            "inc" | "incq" | "incb" if ops.len() == 1 => {
                let bits = if mnemonic == "incb" { 8 } else { 64 };
                self.emit_asm_unary_rm(&ops[0], 0, bits);
            },
            "incl" if ops.len() == 1 => self.emit_asm_unary_rm(&ops[0], 0, 32),
            "incw" if ops.len() == 1 => self.emit_asm_unary_rm(&ops[0], 0, 16),
            "dec" | "decq" | "decb" | "decw" if ops.len() == 1 => {
                let bits = if mnemonic == "decb" {
                    8
                } else if mnemonic == "decw" {
                    16
                } else {
                    64
                };
                self.emit_asm_unary_rm(&ops[0], 1, bits);
            },
            "decl" if ops.len() == 1 => self.emit_asm_unary_rm(&ops[0], 1, 32),

            // 旋转
            "rol" | "rolq" if ops.len() == 2 => self.emit_asm_rotate(&ops[0], &ops[1], 0, 64),
            "roll" if ops.len() == 2 => self.emit_asm_rotate(&ops[0], &ops[1], 0, 32),
            "rolb" if ops.len() == 2 => self.emit_asm_rotate(&ops[0], &ops[1], 0, 8),
            "ror" | "rorq" if ops.len() == 2 => self.emit_asm_rotate(&ops[0], &ops[1], 1, 64),
            "rorl" if ops.len() == 2 => self.emit_asm_rotate(&ops[0], &ops[1], 1, 32),
            "rorb" if ops.len() == 2 => self.emit_asm_rotate(&ops[0], &ops[1], 1, 8),

            // 预取 / 系统
            "prefetcht0" if ops.len() == 1 => self.emit_asm_prefetch(&ops[0], 0x18),
            "prefetcht1" if ops.len() == 1 => self.emit_asm_prefetch(&ops[0], 0x19),
            "prefetcht2" if ops.len() == 1 => self.emit_asm_prefetch(&ops[0], 0x1A),
            "prefetchnta" if ops.len() == 1 => self.emit_asm_prefetch(&ops[0], 0x1B),
            "prefetchw" if ops.len() == 1 => self.emit_asm_prefetch(&ops[0], 0x0D),
            // clflush / clflushopt: 0F AE /7 (clflushopt 带 66 前缀); clwb: 66 0F AE /6
            "clflush" if ops.len() == 1 => self.emit_asm_clflush(&ops[0], false, 7),
            "clflushopt" if ops.len() == 1 => self.emit_asm_clflush(&ops[0], true, 7),
            "clwb" if ops.len() == 1 => self.emit_asm_clflush(&ops[0], true, 6),
            // FSGSBASE: F3 0F AE /0-/3 (rdfsbase, rdgsbase, wrfsbase, wrgsbase)
            "rdfsbase" if ops.len() == 1 => self.emit_asm_fsgsbase(&ops[0], 0),
            "rdgsbase" if ops.len() == 1 => self.emit_asm_fsgsbase(&ops[0], 1),
            "wrfsbase" if ops.len() == 1 => self.emit_asm_fsgsbase(&ops[0], 2),
            "wrgsbase" if ops.len() == 1 => self.emit_asm_fsgsbase(&ops[0], 3),
            // 描述符表指令: 0F 00 /2-/5 (lldt, ltr, verr, verw)
            "lldt" if ops.len() == 1 => self.emit_asm_desc(&ops[0], 2),
            "ltr" if ops.len() == 1 => self.emit_asm_desc(&ops[0], 3),
            "verr" if ops.len() == 1 => self.emit_asm_desc(&ops[0], 4),
            "verw" if ops.len() == 1 => self.emit_asm_desc(&ops[0], 5),
            // lar reg, r/m: 0F 02 /r
            "lar" if ops.len() == 2 => {
                if let (AsmOperand::Reg(d, _), AsmOperand::Reg(s, _)) = (&ops[0], &ops[1]) {
                    self.emit(&[
                        Self::rex(false, *d, 0, *s),
                        0x0F,
                        0x02,
                        Self::modrm_reg_reg(*d, *s),
                    ]);
                } else if let (
                    AsmOperand::Reg(d, _),
                    AsmOperand::Mem {
                        disp,
                        base,
                        index,
                        scale,
                    },
                ) = (&ops[0], &ops[1])
                {
                    self.emit(&[Self::rex(false, *d, index.unwrap_or(0), *base), 0x0F, 0x02]);
                    self.emit_asm_modrm_mem_only(*d & 7, *disp, *base, *index, *scale);
                } else {
                    self.asm_warn("lar needs reg, reg/mem operands");
                }
            },
            // 描述符表: str (0F 00 /1) 与 sgdt/sidt/lgdt/lidt (0F 01 /0-/3)
            "str" if ops.len() == 1 => self.emit_asm_desc(&ops[0], 1),
            "sldt" if ops.len() == 1 => self.emit_asm_desc(&ops[0], 0),
            "sgdt" if ops.len() == 1 => self.emit_asm_gdt(&ops[0], 0),
            "sidt" if ops.len() == 1 => self.emit_asm_gdt(&ops[0], 1),
            "lgdt" if ops.len() == 1 => self.emit_asm_gdt(&ops[0], 2),
            "lidt" if ops.len() == 1 => self.emit_asm_gdt(&ops[0], 3),
            // lsl: 0F B3 /r (仅寄存器操作数)
            "lsl" if ops.len() == 2 => self.emit_asm_scan(&ops[0], &ops[1], 0xB3, 64),
            // rdpid: F3 0F C7 /7
            "rdpid" if ops.len() == 1 => self.emit_asm_rdpid(&ops[0]),
            // I/O: outb/outw/outl + 字符串版本; inb/inw/inl
            "outb" if ops.len() == 2 => self.emit_asm_out(&ops[0], &ops[1], 8),
            "outw" if ops.len() == 2 => self.emit_asm_out(&ops[0], &ops[1], 16),
            "outl" if ops.len() == 2 => self.emit_asm_out(&ops[0], &ops[1], 32),
            "outsb" => self.emit(&[0x6E]),
            "outsw" => self.emit(&[0x66, 0x6F]),
            "outsl" => self.emit(&[0x6F]),
            "inb" if ops.len() == 2 => self.emit_asm_in(&ops[0], &ops[1], 8),
            "inw" if ops.len() == 2 => self.emit_asm_in(&ops[0], &ops[1], 16),
            "inl" if ops.len() == 2 => self.emit_asm_in(&ops[0], &ops[1], 32),
            // 裸 in/out (无后缀): 默认 32 位, 支持立即数/DX/内存间接端口
            "in" if ops.len() == 2 => self.emit_asm_in_bare(&ops[0], &ops[1]),
            "out" if ops.len() == 2 => self.emit_asm_out_bare(&ops[0], &ops[1]),
            "insb" => self.emit(&[0x6C]),
            "insw" => self.emit(&[0x66, 0x6D]),
            "insl" => self.emit(&[0x6D]),
            "iret" | "iretl" => self.emit(&[0xCF]),
            "iretq" => self.emit(&[0x48, 0xCF]),
            // 位扫描: tzcnt/lzcnt = F3 + bsf/bsr
            "tzcnt" if ops.len() == 2 => {
                self.emit(&[0xF3]);
                self.emit_asm_scan(&ops[0], &ops[1], 0xBC, 64);
            },
            "lzcnt" if ops.len() == 2 => {
                self.emit(&[0xF3]);
                self.emit_asm_scan(&ops[0], &ops[1], 0xBD, 64);
            },
            // PKRU: rdpkru 0F 01 EE, wrpkru 0F 01 EF
            "rdpkru" => self.emit(&[0x0F, 0x01, 0xEE]),
            "wrpkru" => self.emit(&[0x0F, 0x01, 0xEF]),
            // rdrand: F3 0F C7 /6; rdseed: F3 0F C7 /7 (reg or mem)
            "rdrand" if ops.len() == 1 => match &ops[0] {
                AsmOperand::Reg(d, _) => {
                    self.emit(&[0xF3, 0x0F, 0xC7, 0xC0 | (d & 7)]);
                },
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                } => {
                    self.emit(&[
                        Self::rex(false, 0, index.unwrap_or(0), *base),
                        0xF3,
                        0x0F,
                        0xC7,
                    ]);
                    self.emit_asm_modrm_mem_only(6, *disp, *base, *index, *scale);
                },
                AsmOperand::SegMem {
                    seg: _,
                    disp,
                    base,
                    index,
                    scale,
                } => {
                    self.emit(&[
                        Self::rex(false, 0, index.unwrap_or(0), *base),
                        0xF3,
                        0x0F,
                        0xC7,
                    ]);
                    self.emit_asm_modrm_mem_only(6, *disp, *base, *index, *scale);
                },
                AsmOperand::Sym(sym) | AsmOperand::SymDisp(sym, _) => {
                    // RIP-relative: F3 0F C7 /6 with modrm = 0x35
                    self.emit(&[0x48, 0xF3, 0x0F, 0xC7, 0x35, 0, 0, 0, 0]);
                    let pos = self.pos() - 4;
                    self.pending_data_rels.push((pos, sym.clone()));
                },
                _ => {
                    self.asm_warn("rdrand needs register or memory operand");
                },
            },
            "rdseed" if ops.len() == 1 => match &ops[0] {
                AsmOperand::Reg(d, _) => {
                    self.emit(&[0xF3, 0x0F, 0xC7, 0xC0 | (d & 7) | (1 << 3)]);
                },
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                } => {
                    self.emit(&[
                        Self::rex(false, 0, index.unwrap_or(0), *base),
                        0xF3,
                        0x0F,
                        0xC7,
                    ]);
                    self.emit_asm_modrm_mem_only(7, *disp, *base, *index, *scale);
                },
                AsmOperand::SegMem {
                    seg: _,
                    disp,
                    base,
                    index,
                    scale,
                } => {
                    self.emit(&[
                        Self::rex(false, 0, index.unwrap_or(0), *base),
                        0xF3,
                        0x0F,
                        0xC7,
                    ]);
                    self.emit_asm_modrm_mem_only(7, *disp, *base, *index, *scale);
                },
                AsmOperand::Sym(sym) | AsmOperand::SymDisp(sym, _) => {
                    // RIP-relative: F3 0F C7 /7 with modrm = 0x3D
                    self.emit(&[0x48, 0xF3, 0x0F, 0xC7, 0x3D, 0, 0, 0, 0]);
                    let pos = self.pos() - 4;
                    self.pending_data_rels.push((pos, sym.clone()));
                },
                _ => {
                    self.asm_warn("rdseed needs register or memory operand");
                },
            },
            "invlpg" if ops.len() == 1 => self.emit_asm_sys_mem(&ops[0], 0x01, 0x01),
            "invlpga" => self.emit(&[0x0F, 0x01, 0xDF]),
            "swapgs" => self.emit(&[0x0F, 0x01, 0xF8]),
            "monitor" => self.emit(&[0x0F, 0x01, 0xC8]),
            "mwait" => self.emit(&[0x0F, 0x01, 0xC9]),
            "monitorx" => self.emit(&[0x0F, 0x01, 0xFA]),
            "mwaitx" => self.emit(&[0x0F, 0x01, 0xFB]),
            "wbinvd" => self.emit(&[0x0F, 0x09]),
            "invd" => self.emit(&[0x0F, 0x08]),
            "cpuid" => self.emit(&[0x0F, 0xA2]),
            "rdtsc" => self.emit(&[0x0F, 0x31]),
            "rdtscp" => self.emit(&[0x0F, 0x01, 0xF9]),
            "rdpmc" => self.emit(&[0x0F, 0x33]),
            "syscall" => self.emit(&[0x0F, 0x05]),
            "sysret" => self.emit(&[0x0F, 0x07]),
            "sysenter" => self.emit(&[0x0F, 0x34]),
            "sysexit" => self.emit(&[0x0F, 0x35]),
            "vmlaunch" => self.emit(&[0x0F, 0x01, 0xC2]),
            "vmresume" => self.emit(&[0x0F, 0x01, 0xC3]),
            "vmxoff" => self.emit(&[0x0F, 0x01, 0xC4]),
            "vmptrld" if ops.len() == 1 => self.emit_asm_sys_mem(&ops[0], 0x01, 0x03),
            "vmptrst" if ops.len() == 1 => self.emit_asm_sys_mem(&ops[0], 0x01, 0x07),
            "vmclear" if ops.len() == 1 => self.emit_asm_sys_mem(&ops[0], 0x01, 0x04),
            "vmread" if ops.len() == 2 => self.emit_asm_vmread(&ops[0], &ops[1]),
            "vmwrite" if ops.len() == 2 => self.emit_asm_vmwrite(&ops[0], &ops[1]),
            // vmxon: F3 0F C7 /6 (memory operand)
            "vmxon" if ops.len() == 1 => {
                if let AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                } = &ops[0]
                {
                    self.emit(&[
                        0xF3,
                        Self::rex(false, 0, index.unwrap_or(0), *base),
                        0x0F,
                        0xC7,
                    ]);
                    self.emit_asm_modrm_mem_only(6, *disp, *base, *index, *scale);
                } else {
                    self.asm_warn("vmxon needs memory operand");
                }
            },
            // wrussq: 0F 38 F5 /r (reg, mem) — CET user shadow stack write
            "wrussq" if ops.len() == 2 => {
                if let (
                    AsmOperand::Reg(s, _),
                    AsmOperand::Mem {
                        disp,
                        base,
                        index,
                        scale,
                    },
                ) = (&ops[0], &ops[1])
                {
                    self.emit(&[
                        Self::rex(true, *s, index.unwrap_or(0), *base),
                        0x0F,
                        0x38,
                        0xF5,
                    ]);
                    self.emit_asm_modrm_mem_only(*s & 7, *disp, *base, *index, *scale);
                } else {
                    self.asm_warn("wrussq needs reg, mem operands");
                }
            },
            "ud2" => self.emit(&[0x0F, 0x0B]),
            "hlt" => self.emit(&[0xF4]),
            "cli" => self.emit(&[0xFA]),
            "sti" => self.emit(&[0xFB]),
            "cld" => self.emit(&[0xFC]),
            "std" => self.emit(&[0xFD]),
            "clc" => self.emit(&[0xF8]),
            "stc" => self.emit(&[0xF9]),
            "cmc" => self.emit(&[0xF5]),
            "lahf" => self.emit(&[0x9F]),
            "sahf" => self.emit(&[0x9E]),
            "pushf" | "pushfl" => self.emit(&[0x9C]),
            "popf" | "popfl" => self.emit(&[0x9D]),
            "pushfq" => self.emit(&[0x9C]),
            "popfq" => self.emit(&[0x9D]),
            "stac" => self.emit(&[0x0F, 0x01, 0xCB]),
            "clac" => self.emit(&[0x0F, 0x01, 0xCA]),
            "endbr64" => self.emit(&[0xF3, 0x0F, 0x1E, 0xFA]),
            "endbr32" => self.emit(&[0xF3, 0x0F, 0x1E, 0xFB]),
            // fxsave/fxrstor = 0F AE /0 /1, ldmxcsr/stmxcsr = 0F AE /2 /3
            "fxsave" if ops.len() == 1 => self.emit_asm_fpu_load(&ops[0], 0xAE, 0, true, false),
            "fxrstor" if ops.len() == 1 => self.emit_asm_fpu_load(&ops[0], 0xAE, 1, true, false),
            "fxsaveq" if ops.len() == 1 => self.emit_asm_fpu_load(&ops[0], 0xAE, 0, true, true),
            "fxrstorq" if ops.len() == 1 => self.emit_asm_fpu_load(&ops[0], 0xAE, 1, true, true),
            "ldmxcsr" if ops.len() == 1 => self.emit_asm_fpu_load(&ops[0], 0xAE, 2, true, false),
            "stmxcsr" if ops.len() == 1 => self.emit_asm_fpu_load(&ops[0], 0xAE, 3, true, false),
            // fnsave/fnstenv = D9/DD /6 (无 0F 前缀)
            "fnsave" if ops.len() == 1 => self.emit_asm_fpu_load(&ops[0], 0xDD, 6, false, false),
            "fnstenv" if ops.len() == 1 => self.emit_asm_fpu_load(&ops[0], 0xD9, 6, false, false),
            "lfence" => self.emit(&[0x0F, 0xAE, 0xE8]),
            "sfence" => self.emit(&[0x0F, 0xAE, 0xF8]),
            "mfence" => self.emit(&[0x0F, 0xAE, 0xF0]),
            "pause" => self.emit(&[0xF3, 0x90]),
            "int3" => self.emit(&[0xCC]),
            "int" if ops.len() == 1 => self.emit_asm_int(&ops[0]),

            // 标量 SSE（内联 asm 约束常用）
            "movss" if ops.len() == 2 => self.emit_asm_sse_mov(&ops[0], &ops[1], true, false),
            "movsd" if ops.len() == 2 => self.emit_asm_sse_mov(&ops[0], &ops[1], false, false),
            "movssq" if ops.len() == 2 => self.emit_asm_sse_mov(&ops[0], &ops[1], true, true),
            "addss" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0xF3, 0x58),
            "subss" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0xF3, 0x5C),
            "mulss" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0xF3, 0x59),
            "divss" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0xF3, 0x5E),
            "addsd" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0xF2, 0x58),
            "subsd" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0xF2, 0x5C),
            "mulsd" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0xF2, 0x59),
            "divsd" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0xF2, 0x5E),
            "xorps" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x0F, 0x57),
            "xorpd" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x66, 0x57),
            "ucomiss" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x0F, 0x2E),
            "ucomisd" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x66, 0x2E),
            // 打包 SSE (packed) 算术 / 逻辑 / 移动
            "addps" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x0F, 0x58),
            "addpd" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x66, 0x58),
            "subps" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x0F, 0x5C),
            "subpd" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x66, 0x5C),
            "mulps" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x0F, 0x59),
            "mulpd" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x66, 0x59),
            "divps" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x0F, 0x5E),
            "divpd" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x66, 0x5E),
            "andps" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x0F, 0x54),
            "andpd" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x66, 0x54),
            "andnps" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x0F, 0x55),
            "andnpd" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x66, 0x55),
            "orps" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x0F, 0x56),
            "orpd" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x66, 0x56),
            "minps" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x0F, 0x5D),
            "minpd" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x66, 0x5D),
            "maxps" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x0F, 0x5F),
            "maxpd" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x66, 0x5F),
            "sqrtps" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x0F, 0x51),
            "sqrtpd" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x66, 0x51),
            "rcpps" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x0F, 0x53),
            "rsqrtps" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x0F, 0x52),
            "cmpps" if ops.len() == 3 => {
                self.emit_asm_sse_cmp_packed(&ops[0], &ops[1], &ops[2], 0x0F, 0xC2)
            },
            "cmppd" if ops.len() == 3 => {
                self.emit_asm_sse_cmp_packed(&ops[0], &ops[1], &ops[2], 0x66, 0xC2)
            },
            "movaps" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x0F, 0x28),
            "movups" if ops.len() == 2 => {
                self.emit_asm_sse_mov_packed(&ops[0], &ops[1], 0x0F, 0x10, 0x11)
            },
            "movapd" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x66, 0x28),
            "movupd" if ops.len() == 2 => {
                self.emit_asm_sse_mov_packed(&ops[0], &ops[1], 0x66, 0x10, 0x11)
            },
            "movdqa" if ops.len() == 2 => {
                self.emit_asm_sse_mov_packed(&ops[0], &ops[1], 0x66, 0x6F, 0x7F)
            },
            "movdqu" if ops.len() == 2 => {
                self.emit_asm_sse_mov_packed(&ops[0], &ops[1], 0xF3, 0x6F, 0x7F)
            },
            "movhlps" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x0F, 0x12),
            "movlhps" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x0F, 0x16),
            "movhps" if ops.len() == 2 => {
                self.emit_asm_sse_mov_packed(&ops[0], &ops[1], 0x0F, 0x16, 0x17)
            },
            "movlps" if ops.len() == 2 => {
                self.emit_asm_sse_mov_packed(&ops[0], &ops[1], 0x0F, 0x12, 0x13)
            },
            "movhpd" if ops.len() == 2 => {
                self.emit_asm_sse_mov_packed(&ops[0], &ops[1], 0x66, 0x16, 0x17)
            },
            "movlpd" if ops.len() == 2 => {
                self.emit_asm_sse_mov_packed(&ops[0], &ops[1], 0x66, 0x12, 0x13)
            },
            "shufps" if ops.len() == 3 => {
                self.emit_asm_sse_cmp_packed(&ops[0], &ops[1], &ops[2], 0x0F, 0xC6)
            },
            "shufpd" if ops.len() == 3 => {
                self.emit_asm_sse_cmp_packed(&ops[0], &ops[1], &ops[2], 0x66, 0xC6)
            },
            "unpckhps" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x0F, 0x15),
            "unpcklps" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x0F, 0x14),
            "unpckhpd" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x66, 0x15),
            "unpcklpd" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x66, 0x14),
            "cvtps2pd" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x0F, 0x5A),
            "cvtpd2ps" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x66, 0x5A),
            "cvtdq2ps" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x0F, 0x5B),
            "cvtps2dq" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x66, 0x5B),
            "cvttps2dq" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0xF3, 0x5B),
            "cvtdq2pd" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0xF3, 0xE6),
            "cvtpd2dq" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0xF2, 0xE6),
            "cvttpd2dq" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x66, 0xE6),

            // 移位
            "shl" | "sal" | "shlq" if ops.len() == 2 => {
                self.emit_asm_shift(&ops[0], &ops[1], 4, 64)
            },
            "shll" | "sall" if ops.len() == 2 => self.emit_asm_shift(&ops[0], &ops[1], 4, 32),
            "shr" | "shrq" if ops.len() == 2 => self.emit_asm_shift(&ops[0], &ops[1], 5, 64),
            "shrl" if ops.len() == 2 => self.emit_asm_shift(&ops[0], &ops[1], 5, 32),
            "sar" | "sarq" if ops.len() == 2 => self.emit_asm_shift(&ops[0], &ops[1], 7, 64),
            "sarl" if ops.len() == 2 => self.emit_asm_shift(&ops[0], &ops[1], 7, 32),

            // 双精度移位 / 位测试
            "shld" | "shldq" if ops.len() == 3 => {
                self.emit_asm_shld_shrd(&ops[0], &ops[1], &ops[2], true, 64)
            },
            "shldl" if ops.len() == 3 => {
                self.emit_asm_shld_shrd(&ops[0], &ops[1], &ops[2], true, 32)
            },
            "shldw" if ops.len() == 3 => {
                self.emit_asm_shld_shrd(&ops[0], &ops[1], &ops[2], true, 16)
            },
            "shrd" | "shrdq" if ops.len() == 3 => {
                self.emit_asm_shld_shrd(&ops[0], &ops[1], &ops[2], false, 64)
            },
            "shrdl" if ops.len() == 3 => {
                self.emit_asm_shld_shrd(&ops[0], &ops[1], &ops[2], false, 32)
            },
            "shrdw" if ops.len() == 3 => {
                self.emit_asm_shld_shrd(&ops[0], &ops[1], &ops[2], false, 16)
            },
            "bt" | "btq" if ops.len() == 2 => self.emit_asm_bit(&ops[0], &ops[1], 0xA3, 64),
            "btl" if ops.len() == 2 => self.emit_asm_bit(&ops[0], &ops[1], 0xA3, 32),
            "btw" if ops.len() == 2 => self.emit_asm_bit(&ops[0], &ops[1], 0xA3, 16),
            "bts" | "btsq" if ops.len() == 2 => self.emit_asm_bit(&ops[0], &ops[1], 0xAB, 64),
            "btsl" if ops.len() == 2 => self.emit_asm_bit(&ops[0], &ops[1], 0xAB, 32),
            "btr" | "btrq" if ops.len() == 2 => self.emit_asm_bit(&ops[0], &ops[1], 0xB3, 64),
            "btrl" if ops.len() == 2 => self.emit_asm_bit(&ops[0], &ops[1], 0xB3, 32),
            "btrw" if ops.len() == 2 => self.emit_asm_bit(&ops[0], &ops[1], 0xB3, 16),
            "btc" | "btcq" if ops.len() == 2 => self.emit_asm_bit(&ops[0], &ops[1], 0xBB, 64),
            "btcl" if ops.len() == 2 => self.emit_asm_bit(&ops[0], &ops[1], 0xBB, 32),
            "btcw" if ops.len() == 2 => self.emit_asm_bit(&ops[0], &ops[1], 0xBB, 16),

            // 位扫描 / popcnt / bswap
            "bsf" | "bsfq" if ops.len() == 2 => self.emit_asm_scan(&ops[0], &ops[1], 0xBC, 64),
            "bsfl" if ops.len() == 2 => self.emit_asm_scan(&ops[0], &ops[1], 0xBC, 32),
            "bsr" | "bsrq" if ops.len() == 2 => self.emit_asm_scan(&ops[0], &ops[1], 0xBD, 64),
            "bsrl" if ops.len() == 2 => self.emit_asm_scan(&ops[0], &ops[1], 0xBD, 32),
            "popcnt" | "popcntq" if ops.len() == 2 => self.emit_asm_popcnt(&ops[0], &ops[1], 64),
            "popcntl" if ops.len() == 2 => self.emit_asm_popcnt(&ops[0], &ops[1], 32),
            "bswap" | "bswapq" if ops.len() == 1 => self.emit_asm_bswap(&ops[0], 64),
            "bswapl" if ops.len() == 1 => self.emit_asm_bswap(&ops[0], 32),

            // 非 temporal store / 虚拟化
            "movnti" | "movntil" if ops.len() == 2 => self.emit_asm_movnt(&ops[0], &ops[1], 32),
            "movntiq" if ops.len() == 2 => self.emit_asm_movnt(&ops[0], &ops[1], 64),
            // invpcid: 66 0F 38 82 /r (reg, m128)
            "invpcid" if ops.len() == 2 => self.emit_asm_invpcid(&ops[0], &ops[1]),
            "vmcall" => self.emit(&[0x0F, 0x01, 0xC1]),
            "vmmcall" => self.emit(&[0x0F, 0x01, 0xD9]),
            // SVM: clgi (0F 01 DD), stgi (0F 01 DC), vmsave (0F 01 DB)
            "clgi" => self.emit(&[0x0F, 0x01, 0xDD]),
            "stgi" => self.emit(&[0x0F, 0x01, 0xDC]),
            "vmsave" if ops.len() == 1 => self.emit_asm_sys_mem(&ops[0], 0x01, 0x06),

            // 跳转
            "jmp" if ops.len() == 1 => self.emit_asm_jmp(&ops[0]),
            "je" | "jz" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x84),
            "jne" | "jnz" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x85),
            "jl" | "jnge" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x8C),
            "jle" | "jng" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x8E),
            "jg" | "jnle" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x8F),
            "jge" | "jnl" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x8D),
            "ja" | "jnbe" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x87),
            "jae" | "jnb" | "jnc" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x83),
            "jb" | "jnae" | "jc" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x82),
            "jbe" | "jna" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x86),
            "js" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x88),
            "jns" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x89),
            "jo" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x80),
            "jno" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x81),

            // 调用
            "call" if ops.len() == 1 => self.emit_asm_call(&ops[0]),

            // 远跳转/远调用/远返回 (far jumps/calls/returns)
            "retf" | "retfw" | "retfl" | "retfq" if ops.is_empty() => self.emit(&[0xCB]),
            "retf" | "retfw" | "retfl" | "retfq" if ops.len() == 1 => {
                match &ops[0] {
                    AsmOperand::Imm(v) => {
                        self.emit(&[0xCA]);
                        self.emit(&(*v as i16).to_le_bytes());
                    },
                    AsmOperand::Sym(sym) | AsmOperand::SymDisp(sym, _) => {
                        // retf $sym: CA iw with link-time relocation
                        self.emit(&[0xCA, 0x00, 0x00]);
                        let pos = self.pos() - 2;
                        self.pending_data_rels.push((pos, sym.clone()));
                    },
                    _ => {
                        self.asm_warn("retf needs imm16");
                    },
                }
            },
            "lret" | "lretw" | "lretl" | "lretq" if ops.is_empty() => self.emit(&[0xCB]),
            "lret" | "lretw" | "lretl" | "lretq" if ops.len() == 1 => {
                match &ops[0] {
                    AsmOperand::Imm(v) => {
                        self.emit(&[0xCA]);
                        self.emit(&(*v as i16).to_le_bytes());
                    },
                    AsmOperand::Sym(sym) | AsmOperand::SymDisp(sym, _) => {
                        // lret $sym: CA iw with link-time relocation
                        self.emit(&[0xCA, 0x00, 0x00]);
                        let pos = self.pos() - 2;
                        self.pending_data_rels.push((pos, sym.clone()));
                    },
                    _ => {
                        self.asm_warn("lret needs imm16");
                    },
                }
            },
            "ljmpw" if ops.len() == 2 => match (&ops[0], &ops[1]) {
                (AsmOperand::Imm(seg), AsmOperand::Imm(off)) => {
                    self.emit(&[0xEA]);
                    self.emit(&(*off as i16).to_le_bytes());
                    self.emit(&(*seg as i16).to_le_bytes());
                },
                (AsmOperand::Imm(seg), AsmOperand::Sym(off_sym))
                | (AsmOperand::Imm(seg), AsmOperand::SymDisp(off_sym, _)) => {
                    self.emit(&[0xEA]);
                    self.emit(&[0x00, 0x00]);
                    self.emit(&(*seg as i16).to_le_bytes());
                    let pos = self.pos() - 4;
                    self.pending_data_rels.push((pos, off_sym.clone()));
                },
                _ => {
                    self.asm_warn("ljmpw needs $seg, $off");
                },
            },
            "ljmpl" | "ljmp" if ops.len() == 2 => match (&ops[0], &ops[1]) {
                (AsmOperand::Imm(seg), AsmOperand::Imm(off)) => {
                    self.emit(&[0xEA]);
                    self.emit(&(*off as i32).to_le_bytes());
                    self.emit(&(*seg as i16).to_le_bytes());
                },
                (AsmOperand::Imm(seg), AsmOperand::Sym(off_sym))
                | (AsmOperand::Imm(seg), AsmOperand::SymDisp(off_sym, _)) => {
                    self.emit(&[0xEA]);
                    self.emit(&[0x00, 0x00, 0x00, 0x00]);
                    self.emit(&(*seg as i16).to_le_bytes());
                    let pos = self.pos() - 6;
                    self.pending_data_rels.push((pos, off_sym.clone()));
                },
                _ => {
                    self.asm_warn("ljmpl needs $seg, $off");
                },
            },
            // ljmpw/ljmpl/ljmp 间接形式: FF /5 (mem)
            "ljmpw" if ops.len() == 1 => {
                self.emit_asm_far_indirect(&ops[0], false, 16);
            },
            "ljmpl" | "ljmp" if ops.len() == 1 => {
                self.emit_asm_far_indirect(&ops[0], false, 32);
            },
            "lcallw" if ops.len() == 2 => match (&ops[0], &ops[1]) {
                (AsmOperand::Imm(seg), AsmOperand::Imm(off)) => {
                    self.emit(&[0x9A]);
                    self.emit(&(*off as i16).to_le_bytes());
                    self.emit(&(*seg as i16).to_le_bytes());
                },
                (AsmOperand::Imm(seg), AsmOperand::Sym(off_sym))
                | (AsmOperand::Imm(seg), AsmOperand::SymDisp(off_sym, _)) => {
                    self.emit(&[0x9A]);
                    self.emit(&[0x00, 0x00]);
                    self.emit(&(*seg as i16).to_le_bytes());
                    let pos = self.pos() - 4;
                    self.pending_data_rels.push((pos, off_sym.clone()));
                },
                _ => {
                    self.asm_warn("lcallw needs $seg, $off");
                },
            },
            "lcalll" | "lcall" if ops.len() == 2 => match (&ops[0], &ops[1]) {
                (AsmOperand::Imm(seg), AsmOperand::Imm(off)) => {
                    self.emit(&[0x9A]);
                    self.emit(&(*off as i32).to_le_bytes());
                    self.emit(&(*seg as i16).to_le_bytes());
                },
                (AsmOperand::Imm(seg), AsmOperand::Sym(off_sym))
                | (AsmOperand::Imm(seg), AsmOperand::SymDisp(off_sym, _)) => {
                    self.emit(&[0x9A]);
                    self.emit(&[0x00, 0x00, 0x00, 0x00]);
                    self.emit(&(*seg as i16).to_le_bytes());
                    let pos = self.pos() - 6;
                    self.pending_data_rels.push((pos, off_sym.clone()));
                },
                _ => {
                    self.asm_warn("lcalll needs $seg, $off");
                },
            },
            // lcallw/lcalll/lcall 间接形式: FF /3 (mem)
            "lcallw" if ops.len() == 1 => {
                self.emit_asm_far_indirect(&ops[0], true, 16);
            },
            "lcalll" | "lcall" if ops.len() == 1 => {
                self.emit_asm_far_indirect(&ops[0], true, 32);
            },

            // 设置字节 (setcc)
            "sete" | "setz" if ops.len() == 1 => self.emit_asm_setcc(&ops[0], 0x94),
            "setne" | "setnz" if ops.len() == 1 => self.emit_asm_setcc(&ops[0], 0x95),
            "setl" | "setnge" if ops.len() == 1 => self.emit_asm_setcc(&ops[0], 0x9C),
            "setg" | "setnle" if ops.len() == 1 => self.emit_asm_setcc(&ops[0], 0x9F),
            "setle" | "setng" if ops.len() == 1 => self.emit_asm_setcc(&ops[0], 0x9E),
            "setge" | "setnl" if ops.len() == 1 => self.emit_asm_setcc(&ops[0], 0x9D),
            "setb" | "setc" if ops.len() == 1 => self.emit_asm_setcc(&ops[0], 0x92),
            "seta" if ops.len() == 1 => self.emit_asm_setcc(&ops[0], 0x97),
            "setae" | "setnb" | "setnc" if ops.len() == 1 => self.emit_asm_setcc(&ops[0], 0x93),
            "setbe" | "setna" if ops.len() == 1 => self.emit_asm_setcc(&ops[0], 0x96),
            "setns" if ops.len() == 1 => self.emit_asm_setcc(&ops[0], 0x99),
            "setno" if ops.len() == 1 => self.emit_asm_setcc(&ops[0], 0x91),
            "setp" | "setpe" if ops.len() == 1 => self.emit_asm_setcc(&ops[0], 0x9A),
            "setnp" | "setpo" if ops.len() == 1 => self.emit_asm_setcc(&ops[0], 0x9B),

            // 原子操作
            "cmpxchg" | "cmpxchgb" | "cmpxchgw" | "cmpxchgl" | "cmpxchgq" if ops.len() == 2 => {
                let bits = if mnemonic == "cmpxchgb" {
                    8
                } else if mnemonic == "cmpxchgw" {
                    16
                } else if mnemonic == "cmpxchgl" {
                    32
                } else {
                    64
                };
                self.emit_asm_cmpxchg(&ops[0], &ops[1], bits);
            },
            "xadd" | "xaddb" | "xaddw" | "xaddl" | "xaddq" if ops.len() == 2 => {
                let bits = if mnemonic == "xaddb" {
                    8
                } else if mnemonic == "xaddw" {
                    16
                } else if mnemonic == "xaddl" {
                    32
                } else {
                    64
                };
                self.emit_asm_xadd(&ops[0], &ops[1], bits);
            },
            // cmpxchg16b: 0F C7 /1, 仅内存操作数 (需 lock 前缀)
            "cmpxchg16b" if ops.len() == 1 => self.emit_asm_cmpxchg16b(&ops[0]),
            // cmpxchg8b: 0F C7 /1 (无 REX), 仅内存操作数 (32 位原子)
            "cmpxchg8b" if ops.len() == 1 => self.emit_asm_cmpxchg8b(&ops[0]),

            // 特殊寄存器
            "rdmsr" => self.emit(&[0x0F, 0x32]),
            "wrmsr" => self.emit(&[0x0F, 0x30]),
            "xgetbv" => self.emit(&[0x0F, 0x01, 0xD0]),
            "xsetbv" => self.emit(&[0x0F, 0x01, 0xD1]),
            "movq" if ops.len() == 2 && mnemonic == "movq" => {
                self.emit_asm_mov(&ops[0], &ops[1], 64)
            },

            // === TSX (Transactional Synchronization Extensions) ===
            // xbegin rel16/rel32: C7 F8 iw/id
            "xbegin" if ops.len() == 1 => {
                if let AsmOperand::Imm(v) = &ops[0] {
                    self.emit(&[0xC7, 0xF8]);
                    if *v >= i16::MIN as i64 && *v <= i16::MAX as i64 {
                        self.emit(&(*v as i16).to_le_bytes());
                    } else {
                        // rel32 形式
                        self.emit(&(*v as i32).to_le_bytes());
                    }
                } else if let AsmOperand::Sym(s) = &ops[0] {
                    let at = self.pos();
                    self.emit(&[0xC7, 0xF8, 0, 0, 0, 0]);
                    self.pending.push(PendingJump {
                        at,
                        target: s.clone(),
                        kind: JumpKind::Jmp,
                        disp_offset: at + 2,
                        next_ip: at + 6,
                        is_push_imm: false,
                    });
                } else {
                    self.asm_warn("xbegin needs label or imm");
                }
            },
            "xend" => self.emit(&[0x0F, 0x01, 0xD5]),
            "xabort" if ops.len() == 1 => {
                if let AsmOperand::Imm(v) = &ops[0] {
                    self.emit(&[0xC6, 0xF8, *v as u8]);
                } else if let AsmOperand::Sym(s) = &ops[0] {
                    // 符号作为立即数: 解析为 0 (占位, 链接时重定位)
                    let _ = s;
                    self.emit(&[0xC6, 0xF8, 0x00]);
                } else {
                    self.asm_warn("xabort needs imm8");
                }
            },
            "xtest" => self.emit(&[0x0F, 0x01, 0xD6]),

            // === AES-NI (66 0F 38 DC-DF) ===
            // aesenc aesi, xesi   : 66 0F 38 DC /r
            // aesenclast          : 66 0F 38 DD /r
            // aesdec              : 66 0F 38 DE /r
            // aesdeclast          : 66 0F 38 DF /r
            "aesenc" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0xDC, 0)
            },
            "aesenclast" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0xDD, 0)
            },
            "aesdec" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0xDE, 0)
            },
            "aesdeclast" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0xDF, 0)
            },
            // aesimc xmm1, xmm2/m128 : 66 0F 38 DB /r
            "aesimc" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0xDB, 0)
            },
            // aeskeygenassist xmm1, xmm2/m128, imm8 : 66 0F 3A DF /r ib
            "aeskeygenassist" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), src, AsmOperand::Reg(d, ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ds == 128 {
                        self.emit_asm_sse_2op_imm_mem(*d, src, 3, 0, 0, 0, 1, 0xDF, *imm);
                    } else {
                        self.asm_warn("aeskeygenassist dst must be xmm");
                    }
                } else {
                    self.asm_warn("aeskeygenassist needs imm, reg/mem, reg");
                }
            },

            // === SHA Extensions ===
            // sha1rnds4 xmm1, xmm2/m128, imm8 : 0F 3A CC /r ib
            "sha1rnds4" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), src, AsmOperand::Reg(d, ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ds == 128 {
                        self.emit_asm_sse_2op_imm_mem(*d, src, 3, 0, 0, 0, 0, 0xCC, *imm);
                    } else {
                        self.asm_warn("sha1rnds4 dst must be xmm");
                    }
                } else {
                    self.asm_warn("sha1rnds4 needs imm, reg/mem, reg");
                }
            },
            // sha1nexte xmm1, xmm2/m128 : 0F 38 C8 /r
            "sha1nexte" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 0, 2, 0xC8, 0)
            },
            // sha1msg1 xmm1, xmm2/m128 : 0F 38 C9 /r
            "sha1msg1" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 0, 2, 0xC9, 0)
            },
            // sha1msg2 xmm1, xmm2/m128 : 0F 38 CA /r
            "sha1msg2" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 0, 2, 0xCA, 0)
            },
            // sha256rnds2 xmm1, xmm2/m128, <XMM0> : 0F 38 CB /r (隐式 xmm0)
            "sha256rnds2" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 0, 2, 0xCB, 0)
            },
            // sha256msg1 xmm1, xmm2/m128 : 0F 38 CC /r
            "sha256msg1" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 0, 2, 0xCC, 0)
            },
            // sha256msg2 xmm1, xmm2/m128 : 0F 38 CD /r
            "sha256msg2" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 0, 2, 0xCD, 0)
            },

            // === FMA (Fused Multiply-Add) — 4 操作数 ===
            // vfmadd132ps/pd/ss/sd: VEX.NDS.128/256.66.0F38.W 98-99 /r
            // vfmadd213ps/pd/ss/sd: VEX.NDS.128/256.66.0F38.W A8-A9 /r
            // vfmadd231ps/pd/ss/sd: VEX.NDS.128/256.66.0F38.W B8-B9 /r
            // 4 操作数: dst, src1, src2/m ; dst = src1 * src2 + dst (132)
            // 模式: 所有 FMA 走 0F38 映射 (4 字节 VEX 前缀)
            // 由于 AT&T 语法中 dst 是第一个操作数, 1 2 3 对应 (3, 1, 2): dst 是 modrm.reg
            "vfmadd132ps" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0x98, 0, false)
            },
            "vfmadd132pd" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0x98, 1, false)
            },
            "vfmadd132ss" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0x99, 0, true)
            },
            "vfmadd132sd" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0x99, 1, true)
            },
            "vfmadd213ps" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xA8, 0, false)
            },
            "vfmadd213pd" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xA8, 1, false)
            },
            "vfmadd213ss" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xA9, 0, true)
            },
            "vfmadd213sd" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xA9, 1, true)
            },
            "vfmadd231ps" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xB8, 0, false)
            },
            "vfmadd231pd" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xB8, 1, false)
            },
            "vfmadd231ss" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xB9, 0, true)
            },
            "vfmadd231sd" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xB9, 1, true)
            },
            // 256-bit 版本 — 删去 (Reg 字段 u8, 256 不在范围内)
            // 256 位 FMA 走同一个 emit_asm_fma_3op, 调用方显式传 l=1
            // vfmsub132ps/pd/ss/sd (0x9A/9B/A8/AB/B8/BB)
            "vfmsub132ps" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0x9A, 0, false)
            },
            "vfmsub132pd" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0x9A, 1, false)
            },
            "vfmsub132ss" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0x9B, 0, true)
            },
            "vfmsub132sd" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0x9B, 1, true)
            },
            "vfmsub213ps" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xAA, 0, false)
            },
            "vfmsub213pd" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xAA, 1, false)
            },
            "vfmsub231ps" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 0, 1, 0xBA, 0, false)
            },
            "vfmsub231pd" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 1, 1, 0xBA, 1, false)
            },
            "vfmsub231ss" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 0, 1, 0xBB, 0, true)
            },
            "vfmsub231sd" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 1, 1, 0xBB, 1, true)
            },
            "vfmsub213ss" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 0, 1, 0xAB, 0, true)
            },
            "vfmsub213sd" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 1, 1, 0xAB, 1, true)
            },
            "vfmsub132ss" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 0, 1, 0x9B, 0, true)
            },
            "vfmsub132sd" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 1, 1, 0x9B, 1, true)
            },
            "vfmsub231pd" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xBA, 1, false)
            },
            // vfnmadd132ps/pd (0x9C/9D/AC/AD/BC/BD)
            "vfnmadd132ps" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0x9C, 0, false)
            },
            "vfnmadd132pd" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0x9C, 1, false)
            },
            "vfnmadd132ss" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0x9D, 0, true)
            },
            "vfnmadd132sd" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0x9D, 1, true)
            },
            "vfnmadd213ps" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xAC, 0, false)
            },
            "vfnmadd213pd" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xAC, 1, false)
            },
            "vfnmadd231ps" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xBC, 0, false)
            },
            "vfnmadd231pd" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xBC, 1, false)
            },
            // vfnmsub132ps/pd (0x9E/9F/AE/AF/BE/BF)
            "vfnmsub132ps" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0x9E, 0, false)
            },
            "vfnmsub132pd" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0x9E, 1, false)
            },
            "vfnmsub132ss" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0x9F, 0, true)
            },
            "vfnmsub132sd" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0x9F, 1, true)
            },
            "vfnmsub213ps" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xAE, 0, false)
            },
            "vfnmsub213pd" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xAE, 1, false)
            },
            "vfnmsub213ss" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 0, 1, 0xAF, 0, true)
            },
            "vfnmsub213sd" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 1, 1, 0xAF, 1, true)
            },
            "vfnmsub231ps" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xBE, 0, false)
            },
            "vfnmsub231pd" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xBE, 1, false)
            },
            "vfnmsub231ss" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 0, 1, 0xBF, 0, true)
            },
            "vfnmsub231sd" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 1, 1, 0xBF, 1, true)
            },
            // vfnmadd132/213/231 标量 (0x9D/AD/BD)
            "vfnmadd213ss" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 0, 1, 0xAD, 0, true)
            },
            "vfnmadd213sd" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 1, 1, 0xAD, 1, true)
            },
            "vfnmadd231ss" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 0, 1, 0xBD, 0, true)
            },
            "vfnmadd231sd" if ops.len() == 3 => {
                self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 1, 1, 0xBD, 1, true)
            },

            // === F16C (Half-precision ↔ Single-precision) ===
            // vcvtph2ps xmm1, xmm2/m128 : VEX.128.66.0F38.W0 13 /r
            "vcvtph2ps" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x13, 0)
            },
            // vcvtps2ph xmm1/m128, xmm2, imm8 : VEX.128.66.0F3A.W0 1D /r ib
            "vcvtps2ph" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*d, *s, 3, 0, 0, 0, 1);
                        self.emit(&[0x1D, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else if let (
                    AsmOperand::Imm(imm),
                    AsmOperand::Reg(s, ss),
                    AsmOperand::Mem {
                        disp,
                        base,
                        index,
                        scale,
                    },
                ) = (&ops[0], &ops[1], &ops[2])
                {
                    if *ss == 128 {
                        self.emit_vex4_prefix(*s, *base, 3, 0, 0, 0, 1);
                        self.emit(&[0x1D]);
                        self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
                        self.emit(&[*imm as u8]);
                    }
                } else if let (
                    AsmOperand::Imm(imm),
                    AsmOperand::Reg(s, ss),
                    AsmOperand::SymDisp(sym, _),
                )
                | (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Sym(sym)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ss == 128 {
                        // RIP-relative dest: mod=00 r/m=101
                        self.emit_vex4_prefix(*s, 0, 3, 0, 0, 0, 1);
                        self.emit(&[0x1D, 0x05 | ((s & 7) << 3), 0, 0, 0, 0, *imm as u8]);
                        let pos = self.pos() - 5;
                        self.pending_data_rels.push((pos, sym.clone()));
                    }
                } else {
                    self.asm_warn("vcvtps2ph needs imm, reg, reg/mem");
                }
            },

            // === AVX/AVX2 整数 SIMD (0F 38 / 0F 3A 映射) ===
            // pshufb xmm1, xmm2/m128 : 66 0F 38 00 /r
            "pshufb" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x00, 0)
            },
            // pshufd xmm1, xmm2/m128, imm8 : 66 0F 70 /r ib
            "pshufd" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), src, AsmOperand::Reg(d, ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ds == 128 {
                        match src {
                            AsmOperand::Reg(s, ss) if *ss == 128 => {
                                self.emit_vex3_prefix(*d, 0, 0, 1);
                                self.emit(&[0x70, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                            },
                            AsmOperand::Mem {
                                disp,
                                base,
                                index,
                                scale,
                            } => {
                                self.emit_vex3_prefix(*d, 0, 0, 1);
                                self.emit(&[0x70]);
                                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
                                self.emit(&[*imm as u8]);
                            },
                            AsmOperand::SymDisp(sym, _) | AsmOperand::Sym(sym) => {
                                self.emit_vex3_prefix(*d, 0, 0, 1);
                                self.emit(&[0x70, 0x05 | ((d & 7) << 3), 0, 0, 0, 0, *imm as u8]);
                                let pos = self.pos() - 5;
                                self.pending_data_rels.push((pos, sym.clone()));
                            },
                            _ => {
                                self.asm_warn("pshufd src must be xmm or mem");
                            },
                        }
                    } else {
                        self.asm_warn("pshufd dst must be xmm");
                    }
                } else {
                    self.asm_warn("pshufd needs imm, xmm/mem, xmm");
                }
            },
            "pshufhw" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), src, AsmOperand::Reg(d, ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ds == 128 {
                        match src {
                            AsmOperand::Reg(s, ss) if *ss == 128 => {
                                self.emit_vex3_prefix(*d, 0, 0, 3);
                                self.emit(&[0x70, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                            },
                            AsmOperand::Mem {
                                disp,
                                base,
                                index,
                                scale,
                            } => {
                                self.emit_vex3_prefix(*d, 0, 0, 3);
                                self.emit(&[0x70]);
                                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
                                self.emit(&[*imm as u8]);
                            },
                            AsmOperand::SymDisp(sym, _) | AsmOperand::Sym(sym) => {
                                self.emit_vex3_prefix(*d, 0, 0, 3);
                                self.emit(&[0x70, 0x05 | ((d & 7) << 3), 0, 0, 0, 0, *imm as u8]);
                                let pos = self.pos() - 5;
                                self.pending_data_rels.push((pos, sym.clone()));
                            },
                            _ => {
                                self.asm_warn("pshufhw src must be xmm or mem");
                            },
                        }
                    } else {
                        self.asm_warn("pshufhw dst must be xmm");
                    }
                } else {
                    self.asm_warn("pshufhw needs imm, xmm/mem, xmm");
                }
            },
            "pshuflw" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), src, AsmOperand::Reg(d, ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ds == 128 {
                        match src {
                            AsmOperand::Reg(s, ss) if *ss == 128 => {
                                self.emit_vex3_prefix(*d, 0, 0, 2);
                                self.emit(&[0x70, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                            },
                            AsmOperand::Mem {
                                disp,
                                base,
                                index,
                                scale,
                            } => {
                                self.emit_vex3_prefix(*d, 0, 0, 2);
                                self.emit(&[0x70]);
                                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
                                self.emit(&[*imm as u8]);
                            },
                            AsmOperand::SymDisp(sym, _) | AsmOperand::Sym(sym) => {
                                self.emit_vex3_prefix(*d, 0, 0, 2);
                                self.emit(&[0x70, 0x05 | ((d & 7) << 3), 0, 0, 0, 0, *imm as u8]);
                                let pos = self.pos() - 5;
                                self.pending_data_rels.push((pos, sym.clone()));
                            },
                            _ => {
                                self.asm_warn("pshuflw src must be xmm or mem");
                            },
                        }
                    } else {
                        self.asm_warn("pshuflw dst must be xmm");
                    }
                } else {
                    self.asm_warn("pshuflw needs imm, xmm/mem, xmm");
                }
            },
            // pmulhrsw xmm1, xmm2/m128 : 66 0F 38 0B /r
            "pmulhrsw" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x0B, 0)
            },
            // pmaddubsw xmm1, xmm2/m128 : 66 0F 38 04 /r
            "pmaddubsw" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x04, 0)
            },
            // pabsb/pabsw/pabsd : 66 0F 38 1C/1D/1E /r
            "pabsb" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x1C, 0)
            },
            "pabsw" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x1D, 0)
            },
            "pabsd" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x1E, 0)
            },
            // palignr xmm1, xmm2/m128, imm8 : 66 0F 3A 0F /r ib
            "palignr" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), src, AsmOperand::Reg(d, ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ds == 128 {
                        self.emit_asm_sse_2op_imm_mem(*d, src, 3, 0, 0, 0, 1, 0x0F, *imm);
                    } else {
                        self.asm_warn("palignr dst must be xmm");
                    }
                } else {
                    self.asm_warn("palignr needs imm, reg/mem, reg");
                }
            },
            // pblendw xmm1, xmm2/m128, imm8 : 66 0F 3A 0E /r ib
            "pblendw" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), src, AsmOperand::Reg(d, ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ds == 128 {
                        self.emit_asm_sse_2op_imm_mem(*d, src, 3, 0, 0, 0, 1, 0x0E, *imm);
                    } else {
                        self.asm_warn("pblendw dst must be xmm");
                    }
                } else {
                    self.asm_warn("pblendw needs imm, reg/mem, reg");
                }
            },
            // phminposuw xmm1, xmm2/m128, imm8 : 66 0F 38 41 /r ib
            "phminposuw" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x41, 0)
            },
            // pmuldq xmm1, xmm2/m128 : 66 0F 38 28 /r
            "pmuldq" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x28, 0)
            },
            // pmulld xmm1, xmm2/m128 : 66 0F 38 40 /r
            "pmulld" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x40, 0)
            },
            // pmuludq xmm1, xmm2/m128 : 66 0F F4 /r
            "pmuludq" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 1, 0xF4, 0)
            },
            // pminub/pmaxub/pminuw/pmaxuw/pminsb/pmaxsb/pminud/pmaxud/pminsd/pmaxsd
            // 66 0F DA/DE/D8/DC/38/3C/3B/3F/39/3D
            "pminub" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 1, 0xDA, 0)
            },
            "pmaxub" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 1, 0xDE, 0)
            },
            "pminuw" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 1, 0xD8, 0)
            },
            "pmaxuw" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 1, 0xDC, 0)
            },
            "pminsb" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x38, 0)
            },
            "pmaxsb" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x3C, 0)
            },
            "pminud" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x3B, 0)
            },
            "pmaxud" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x3F, 0)
            },
            "pminsd" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x39, 0)
            },
            "pmaxsd" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x3D, 0)
            },
            // pcmpeqq/pcmpgtq : 66 0F 38 29/37 /r
            "pcmpeqq" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x29, 0)
            },
            "pcmpgtq" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x37, 0)
            },
            // pinsrb/pinsrd/pinsrq : 66 0F 3A 20/22/21 /r ib
            "pinsrb" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), src, AsmOperand::Reg(d, ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ds == 128 {
                        self.emit_asm_pinsr_imm8(*imm, src, *d, 0x20, 0);
                    } else {
                        self.asm_warn("pinsrb dst must be xmm");
                    }
                } else {
                    self.asm_warn("pinsrb needs imm, src, xmm");
                }
            },
            "pinsrd" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), src, AsmOperand::Reg(d, ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ds == 128 {
                        self.emit_asm_pinsr_imm8(*imm, src, *d, 0x22, 0);
                    } else {
                        self.asm_warn("pinsrd dst must be xmm");
                    }
                } else {
                    self.asm_warn("pinsrd needs imm, src, xmm");
                }
            },
            "pinsrq" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), src, AsmOperand::Reg(d, ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ds == 128 {
                        self.emit_asm_pinsr_imm8(*imm, src, *d, 0x22, 1);
                    } else {
                        self.asm_warn("pinsrq dst must be xmm");
                    }
                } else {
                    self.asm_warn("pinsrq needs imm, src, xmm");
                }
            },
            // pextrb/pextrd/pextrq : 66 0F 3A 14/16/15 /r ib
            "pextrb" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), dst) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ss == 128 {
                        self.emit_asm_pextr_imm8(*imm, *s, dst, 0x14, 0);
                    } else {
                        self.asm_warn("pextrb src must be xmm");
                    }
                } else {
                    self.asm_warn("pextrb needs imm, xmm, dst");
                }
            },
            "pextrd" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), dst) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ss == 128 {
                        self.emit_asm_pextr_imm8(*imm, *s, dst, 0x16, 0);
                    } else {
                        self.asm_warn("pextrd src must be xmm");
                    }
                } else {
                    self.asm_warn("pextrd needs imm, xmm, dst");
                }
            },
            "pextrq" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), dst) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ss == 128 {
                        self.emit_asm_pextr_imm8(*imm, *s, dst, 0x16, 1);
                    } else {
                        self.asm_warn("pextrq src must be xmm");
                    }
                } else {
                    self.asm_warn("pextrq needs imm, xmm, dst");
                }
            },
            // extractps : 66 0F 3A 17 /r ib
            "extractps" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), dst) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ss == 128 {
                        self.emit_asm_pextr_imm8(*imm, *s, dst, 0x17, 0);
                    } else {
                        self.asm_warn("extractps src must be xmm");
                    }
                } else {
                    self.asm_warn("extractps needs imm, xmm, dst");
                }
            },
            // insertps xmm1, xmm2/m128, imm8 : 66 0F 3A 21 /r ib
            "insertps" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), src, AsmOperand::Reg(d, ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ds == 128 {
                        match src {
                            AsmOperand::Reg(s, ss) if *ss == 128 => {
                                self.emit_vex4_prefix(*d, *s, 3, 0, 0, 0, 1);
                                self.emit(&[0x21, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                            },
                            AsmOperand::Mem {
                                disp,
                                base,
                                index,
                                scale,
                            } => {
                                self.emit_vex4_prefix(*d, *base, 3, 0, 0, 0, 1);
                                self.emit(&[0x21]);
                                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
                                self.emit(&[*imm as u8]);
                            },
                            AsmOperand::SymDisp(sym, _) | AsmOperand::Sym(sym) => {
                                self.emit_vex4_prefix(*d, 0, 3, 0, 0, 0, 1);
                                self.emit(&[0x21, 0x05 | ((d & 7) << 3), 0, 0, 0, 0, *imm as u8]);
                                let pos = self.pos() - 5;
                                self.pending_data_rels.push((pos, sym.clone()));
                            },
                            _ => {
                                self.asm_warn("insertps src must be xmm or mem");
                            },
                        }
                    } else {
                        self.asm_warn("insertps dst must be xmm");
                    }
                } else {
                    self.asm_warn("insertps needs imm, xmm/mem, xmm");
                }
            },
            // AVX vpinsrb/vpinsrd/vpinsrq : VEX.128.66.0F3A.W0 20/22/22 /r ib
            "vpinsrb" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), src, AsmOperand::Reg(d, ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ds == 128 {
                        self.emit_asm_vpinsr_imm8(*imm, src, *d, *d, 0x20, 0);
                    } else {
                        self.asm_warn("vpinsrb dst must be xmm");
                    }
                } else {
                    self.asm_warn("vpinsrb needs imm, src, xmm");
                }
            },
            "vpinsrb" if ops.len() == 4 => {
                if let (AsmOperand::Imm(imm), src, AsmOperand::Reg(v, vs), AsmOperand::Reg(d, ds)) =
                    (&ops[0], &ops[1], &ops[2], &ops[3])
                {
                    if *vs == 128 && *ds == 128 {
                        self.emit_asm_vpinsr_imm8(*imm, src, *v, *d, 0x20, 0);
                    } else {
                        self.asm_warn("vpinsrb needs xmm vvvv/dst");
                    }
                } else {
                    self.asm_warn("vpinsrb needs imm, src, xmm, xmm");
                }
            },
            "vpinsrd" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), src, AsmOperand::Reg(d, ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ds == 128 {
                        self.emit_asm_vpinsr_imm8(*imm, src, *d, *d, 0x22, 0);
                    } else {
                        self.asm_warn("vpinsrd dst must be xmm");
                    }
                } else {
                    self.asm_warn("vpinsrd needs imm, src, xmm");
                }
            },
            "vpinsrd" if ops.len() == 4 => {
                if let (AsmOperand::Imm(imm), src, AsmOperand::Reg(v, vs), AsmOperand::Reg(d, ds)) =
                    (&ops[0], &ops[1], &ops[2], &ops[3])
                {
                    if *vs == 128 && *ds == 128 {
                        self.emit_asm_vpinsr_imm8(*imm, src, *v, *d, 0x22, 0);
                    } else {
                        self.asm_warn("vpinsrd needs xmm vvvv/dst");
                    }
                } else {
                    self.asm_warn("vpinsrd needs imm, src, xmm, xmm");
                }
            },
            "vpinsrq" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), src, AsmOperand::Reg(d, ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ds == 128 {
                        self.emit_asm_vpinsr_imm8(*imm, src, *d, *d, 0x22, 1);
                    } else {
                        self.asm_warn("vpinsrq dst must be xmm");
                    }
                } else {
                    self.asm_warn("vpinsrq needs imm, src, xmm");
                }
            },
            "vpinsrq" if ops.len() == 4 => {
                if let (AsmOperand::Imm(imm), src, AsmOperand::Reg(v, vs), AsmOperand::Reg(d, ds)) =
                    (&ops[0], &ops[1], &ops[2], &ops[3])
                {
                    if *vs == 128 && *ds == 128 {
                        self.emit_asm_vpinsr_imm8(*imm, src, *v, *d, 0x22, 1);
                    } else {
                        self.asm_warn("vpinsrq needs xmm vvvv/dst");
                    }
                } else {
                    self.asm_warn("vpinsrq needs imm, src, xmm, xmm");
                }
            },
            // AVX vpextrb/vpextrd/vpextrq、vextractps/vextractpd
            "vpextrb" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), dst) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ss == 128 {
                        self.emit_asm_vpextr_imm8(*imm, *s, dst, 0x14, 0);
                    } else {
                        self.asm_warn("vpextrb src must be xmm");
                    }
                } else {
                    self.asm_warn("vpextrb needs imm, xmm, dst");
                }
            },
            "vpextrd" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), dst) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ss == 128 {
                        self.emit_asm_vpextr_imm8(*imm, *s, dst, 0x16, 0);
                    } else {
                        self.asm_warn("vpextrd src must be xmm");
                    }
                } else {
                    self.asm_warn("vpextrd needs imm, xmm, dst");
                }
            },
            "vpextrq" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), dst) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ss == 128 {
                        self.emit_asm_vpextr_imm8(*imm, *s, dst, 0x16, 1);
                    } else {
                        self.asm_warn("vpextrq src must be xmm");
                    }
                } else {
                    self.asm_warn("vpextrq needs imm, xmm, dst");
                }
            },
            "vextractps" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), dst) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ss == 128 {
                        self.emit_asm_vpextr_imm8(*imm, *s, dst, 0x17, 0);
                    } else {
                        self.asm_warn("vextractps src must be xmm");
                    }
                } else {
                    self.asm_warn("vextractps needs imm, xmm, dst_reg/mem");
                }
            },
            "vextractpd" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), dst) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ss == 128 {
                        match dst {
                            // vextractpd to XMM register: VEX.NDS.128.66.0F3A.WIG 19 /r
                            AsmOperand::Reg(d, ds) if *ds == 128 => {
                                self.emit_vex4_prefix(*d, *s, 3, 0, 0, 0, 1);
                                self.emit(&[0x19, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                            },
                            // vextractpd to GPR or memory: same as vpextr path
                            _ => {
                                self.emit_asm_vpextr_imm8(*imm, *s, dst, 0x19, 0);
                            },
                        }
                    } else {
                        self.asm_warn("vextractpd src must be xmm");
                    }
                } else {
                    self.asm_warn("vextractpd needs imm, xmm, dst_reg/mem");
                }
            },
            // pmovsxbw / pmovsxbd / pmovsxbq / pmovsxwd / pmovsxwq / pmovsxdq
            // pmovzxbw / pmovzxbd / pmovzxbq / pmovzxwd / pmovzxwq / pmovzxdq
            // 66 0F 38 20-25 (sx) / 30-35 (zx)
            "pmovsxbw" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x20, 0)
            },
            "pmovsxbd" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x21, 0)
            },
            "pmovsxbq" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x22, 0)
            },
            "pmovsxwd" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x23, 0)
            },
            "pmovsxwq" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x24, 0)
            },
            "pmovsxdq" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x25, 0)
            },
            "pmovzxbw" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x30, 0)
            },
            "pmovzxbd" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x31, 0)
            },
            "pmovzxbq" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x32, 0)
            },
            "pmovzxwd" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x33, 0)
            },
            "pmovzxwq" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x34, 0)
            },
            "pmovzxdq" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x35, 0)
            },
            // pcmpestri/pcmpestrm/pcmpistri/pcmpistrm 复杂指令, 此处仅预留 nop
            // roundps/roundpd/roundss/roundsd : 66 0F 3A 08/09/0A/0B /r ib
            "roundps" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*d, *s, 3, 0, 0, 0, 1);
                        self.emit(&[0x08, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else {
                    self.emit(&[0x90]);
                }
            },
            "roundpd" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*d, *s, 3, 0, 0, 0, 1);
                        self.emit(&[0x09, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else {
                    self.emit(&[0x90]);
                }
            },
            "roundss" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*d, *s, 3, 0, 0, 0, 1);
                        self.emit(&[0x0A, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else {
                    self.emit(&[0x90]);
                }
            },
            "roundsd" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*d, *s, 3, 0, 0, 0, 1);
                        self.emit(&[0x0B, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else {
                    self.emit(&[0x90]);
                }
            },

            // === AVX/AVX2 浮点与整数 3 操作数 ===
            // vaddps/vaddpd/vmulps/vmulpd/vsubps/vsubpd/vdivps/vdivpd/vminps/vmaxps
            // VEX.NDS.128/256.0F 58/59/5C/5D/5E/5F/5E(vdiv)/5D(vdiv)
            "vaddps" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 0, 0x58, 0, false)
            },
            "vaddpd" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x58, 0, false)
            },
            "vmulps" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 0, 0x59, 0, false)
            },
            "vmulpd" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x59, 0, false)
            },
            "vsubps" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 0, 0x5C, 0, false)
            },
            "vsubpd" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x5C, 0, false)
            },
            "vdivps" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 0, 0x5E, 0, false)
            },
            "vdivpd" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x5E, 0, false)
            },
            "vminps" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 0, 0x5D, 0, false)
            },
            "vmaxps" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 0, 0x5F, 0, false)
            },
            "vminpd" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x5D, 0, false)
            },
            "vmaxpd" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x5F, 0, false)
            },
            "vsqrtps" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 0, 1, 0x51, 0)
            },
            "vsqrtpd" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 1, 0, 1, 0x51, 0)
            },
            "vandps" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 0, 0x54, 0, false)
            },
            "vandpd" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x54, 0, false)
            },
            "vorps" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 0, 0x56, 0, false)
            },
            "vorpd" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x56, 0, false)
            },
            "vxorps" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 0, 0x57, 0, false)
            },
            "vxorpd" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x57, 0, false)
            },
            // vaddss/vaddsd/vmulss/vmulsd/vsubss/vsubsd/vdivss/vdivsd/vminss/vmaxss
            // F3 0F 58/F2 0F 58/F3 0F 59/F2 0F 59/F3 0F 5C/F2 0F 5C/F3 0F 5E/F2 0F 5E/F3 0F 5D/F2 0F 5D/F3 0F 5F/F2 0F 5F
            "vaddss" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 0, 0x58, 2, true)
            },
            "vaddsd" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x58, 3, true)
            },
            "vmulss" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 0, 0x59, 2, true)
            },
            "vmulsd" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x59, 3, true)
            },
            "vsubss" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 0, 0x5C, 2, true)
            },
            "vsubsd" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x5C, 3, true)
            },
            "vdivss" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 0, 0x5E, 2, true)
            },
            "vdivsd" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x5E, 3, true)
            },
            "vminss" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 0, 0x5D, 2, true)
            },
            "vmaxss" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 0, 0x5F, 2, true)
            },
            "vminsd" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x5D, 3, true)
            },
            "vmaxsd" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x5F, 3, true)
            },
            "vsqrtss" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 2, 1, 0x51, 0)
            },
            "vsqrtsd" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 3, 1, 0x51, 0)
            },
            // vblendps/vblendpd : 66 0F 3A 0C/0D /r ib
            // AT&T: vblendps $imm, src, dst (3 ops)
            "vblendps" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*d, *s, 3, 0, 0, 0, 1);
                        self.emit(&[0x0C, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else {
                    self.emit(&[0x90]);
                }
            },
            "vblendpd" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*d, *s, 3, 0, 0, 0, 1);
                        self.emit(&[0x0D, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else {
                    self.emit(&[0x90]);
                }
            },
            // vshufps : 0F C6 /r ib ; vshufpd : 66 0F C6 /r ib
            // AT&T: vshufps $imm, src, dst (3 ops)
            "vshufps" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex3_prefix(*d, *s, 0, 0);
                        self.emit(&[0xC6, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else {
                    self.emit(&[0x90]);
                }
            },
            "vshufpd" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex3_prefix(*d, *s, 0, 1);
                        self.emit(&[0xC6, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else {
                    self.emit(&[0x90]);
                }
            },
            // vunpcklps/vunpckhps : 0F 14/15 /r
            "vunpcklps" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 0, 0x14, 0, false)
            },
            "vunpckhps" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 0, 0x15, 0, false)
            },
            "vunpcklpd" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x14, 0, false)
            },
            "vunpckhpd" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x15, 0, false)
            },
            // vunpck*ps/pd 2-op form: AT&T `vunpcklps src, dst` 等价于 `dst = unpack_low(dst, src)`
            // Intel 编码: vvvv = dst (modrm.reg = dst, modrm.rm = src)
            "vunpcklps" if ops.len() == 2 => {
                if let (AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex3_prefix(*d, *d, 0, 0);
                        self.emit(&[0x14, Self::modrm_reg_reg(*d, *s)]);
                    }
                } else {
                    self.emit(&[0x90]);
                }
            },
            "vunpckhps" if ops.len() == 2 => {
                if let (AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex3_prefix(*d, *d, 0, 0);
                        self.emit(&[0x15, Self::modrm_reg_reg(*d, *s)]);
                    }
                } else {
                    self.emit(&[0x90]);
                }
            },
            "vunpcklpd" if ops.len() == 2 => {
                if let (AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex3_prefix(*d, *d, 0, 1);
                        self.emit(&[0x14, Self::modrm_reg_reg(*d, *s)]);
                    }
                } else {
                    self.emit(&[0x90]);
                }
            },
            "vunpckhpd" if ops.len() == 2 => {
                if let (AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex3_prefix(*d, *d, 0, 1);
                        self.emit(&[0x15, Self::modrm_reg_reg(*d, *s)]);
                    }
                } else {
                    self.emit(&[0x90]);
                }
            },
            // vpermilps/vpermilpd : 0F 38 0C/0D /r (reg-reg-reg 3-op)
            "vpermilps" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 0, 0x0C, 2, false)
            },
            "vpermilpd" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x0D, 2, false)
            },
            // vbroadcastss : 66 0F 38 18 /r (3-op 形式)
            "vbroadcastss" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 1, 2, 0x18, 0)
            },
            // vbroadcastsd : VEX.256.66.0F38.W0 19 /r
            "vbroadcastsd" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 1, 1, 2, 0x19, 0)
            },
            // vbroadcasti128 : VEX.256.66.0F38.W0 5A /r
            "vbroadcasti128" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 1, 1, 2, 0x5A, 0)
            },
            // vperm2f128/vperm2i128 : 66 0F 3A 06/46 /r ib
            "vperm2f128" if ops.len() == 4 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds), _) =
                    (&ops[0], &ops[1], &ops[2], &ops[3])
                {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*d, *s, 3, 0, 0, 1, 1);
                        self.emit(&[0x06, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else {
                    self.emit(&[0x90]);
                }
            },
            "vperm2i128" if ops.len() == 4 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds), _) =
                    (&ops[0], &ops[1], &ops[2], &ops[3])
                {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*d, *s, 3, 0, 0, 1, 1);
                        self.emit(&[0x46, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else {
                    self.emit(&[0x90]);
                }
            },
            // vinsertf128/vinserti128 : 66 0F 3A 18/38 /r ib
            "vinsertf128" if ops.len() == 4 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds), _) =
                    (&ops[0], &ops[1], &ops[2], &ops[3])
                {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*d, *s, 3, 0, 0, 1, 1);
                        self.emit(&[0x18, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else {
                    self.emit(&[0x90]);
                }
            },
            "vinserti128" if ops.len() == 4 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds), _) =
                    (&ops[0], &ops[1], &ops[2], &ops[3])
                {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*d, *s, 3, 0, 0, 1, 1);
                        self.emit(&[0x38, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else {
                    self.emit(&[0x90]);
                }
            },
            // vextractf128/vextracti128 : 66 0F 3A 19/39 /r ib
            "vextractf128" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*s, *d, 3, 0, 0, 1, 1);
                        self.emit(&[0x19, Self::modrm_reg_reg(*s, *d), *imm as u8]);
                    }
                } else {
                    self.emit(&[0x90]);
                }
            },
            "vextracti128" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*s, *d, 3, 0, 0, 1, 1);
                        self.emit(&[0x39, Self::modrm_reg_reg(*s, *d), *imm as u8]);
                    }
                } else {
                    self.emit(&[0x90]);
                }
            },

            // === AVX/AVX2 整数 SIMD ===
            "vpaddb" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xFC, 0, false)
            },
            "vpaddw" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xFD, 0, false)
            },
            "vpaddd" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xFE, 0, false)
            },
            "vpaddq" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xD4, 0, false)
            },
            "vpsubb" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xF8, 0, false)
            },
            "vpsubw" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xF9, 0, false)
            },
            "vpsubd" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xFA, 0, false)
            },
            "vpsubq" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xFB, 0, false)
            },
            "vpmullw" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xD5, 0, false)
            },
            "vpmulld" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x40, 2, false)
            },
            "vpand" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xDB, 0, false)
            },
            "vpandn" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xDF, 0, false)
            },
            "vpor" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xEB, 0, false)
            },
            "vpxor" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xEF, 0, false)
            },
            "vpcmpeqb" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x74, 0, false)
            },
            "vpcmpeqw" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x75, 0, false)
            },
            "vpcmpeqd" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x76, 0, false)
            },
            "vpcmpgtb" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x64, 0, false)
            },
            "vpcmpgtw" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x65, 0, false)
            },
            "vpcmpgtd" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x66, 0, false)
            },
            "vpshufd" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex3_prefix(*d, 0, 0, 1);
                        self.emit(&[0x70, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else {
                    self.emit(&[0x90]);
                }
            },
            "vpshufb" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 1, 2, 0x00, 0)
            },
            "vpminub" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xDA, 0, false)
            },
            "vpmaxub" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xDE, 0, false)
            },
            "vpminuw" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xD8, 0, false)
            },
            "vpmaxuw" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xDC, 0, false)
            },
            "vpminsb" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x38, 2, false)
            },
            "vpmaxsb" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x3C, 2, false)
            },
            "vpminsd" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x39, 2, false)
            },
            "vpmaxsd" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x3D, 2, false)
            },
            "vpminud" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x3B, 2, false)
            },
            "vpmaxud" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x3F, 2, false)
            },
            "vpmuldq" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x28, 2, false)
            },
            "vpmuludq" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xF4, 0, false)
            },
            "vpmulhrsw" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x0B, 2, false)
            },
            "vpabsb" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 1, 2, 0x1C, 0),
            "vpabsw" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 1, 2, 0x1D, 0),
            "vpabsd" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 1, 2, 0x1E, 0),
            "vpaddusb" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xDC, 2, false)
            },
            "vpaddsb" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xEC, 2, false)
            },
            "vpsubusb" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xD8, 2, false)
            },
            "vpsubsb" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xE8, 2, false)
            },
            "vpsignb" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x08, 2, false)
            },
            "vpsignw" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x09, 2, false)
            },
            "vpsignd" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x0A, 2, false)
            },
            "vphaddw" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x01, 2, false)
            },
            "vphaddd" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x02, 2, false)
            },
            "vphaddsw" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x03, 2, false)
            },
            "vphsubw" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x05, 2, false)
            },
            "vphsubd" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x06, 2, false)
            },
            "vphsubsw" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x07, 2, false)
            },
            "vpmaddwd" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xF5, 0, false)
            },
            "vpmaddubsw" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x04, 2, false)
            },
            "vpavgb" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xE0, 2, false)
            },
            "vpavgw" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xE3, 2, false)
            },
            "vpacksswb" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x63, 0, false)
            },
            "vpackssdw" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x6B, 0, false)
            },
            "vpackuswb" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x67, 0, false)
            },
            "vpackusdw" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x2B, 2, false)
            },
            "vpcmpeqq" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x29, 2, false)
            },
            "vpcmpgtq" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x37, 2, false)
            },

            // === AVX mov 指令 (256-bit) ===
            "vmovaps" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 0, 1, 0x28, 0)
            },
            "vmovapd" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 1, 1, 0x28, 0)
            },
            "vmovups" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 0, 1, 0x10, 0)
            },
            "vmovupd" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 1, 1, 0x10, 0)
            },
            "vmovdqa" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 1, 1, 0x6F, 0)
            },
            "vmovdqu" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 2, 1, 0x6F, 0)
            },
            "vmovdqa32" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 1, 1, 0x6F, 0)
            },
            "vmovdqu32" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 2, 1, 0x6F, 0)
            },
            "vmovdqa64" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 1, 1, 0x6F, 1)
            },
            "vmovdqu64" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 2, 1, 0x6F, 1)
            },

            // vzeroupper / vzeroall : 0F 77 (vex.128.0F)
            "vzeroupper" => self.emit(&[0xC5, 0xF8, 0x77]),
            "vzeroall" => self.emit(&[0xC5, 0xFC, 0x77]),

            // vpmovmskb : 0F D7 /r (vex.128/256.0F D7)
            "vpmovmskb" if ops.len() == 2 => {
                if let (AsmOperand::Reg(s, ss), AsmOperand::Reg(d, _)) = (&ops[0], &ops[1]) {
                    if *ss == 128 {
                        self.emit_vex3_prefix(*d, 0, 0, 1);
                        self.emit(&[0xD7, Self::modrm_reg_reg(*d, *s)]);
                    }
                } else {
                    self.emit(&[0x90]);
                }
            },
            // vmovmskps : 0F 50 /r ; vmovmskpd : 66 0F 50 /r
            "vmovmskps" if ops.len() == 2 => {
                if let (AsmOperand::Reg(s, ss), AsmOperand::Reg(d, _)) = (&ops[0], &ops[1]) {
                    if *ss == 128 {
                        self.emit_vex3_prefix(*d, 0, 0, 0);
                        self.emit(&[0x50, Self::modrm_reg_reg(*d, *s)]);
                    }
                } else {
                    self.emit(&[0x90]);
                }
            },
            "vmovmskpd" if ops.len() == 2 => {
                if let (AsmOperand::Reg(s, ss), AsmOperand::Reg(d, _)) = (&ops[0], &ops[1]) {
                    if *ss == 128 {
                        self.emit_vex3_prefix(*d, 0, 0, 1);
                        self.emit(&[0x50, Self::modrm_reg_reg(*d, *s)]);
                    }
                } else {
                    self.emit(&[0x90]);
                }
            },

            // vcvtps2pd / vcvtpd2ps : VEX.256/128
            "vcvtps2pd" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 0, 1, 0x5A, 0)
            },
            "vcvtpd2ps" if ops.len() == 2 => {
                self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 1, 1, 0x5A, 0)
            },
            "vcvtss2si" if ops.len() == 2 => {
                if let (AsmOperand::Reg(s, ss), AsmOperand::Reg(d, _)) = (&ops[0], &ops[1]) {
                    if *ss == 128 {
                        self.emit_vex3_prefix(*d, 0, 0, 2);
                        self.emit(&[0x2D, Self::modrm_reg_reg(*d, *s)]);
                    }
                } else {
                    self.emit(&[0x90]);
                }
            },
            "vcvtsd2si" if ops.len() == 2 => {
                if let (AsmOperand::Reg(s, ss), AsmOperand::Reg(d, _)) = (&ops[0], &ops[1]) {
                    if *ss == 128 {
                        self.emit_vex3_prefix(*d, 0, 0, 3);
                        self.emit(&[0x2D, Self::modrm_reg_reg(*d, *s)]);
                    }
                } else {
                    self.emit(&[0x90]);
                }
            },
            "vcvtsi2ss" if ops.len() == 3 => {
                if let (AsmOperand::Reg(s, _), AsmOperand::Reg(d, ds), AsmOperand::Reg(_, _)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ds == 128 {
                        self.emit_vex3_prefix(*d, *s, 0, 2);
                        self.emit(&[0x2A, Self::modrm_reg_reg(*d, *s)]);
                    }
                } else {
                    self.emit(&[0x90]);
                }
            },
            "vcvtsi2sd" if ops.len() == 3 => {
                if let (AsmOperand::Reg(s, _), AsmOperand::Reg(d, ds), AsmOperand::Reg(_, _)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ds == 128 {
                        self.emit_vex3_prefix(*d, *s, 0, 3);
                        self.emit(&[0x2A, Self::modrm_reg_reg(*d, *s)]);
                    }
                } else {
                    self.emit(&[0x90]);
                }
            },

            // === AVX-VNNI (VEX 形式 0F 38 /r) ===
            // vpdpbusd : 66 0F 38 50 /r ; vpdpbssd : 66 0F 38 51 /r ; vpdpwssd : 66 0F 38 52 /r
            "vpdpbusd" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x50, 2, false)
            },
            "vpdpbssd" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x51, 2, false)
            },
            "vpdpwssd" if ops.len() == 3 => {
                self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x52, 2, false)
            },

            // === AMX (Advanced Matrix Extensions) — Tile 指令 ===
            // ldtilecfg : VEX.128.0F38.W0 49 /r (mem)
            "ldtilecfg" if ops.len() == 1 => {
                self.emit_asm_vex_2op(&AsmOperand::None, &ops[0], 0, 0, 2, 0x49, 0)
            },
            // sttilecfg : VEX.128.0F38.W0 4B /r (mem)
            "sttilecfg" if ops.len() == 1 => {
                self.emit_asm_vex_2op(&ops[0], &AsmOperand::None, 0, 0, 2, 0x4B, 0)
            },
            // tilezero tmm1 : VEX.128.F2.0F38.W0 49 /r (reg)
            "tilezero" if ops.len() == 1 => {
                if let AsmOperand::Reg(d, sz) = &ops[0] {
                    if *sz == ASM_REG_TMM {
                        self.emit(&[0xC4, 0xE2, 0x7B, 0x49, 0xC0 | (d & 7)]);
                    } else {
                        self.asm_warn("tilezero needs tmm register");
                    }
                } else {
                    self.asm_warn("tilezero needs tmm register");
                }
            },
            // tileloadd tmm1, sibmem : VEX.128.F2.0F38.W0 4B /r
            "tileloadd" if ops.len() == 2 => {
                self.emit_asm_tileloadd(&ops[0], &ops[1]);
            },
            // tilestored sibmem, tmm1 : VEX.128.F3.0F38.W0 4B /r
            "tilestored" if ops.len() == 2 => {
                self.emit_asm_tilestored(&ops[0], &ops[1]);
            },
            // tdpbssd tmm1, tmm2, tmm3 : VEX.128.F2.0F38.W0 5E /r
            "tdpbssd" if ops.len() == 3 => {
                self.emit_asm_amx_3op(&ops[0], &ops[1], &ops[2], 0x5E, 2)
            },
            "tdpbusd" if ops.len() == 3 => {
                self.emit_asm_amx_3op(&ops[0], &ops[1], &ops[2], 0x5E, 0)
            },
            "tdpbuud" if ops.len() == 3 => {
                self.emit_asm_amx_3op(&ops[0], &ops[1], &ops[2], 0x5E, 1)
            },
            "tdpbf16ps" if ops.len() == 3 => {
                self.emit_asm_amx_3op(&ops[0], &ops[1], &ops[2], 0x5F, 2)
            },

            // === AVX-512 简化版 (无 mask / 无 broadcast) ===
            // vmovdqu32 (zmm) : 0F 6F (map=1, w=0) ; vmovdqu64 : 0F 6F (w=1)
            "vmovdqu32_z" if ops.len() == 2 => {
                self.emit_asm_evex_2op(&ops[0], &ops[1], 512, 2, 1, 0x6F, 0)
            },
            "vmovdqu64_z" if ops.len() == 2 => {
                self.emit_asm_evex_2op(&ops[0], &ops[1], 512, 2, 1, 0x6F, 1)
            },
            "vmovdqa32_z" if ops.len() == 2 => {
                self.emit_asm_evex_2op(&ops[0], &ops[1], 512, 1, 1, 0x6F, 0)
            },
            "vmovdqa64_z" if ops.len() == 2 => {
                self.emit_asm_evex_2op(&ops[0], &ops[1], 512, 1, 1, 0x6F, 1)
            },
            // vaddps zmm : 0F 58 (map=1, w=0)
            // AT&T 顺序: (src1, src2, dst) -> dst=ops[2], v(src1)=ops[0], b(src2)=ops[1]
            "vaddps_z" if ops.len() == 3 => self.emit_asm_evex_3op_reg(
                if let AsmOperand::Reg(d, _) = &ops[2] { *d } else { 0 },
                if let AsmOperand::Reg(s, _) = &ops[0] { *s } else { 0 },
                if let AsmOperand::Reg(s, _) = &ops[1] { *s } else { 0 },
                512,
                0,
                1,
                0x58,
                0,
            ),
            "vmulps_z" if ops.len() == 3 => self.emit_asm_evex_3op_reg(
                if let AsmOperand::Reg(d, _) = &ops[2] { *d } else { 0 },
                if let AsmOperand::Reg(s, _) = &ops[0] { *s } else { 0 },
                if let AsmOperand::Reg(s, _) = &ops[1] { *s } else { 0 },
                512,
                0,
                1,
                0x59,
                0,
            ),

            // === Mask 寄存器 (k0-k7) 操作 ===
            // kmovw : VEX.128.0F (k1, k2)
            // 简化版: 假设 REX 编码: 仅支持 reg-reg 和 reg-mem
            "kmovw" if ops.len() == 2 => {
                if let (AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1]) {
                    if *ss <= 16 && *ds <= 16 {
                        // 0F 90 /r (vex.L0.0F.W0)
                        self.emit_vex3_prefix(*d, 0, 0, 0);
                        self.emit(&[0x90, Self::modrm_reg_reg(*d, *s)]);
                    }
                } else {
                    self.emit(&[0x90]);
                }
            },

            // crc32 : F2 0F 38 F0/F1 /r
            "crc32b" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 0, 0, 0xF0, 0), // F2 0F 38 F0
            "crc32w" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 1, 0, 0xF1, 0), // 66 0F 38 F1
            "crc32l" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 0, 0, 0xF1, 0), // F2 0F 38 F1
            "crc32q" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 0, 0, 0xF1, 1), // F2 0F 38 F1 + REX.W

            // movbe: 0F 38 F0 (load) / 0F 38 F1 (store)
            // AT&T: movbe mem, reg (load, byte-swap) / movbe reg, mem (store)
            "movbe" | "movbeq" if ops.len() == 2 => self.emit_asm_movbe(&ops[0], &ops[1], 64),
            "movbew" if ops.len() == 2 => self.emit_asm_movbe(&ops[0], &ops[1], 16),
            "movbel" if ops.len() == 2 => self.emit_asm_movbe(&ops[0], &ops[1], 32),

            // === MMX 指令 ===
            "emms" => self.emit(&[0x0F, 0x77]),
            "packsswb" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0x63),
            "packssdw" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0x6B),
            "packuswb" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0x67),
            "punpckhbw" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0x68),
            "punpckhwd" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0x69),
            "punpckhdq" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0x6A),
            "punpcklbw" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0x60),
            "punpcklwd" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0x61),
            "punpckldq" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0x62),
            "pshufw" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), src, AsmOperand::Reg(d, ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *ds == ASM_REG_MMX {
                        match src {
                            AsmOperand::Reg(s, ss) if *ss == ASM_REG_MMX => {
                                self.emit(&[0x0F, 0x70, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                            },
                            AsmOperand::Mem {
                                disp,
                                base,
                                index,
                                scale,
                            } => {
                                self.emit(&[
                                    Self::rex(false, 0, index.unwrap_or(0), *base),
                                    0x0F,
                                    0x70,
                                ]);
                                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
                                self.emit(&[*imm as u8]);
                            },
                            AsmOperand::SymDisp(sym, _) | AsmOperand::Sym(sym) => {
                                // RIP-relative source: mod=00 r/m=101
                                self.emit(&[
                                    0x0F,
                                    0x70,
                                    0x05 | ((d & 7) << 3),
                                    0,
                                    0,
                                    0,
                                    0,
                                    *imm as u8,
                                ]);
                                let pos = self.pos() - 5;
                                self.pending_data_rels.push((pos, sym.clone()));
                            },
                            _ => {
                                self.asm_warn("pshufw src must be mmx or mem");
                            },
                        }
                    } else {
                        self.asm_warn("pshufw dst must be mmx");
                    }
                } else {
                    self.asm_warn("pshufw needs imm, mmx/mem, mmx");
                }
            },
            "pmaddwd" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0xF5),
            "pmulhw" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0xE5),
            "pmullw" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0xD5),
            "pmulhrw" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0xE5), // pmulhrw 是 pmulhw 的别名
            "psllw" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0xF1),
            "psrlw" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0xD1),
            "psraw" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0xE1),
            "pslld" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0xF2),
            "psrld" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0xD2),
            "psrad" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0xE2),
            "psllq" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0xF3),
            "psrlq" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0xD3),
            // MMX 逻辑指令
            "por" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0xEB),
            "pand" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0xDB),
            "pandn" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0xDF),
            "pxor" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0xEF),
            // MMX 算术指令
            "paddb" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0xFC),
            "paddw" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0xFD),
            "paddd" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0xFE),
            "psubb" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0xF8),
            "psubw" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0xF9),
            "psubd" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0xFA),
            "paddsb" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0xEC),
            "paddsw" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0xED),
            "psubsb" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0xE8),
            "psubsw" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0xE9),
            "paddusb" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0xDC),
            "paddusw" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0xDD),
            "psubusb" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0xD8),
            "psubusw" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0xD9),
            // MMX 比较指令
            "pcmpeqb" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0x74),
            "pcmpeqw" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0x75),
            "pcmpeqd" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0x76),
            "pcmpgtb" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0x64),
            "pcmpgtw" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0x65),
            "pcmpgtd" if ops.len() == 2 => self.emit_asm_mmx_2op(&ops[0], &ops[1], 0x66),

            // === 字符串操作 (无操作数) ===
            "lodsb" => self.emit(&[0xAC]),
            "lodsw" => self.emit(&[0x66, 0xAD]),
            "lodsl" | "lodsd" => self.emit(&[0xAD]),
            "lodsq" => self.emit(&[0x48, 0xAD]),
            "cmpsw" => self.emit(&[0x66, 0xA7]),
            "cmpsl" | "cmpsd" => self.emit(&[0xA7]),
            "cmpsq" => self.emit(&[0x48, 0xA7]),
            "scasw" => self.emit(&[0x66, 0xAF]),
            "scasl" | "scasd" => self.emit(&[0xAF]),
            "scasq" => self.emit(&[0x48, 0xAF]),

            // === 循环指令 (单操作数: 相对偏移) ===
            "loop" if ops.len() == 1 => {
                if let AsmOperand::Imm(v) = &ops[0] {
                    self.emit(&[0xE2, *v as u8]);
                } else {
                    self.asm_warn("loop needs imm8 offset");
                }
            },
            "loope" | "loopz" if ops.len() == 1 => {
                if let AsmOperand::Imm(v) = &ops[0] {
                    self.emit(&[0xE1, *v as u8]);
                } else {
                    self.asm_warn("loope needs imm8 offset");
                }
            },
            "loopne" | "loopnz" if ops.len() == 1 => {
                if let AsmOperand::Imm(v) = &ops[0] {
                    self.emit(&[0xE0, *v as u8]);
                } else {
                    self.asm_warn("loopne needs imm8 offset");
                }
            },

            // === 杂项 ===
            "bound" if ops.len() == 2 => {
                self.asm_warn("bound is 80286 legacy");
            },
            "enter" if ops.len() == 2 => {
                if let (AsmOperand::Imm(imm16), AsmOperand::Imm(imm8)) = (&ops[0], &ops[1]) {
                    self.emit(&[0xC8, *imm16 as u8, (*imm16 >> 8) as u8, *imm8 as u8]);
                } else {
                    self.asm_warn("enter needs imm16, imm8");
                }
            },
            "into" => {
                self.asm_warn("into is 80286 legacy");
            },

            // === BMI1/BMI2 扩展 ===
            // andn: VEX.NDS.128.0F38.W0 F2 /r (dst, src1, src2)
            "andn" if ops.len() == 3 => {
                if let (AsmOperand::Reg(s, _ss), AsmOperand::Reg(v, _vs), AsmOperand::Reg(d, _ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *s <= 15 && *d <= 15 && *v <= 15 {
                        self.emit_vex4_prefix(*d, *s, 2, 1, *v, 0, 0);
                        self.emit(&[0xF2, Self::modrm_reg_reg(*d, *s)]);
                    } else {
                        self.asm_warn("andn needs GPR operands");
                    }
                } else {
                    self.asm_warn("andn needs reg, reg, reg");
                }
            },
            // bextr: VEX.NDS.128.0F38.W0 F7 /r (dst, src1, src2)
            "bextr" if ops.len() == 3 => {
                if let (AsmOperand::Reg(s, _ss), AsmOperand::Reg(v, _vs), AsmOperand::Reg(d, _ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *s <= 15 && *d <= 15 && *v <= 15 {
                        self.emit_vex4_prefix(*d, *s, 2, 1, *v, 0, 0);
                        self.emit(&[0xF7, Self::modrm_reg_reg(*d, *s)]);
                    } else {
                        self.asm_warn("bextr needs GPR operands");
                    }
                } else {
                    self.asm_warn("bextr needs reg, reg, reg");
                }
            },
            // blsi: VEX.NDS.128.0F38.W0 F3/3 (dst, src)
            "blsi" if ops.len() == 2 => {
                if let (AsmOperand::Reg(s, _ss), AsmOperand::Reg(d, _ds)) = (&ops[0], &ops[1]) {
                    if *s <= 15 && *d <= 15 {
                        self.emit_vex4_prefix(*d, *s, 2, 1, *d, 0, 0);
                        self.emit(&[0xF3, Self::modrm_reg_reg(3, *s)]);
                    } else {
                        self.asm_warn("blsi needs GPR operands");
                    }
                } else {
                    self.asm_warn("blsi needs reg, reg");
                }
            },
            // blsmsk: VEX.NDS.128.0F38.W0 F3/2 (dst, src)
            "blsmsk" if ops.len() == 2 => {
                if let (AsmOperand::Reg(s, _ss), AsmOperand::Reg(d, _ds)) = (&ops[0], &ops[1]) {
                    if *s <= 15 && *d <= 15 {
                        self.emit_vex4_prefix(*d, *s, 2, 1, *d, 0, 0);
                        self.emit(&[0xF3, Self::modrm_reg_reg(2, *s)]);
                    } else {
                        self.asm_warn("blsmsk needs GPR operands");
                    }
                } else {
                    self.asm_warn("blsmsk needs reg, reg");
                }
            },
            // blsr: VEX.NDS.128.0F38.W0 F3/1 (dst, src)
            "blsr" if ops.len() == 2 => {
                if let (AsmOperand::Reg(s, _ss), AsmOperand::Reg(d, _ds)) = (&ops[0], &ops[1]) {
                    if *s <= 15 && *d <= 15 {
                        self.emit_vex4_prefix(*d, *s, 2, 1, *d, 0, 0);
                        self.emit(&[0xF3, Self::modrm_reg_reg(1, *s)]);
                    } else {
                        self.asm_warn("blsr needs GPR operands");
                    }
                } else {
                    self.asm_warn("blsr needs reg, reg");
                }
            },
            // bzhi: VEX.NDS.128.0F38.W0 F5 /r (dst, src1, src2)
            "bzhi" if ops.len() == 3 => {
                if let (AsmOperand::Reg(s, _ss), AsmOperand::Reg(v, _vs), AsmOperand::Reg(d, _ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *s <= 15 && *d <= 15 && *v <= 15 {
                        self.emit_vex4_prefix(*d, *s, 2, 1, *v, 0, 0);
                        self.emit(&[0xF5, Self::modrm_reg_reg(*d, *s)]);
                    } else {
                        self.asm_warn("bzhi needs GPR operands");
                    }
                } else {
                    self.asm_warn("bzhi needs reg, reg, reg");
                }
            },
            // mulx: VEX.NDS.128.F2.0F38.W0 F6 /r (dst, src1, src2)
            "mulx" if ops.len() == 3 => {
                if let (AsmOperand::Reg(s, _ss), AsmOperand::Reg(v, _vs), AsmOperand::Reg(d, _ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *s <= 15 && *d <= 15 && *v <= 15 {
                        self.emit_vex4_prefix(*d, *s, 2, 1, *v, 0, 3);
                        self.emit(&[0xF6, Self::modrm_reg_reg(*d, *s)]);
                    } else {
                        self.asm_warn("mulx needs GPR operands");
                    }
                } else {
                    self.asm_warn("mulx needs reg, reg, reg");
                }
            },
            // pdep: VEX.NDS.128.F2.0F38.W0 F2 /r (dst, src1, src2)
            "pdep" if ops.len() == 3 => {
                if let (AsmOperand::Reg(s, _ss), AsmOperand::Reg(v, _vs), AsmOperand::Reg(d, _ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *s <= 15 && *d <= 15 && *v <= 15 {
                        self.emit_vex4_prefix(*d, *s, 2, 1, *v, 0, 3);
                        self.emit(&[0xF2, Self::modrm_reg_reg(*d, *s)]);
                    } else {
                        self.asm_warn("pdep needs GPR operands");
                    }
                } else {
                    self.asm_warn("pdep needs reg, reg, reg");
                }
            },
            // pext: VEX.NDS.128.F3.0F38.W0 F5 /r (dst, src1, src2)
            "pext" if ops.len() == 3 => {
                if let (AsmOperand::Reg(s, _ss), AsmOperand::Reg(v, _vs), AsmOperand::Reg(d, _ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *s <= 15 && *d <= 15 && *v <= 15 {
                        self.emit_vex4_prefix(*d, *s, 2, 1, *v, 0, 2);
                        self.emit(&[0xF5, Self::modrm_reg_reg(*d, *s)]);
                    } else {
                        self.asm_warn("pext needs GPR operands");
                    }
                } else {
                    self.asm_warn("pext needs reg, reg, reg");
                }
            },
            // rorx: VEX.128.F2.0F3A.W0 F0 /r ib (dst, src, imm8)
            "rorx" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, _ss), AsmOperand::Reg(d, _ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *s <= 15 && *d <= 15 {
                        self.emit_vex4_prefix(*d, *s, 3, 1, 0, 0, 3);
                        self.emit(&[0xF0, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    } else {
                        self.asm_warn("rorx needs GPR operands");
                    }
                } else {
                    self.asm_warn("rorx needs imm, reg, reg");
                }
            },
            // sarx/shlx/shrx: VEX.NDS.128.0F38.W0 F7/F7/F7 /r (dst, src1, src2)
            "sarx" if ops.len() == 3 => {
                if let (AsmOperand::Reg(s, _ss), AsmOperand::Reg(v, _vs), AsmOperand::Reg(d, _ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *s <= 15 && *d <= 15 && *v <= 15 {
                        self.emit_vex4_prefix(*d, *s, 2, 1, *v, 0, 0);
                        self.emit(&[0xF7, Self::modrm_reg_reg(7, *s)]);
                    } else {
                        self.asm_warn("sarx needs GPR operands");
                    }
                } else {
                    self.asm_warn("sarx needs reg, reg, reg");
                }
            },
            "shlx" if ops.len() == 3 => {
                if let (AsmOperand::Reg(s, _ss), AsmOperand::Reg(v, _vs), AsmOperand::Reg(d, _ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *s <= 15 && *d <= 15 && *v <= 15 {
                        self.emit_vex4_prefix(*d, *s, 2, 1, *v, 0, 0);
                        self.emit(&[0xF7, Self::modrm_reg_reg(6, *s)]);
                    } else {
                        self.asm_warn("shlx needs GPR operands");
                    }
                } else {
                    self.asm_warn("shlx needs reg, reg, reg");
                }
            },
            "shrx" if ops.len() == 3 => {
                if let (AsmOperand::Reg(s, _ss), AsmOperand::Reg(v, _vs), AsmOperand::Reg(d, _ds)) =
                    (&ops[0], &ops[1], &ops[2])
                {
                    if *s <= 15 && *d <= 15 && *v <= 15 {
                        self.emit_vex4_prefix(*d, *s, 2, 1, *v, 0, 0);
                        self.emit(&[0xF7, Self::modrm_reg_reg(5, *s)]);
                    } else {
                        self.asm_warn("shrx needs GPR operands");
                    }
                } else {
                    self.asm_warn("shrx needs reg, reg, reg");
                }
            },
            // adcx: 66 0F 38 F6 /r (dst, src)
            "adcx" if ops.len() == 2 => {
                if let (AsmOperand::Reg(s, _ss), AsmOperand::Reg(d, _ds)) = (&ops[0], &ops[1]) {
                    if *s <= 15 && *d <= 15 {
                        self.emit(&[
                            Self::rex(false, *d, 0, *s),
                            0x66,
                            0x0F,
                            0x38,
                            0xF6,
                            Self::modrm_reg_reg(*d, *s),
                        ]);
                    } else {
                        self.asm_warn("adcx needs GPR operands");
                    }
                } else {
                    self.asm_warn("adcx needs reg, reg");
                }
            },
            // adox: F3 0F 38 F6 /r (dst, src)
            "adox" if ops.len() == 2 => {
                if let (AsmOperand::Reg(s, _ss), AsmOperand::Reg(d, _ds)) = (&ops[0], &ops[1]) {
                    if *s <= 15 && *d <= 15 {
                        self.emit(&[
                            Self::rex(false, *d, 0, *s),
                            0xF3,
                            0x0F,
                            0x38,
                            0xF6,
                            Self::modrm_reg_reg(*d, *s),
                        ]);
                    } else {
                        self.asm_warn("adox needs GPR operands");
                    }
                } else {
                    self.asm_warn("adox needs reg, reg");
                }
            },

            // 未识别指令: 硬错误 (P0-5: 不再静默替换为 NOP)
            _ => {
                self.asm_warn(&format!(
                    "unsupported inline asm instruction '{}' (asm line {})",
                    mnemonic, self.current_asm_line,
                ));
            },
        }
    }
    fn emit_asm_line(&mut self, line: &str) {
        let line = Self::unescape_asm_template(AsmLocalLabels::strip_asm_comment(line).trim());
        if line.is_empty() {
            return;
        }
        if self.emit_asm_directive(&line) {
            return;
        }
        // 非 directive: 记录指令起始 PC (用于逐指令 DWARF 行号表)
        let debug_pc = self.code.len() as u64;
        let debug_src_line = self.current_asm_src_line;
        if Self::asm_has_unresolved_operand(&line) {
            self.warn_unresolved_asm_operand(&line, self.current_asm_line);
            // 未解析操作数: 错误已由 warn_unresolved_asm_operand 报告,
            // 不再发射 NOP 占位 (P0-5: 硬错误而非静默回退)
            if self.debug_level > 0 && debug_src_line > 0 {
                let disc = self.asm_insn_debug_entries.len() as u16;
                self.asm_insn_debug_entries
                    .push((debug_pc, debug_src_line, disc));
            }
            return;
        }
        // 处理前缀: lock, rep, repz, repnz, 段前缀 (ds/cs/ss/es/fs/gs)
        let (prefix, rest) = if line.starts_with("lock ") || line.starts_with("lock\t") {
            (Some(0xF0u8), line[5..].trim())
        } else if line.starts_with("rep ")
            || line.starts_with("rep\t")
            || line.starts_with("repz ")
            || line.starts_with("repz\t")
        {
            (Some(0xF3u8), line[4..].trim())
        } else if line.starts_with("repnz ") || line.starts_with("repnz\t") {
            (Some(0xF2u8), line[6..].trim())
        } else if line.starts_with("repe ") || line.starts_with("repe\t") || line == "repe" {
            // repe 与 repz 同义: F3 前缀 (不是 F2)
            (Some(0xF3u8), line.strip_prefix("repe").unwrap_or("").trim())
        } else {
            // 段前缀: 形如 "ds clflush %0" / "gs movq ..."
            let seg_pfx = ["ds ", "cs ", "ss ", "es ", "fs ", "gs "]
                .iter()
                .find(|p| line.starts_with(*p));
            if let Some(p) = seg_pfx {
                (
                    Some(match p.as_bytes()[0] {
                        b'd' => 0x3E,
                        b'c' => 0x2E,
                        b's' => 0x36,
                        b'e' => 0x26,
                        b'f' => 0x64,
                        _ => 0x65,
                    }),
                    line[p.len()..].trim(),
                )
            } else {
                (None, line.as_str())
            }
        };
        // 分割助记符和操作数
        let (mnemonic, operands_str) = if let Some(pos) = rest.find([' ', '\t']) {
            (&rest[..pos], rest[pos..].trim())
        } else {
            (rest, "")
        };
        // 前缀之后无指令: 如单独一行的 "repe" (前缀已发射, 无指令需要编码)
        if mnemonic.is_empty() {
            return;
        }
        // 纯寄存器引用: asm 模板仅为操作数占位符 (如 "%0"),
        // 替换后只剩寄存器名 (如 "%rax"). 这不是有效指令, 但
        // GCC 兼容: 视为无操作 (920520-1.c).
        if mnemonic.starts_with('%') && operands_str.is_empty() {
            return;
        }
        // 汇编器宏调用 (如 extable_type_reg reg=.., type=..): 无机器码, 静默忽略
        if operands_str.contains('=')
            && !mnemonic.is_empty()
            && !matches!(mnemonic, "mov" | "movq" | "movl" | "movw" | "movb")
        {
            return;
        }
        // 剥离操作数中的内联段前缀 (%gs:/%fs:/%ds: 等)
        // 内核 per-cpu 模式: "add %%gs:%1, %0" → 替换后 "add %gs:gs_val(%rip), %rax"
        // 剥离段前缀字节并在指令前发射, 操作数按无前缀形式解析
        // 注意: 如果段前缀后只有位移量 (如 %fs:0), 需要转换为 %fs:0(%rip) 形式
        // 以便正确解析为内存操作数
        let mut inline_seg_prefix: Option<u8> = None;
        let operands_str = {
            let seg_patterns = [
                ("%gs:", 0x65u8),
                ("%fs:", 0x64),
                ("%ds:", 0x3E),
                ("%cs:", 0x2E),
                ("%ss:", 0x36),
                ("%es:", 0x26),
            ];
            let mut os: String = operands_str.to_string();
            for &(pat, code) in &seg_patterns {
                if os.contains(pat) {
                    inline_seg_prefix = Some(code);
                    // 检查段前缀后是否是纯位移量 (没有括号)
                    // 如果是, 转换为 disp(%rip) 形式以便正确解析为内存操作数
                    let after_pat = os.replacen(pat, "", 1);
                    let first_operand = after_pat.split(',').next().unwrap_or("").trim();
                    if !first_operand.is_empty()
                        && !first_operand.contains('(')
                        && !first_operand.starts_with('%')
                        && !first_operand.starts_with('$')
                    {
                        // 纯位移量: 转换为 disp(%rip) 形式
                        os = after_pat.replacen(
                            first_operand,
                            &format!("{}(%rip)", first_operand),
                            1,
                        );
                    } else {
                        os = after_pat;
                    }
                    break;
                }
            }
            os
        };
        let operands: Vec<&str> = if operands_str.is_empty() {
            vec![]
        } else {
            operands_str.split(',').collect()
        };
        let ops: Vec<AsmOperand> = operands
            .iter()
            .map(|s| Self::parse_asm_operand_str(s))
            .collect();
        if let Some(pfx) = prefix {
            self.emit(&[pfx]);
        }
        if let Some(seg_pfx) = inline_seg_prefix {
            self.emit(&[seg_pfx]);
        }
        self.emit_asm_instr(mnemonic, &ops);
        // 记录逐指令调试信息 (用于 DWARF .debug_line per-instruction 条目)
        if self.debug_level > 0 && debug_src_line > 0 {
            let pc_end = self.code.len() as u64;
            if pc_end > debug_pc {
                let disc = self.asm_insn_debug_entries.len() as u16;
                self.asm_insn_debug_entries
                    .push((debug_pc, debug_src_line, disc));
            }
        }
    }
    pub(crate) fn emit_asm_text_lines(
        &mut self,
        resolved_asm: &str,
        span: &crate::diagnostics::Span,
    ) {
        let lines: Vec<String> = resolved_asm
            .split('\n')
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        for line in &lines {
            self.current_asm_src_line = span.line;
            self.emit_asm_line(line);
        }
    }
    pub(crate) fn emit_resolved_asm_lines(&mut self, resolved_lines: &[String], is_global: bool) {
        let scope = self.next_label;
        self.next_label += 1;
        let locals = AsmLocalLabels::scan(resolved_lines, scope);
        if is_global {
            self.asm_last_text_label = None;
        }
        // Track .pushsection/.popsection: collect data into custom sections
        // (e.g. .altinstructions, __bug_table, __ex_table, .fixup).
        let mut skip_depth: u32 = 0;
        // 数字标签映射: "N" → 内部名 (用于 pushsection 数据中的符号引用)
        let mut numeric_label_map: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for (i, resolved) in resolved_lines.iter().enumerate() {
            let trimmed = resolved.trim();
            if trimmed.is_empty() {
                continue;
            }
            // .pushsection: 非文本节 → 收集数据; 文本节 → 正常处理
            if trimmed.starts_with(".pushsection") {
                // 解析 section name: 第二个 whitespace token, 截取逗号前部分
                // .pushsection name, "flags", type, entsize
                // .pushsection name "flags"  (无逗号)
                let second_token = trimmed.split_whitespace().nth(1).unwrap_or("");
                let sec_name = second_token
                    .split(',')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .trim_matches('"');
                if !sec_name.starts_with(".text") && !sec_name.is_empty() {
                    // 解析 flags ("ax" → 可执行, "aw" → 可写, "aMS" → merge/strings)
                    let (is_writable, is_exec, is_merge, is_strings, entsize) =
                        Self::parse_pushsection_flags(trimmed);
                    let sec_idx = self.ensure_custom_section_with_flags(
                        sec_name,
                        is_exec,
                        is_writable,
                        is_merge,
                        is_strings,
                        entsize,
                    );
                    // 可执行节 (.fixup/.altinstr_replacement): 代码先发射到
                    // self.code, .popsection 时记录区间, 最终整体搬迁到自定义节.
                    if is_exec {
                        self.pushsection_exec_stack
                            .push((sec_idx, self.code.len() as u64));
                    }
                    self.pushsection_stack.push(sec_idx);
                    self.section_active = false;
                    continue;
                }
            }
            // .section name, "flags": 切换节 (类似 .pushsection 但用独立栈)
            // VDSO 构建中 .section/.previous 模式常见
            if trimmed.starts_with(".section") && !trimmed.starts_with(".section.") {
                let second_token = trimmed.split_whitespace().nth(1).unwrap_or("");
                let sec_name = second_token
                    .split(',')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .trim_matches('"');
                if !sec_name.starts_with(".text") && !sec_name.is_empty() {
                    let (is_writable, is_exec, is_merge, is_strings, entsize) =
                        Self::parse_pushsection_flags(trimmed);
                    let sec_idx = self.ensure_custom_section_with_flags(
                        sec_name,
                        is_exec,
                        is_writable,
                        is_merge,
                        is_strings,
                        entsize,
                    );
                    if is_exec {
                        self.pushsection_exec_stack
                            .push((sec_idx, self.code.len() as u64));
                    }
                    self.section_stack.push(sec_idx);
                    self.pushsection_stack.push(sec_idx);
                    self.section_active = true;
                    continue;
                }
            }
            // .previous: 回到 .section 切换前的节
            if trimmed.starts_with(".previous") {
                if self.section_active {
                    // 当前上下文由 .section 创建, 需关闭并从 section_stack 恢复
                    if let Some(&top_sec) = self.pushsection_stack.last() {
                        let top_is_exec =
                            self.extra_sections[top_sec].flags & shared_elf::SHF_EXECINSTR != 0;
                        if top_is_exec {
                            if let Some((sec_idx, start)) = self.pushsection_exec_stack.pop() {
                                let size = self.code.len() as u64 - start;
                                if size > 0 {
                                    let align = self.extra_sections[sec_idx].align.max(16);
                                    let len = self.extra_sections[sec_idx].data.len();
                                    let aligned =
                                        (len + align as usize - 1) & !(align as usize - 1);
                                    self.pad_custom_section_to(sec_idx, aligned);
                                    let sec_off = self.extra_sections[sec_idx].data.len() as u64;
                                    self.pad_custom_section_to(sec_idx, (sec_off + size) as usize);
                                    self.pushsection_code_ranges
                                        .push((start, size, sec_idx, sec_off));
                                }
                            }
                        }
                        self.pushsection_stack.pop();
                    }
                    // 从 section_stack 恢复上一节
                    if let Some(prev_sec) = self.section_stack.pop() {
                        let prev_is_exec =
                            self.extra_sections[prev_sec].flags & shared_elf::SHF_EXECINSTR != 0;
                        if prev_is_exec {
                            self.pushsection_exec_stack
                                .push((prev_sec, self.code.len() as u64));
                        }
                        self.pushsection_stack.push(prev_sec);
                        self.section_active = true;
                    } else {
                        self.section_active = false;
                    }
                }
                continue;
            }
            // .popsection: 退出 pushsection 模式
            if trimmed.starts_with(".popsection") {
                // Determine if the section being closed is executable.
                // The exec_stack only tracks exec sections, but pushsection_stack
                // tracks ALL non-text sections. A nested non-exec pushsection inside
                // an exec pushsection must NOT pop the exec stack.
                let top_is_exec = self
                    .pushsection_stack
                    .last()
                    .map(|&idx| self.extra_sections[idx].flags & shared_elf::SHF_EXECINSTR != 0)
                    .unwrap_or(false);
                if top_is_exec {
                    if let Some((sec_idx, start)) = self.pushsection_exec_stack.pop() {
                        // 记录可执行节的代码区间, 供最终搬迁到自定义节
                        let size = self.code.len() as u64 - start;
                        if size > 0 {
                            let align = self.extra_sections[sec_idx].align.max(16);
                            let len = self.extra_sections[sec_idx].data.len();
                            let aligned = (len + align as usize - 1) & !(align as usize - 1);
                            self.pad_custom_section_to(sec_idx, aligned);
                            let sec_off = self.extra_sections[sec_idx].data.len() as u64;
                            self.pad_custom_section_to(sec_idx, (sec_off + size) as usize);
                            self.pushsection_code_ranges
                                .push((start, size, sec_idx, sec_off));
                        }
                    }
                    self.pushsection_stack.pop();
                } else if !self.pushsection_stack.is_empty() {
                    // Non-exec pushsection closing: just pop the regular stack
                    self.pushsection_stack.pop();
                } else {
                    skip_depth = skip_depth.saturating_sub(1);
                }
                continue;
            }
            // 在 pushsection 块内: 收集数据到自定义节
            if !self.pushsection_stack.is_empty() {
                let sec_idx = *self.pushsection_stack.last().unwrap();
                let is_exec = self.extra_sections[sec_idx].flags & shared_elf::SHF_EXECINSTR != 0;
                if is_exec {
                    // Handle nested .pushsection/.popsection inside exec pushsection
                    if trimmed.starts_with(".pushsection") {
                        let second_token = trimmed.split_whitespace().nth(1).unwrap_or("");
                        let sec_name = second_token.split(',').next().unwrap_or("").trim();
                        if !sec_name.starts_with(".text") && !sec_name.is_empty() {
                            let (is_w, is_e, is_m, is_s, ent) =
                                Self::parse_pushsection_flags(trimmed);
                            let nested_idx = self.ensure_custom_section_with_flags(
                                sec_name, is_e, is_w, is_m, is_s, ent,
                            );
                            if is_e {
                                self.pushsection_exec_stack
                                    .push((nested_idx, self.code.len() as u64));
                            }
                            self.pushsection_stack.push(nested_idx);
                            self.section_active = false;
                            continue;
                        }
                    }
                    // Handle nested .section inside exec pushsection
                    if trimmed.starts_with(".section") && !trimmed.starts_with(".section.") {
                        let second_token = trimmed.split_whitespace().nth(1).unwrap_or("");
                        let sec_name = second_token.split(',').next().unwrap_or("").trim();
                        if !sec_name.starts_with(".text") && !sec_name.is_empty() {
                            let (is_w, is_e, is_m, is_s, ent) =
                                Self::parse_pushsection_flags(trimmed);
                            let nested_idx = self.ensure_custom_section_with_flags(
                                sec_name, is_e, is_w, is_m, is_s, ent,
                            );
                            if is_e {
                                self.pushsection_exec_stack
                                    .push((nested_idx, self.code.len() as u64));
                            }
                            self.section_stack.push(nested_idx);
                            self.pushsection_stack.push(nested_idx);
                            self.section_active = true;
                            continue;
                        }
                    }
                    // Handle nested .previous inside exec pushsection
                    if trimmed.starts_with(".previous") {
                        if self.section_active {
                            if let Some(&top_sec) = self.pushsection_stack.last() {
                                let top_is_exec = self.extra_sections[top_sec].flags
                                    & shared_elf::SHF_EXECINSTR
                                    != 0;
                                if top_is_exec {
                                    if let Some((sec_idx, start)) =
                                        self.pushsection_exec_stack.pop()
                                    {
                                        let size = self.code.len() as u64 - start;
                                        if size > 0 {
                                            let align = self.extra_sections[sec_idx].align.max(16);
                                            let len = self.extra_sections[sec_idx].data.len();
                                            let aligned =
                                                (len + align as usize - 1) & !(align as usize - 1);
                                            self.pad_custom_section_to(sec_idx, aligned);
                                            let sec_off =
                                                self.extra_sections[sec_idx].data.len() as u64;
                                            self.pad_custom_section_to(
                                                sec_idx,
                                                (sec_off + size) as usize,
                                            );
                                            self.pushsection_code_ranges
                                                .push((start, size, sec_idx, sec_off));
                                        }
                                    }
                                }
                                self.pushsection_stack.pop();
                            }
                            if let Some(prev_sec) = self.section_stack.pop() {
                                let prev_is_exec = self.extra_sections[prev_sec].flags
                                    & shared_elf::SHF_EXECINSTR
                                    != 0;
                                if prev_is_exec {
                                    self.pushsection_exec_stack
                                        .push((prev_sec, self.code.len() as u64));
                                }
                                self.pushsection_stack.push(prev_sec);
                                self.section_active = true;
                            } else {
                                self.section_active = false;
                            }
                        }
                        continue;
                    }
                    if trimmed.starts_with(".popsection") {
                        let top_is_exec = self
                            .pushsection_stack
                            .last()
                            .map(|&idx| {
                                self.extra_sections[idx].flags & shared_elf::SHF_EXECINSTR != 0
                            })
                            .unwrap_or(false);
                        if top_is_exec {
                            if let Some((sec_idx, start)) = self.pushsection_exec_stack.pop() {
                                let size = self.code.len() as u64 - start;
                                if size > 0 {
                                    let align = self.extra_sections[sec_idx].align.max(16);
                                    let len = self.extra_sections[sec_idx].data.len();
                                    let aligned =
                                        (len + align as usize - 1) & !(align as usize - 1);
                                    self.pad_custom_section_to(sec_idx, aligned);
                                    let sec_off = self.extra_sections[sec_idx].data.len() as u64;
                                    self.pad_custom_section_to(sec_idx, (sec_off + size) as usize);
                                    self.pushsection_code_ranges
                                        .push((start, size, sec_idx, sec_off));
                                }
                            }
                            self.pushsection_stack.pop();
                        } else if !self.pushsection_stack.is_empty() {
                            self.pushsection_stack.pop();
                        } else {
                            skip_depth = skip_depth.saturating_sub(1);
                        }
                        continue;
                    }
                    // 可执行 pushsection: 指令/数据全部发射到 self.code,
                    // 标签与跳转在 .text 坐标解析, 退出时整体搬迁.
                    if trimmed.starts_with(".globl ") || trimmed.starts_with(".global ") {
                        if let Some(sym) = trimmed.split_whitespace().nth(1) {
                            self.global_asm_exports.insert(Self::asm_symbol_name(sym));
                        }
                        continue;
                    }
                    if trimmed.split_whitespace().next() == Some(".type") {
                        self.record_asm_type_directive(trimmed);
                        continue;
                    }
                    if trimmed.split_whitespace().next() == Some(".size") {
                        self.record_asm_size_directive(trimmed);
                        continue;
                    }
                    if let Some(name) = locals.name_at(i) {
                        self.label(name);
                        continue;
                    }
                    if let Some(label) = Self::parse_asm_named_label(trimmed) {
                        self.label(label);
                        if is_global {
                            self.record_asm_text_label(label);
                        }
                        let has_body = trimmed
                            .split_once(':')
                            .map(|(_, rest)| !rest.trim().is_empty())
                            .unwrap_or(false);
                        if !has_body {
                            continue;
                        }
                    }
                    let subbed = locals.substitute(trimmed, i);
                    self.current_asm_line = i + 1;
                    if Self::asm_has_unresolved_operand(&subbed) {
                        self.warn_unresolved_asm_operand(&subbed, i + 1);
                    } else if subbed.trim().starts_with('.') {
                        self.emit_asm_directive(&subbed);
                    } else {
                        self.emit_asm_line(&subbed);
                    }
                    continue;
                }
                // 仍然注册数字标签
                if let Some(name) = locals.name_at(i) {
                    self.label(name);
                    numeric_label_map.insert(
                        name.split('_').nth(3).unwrap_or("0").to_string(),
                        name.to_string(),
                    );
                }
                self.emit_pushsection_data(trimmed, sec_idx, &locals, i, &numeric_label_map);
                continue;
            }
            // Skip lines in non-text sections (legacy fallback), but still
            // register numeric labels so references don't panic.
            if skip_depth > 0 {
                if let Some(name) = locals.name_at(i) {
                    self.label(name);
                }
                continue;
            }
            // 数字局部标签 N:
            if let Some(name) = locals.name_at(i) {
                self.label(name);
                if self.emit_asm_text && (is_global || trimmed.ends_with(':')) {
                    self.asm_directives_at
                        .push((self.pos(), trimmed.to_string()));
                    self.asm_directives.push(trimmed.to_string());
                }
                continue;
            }
            if self.emit_asm_text && (trimmed.starts_with('.') || trimmed.ends_with(':')) {
                self.asm_directives_at
                    .push((self.pos(), trimmed.to_string()));
                self.asm_directives.push(trimmed.to_string());
                if !trimmed.ends_with(':') {
                    continue;
                }
            }
            if is_global
                && !self.emit_asm_text
                && (trimmed.starts_with(".globl ") || trimmed.starts_with(".global "))
            {
                if let Some(sym) = trimmed.split_whitespace().nth(1) {
                    self.global_asm_exports.insert(Self::asm_symbol_name(sym));
                }
                continue;
            }
            if !self.emit_asm_text && trimmed.split_whitespace().next() == Some(".type") {
                self.record_asm_type_directive(trimmed);
                continue;
            }
            if !self.emit_asm_text && trimmed.split_whitespace().next() == Some(".size") {
                self.record_asm_size_directive(trimmed);
                continue;
            }
            // 命名标签 foo:（含 . 局部标签如 .slowpath:，排除 directive）
            if let Some(label) = Self::parse_asm_named_label(trimmed) {
                self.label(label);
                if is_global {
                    self.record_asm_text_label(label);
                }
                continue;
            }
            if self.emit_asm_text && trimmed.starts_with('.') {
                continue;
            }
            let subbed = locals.substitute(trimmed, i);
            self.current_asm_line = i + 1;
            self.emit_asm_line(&subbed);
        }
        // Resolve deferred pushsection fixups (forward label references)
        self.resolve_deferred_pushsection_fixups();
        if is_global && !self.emit_asm_text {
            self.finish_pending_empty_asm_function();
        }
    }
    pub(crate) fn emit_unsupported_asm_operands(&mut self, context: &str) {
        self.asm_warn(&format!("unsupported {context} operand pattern"));
    }
    pub(crate) fn finish_empty_asm_function(&mut self, name: &str, start: u64) {
        if self.pos() != start || !self.asm_symbol_is_function(name) {
            return;
        }
        self.emit_return_tail();
        self.func_sizes
            .insert(name.to_string(), self.pos().saturating_sub(start));
    }
    fn finish_pending_empty_asm_function(&mut self) {
        let Some((name, start)) = self.asm_last_text_label.clone() else {
            return;
        };
        if self.func_sizes.contains_key(&name) {
            return;
        }
        self.finish_empty_asm_function(&name, start);
    }
}
