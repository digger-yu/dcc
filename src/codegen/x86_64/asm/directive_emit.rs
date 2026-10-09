//! asm/directive_emit.rs —— 自历史完整实现（git 28a9063 单文件 Codegen）按职责簇恢复的模块。
//! 仅移动/可见性调整，不改语义（backend_refactor_plan §3.1 原则 1）。
#![allow(clippy::all)]
#![allow(unused_imports)]
use crate::ast::*;
use crate::codegen::shared::elf as shared_elf;
use crate::codegen::shared::elf::*;
use crate::codegen::shared::*;
use crate::codegen::x86_64::asm::*;
use crate::codegen::x86_64::Codegen;
use crate::codegen::x86_64::insn::Insn;
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
    fn emit_asm_code_data_directive(&mut self, args: &str, size: usize) {
        for expr in Self::split_asm_args(args) {
            let expr = expr.trim();
            let off = self.pos();
            // 纯数字算术表达式 (含括号、*、+、-): 直接求值成立即数. 典型样本
            // `.altinstructions` 节里 `.long (20*32+ 2)` —— 内核 CPU 位号宏展开后
            // 就是这种立即数. 老代码会把整串当 symbol 名发出去, 链接时报
            // `undefined reference to '(20*32+ 2)'`.
            if let Some(v) = Self::eval_pure_numeric_arith(expr) {
                let bytes = v.to_le_bytes();
                self.emit_insn(&Insn::raw_bytes(&bytes[..size]));
                continue;
            }
            if let Some((symbol, addend)) = Self::parse_asm_pc_relative_long(expr) {
                self.pending_code_rels.push(PendingCodeRel {
                    offset: off,
                    symbol,
                    rel_type: R_X86_64_PC32,
                    addend,
                });
                self.emit_insn(&Insn::raw_bytes(&vec![0u8; size]));
                continue;
            }
            if let Some(v) = Self::parse_asm_imm_token(expr) {
                let bytes = v.to_le_bytes();
                self.emit_insn(&Insn::raw_bytes(&bytes[..size]));
            } else if size == 4 && !expr.is_empty() {
                self.pending_code_rels.push(PendingCodeRel {
                    offset: off,
                    symbol: expr.to_string(),
                    rel_type: R_X86_64_32,
                    addend: 0,
                });
                self.emit_insn(&Insn::raw_bytes(&[0u8; 4]));
            } else {
                self.emit_insn(&Insn::raw_bytes(&vec![0u8; size]));
            }
        }
    }
    pub(crate) fn emit_asm_desc(&mut self, op: &AsmOperand, ext: u8) {
        match op {
            AsmOperand::Reg(r, _) => {
                self.emit_insn(&Insn::raw_bytes(&[
                    Self::rex(false, 0, 0, *r),
                    0x0F,
                    0x00,
                    ((ext & 7) << 3) | (r & 7),
                ]));
            },
            AsmOperand::Mem {
                disp,
                base,
                index,
                scale,
            } => {
                self.emit_insn(&Insn::raw_bytes(&[Self::rex(false, 0, index.unwrap_or(0), *base), 0x0F, 0x00]));
                self.emit_asm_modrm_mem_only(ext, *disp, *base, *index, *scale);
            },
            _ => {
                self.asm_reject("unsupported descriptor operand pattern");
            },
        }
    }
    pub(crate) fn emit_asm_directive(&mut self, line: &str) -> bool {
        let t = line.trim();
        if !t.starts_with('.') {
            return false;
        }
        let parts: Vec<&str> = t.split_whitespace().collect();
        if parts.is_empty() {
            return true;
        }
        match parts[0] {
            ".pushsection" | ".popsection" | ".section" | ".previous" | ".type" | ".size"
            | ".ident" | ".globl" | ".global" | ".weak" | ".hidden" | ".protected" => true,
            ".balign" | ".p2align" | ".align" => {
                if let Some(n) = parts
                    .get(1)
                    .and_then(|s| s.trim_end_matches(',').parse::<usize>().ok())
                {
                    let align = if parts[0] == ".p2align" {
                        1usize << n.min(12)
                    } else {
                        n.max(1)
                    };
                    while !self.code.is_empty() && !self.code.len().is_multiple_of(align) {
                        self.emit_insn(&Insn::raw_bytes(&[0x90]));
                    }
                }
                true
            },
            ".int" | ".long" | ".4byte" => {
                let args = t[parts[0].len()..].trim();
                self.emit_asm_code_data_directive(args, 4);
                true
            },
            ".quad" | ".8byte" => {
                let args = t[parts[0].len()..].trim();
                self.emit_asm_code_data_directive(args, 8);
                true
            },
            ".short" | ".word" | ".2byte" => {
                let args = t[parts[0].len()..].trim();
                self.emit_asm_code_data_directive(args, 2);
                true
            },
            ".byte" => {
                let args = t[parts[0].len()..].trim();
                self.emit_asm_code_data_directive(args, 1);
                true
            },
            ".asciz" | ".string" => {
                if let Some(s) = parts.get(1) {
                    let s = s.trim_matches('"');
                    if self.emit_asm_text {
                        // 文本模式: 保留为 .s 输出行 (DEFINE() 支持)
                        self.asm_directives_at.push((self.pos(), t.to_string()));
                        self.asm_directives.push(t.to_string());
                    } else {
                        for b in s.bytes() {
                            self.emit_insn(&Insn::raw_bytes(&[b]));
                        }
                        self.emit_insn(&Insn::raw_bytes(&[0]));
                    }
                }
                true
            },
            ".ascii" => {
                if self.emit_asm_text {
                    // 文本模式: 保留 .ascii 指令为 .s 输出行
                    // 内核 DEFINE() 宏通过 .ascii "->SYMBOL %0 value" 生成
                    // 结构体偏移常量, 由 Kbuild sed 脚本提取.
                    self.asm_directives_at.push((self.pos(), t.to_string()));
                    self.asm_directives.push(t.to_string());
                } else {
                    // 二进制模式: 解析字符串并发射到当前节
                    let args_str = t[".ascii".len()..].trim();
                    let s = Self::parse_asm_quoted_string(args_str);
                    for b in s.bytes() {
                        self.emit_insn(&Insn::raw_bytes(&[b]));
                    }
                }
                true
            },
            ".incbin" => {
                // .incbin "file" [, skip [, count]]
                // 将文件二进制内容嵌入当前节
                if self.emit_asm_text {
                    // 文本模式: 保留为 .s 输出行
                    self.asm_directives_at.push((self.pos(), t.to_string()));
                    self.asm_directives.push(t.to_string());
                } else {
                    let args_str = t[".incbin".len()..].trim();
                    let filename = Self::parse_asm_quoted_string(args_str);
                    if !filename.is_empty() {
                        // 解析可选的 skip 和 count 参数
                        let after_quote = args_str
                            .find('"')
                            .and_then(|p| args_str[p + 1..].find('"'))
                            .map(|p| p + 1)
                            .unwrap_or(0);
                        let rest = if after_quote < args_str.len() {
                            &args_str[after_quote + 1..]
                        } else {
                            ""
                        };
                        let params: Vec<&str> = rest
                            .split(',')
                            .map(|s| s.trim())
                            .filter(|s| !s.is_empty())
                            .collect();
                        let skip = params
                            .first()
                            .and_then(|s| {
                                let s = s.trim_end_matches(',');
                                if let Some(hex) =
                                    s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))
                                {
                                    usize::from_str_radix(hex, 16).ok()
                                } else {
                                    s.parse().ok()
                                }
                            })
                            .unwrap_or(0);
                        let count: Option<usize> = params.get(1).and_then(|s| {
                            let s = s.trim_end_matches(',');
                            if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))
                            {
                                usize::from_str_radix(hex, 16).ok()
                            } else {
                                s.parse().ok()
                            }
                        });
                        // 读取文件并嵌入
                        if let Ok(data) = std::fs::read(&filename) {
                            let start = skip.min(data.len());
                            let end = match count {
                                Some(c) => (start + c).min(data.len()),
                                None => data.len(),
                            };
                            if start < data.len() {
                                self.emit_insn(&Insn::raw_bytes(&data[start..end]));
                            }
                        }
                    }
                }
                true
            },
            ".cfi_startproc"
            | ".cfi_endproc"
            | ".cfi_def_cfa_offset"
            | ".cfi_def_cfa"
            | ".cfi_offset"
            | ".cfi_restore"
            | ".cfi_remember_state"
            | ".cfi_restore_state"
            | ".cfi_adjust_cfa_offset"
            | ".cfi_def_cfa_register"
            | ".cfi_return_column"
            | ".cfi_signal_frame"
            | ".cfi_window_save"
            | ".cfi_negate_ra_state" => {
                // CFI (Call Frame Information) directives for unwind info.
                // Text mode: preserve as .s output for assembler consumption.
                // Binary mode: silently accept (eh_frame generation is handled
                // by the compiler's own unwind table emission, not inline CFI).
                if self.emit_asm_text {
                    self.asm_directives_at.push((self.pos(), t.to_string()));
                    self.asm_directives.push(t.to_string());
                }
                true
            },
            _ => t.starts_with('.'),
        }
    }
    pub(crate) fn record_asm_size_directive(&mut self, line: &str) {
        let rest = line.trim().strip_prefix(".size").unwrap_or("").trim();
        let Some((name, expr)) = rest.split_once(',') else {
            return;
        };
        let name = Self::asm_symbol_name(name);
        let expr = expr.trim().replace(' ', "");
        if expr != format!(".-{name}") {
            return;
        }
        if let Some(&start) = self.labels.get(&name) {
            self.finish_empty_asm_function(&name, start);
            self.func_sizes
                .insert(name.clone(), self.pos().saturating_sub(start));
        }
    }
    pub(crate) fn record_asm_text_label(&mut self, label: &str) {
        self.asm_last_text_label = Some((label.to_string(), self.pos()));
    }
    pub(crate) fn record_asm_type_directive(&mut self, line: &str) {
        let rest = line.trim().strip_prefix(".type").unwrap_or("").trim();
        let Some((name, ty)) = rest.split_once(',') else {
            return;
        };
        let name = Self::asm_symbol_name(name);
        if name.is_empty() || !ty.contains("function") {
            return;
        }
        self.asm_func_symbols.insert(name);
    }
}
