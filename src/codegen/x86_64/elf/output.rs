//! elf/output.rs —— 自历史完整实现（git 28a9063 单文件 Codegen）按职责簇恢复的模块。
//! 仅移动/可见性调整，不改语义（backend_refactor_plan §3.1 原则 1）。
#![allow(clippy::all)]
#![allow(unused_imports)]
use crate::ast::*;
use crate::codegen::shared::elf as shared_elf;
use crate::codegen::shared::elf::*;
use crate::codegen::shared::*;
use crate::codegen::x86_64::asm::*;
use crate::codegen::x86_64::insn::Insn;
use crate::codegen::x86_64::opcode::X86_64Opcode;
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

/// 序言后 `%r13` 与 DWARF **CFA** 的关系：CFI 定的是 `CFA = rsp + 本值`，而 `push r13`
/// 后 `r13` 就是当时的 rsp，所以 **r13 = CFA - 本值**（无 frame_pointer 16，有则 24）。
///
/// 为什么抽成一个函数：CFI 与 DWARF 的 `DW_OP_fbreg` 算的是同一个数。两处各写一份
/// `if frame_pointer {24} else {16}` 的话，改序言就只修一侧 —— 那调试器拿到的偏移会
/// 整体上跳 8 字节，而从产物上看不出来（与 §7.15 那族「同一事实多套判据」同形状）。
pub(crate) fn cfa_frame_offset(frame_pointer: bool) -> u64 {
    if frame_pointer {
        24
    } else {
        16
    }
}

impl Codegen {
    /// P9 方案 C: 通用「遍历字节区间 → 按 pc 输出 GAS 行」辅助函数.
    ///
    /// 与老路径 `disassemble_range` 的差别:
    /// - 按 pc 顺序逐条调用 `disassemble_one_at` (解码单条指令);
    /// - 在每条指令**之前**插入该 pc 处的 labels / directives (与老 disassemble_range
    ///   的循环语义完全一致);
    /// - 失败/越界走 `.byte` 兜底 (与老路径一致, 避免静默跳过).
    ///
    /// `bytes` / `base_offset` 与 `disassemble_range` 同语义: `bytes` 是本段的**切片**,
    /// `base_offset` 是它在 self.code 里的起点 (labels/calls/data 表的键用的是
    /// 全 self.code 的绝对偏移). 所以函数内部计算 `off = base_offset + pos as u64`.
    ///
    /// 拆分出这个 helper 的原因: output.rs / ifunc.rs / 未来 v2 渲染器都需要同一
    /// 段逻辑. 集中在一处修改, 别处只传切片.
    pub(crate) fn render_byte_range(
        bytes: &[u8],
        start: usize,
        end: usize,
        base_offset: u64,
        labels: &BTreeMap<u64, Vec<&str>>,
        directives: &BTreeMap<u64, Vec<&str>>,
        calls: &HashMap<u64, String>,
        data: &HashMap<u64, String>,
    ) -> String {
        use crate::codegen::x86_64::render_one::disassemble_one_at;
        let mut out = String::new();
        let mut pos = start;
        while pos < end && pos < bytes.len() {
            let off = base_offset + pos as u64;
            if let Some(names) = labels.get(&off) {
                for n in names {
                    out.push_str(n);
                    out.push_str(":\n");
                }
            }
            if let Some(lines) = directives.get(&off) {
                for line in lines {
                    out.push('\t');
                    out.push_str(line);
                    out.push('\n');
                }
            }
            match disassemble_one_at(bytes, pos, base_offset, calls, data, labels) {
                Some((len, text)) if len > 0 => {
                    out.push('\t');
                    out.push_str(&text);
                    out.push('\n');
                    pos += len;
                },
                _ => {
                    // `.byte` 兜底 —— 与老 disassemble_range 同语义.
                    let mut byte_strs: Vec<String> = Vec::new();
                    let mut p = pos;
                    let end_pos = (pos + 1).min(bytes.len());
                    while p < end_pos {
                        byte_strs.push(format!("0x{:02x}", bytes[p]));
                        p += 1;
                    }
                    out.push('\t');
                    out.push_str(".byte ");
                    out.push_str(&byte_strs.join(", "));
                    out.push('\n');
                    pos += 1;
                },
            }
        }
        out
    }

