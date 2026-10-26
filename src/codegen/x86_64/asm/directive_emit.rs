//! asm/directive_emit.rs —— 自历史完整实现（git 28a9063 单文件 Codegen）按职责簇恢复的模块。
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
                self.emit_insn(&Insn::raw(&bytes[..size]));
                continue;
            }
            if let Some((symbol, addend)) = Self::parse_asm_pc_relative_long(expr) {
                self.pending_code_rels.push(PendingCodeRel {
                    offset: off,
                    symbol,
                    rel_type: R_X86_64_PC32,
                    addend,
                });
                self.emit_insn(&Insn::raw(&vec![0u8; size]));
                continue;
            }
            if let Some(v) = Self::parse_asm_imm_token(expr) {
                let bytes = v.to_le_bytes();
                self.emit_insn(&Insn::raw(&bytes[..size]));
            } else if size == 4 && !expr.is_empty() {
                self.pending_code_rels.push(PendingCodeRel {
                    offset: off,
                    symbol: expr.to_string(),
                    rel_type: R_X86_64_32,
                    addend: 0,
                });
                self.emit_insn(&Insn::raw(&[0u8; 4]));
            } else {
                self.emit_insn(&Insn::raw(&vec![0u8; size]));
            }
        }
    }
    /// `.skip`/`.space` 的发射：算出字节数与填充字节，往 .text 里插。
    fn emit_asm_skip(&mut self, args: &str) {
        let args_split = Self::split_asm_args(args);
        let count_expr = args_split.first().copied().unwrap_or("").trim().to_string();
        let fill_expr = args_split.get(1).copied().unwrap_or("").trim().to_string();
        let mut fill: u8 = 0;
        if !fill_expr.is_empty() {
            fill = match Self::eval_pure_numeric_arith(&fill_expr) {
                Some(v) => v as u8,
                None => 0,
            };
        }
        let n = match self.eval_asm_label_expr(&count_expr) {
            Some(v) => v.max(0) as usize,
            None => {
                // 测距遍里算不出是正常的（那份实例故意不带测距表），静默按 0 字节；
                // 不能走下面的报错路径——报错路径会往 .text 里留占位字节，
                // 把要量的标签距离推偏（实测嵌套 ALTERNATIVE 因此多算 1 字节）。
                if self.asm_measuring {
                    return;
                }
                // 真发射遍算不出就是「该补的没补」；旧实现根本没有 .skip 这一条，
                // 默默当 0 字节，要到链接后的 objtool/启动阶段才暴露，当场点名才能归因。
                self.asm_reject(&format!("cannot evaluate .skip count: {count_expr}"));
                return;
            },
        };
        for _ in 0..n {
            self.emit_insn(&Insn::raw(&[fill]));
        }
    }

    /// GAS 语义下，可执行 pushsection 的代码**不留在 .text**；dcc 的做法是「复制到目标节
    /// + 原位填 0x90」，于是 .text 里留下洞。凡是「两个标签之间到底多少字节」这类计算
    /// （ALTERNATIVE 的 `.skip` 补齐、`.byte 773b-771b` 的条目长）都必须把洞扣掉，否则
    /// 嵌套 ALTERNATIVE 会把内层替换代码重复计进外层原文长度（实测：内层 3 字节 ⇒
    /// 中层算成 6、外层算成 9，而 gcc 三者都是 3）。
    ///
    /// 只适用于**留在 .text 里的标号**：区间内定义的标号由 `asm_label_coord` 先按
    /// `pushsection_label_offsets` 直接拿到节内坐标，不会走到这里。
    pub(crate) fn asm_text_pos(&self, p: u64) -> u64 {
        let holes: Vec<(u64, u64)> = self
            .pushsection_code_ranges
            .iter()
            .map(|(s, z, _, _)| (*s, s.saturating_add(*z)))
            .collect();
        Self::asm_text_pos_with_holes(p, &holes)
    }

    /// 同一套「扣掉 .text 里已关闭的可执行 pushsection 洞」规则，输入是洞的
    /// `[起点, 终点)` 列表；给没法借 `&self` 的调用点用。
    pub(crate) fn asm_text_pos_with_holes(p: u64, holes: &[(u64, u64)]) -> u64 {
        let mut sub = 0u64;
        for &(start, end) in holes {
            if end <= p {
                sub += end - start;
            }
        }
        p.saturating_sub(sub)
    }

    /// 标号在「它最终所在节」里的坐标：
    ///   * pushsection 内的标号（`.altinstr_replacement` 的 `774:`/`775:`、`__ex_table`
    ///     里那种数据节标号）走 `pushsection_label_offsets`，那是在登记那一刻直接算好的
    ///     节内偏移；
    ///   * 其余标号就是 `.text` 坐标，扣掉可执行 pushsection 留下的洞。
    ///
    /// 不能拿 `.text` 位置去反查搬迁区间：内层区间的**终点**与外层的**起点**是同一个
    /// 发射位置（`775:` 后紧跟 `.popsection`，外层下一个标号就在同一字节上），按位置
    /// 二选一必然选错。实测嵌套 ALTERNATIVE 的 `.byte 775f-774f` 因此从 3 算成 16。
    pub(crate) fn asm_label_coord(&self, name: &str) -> Option<u64> {
        if let Some(&(_, off)) = self.pushsection_label_offsets.get(name) {
            return Some(off);
        }
        let p = *self.labels.get(name)?;
        Some(self.asm_text_pos(p))
    }

    /// `asm_label_coord` 的整表版本，给只能拿 `&HashMap<String, u64>` 的数据求值路径用。
    pub(crate) fn asm_label_coords(&self) -> HashMap<String, u64> {
        let holes: Vec<(u64, u64)> = self
            .pushsection_code_ranges
            .iter()
            .map(|(s, z, _, _)| (*s, s.saturating_add(*z)))
            .collect();
        self.labels
            .iter()
            .map(|(k, v)| {
                let coord = match self.pushsection_label_offsets.get(k) {
                    Some(&(_, off)) => off,
                    None => Self::asm_text_pos_with_holes(*v, &holes),
                };
                (k.clone(), coord)
            })
            .collect()
    }

    /// 求一个可能带标签引用的表达式：先把标号名折成「它所在节里的坐标」，再走纯数求值。
    ///
    /// 位置优先取「测距遍」那张表（它存的已经是同一口径的节内坐标），没有才按
    /// `asm_label_coord` 当场结算。
    fn eval_asm_label_expr(&self, expr: &str) -> Option<i64> {
        if expr.is_empty() {
            return None;
        }
        let mut out = String::with_capacity(expr.len());
        let b = expr.as_bytes();
        let mut i = 0usize;
        let is_ident = |c: u8| c == b'_' || c == b'.' || c.is_ascii_alphanumeric();
        while i < b.len() {
            let c = b[i];
            if c == b'.' || c == b'_' {
                let s = i;
                while i < b.len() && is_ident(b[i]) {
                    i += 1;
                }
                let word = &expr[s..i];
                // 位置类 token（`.` 单独代表当前 PC）与数字标签的内部名分开处理
                if word == "." {
                    let pos = self.asm_text_pos(self.pos()).to_string();
                    out.push_str(&pos);
                    continue;
                }
                if let Some(v) = self.asm_skip_measure.get(word).copied().or_else(|| {
                    self.labels
                        .get(word)
                        .and_then(|_| self.asm_label_coord(word))
                }) {
                    out.push_str(&v.to_string());
                    continue;
                }
                // 认不出的名字（例如宏参没代入、或厂家外的标签）直接带过，
                // 下一行得纯数求值会失败 ⇒ 调用方报错而不是默默补 0。
                out.push_str(word);
            } else {
                out.push(c as char);
                i += 1;
            }
        }
        Self::eval_pure_numeric_arith(&out)
    }

    pub(crate) fn emit_asm_desc(&mut self, op: &AsmOperand, ext: u8) {
        match op {
            AsmOperand::Reg(r, _) => {
                self.emit_insn(&Insn::raw(&[
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
                self.emit_insn(&Insn::raw(&[
                    Self::rex(false, 0, index.unwrap_or(0), *base),
                    0x0F,
                    0x00,
                ]));
                self.emit_asm_modrm_mem_only(ext, *disp, *base, *index, *scale);
            },
            AsmOperand::SymDisp(sym, _) => {
                // RIP 相对形式（`verw ds(%rip)`）：modrm = `(ext<<3)|5`，后跟 disp32 重定位。
                // 内核 `arch/x86/include/asm/nospec-branch.h` 的 `verw %[ds]` 就是这一形状（GAS
                // 实测 `0f 00 2d <disp32>`），而且只有这条内存形式才真的刷 CPU buffer。
                self.emit_insn(&Insn::raw(&[0x0F, 0x00, ((ext & 7) << 3) | 5, 0, 0, 0, 0]));
                let pos = self.pos() - 4;
                self.pending_data_rels.push((pos, sym.clone()));
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
                        self.emit_insn(&Insn::raw(&[0x90]));
                    }
                }
                true
            },
            // `.skip <n>[, <填充>]` / `.space`：往当前坐标流里插 n 个填充字节。
            //
            // GAS 语义照抄：n 可以是表达式（含前向标签差，由 `emit_resolved_asm_lines`
            // 的测距遍折成数）、n 为负 ⇒ 0、填充省略时为 0。
            // 内核 ALTERNATIVE 用 `.skip -(((新长)-(旧长)) > 0) * (...),0x90` 把旧指令区
            // 补到与新指令同长；不实现就等于**空 oldinstr 的替代没补齐**：
            // apply_alternatives 会盖掉后面的指令，且 `.byte 773b-771b` 算成 0 使 objtool
            // 报 `empty alternative entry`，vmlinux.o 直接链接不过。
            ".skip" | ".space" => {
                let args = t[parts[0].len()..].trim();
                self.emit_asm_skip(args);
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
                            self.emit_insn(&Insn::raw(&[b]));
                        }
                        self.emit_insn(&Insn::raw(&[0]));
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
                        self.emit_insn(&Insn::raw(&[b]));
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
                                self.emit_insn(&Insn::raw(&data[start..end]));
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