    pub fn build_asm_output(&self, prog: &Program) -> String {
        use crate::codegen::x86_64::render_one as tables;

        let mut out = String::new();
        out.push_str(&format!("\t.file \"{}\"\n", self.source_name));

        // --- Disassembly-only rendering path (legacy fallback for -S) ---
        use std::collections::BTreeMap;
        let labels = tables::label_map(self.labels.iter().map(|(n, o)| (o, n.as_str())));
        let directives =
            tables::directive_map(self.asm_directives_at.iter().map(|(o, l)| (o, l.as_str())));

        // P9 方案 C 真闭环: gas_text_map 不再来自预反推. 直接由 insn_records 携带.
        // 主循环用 gas_text_by_pc = 来自 insn_records 的 gas_text, 长度也从 record 推.
        let mut gas_text_map: std::collections::HashMap<u64, String> =
            std::collections::HashMap::new();
        let mut gas_len_map: std::collections::HashMap<u64, usize> =
            std::collections::HashMap::new();
        for (pc, insn) in &self.insn_records {
            if let Some(g) = &insn.gas_text {
                // 长度: 类型化变体的字节由 op.encode() 现场算 (insn.bytes 为空);
                // Raw 变体的字节也由 encode() 返回 (Raw::bytes 存 op 内).
                let len = insn.encode_bytes().map(|b| b.len()).unwrap_or(0);
                gas_text_map.insert(*pc, g.clone());
                gas_len_map.insert(*pc, len.max(1));
            }
        }
        // P10 闭环: 对所有「insn_records 里存在但没有 gas_text」的记录, 在渲染前
        // 一次性补 gas_text. 两级 fallback:
        //   1) 类型化变体 (Lea/AluRm/MovExt/...) → 直接调 op.render() 现场生成
        //      (G6 已证明 render 无损).
        //   2) Raw 变体 (发射点未带 mnemonic) → 走 disassemble_one_at 反推字节.
        //
        // 目的: 让渲染主循环 100% 走 gas_text_by_pc 命中路径, 逐步下线 disasm.
        for (pc, insn) in &self.insn_records {
            if insn.gas_text.is_some() {
                continue;
            }
            let bytes = match insn.encode_bytes() {
                Some(b) => b,
                None => continue,
            };
            // 第一级: op.render() (类型化变体).
            let render_text = insn.op.render();
            if !render_text.is_empty() && render_text != "?" {
                gas_text_map.insert(*pc, render_text);
                gas_len_map.insert(*pc, bytes.len().max(1));
                continue;
            }
            // 第二级: disassemble_one_at 反推 (Raw 兜底).
            if *pc as usize + bytes.len() > self.code.len() {
                continue;
            }
            // 注意: 这里 base_offset 必须用本函数的 text_start, 否则 disasm_calls /
            // labels 用绝对地址, 而 disassemble_one_at 内部 `off = base + pos` 算出
            // 的 off 与 disasm_calls 里存的键对不上 (差了函数 text_start 个字节).
            // 在 build_asm_output 里没有统一 base; insn_records 的 pc 就是绝对地址,
            // 所以这里传 base=0, pos=*pc as usize. 这样 off = *pc, 与 disasm_calls
            // 里存的键 (at + 1, 也是绝对地址) 匹配.
            match tables::disassemble_one_at(
                &self.code,
                *pc as usize,
                0,
                &self.disasm_calls,
                &self.disasm_data,
                &labels,
            ) {
                Some((len, text)) if len == bytes.len() => {
                    gas_text_map.insert(*pc, text);
                    gas_len_map.insert(*pc, bytes.len());
                },
                _ => {},
            }
        }

        let mut funcs = Vec::new();
        Self::collect_all_funcs(prog, &mut funcs);
        funcs.sort_by_key(|name| self.labels.get(name).copied().unwrap_or(u64::MAX));

        let mut sections: Vec<String> = vec![".text".to_string()];
        for name in &funcs {
            if let Some(layout) = self.func_asm_layout.get(name) {
                if layout.section != ".text" && !sections.contains(&layout.section) {
                    sections.push(layout.section.clone());
                }
            }
        }

        for sec_name in &sections {
            if sec_name == ".text" {
                out.push_str("\t.text\n");
            } else {
                let flags = self.gas_section_flags(sec_name);
                out.push_str(&format!("\t.section {sec_name},\"{flags}\"\n"));
            }

            if sec_name == ".text" {
                let custom_starts: std::collections::HashSet<u64> = self
                    .func_asm_layout
                    .values()
                    .filter(|l| l.section != ".text")
                    .map(|l| l.text_start)
                    .collect();
                let prologue_labels: BTreeMap<u64, Vec<&str>> = labels
                    .iter()
                    .filter(|(off, _)| !custom_starts.contains(off))
                    .map(|(off, names)| (*off, names.clone()))
                    .collect();
                let first_text = funcs
                    .iter()
                    .filter_map(|n| self.func_asm_layout.get(n))
                    .filter(|l| l.section == ".text")
                    .map(|l| l.text_start)
                    .min()
                    .unwrap_or(self.code.len() as u64);
                if first_text > 0 {
                    out.push_str(&Self::render_byte_range(
                        &self.code,
                        0,
                        first_text as usize,
                        0,
                        &prologue_labels,
                        &directives,
                        &self.disasm_calls,
                        &self.disasm_data,
                    ));
                }
            }

            for name in &funcs {
                let layout = match self.func_asm_layout.get(name) {
                    Some(l) => l,
                    None => continue,
                };
                if layout.section != *sec_name {
                    continue;
                }
                let is_static = prog
                    .decls
                    .iter()
                    .any(|d| matches!(d, Decl::Func(f) if f.name.name == *name && f.is_static));
                let is_ifunc = prog.decls.iter().any(|d| {
                    matches!(d, Decl::Func(f) if f.name.name == *name
                            && (f.ifunc_resolver.is_some() || !f.target_clones.is_empty()))
                });
                if !is_static {
                    out.push_str(&format!("\t.globl {name}\n"));
                }
                let sym_type = if is_ifunc {
                    "@gnu_indirect_function"
                } else {
                    "@function"
                };
                out.push_str(&format!("\t.type {name}, {}\n", sym_type));
                if self.emit_cfi {
                    out.push_str("\t.cfi_startproc\n");
                }
                // 透传区域: 这些行 dcc 编不出来, 因此**没有字节**、只有文本。
                // 它们必须按记录时的 pc 插回渲染流 —— 否则 `-c` 降级成 `-S` 后
                // 会静默丢掉整条内联汇编 (正是本方案要消灭的缺陷)。
                let mut regions: Vec<(usize, &Vec<String>)> = self
                    .asm_passthrough_regions
                    .iter()
                    .filter_map(|(pc, lines)| {
                        let off = (*pc).checked_sub(layout.text_start)? as usize;
                        (off <= layout.bytes.len()).then_some((off, lines))
                    })
                    .collect();
                regions.sort_by_key(|(off, _)| *off);

                // P9 方案 C 真闭环: 主循环改为「先按 insn_records 走」.
                //
                // 思路: insn_records 是发射时记录的 (pc, Insn) 完整清单, 每条 Insn
                // 带 gas_text 与原始字节 (Raw::bytes). 渲染时:
                //   1. 按 pc 顺序遍历本函数区间内的 insn_records;
                //   2. 对每条记录, 若有 gas_text, 直接输出 (字节长度由 Raw::bytes 给,
                //      与发射时一致; patch_callee_saves 只会在「未使用寄存器」时把 mov→nop,
                //      那条 mov 的 gas_text 我们在 helper 里没填 (fallback), 会走 disasm
                //      路径; 若填了 gas_text 而它恰好被 patch 成 nop, 我们需要
                //      检查 code[p] 首字节是否 0x90 且整条被 nop 填充, 若是则改写为 nop).
                //   3. 若没有 gas_text, 走 disassemble_one_at 拿 (len, text) —— 与老行为
                //      完全一致.
                //   4. insn_records 之间的 gap (对齐 nop/padding) 也走 disasm.
                //
                // G10 字节快照不受影响: 我们只改 -S 文本生成, 不改 self.code.
                let base = layout.text_start;
                let end_abs = base + layout.bytes.len() as u64;
                // 先收集本函数区间内的 (pc, insn) 并按 pc 排序 (理论上已经有序, 但防御).
                let mut recs: Vec<(u64, &Insn)> = self
                    .insn_records
                    .iter()
                    .filter(|(pc, _)| *pc >= base && *pc < end_abs)
                    .map(|(pc, insn)| (*pc, insn))
                    .collect();
                recs.sort_by_key(|(pc, _)| *pc);

                // P9 方案 C 真闭环: gas_text_by_pc 来自 pre-built gas_text_map
                // (渲染前一次性反推好). 关键: gas_text_map 的 key 是**整条指令的
                // 起始 pc** (扫描 code 时逐指令前进得到), 不是单字节 record 的 pc.
                // 所以 gas_text_by_pc 要**直接**从 gas_text_map 复制本函数区间内的条目,
                // 不能按 insn_records 过滤 —— 否则只有 record 起点命中, 中段字节全部 fallback.
                let func_start_abs = base;
                let func_end_abs = base + layout.bytes.len() as u64;
                let mut gas_text_by_pc: std::collections::HashMap<u64, &str> =
                    std::collections::HashMap::new();
                for (pc, g) in &gas_text_map {
                    if *pc >= func_start_abs && *pc < func_end_abs {
                        gas_text_by_pc.insert(*pc, g.as_str());
                    }
                }
                // 合并 record 自带的 gas_text (若有).
                for (pc, insn) in &recs {
                    if let Some(g) = &insn.gas_text {
                        gas_text_by_pc.entry(*pc).or_insert(g.as_str());
                    }
                }
                use crate::codegen::x86_64::render_one::disassemble_one_at;
                let disasm_one = |pos: usize, out: &mut String| -> usize {
                    match disassemble_one_at(
                        &layout.bytes,
                        pos,
                        base,
                        &self.disasm_calls,
                        &self.disasm_data,
                        &labels,
                    ) {
                        Some((len, text)) if len > 0 => {
                            out.push('\t');
                            out.push_str(&text);
                            out.push('\n');
                            len
                        },
                        _ => {
                            // .byte 兜底.
                            let mut byte_strs: Vec<String> = Vec::new();
                            for p in pos..(pos + 1).min(layout.bytes.len()) {
                                byte_strs.push(format!("0x{:02x}", layout.bytes[p]));
                            }
                            out.push('\t');
                            out.push_str(".byte ");
                            out.push_str(&byte_strs.join(", "));
                            out.push('\n');
                            1
                        },
                    }
                };

                let dis = |from: usize, to: usize, out: &mut String| {
                    let mut pos = from;
                    while pos < to && pos < layout.bytes.len() {
                        let off_abs = base + pos as u64;
                        // 1) labels/directives 前置.
                        if let Some(names) = labels.get(&off_abs) {
                            for n in names {
                                out.push_str(n);
                                out.push_str(":\n");
                            }
                        }
                        if let Some(lines) = directives.get(&off_abs) {
                            for line in lines {
                                out.push('\t');
                                out.push_str(line);
                                out.push('\n');
                            }
                        }
                        // 2) 主路径: gas_text_by_pc 命中. 若被 patch_callee_saves 改成全 nop,
                        //    则不能用 gas_text (语义已变). patch_callee_saves 把 3 字节 mov
                        //    改成 4 个 0x90. 此时该指令的 gas_text 是原 mov 的文本, 不能用.
                        //    判定: 若 gas_text 对应的 disasm 长度 (gas_len_map) 与 code 中
                        //    实际的 nop 序列长度不一致, 说明被 patch 过. 简单做法: 直接用
                        //    gas_text, 因为 disasm_one 已经把 nop 解码为 "nop", 我们这里
                        //    也用 gas_text 对应的 "mov %rsp,%r13" 会失真. 所以跳过:
                        //    首字节 0x90 且 gas_text 不是 "nop" 开头 → 跳过.
                        let first_byte_is_nop = layout.bytes[pos] == 0x90;
                        let gas_is_nop = gas_text_by_pc
                            .get(&off_abs)
                            .map(|g| g.starts_with("nop"))
                            .unwrap_or(false);
                        if let Some(g) = gas_text_by_pc.get(&off_abs) {
                            let len = gas_len_map.get(&off_abs).copied().unwrap_or(1).max(1);
                            // 若首字节是 nop 但 gas_text 不是 nop (说明被 patch 过), 跳过.
                            // 若首字节是 nop 且 gas_text 也是 nop, 用 gas_text (虽然 disasm
                            // 已经给出了正确文本, 但我们这里直接用 gas_text 也一样).
                            let skip = first_byte_is_nop && !gas_is_nop;
                            if !skip {
                                out.push('\t');
                                out.push_str(g);
                                out.push('\n');
                                pos += len;
                                continue;
                            }
                        }
                        // 3) fallback: 无 gas_text 或被 patch 成 nop, 走 disasm_one 输出.
                        let mut fb_out = String::new();
                        let len = disasm_one(pos, &mut fb_out);
                        out.push_str(&fb_out);
                        pos += len;
                    }
                };

                let mut cursor = 0usize;
                for (off, lines) in &regions {
                    if *off > cursor {
                        dis(cursor, *off, &mut out);
                        cursor = *off;
                    }
                    for l in lines.iter() {
                        out.push_str(&format!("\t{l}\n"));
                    }
                    // passthrough 不占字节, cursor 不动。
                }
                if cursor < layout.bytes.len() {
                    dis(cursor, layout.bytes.len(), &mut out);
                }
                if self.emit_cfi {
                    out.push_str("\t.cfi_endproc\n");
                }
                out.push_str(&format!("\t.size {name}, .-{name}\n"));
            }
        }

        // __attribute__((alias)): emit .set directives for alias symbols
        for decl in &prog.decls {
            match decl {
                Decl::Func(f) => {
                    if let Some(ref alias_target) = f.alias_name {
                        out.push_str(&format!("\t.globl {}\n", f.name.name));
                        out.push_str(&format!("\t.set {}, {}\n", f.name.name, alias_target));
                    }
                },
                Decl::Var(v) => {
                    if let Some(ref alias_target) = v.alias_name {
                        out.push_str(&format!("\t.globl {}\n", v.name.name));
                        out.push_str(&format!("\t.set {}, {}\n", v.name.name, alias_target));
                    }
                },
                _ => {},
            }
        }

        // __attribute__((target_clones(...))): IFUNC 基名 = 它自己的 resolver
        //
        // 旧行为：给每个变体发一条 `.set <variant>, <base>`（因为它们本来就是基名的
        // 别名）。新行为下变体各自有真实函数体（由上面的 `funcs` 循环渲染），基名反而
        // **没有体** —— 它的值是 `ifunc::emit_resolver_body` 合成的 `<base>.resolver`。
        // 与 `.o` 侧 `elf/rel_symbols.rs` 的 `ifunc_defs` 那一段同一个口径。
        for decl in &prog.decls {
            if let Decl::Func(f) = decl {
                if f.target_clones.is_empty() || f.body.is_some() {
                    continue;
                }
                let base = self.resolve_asm_label(&f.name.name).to_string();
                let resolver = ifunc::resolver_sym(&base);
                if !self.labels.contains_key(&resolver) {
                    continue; // 合成没跑（报错提前终止），不发指向不存在符号的 .set
                }
                if !f.is_static {
                    out.push_str(&format!("\t.globl {base}\n"));
                } else if f.is_weak {
                    out.push_str(&format!("\t.weak {base}\n"));
                }
                out.push_str(&format!("\t.set {base}, {resolver}\n"));
                out.push_str(&format!("\t.type {base}, @gnu_indirect_function\n"));
            }
        }

        // `__attribute__((ifunc("r")))`：同形，值指向声明里那个 resolver。
        for decl in &prog.decls {
            let Decl::Func(f) = decl else { continue };
            let Some(resolver) = f.ifunc_resolver.clone() else {
                continue;
            };
            if f.body.is_some() || !f.target_clones.is_empty() {
                continue; // 有体形状由上面的函数体循环负责；target_clones 已有自家用法
            }
            let base = self.resolve_asm_label(&f.name.name).to_string();
            let r = self.resolve_asm_label(&resolver).to_string();
            if !self.labels.contains_key(&r) {
                continue; // 不指不存在符号（rel_symbols 那边已逐条报诊）
            }
            if !f.is_static {
                out.push_str(&format!("\t.globl {base}\n"));
            }
            out.push_str(&format!("\t.set {base}, {r}\n"));
            out.push_str(&format!("\t.type {base}, @gnu_indirect_function\n"));
        }

        // .rodata section: floating-point literals and string constants
        if !self.rodata.is_empty() {
            out.push_str("\t.section .rodata\n");
            // Output rodata contents as .byte directives
            for chunk in self.rodata.chunks(8) {
                let bytes: Vec<String> = chunk.iter().map(|b| format!("0x{:02x}", b)).collect();
                out.push_str(&format!("\t.byte {}\n", bytes.join(", ")));
            }
        }

        // 合成体（`__dcc_cpu_indicator_init` / `<f>.resolver`）：与上面事件路径同一个
        // 口径。它们不在 `prog.decls` 里，`collect_all_funcs` 看不到；不渲染的话
        // `-S` 出来的 `.set compute, compute.resolver` 会指向一个汇编器里不存在
        // 的符号（实测：`as /tmp/tc.s` rc=0 但 `compute.resolver` 变成 UND）。
        out.push_str(&self.render_ifunc_synth_asm());

        // P9 方案 C 临时统计: DCC_GAS_STATS=1 时打印发射点 top 50 与全局 hit rate.
        if std::env::var_os("DCC_GAS_STATS").is_some() {
            let mut v: Vec<(&String, &usize)> = self.emit_insn_stats.iter().collect();
            v.sort_by(|a, b| b.1.cmp(a.1));
            eprintln!("=== EMIT-TOP {} entries ===", v.len());
            for (k, n) in v.iter().take(50) {
                eprintln!("  {n:5}  {k}");
            }
            let total: usize = self.emit_insn_gas_hits.values().sum();
            let with_gas = self.emit_insn_gas_hits.get(&true).copied().unwrap_or(0);
            eprintln!(
                "=== EMIT-TOTAL total={total} has_gas={with_gas} ({:.1}%) ===",
                if total > 0 {
                    with_gas as f64 / total as f64 * 100.0
                } else {
                    0.0
                }
            );
        }

        out
    }
    pub fn build_cfi_ops(&self) -> Vec<u8> {
        // uleb128 编码 (借用 debug::write_uleb128 但不导出, 这里 inline)
        fn enc_uleb128(out: &mut Vec<u8>, mut v: u64) {
            loop {
                let mut b = (v & 0x7f) as u8;
                v >>= 7;
                if v != 0 {
                    b |= 0x80;
                }
                out.push(b);
                if v == 0 {
                    break;
                }
            }
        }
        let mut ops = Vec::new();
        // CFA = rsp + 16 (无 frame_pointer) / rsp + 24 (有 frame_pointer)
        let cfa_off: u32 =
            crate::codegen::x86_64::elf::output::cfa_frame_offset(self.target.frame_pointer) as u32;
        // DW_CFA_def_cfa (0x0c) uleb128(rsp=7) uleb128(offset)
        ops.push(0x0c);
        enc_uleb128(&mut ops, 7);
        enc_uleb128(&mut ops, cfa_off as u64);
        // r13 (reg 13) saved at CFA-16 -> factor 2
        ops.push(0x80 | 13);
        enc_uleb128(&mut ops, 2);
        // %rip (reg 16) saved at CFA-8 -> factor 1
        ops.push(0x80 | 16);
        enc_uleb128(&mut ops, 1);
        // rbp (reg 6) saved at CFA-24 -> factor 3, only with frame_pointer
        if self.target.frame_pointer {
            ops.push(0x80 | 6);
            enc_uleb128(&mut ops, 3);
        }
        ops
    }
    pub(crate) fn emit_instrument_func_enter(&mut self) {
        // 获取当前函数地址 (lea rdi, [rip+0])
        // 实际地址需要回补到函数起始位置
        let lea_pos = self.pos();
        self.emit_insn(&Insn::raw_bytes(&[0x48, 0x8D, 0x3D, 0x00, 0x00, 0x00, 0x00])); // lea rdi, [rip+disp32]
                                                                                       // LEA 是直接 RIP-relative 寻址, 用 PC32 重定位
        self.pending_func_addr_rels
            .push((lea_pos + 3, self.current_func_name.clone(), true));

        // 获取调用者地址: 返回地址在 [r13+8]
        // (call 压入返回地址 → push r13 后 r13 指向 saved r13 →
        //  返回地址就在 r13+8, 无论中间 sub rsp 多少都不变)
        self.emit_insn(&Insn::mov_rm_mem(
            true,
            crate::codegen::shared::asm_operand::Width::B64,
            6,
            13,
            8,
        )); // mov rsi, [r13+8]

        // call __cyg_profile_func_enter
        self.call_label("__cyg_profile_func_enter");
    }
    pub(crate) fn emit_instrument_func_exit(&mut self) {
        // 获取当前函数地址 (lea rdi, [rip+0])
        let lea_pos = self.pos();
        self.emit_insn(&Insn::raw_bytes(&[0x48, 0x8D, 0x3D, 0x00, 0x00, 0x00, 0x00])); // lea rdi, [rip+disp32]
                                                                                       // LEA 是直接 RIP-relative 寻址, 用 PC32 重定位
        self.pending_func_addr_rels
            .push((lea_pos + 3, self.current_func_name.clone(), true));

        // 获取调用者地址: 返回地址在 [r13+8]
        self.emit_insn(&Insn::mov_rm_mem(
            true,
            crate::codegen::shared::asm_operand::Width::B64,
            6,
            13,
            8,
        )); // mov rsi, [r13+8]

        // call __cyg_profile_func_exit
        self.call_label("__cyg_profile_func_exit");
    }
    pub(crate) fn emit_mcount_loc_entry(&mut self, call_pc: u64, _func_name: &str) {
        // Record the position; actual section data and relocation
        // are emitted during ELF finalization.
        self.mcount_loc_entries.push(call_pc);
    }
    pub(crate) fn emit_pushsection_data(
        &mut self,
        trimmed: &str,
        sec_idx: usize,
        locals: &AsmLocalLabels,
        line_idx: usize,
        numeric_label_map: &std::collections::HashMap<String, String>,
    ) {
        // 标签定义 (N: 或 name:)
        if let Some(rest) = trimmed.strip_suffix(':') {
            let label = rest.trim();
            if !label.is_empty() && !Self::is_asm_directive(label) {
                self.label(label);
                // 非执行 pushsection: label 位置不在 self.code 而在自定义节数据内.
                // 显式记录 (sec_idx, sec 内偏移) 供 symtab 构建阶段把 label 编码为
                // STT_SECTION + offset 本地符号. 可执行 pushsection 已经由
                // pushsection_code_ranges 反查, 无需额外登记.
                let is_exec = self.extra_sections[sec_idx].flags & shared_elf::SHF_EXECINSTR != 0;
                if !is_exec {
                    let sec_off = self.extra_sections[sec_idx].data.len() as u64;
                    self.pushsection_label_offsets
                        .insert(label.to_string(), (sec_idx, sec_off));
                }
            }
            return;
        }
        if !trimmed.starts_with('.') {
            return;
        }
        let parts: Vec<&str> = trimmed.splitn(2, char::is_whitespace).collect();
        let directive = parts[0];
        let args_str = if parts.len() > 1 { parts[1].trim() } else { "" };
        match directive {
            ".byte" => {
                for expr in Self::split_asm_args(args_str) {
                    let (val, sym, deferred) = Self::parse_asm_data_expr(
                        expr.trim(),
                        &self.labels,
                        locals,
                        line_idx,
                        numeric_label_map,
                    );
                    if let Some((left_label, right_label)) = deferred {
                        let data_offset = self.extra_sections[sec_idx].data.len();
                        self.deferred_pushsection_fixups
                            .push(DeferredPushsectionFixup {
                                sec_idx,
                                data_offset,
                                left_label,
                                right_label,
                                byte_size: 1,
                            });
                    } else if let Some((sym_name, rel_type, addend)) = sym {
                        let off = self.extra_sections[sec_idx].data.len() as u64;
                        self.pending_custom_sec_rels.push(CustomSecRel {
                            sec_idx,
                            offset: off,
                            symbol: sym_name,
                            rel_type,
                            addend,
                        });
                    }
                    self.extra_sections[sec_idx].data.push(val as u8);
                }
            },
            ".short" | ".word" | ".2byte" => {
                for expr in Self::split_asm_args(args_str) {
                    let (val, sym, deferred) = Self::parse_asm_data_expr(
                        expr.trim(),
                        &self.labels,
                        locals,
                        line_idx,
                        numeric_label_map,
                    );
                    if let Some((left_label, right_label)) = deferred {
                        let data_offset = self.extra_sections[sec_idx].data.len();
                        self.deferred_pushsection_fixups
                            .push(DeferredPushsectionFixup {
                                sec_idx,
                                data_offset,
                                left_label,
                                right_label,
                                byte_size: 2,
                            });
                    } else if let Some((sym_name, rel_type, addend)) = sym {
                        let off = self.extra_sections[sec_idx].data.len() as u64;
                        self.pending_custom_sec_rels.push(CustomSecRel {
                            sec_idx,
                            offset: off,
                            symbol: sym_name,
                            rel_type,
                            addend,
                        });
                    }
                    self.extra_sections[sec_idx]
                        .data
                        .extend_from_slice(&(val as u16).to_le_bytes());
                }
            },
            ".int" | ".long" | ".4byte" => {
                for expr in Self::split_asm_args(args_str) {
                    let (val, sym, deferred) = Self::parse_asm_data_expr(
                        expr.trim(),
                        &self.labels,
                        locals,
                        line_idx,
                        numeric_label_map,
                    );
                    if let Some((left_label, right_label)) = deferred {
                        let data_offset = self.extra_sections[sec_idx].data.len();
                        self.deferred_pushsection_fixups
                            .push(DeferredPushsectionFixup {
                                sec_idx,
                                data_offset,
                                left_label,
                                right_label,
                                byte_size: 4,
                            });
                    } else if let Some((sym_name, rel_type, addend)) = sym {
                        let off = self.extra_sections[sec_idx].data.len() as u64;
                        self.pending_custom_sec_rels.push(CustomSecRel {
                            sec_idx,
                            offset: off,
                            symbol: sym_name,
                            rel_type,
                            addend,
                        });
                    }
                    self.extra_sections[sec_idx]
                        .data
                        .extend_from_slice(&(val as u32).to_le_bytes());
                }
            },
            ".quad" | ".8byte" => {
                for expr in Self::split_asm_args(args_str) {
                    let (val, sym, _deferred) = Self::parse_asm_data_expr(
                        expr.trim(),
                        &self.labels,
                        locals,
                        line_idx,
                        numeric_label_map,
                    );
                    if let Some((sym_name, _rel_type, addend)) = sym {
                        let off = self.extra_sections[sec_idx].data.len() as u64;
                        self.pending_custom_sec_rels.push(CustomSecRel {
                            sec_idx,
                            offset: off,
                            symbol: sym_name,
                            rel_type: shared_elf::R_X86_64_64,
                            addend,
                        });
                    }
                    self.extra_sections[sec_idx]
                        .data
                        .extend_from_slice(&(val as u64).to_le_bytes());
                }
            },
            ".asciz" | ".string" => {
                let s = Self::parse_asm_quoted_string(args_str);
                self.extra_sections[sec_idx]
                    .data
                    .extend_from_slice(s.as_bytes());
                self.extra_sections[sec_idx].data.push(0);
            },
            ".ascii" => {
                let s = Self::parse_asm_quoted_string(args_str);
                self.extra_sections[sec_idx]
                    .data
                    .extend_from_slice(s.as_bytes());
            },
            // On x86/x86_64, GAS .align uses power-of-2 alignment
            // (.align N means align to 2^N bytes), while .balign uses
            // byte alignment (.balign N means align to N bytes).
            // Only update section alignment for non-merge sections;
            // merge sections (SHF_MERGE) must keep alignment consistent
            // with entsize (e.g. __ex_table entsize=12).
            ".align" => {
                if let Ok(p) = args_str
                    .split(',')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .parse::<u32>()
                {
                    let a = 1usize << p;
                    if a > 0 {
                        let len = self.extra_sections[sec_idx].data.len();
                        let aligned = (len + a - 1) & !(a - 1);
                        self.pad_custom_section_to(sec_idx, aligned);
                        if self.extra_sections[sec_idx].flags & shared_elf::SHF_MERGE == 0
                            && (self.extra_sections[sec_idx].align as usize) < a
                        {
                            self.extra_sections[sec_idx].align = a as u64;
                        }
                    }
                }
            },
            ".balign" => {
                if let Ok(a) = args_str
                    .split(',')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .parse::<usize>()
                {
                    if a > 0 {
                        let len = self.extra_sections[sec_idx].data.len();
                        let aligned = (len + a - 1) & !(a - 1);
                        self.pad_custom_section_to(sec_idx, aligned);
                        if self.extra_sections[sec_idx].flags & shared_elf::SHF_MERGE == 0 {
                            let align_po2 = a.next_power_of_two();
                            if (self.extra_sections[sec_idx].align as usize) < align_po2 {
                                self.extra_sections[sec_idx].align = align_po2 as u64;
                            }
                        }
                    }
                }
            },
            ".p2align" => {
                if let Ok(p) = args_str
                    .split(',')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .parse::<u32>()
                {
                    let a = 1usize << p;
                    let len = self.extra_sections[sec_idx].data.len();
                    let aligned = (len + a - 1) & !(a - 1);
                    self.pad_custom_section_to(sec_idx, aligned);
                    if self.extra_sections[sec_idx].flags & shared_elf::SHF_MERGE == 0
                        && (self.extra_sections[sec_idx].align as usize) < a
                    {
                        self.extra_sections[sec_idx].align = a as u64;
                    }
                }
            },
            ".zero" => {
                if let Ok(n) = args_str.trim().parse::<usize>() {
                    let cur_len = self.extra_sections[sec_idx].data.len();
                    self.extra_sections[sec_idx].data.resize(cur_len + n, 0);
                }
            },
            _ => {}, // 未知指令，静默跳过
        }
    }
    pub(crate) fn ensure_custom_section(
        &mut self,
        name: &str,
        is_exec: bool,
        is_writable: bool,
    ) -> usize {
        // Auto-detect SHF_MERGE/SHF_STRINGS from section name patterns
        let is_merge = name.contains(".rodata.str")
            || name.contains(".merge")
            || name.starts_with(".gnu.debuglto");
        let is_strings = name.contains(".rodata.str") || name.contains(".str");
        self.ensure_custom_section_with_flags(
            name,
            is_exec,
            is_writable,
            is_merge,
            is_strings,
            if is_strings { 1 } else { 0 },
        )
    }
    pub(crate) fn ensure_custom_section_with_flags(
        &mut self,
        name: &str,
        is_exec: bool,
        is_writable: bool,
        is_merge: bool,
        is_strings: bool,
        entsize: u64,
    ) -> usize {
        if let Some(&idx) = self.custom_sec_indices.get(name) {
            return idx;
        }
        let idx = self.extra_sections.len();
        let mut flags = SHF_ALLOC;
        if is_writable {
            flags |= SHF_WRITE;
        }
        if is_exec {
            flags |= SHF_EXECINSTR;
        }
        if is_merge {
            flags |= shared_elf::SHF_MERGE;
        }
        if is_strings {
            flags |= shared_elf::SHF_STRINGS;
        }
        // SHF_MERGE sections with SHF_STRINGS have entsize=1 (char)
        // SHF_MERGE sections without SHF_STRINGS have entsize=entry_size
        // ELF requires sh_addralign to be 0 or a power of 2, so round up
        // entsize (e.g. EXTABLE_SIZE=12) to the next power of 2.
        let align = if is_exec {
            16
        } else if is_merge && is_strings {
            1
        } else if is_merge && entsize > 0 {
            entsize.next_power_of_two()
        } else {
            8
        };
        self.extra_sections.push(ElfSection {
            name: name.to_string(),
            data: Vec::new(),
            ty: SHT_PROGBITS,
            flags,
            align,
            group_name: None,
        });
        self.custom_sec_indices.insert(name.to_string(), idx);
        idx
    }
    pub fn func_frames(&self, module: &crate::ir::Module) -> Vec<crate::debug::FuncFrame> {
        // 同名重复定义 (weak/comdat 的多个定义、先声明后定义) 在 `labels` 里只
        // 保留一个偏移, 直接逐个函数生成 FDE 会让多条 FDE 指向**同一段 PC 区间**:
        // `ld` 建 `.eh_frame_hdr` 搜索表时报 `.eh_frame_hdr refers to overlapping FDEs`
        // 并拒绝链接。按 PC 去重 (保留首次出现), 每条 FDE 描述一段互不重叠的区间。
        let mut seen_pc: std::collections::HashSet<u64> = std::collections::HashSet::new();
        module
            .functions
            .iter()
            .filter_map(|f| {
                let pc = *self.labels.get(&f.name)?;
                if !seen_pc.insert(pc) {
                    return None;
                }
                let sz = self
                    .func_sizes
                    .get(&f.name)
                    .copied()
                    .unwrap_or_else(|| self.compute_func_size_fallback(&f.name, pc))
                    as u32;
                let cfi_ops = self.cfi_ops_for(&f.name);
                let frame = match self.eh_lsda_offsets.get(&f.name).copied() {
                    Some(lsda) => crate::debug::FuncFrame::with_lsda(pc, sz, cfi_ops, lsda),
                    None => crate::debug::FuncFrame::with_cfi(pc, sz, cfi_ops),
                };
                Some(frame)
            })
            .collect()
    }
    /// 按**本函数的真实序言**生成 FDE 的 CFI 指令。
    ///
    /// 旧实现对每个函数发同一段常量 (`CFA = rsp + 16`), 只要序言里有
    /// `sub $N,%rsp` (N != 0) 就是错的 —— 表驱动展开完全依赖 CFA 规则,
    /// 错规则的后果是 unwinder 走不出本帧 (抛异常直接 terminate)。
    ///
    /// 本后端序言固定为 `push %r13; mov %rsp,%r13`, 之后 r13 在全函数不变,
    /// rsp 可以自由起伏 (call 参数区 / VLA), 所以下面**一条** `CFA = r13 + 16`
    /// 就能覆盖整个函数体 (含落地垫与所有 `goto` 目标)。
    fn cfi_ops_for(&self, name: &str) -> Vec<u8> {
        fn enc_uleb(out: &mut Vec<u8>, mut v: u64) {
            loop {
                let mut b = (v & 0x7f) as u8;
                v >>= 7;
                if v != 0 {
                    b |= 0x80;
                }
                out.push(b);
                if v == 0 {
                    break;
                }
            }
        }
        // DWARF CFA 主操作码 (dwarf2.h)
        const DW_CFA_ADVANCE_LOC: u8 = 0x40; // 高 2 位 = 1, 低 6 位 = delta (code_align = 1)
        const DW_CFA_ADVANCE_LOC2: u8 = 0x03;
        const DW_CFA_DEF_CFA: u8 = 0x0c;
        const DW_CFA_OFFSET: u8 = 0x80; // 高 2 位 = 2, 低 6 位 = reg, 后跟 uleb 因子
        let Some(cfi) = self.func_cfi.get(name).copied() else {
            // 没有归档 (非 compile_func 路径生成的函数): 保持旧行为, 不改变现有产物。
            return self.build_cfi_ops();
        };
        let mut ops = Vec::new();
        // 序言期间 CFA 沿用 CIE 默认的 `rsp + 8`, 到序言结束才切换到 r13。
        let delta = cfi.prologue_end;
        if delta > 0 {
            if delta <= 0x3f {
                ops.push(DW_CFA_ADVANCE_LOC | delta as u8);
            } else {
                ops.push(DW_CFA_ADVANCE_LOC2);
                ops.extend_from_slice(&(delta as u16).to_le_bytes());
            }
        }
        // CFA = r13 + 16 (frame_pointer 时多一层 `push rbp`: r13+8 = 保存的 rbp,
        // r13+16 = 返回地址, 所以 CFA = r13 + 24)
        let cfa_off: u64 = if cfi.frame_pointer { 24 } else { 16 };
        ops.push(DW_CFA_DEF_CFA);
        enc_uleb(&mut ops, 13);
        enc_uleb(&mut ops, cfa_off);
        // r13 自身存在 [r13+0]：默认档 CFA = r13+16，所以下界是 CFA-16 → 因子 2；
        // frame-pointer 档序言多推了一层 rbp，CFA = r13+24，同一个槽就是 CFA-24
        // → 因子 3。写死 2 会让 r13 与 rbp 两条 DW_CFA_offset 指向同一个地址，
        // 展开穿过 FP 帧时 r13 被恢复成 rbp 的值（实测产物里两条都是 at cfa-16）。
        ops.push(DW_CFA_OFFSET | 13);
        enc_uleb(&mut ops, if cfi.frame_pointer { 3 } else { 2 });
        if cfi.frame_pointer {
            // rbp 存在 [r13+8] = CFA-16 -> 因子 2
            ops.push(DW_CFA_OFFSET | 6);
            enc_uleb(&mut ops, 2);
        }
        // 被调用者保存寄存器: 序言把 rbx/r12/r14/r15 存到 [r13-16/-24/-32/-40],
        // 相对 CFA 即 -(cfa_off + 该偏移)。异常展开必须能恢复它们, 否则调用者
        // 的寄存器会被本帧的残留值覆盖。
        const SAVE_SLOTS: [(u8, u64); 4] = [(3, 16), (12, 24), (14, 32), (15, 40)];
        for (bit, (reg, off)) in SAVE_SLOTS.iter().enumerate() {
            if cfi.callee_saved_used & (1 << bit) != 0 {
                ops.push(DW_CFA_OFFSET | reg);
                enc_uleb(&mut ops, (cfa_off + off) / 8);
            }
        }
        ops
    }
    fn gas_section_flags(&self, name: &str) -> &'static str {
        self.extra_sections
            .iter()
            .find(|s| s.name == name)
            .map(|s| {
                if s.flags & SHF_EXECINSTR != 0 {
                    "ax"
                } else if s.flags & SHF_WRITE != 0 {
                    "aw"
                } else {
                    "a"
                }
            })
            .unwrap_or("ax")
    }
    pub(crate) fn parse_pushsection_flags(line: &str) -> (bool, bool, bool, bool, u64) {
        let mut is_writable = false;
        let mut is_exec = false;
        let mut is_merge = false;
        let mut is_strings = false;
        let mut entsize: u64 = 0;
        // Find the flags string: skip past the section name (which may be quoted)
        // Strategy: find all quoted strings, the LAST one before @type is flags
        let mut quote_positions: Vec<usize> = Vec::new();
        let mut i = 0;
        let bytes = line.as_bytes();
        while i < bytes.len() {
            if bytes[i] == b'"' {
                quote_positions.push(i);
            }
            i += 1;
        }
        // We need at least 2 quote positions for a flags string
        if quote_positions.len() >= 4 {
            // Two quoted strings: section name + flags
            // Flags are in the second pair of quotes
            let flags_start = quote_positions[2] + 1;
            let flags_end = quote_positions[3];
            if flags_start <= flags_end && flags_end <= line.len() {
                let flags = &line[flags_start..flags_end];
                is_writable = flags.contains('w') || flags.contains('W');
                is_exec = flags.contains('x') || flags.contains('X');
                is_merge = flags.contains('M');
                is_strings = flags.contains('S');
            }
            // Parse entsize after the flags quotes
            let after_flags = &line[flags_end + 1..];
            let parts: Vec<&str> = after_flags.split(',').map(|s| s.trim()).collect();
            if let Some(last) = parts.last() {
                if let Ok(n) = last.parse::<u64>() {
                    entsize = n;
                }
            }
        } else if quote_positions.len() >= 2 {
            // One quoted string: just the flags (section name is unquoted)
            let flags_start = quote_positions[0] + 1;
            let flags_end = quote_positions[1];
            if flags_start <= flags_end && flags_end <= line.len() {
                let flags = &line[flags_start..flags_end];
                is_writable = flags.contains('w') || flags.contains('W');
                is_exec = flags.contains('x') || flags.contains('X');
                is_merge = flags.contains('M');
                is_strings = flags.contains('S');
            }
            // Parse entsize after the flags quotes
            let after_flags = &line[flags_end + 1..];
            let parts: Vec<&str> = after_flags.split(',').map(|s| s.trim()).collect();
            if let Some(last) = parts.last() {
                if let Ok(n) = last.parse::<u64>() {
                    entsize = n;
                }
            }
        }
        (is_writable, is_exec, is_merge, is_strings, entsize)
    }
    pub(crate) fn resolve_deferred_pushsection_fixups(&mut self) {
        let fixups = std::mem::take(&mut self.deferred_pushsection_fixups);
        for fixup in fixups {
            let lv = self.labels.get(&fixup.left_label).copied();
            let rv = self.labels.get(&fixup.right_label).copied();
            if let (Some(lv), Some(rv)) = (lv, rv) {
                let diff = (lv as i64) - (rv as i64);
                let data = &mut self.extra_sections[fixup.sec_idx].data;
                match fixup.byte_size {
                    1 => {
                        data[fixup.data_offset] = diff as u8;
                    },
                    2 => {
                        let bytes = (diff as u16).to_le_bytes();
                        data[fixup.data_offset..fixup.data_offset + 2].copy_from_slice(&bytes);
                    },
                    4 => {
                        let bytes = (diff as u32).to_le_bytes();
                        data[fixup.data_offset..fixup.data_offset + 4].copy_from_slice(&bytes);
                    },
                    _ => {},
                }
            }
            // If labels still not found, leave as 0 (shouldn't happen in normal flow)
        }
    }

    /// 在 resolve 后捕获每个函数的最终机器码与所属节。
    /// (自 monolith compile_rel 尾部恢复: -S 输出依此将自定义节函数
    /// 列入对应 `.section` 段, 缺失时 func_asm_layout 永为空表。)
    pub(crate) fn finalize_func_asm_layout(
        &mut self,
        prog: &Program,
        custom: &[(u64, u64, usize, u64, u64)],
    ) {
        let mut custom_by_start: HashMap<u64, (String, usize, u64)> = HashMap::new();
        for (entry_start, emitted_start, _emitted_size, sec_idx, sec_off) in custom {
            let sec_name = self.extra_sections[*sec_idx as usize].name.clone();
            custom_by_start.insert(
                *entry_start,
                (sec_name, *sec_idx as usize, *sec_off + (*entry_start - *emitted_start)),
            );
        }
        let mut names = Vec::new();
        Self::collect_all_funcs(prog, &mut names);
        // 合成体也要进 `func_asm_layout`：它们不在 `prog.decls` 里，但 `-S` 需要它们
        // 的字节（命名口径与 `elf/rel_symbols.rs` 完全同一：两边都读 `ifunc_synth`）。
        for sf in &self.ifunc_synth {
            if !names.iter().any(|n| n == &sf.name) {
                names.push(sf.name.clone());
            }
        }
        for name in names {
            let start = match self.labels.get(&name) {
                Some(s) => *s,
                None => continue,
            };
            let size = self
                .func_sizes
                .get(&name)
                .copied()
                .unwrap_or_else(|| self.compute_func_size_fallback(&name, start));
            if size == 0 {
                continue;
            }
            let section;
            let bytes;
            if let Some((sec_name, sec_idx, sec_off)) = custom_by_start.get(&start) {
                section = sec_name.clone();
                let off = *sec_off as usize;
                bytes = self.extra_sections[*sec_idx].data[off..off + size as usize].to_vec();
            } else {
                section = ".text".to_string();
                bytes = self.code[start as usize..(start + size) as usize].to_vec();
            }
            self.func_asm_layout.insert(
                name,
                FuncAsmLayout {
                    section,
                    bytes,
                    text_start: start,
                },
            );
        }
    }
}
