//! asm/constraint.rs —— 自历史完整实现（git 28a9063 单文件 Codegen）按职责簇恢复的模块。
//! 仅移动/可见性调整，不改语义（backend_refactor_plan §3.1 原则 1）。
#![allow(clippy::all)]
#![allow(unused_imports)]
use crate::ast::*;
use crate::codegen::shared::asm_source::AsmPassthroughRecord;
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
    pub(crate) fn asm_constraint_is_address(constraint: &str) -> bool {
        constraint.chars().any(|c| c == 'p')
    }
    pub(crate) fn asm_constraint_is_gp(constraint: &str) -> bool {
        let letters: Vec<char> = constraint
            .chars()
            .filter(|c| c.is_ascii_alphabetic())
            .collect();
        letters
            .iter()
            .any(|c| matches!(c, 'r' | 'R' | 'q' | 'Q' | 'a' | 'b' | 'c' | 'd' | 'S' | 'D'))
    }
    pub(crate) fn asm_constraint_is_immediate(constraint: &str) -> bool {
        constraint
            .chars()
            .any(|c| matches!(c, 'i' | 'n' | 'I' | 'N' | 'M' | 'X' | 'E'))
    }
    pub(crate) fn asm_constraint_prefers_memory(constraint: &str, is_inout: bool) -> bool {
        if constraint.is_empty() {
            return false;
        }
        let letters: Vec<char> = constraint
            .chars()
            .filter(|c| c.is_ascii_alphabetic())
            .collect();
        let has_m = letters.contains(&'m');
        let _has_p = letters.contains(&'p');
        let has_r = letters
            .iter()
            .any(|c| matches!(c, 'r' | 'R' | 'q' | 'Q' | 'a' | 'b' | 'c' | 'd' | 'S' | 'D'));
        // "p" 表示内存地址, 但仍需填入寄存器 (地址本身), 不当作内存操作数直接替换
        if has_m && (!has_r || is_inout) {
            return true;
        }
        letters == vec!['m']
    }
    pub(crate) fn asm_constraint_wants_mmx(constraint: &str) -> bool {
        let core = constraint.trim_start_matches(['=', '+', '&']);
        if core.contains('{') {
            return false;
        }
        core.chars()
            .filter(|c| c.is_ascii_alphabetic())
            .any(|c| c == 'y')
    }
    pub(crate) fn asm_constraint_wants_x87(constraint: &str) -> Option<u8> {
        let core = constraint.trim_start_matches(['=', '+', '&']);
        if core.contains('{') {
            return None;
        }
        let letters: Vec<char> = core.chars().filter(|c| c.is_ascii_alphabetic()).collect();
        if letters.contains(&'t') {
            return Some(0);
        }
        if letters.contains(&'u') {
            return Some(1);
        }
        if letters.contains(&'f') {
            return Some(0);
        }
        None
    }
    pub(crate) fn asm_constraint_wants_xmm(constraint: &str) -> Option<u8> {
        let letters: Vec<char> = constraint
            .chars()
            .filter(|c| c.is_ascii_alphabetic())
            .collect();
        if letters.contains(&'x') {
            // 默认 128 位; "Yt" 是 xmm, "Xf" 是 32 位, "Yx" 是 64 位
            return Some(128);
        }
        if letters.contains(&'Y') {
            return Some(128);
        }
        None
    }
    /// Returns true if `ty` can legally be bound to an SSE (xmm) register
    /// constraint. SysV long double and complex long double are x87-only
    /// types; gcc rejects them with "inconsistent operand constraints".
    pub(crate) fn asm_type_allows_xmm(ty: &Type) -> bool {
        match ty {
            Type::LongDouble => false,
            Type::Complex(inner) | Type::Imaginary(inner) => {
                !matches!(inner.base, Type::LongDouble)
            },
            _ => true,
        }
    }
    pub(crate) fn asm_imm_needs_parens(s: &str) -> bool {
        let s = s.strip_prefix('$').unwrap_or(s);
        if s.is_empty() {
            return false;
        }
        // 跳过首字符 (可能是负号或数字), 然后检查是否有运算符
        let bytes = s.as_bytes();
        for b in bytes.iter().skip(1) {
            match b {
                b'+' | b'-' | b'*' | b'/' | b'|' | b'&' | b'^' => return true,
                _ => {},
            }
        }
        false
    }
    pub(crate) fn asm_memory_operand_str(&mut self, expr: &Expr) -> Option<String> {
        if let Expr::Ident { name, .. } = expr {
            // 局部优先: 与全局同名的参数/局部变量必须走栈槽而非 rip
            if self.is_global_not_local(&name.name) {
                // static local 的符号键是函数限定唯一名 (.LslN.func.x),
                // 必须经 resolve_global_key 解析后才能命中 sym_index
                let key = self.resolve_global_key(&name.name).to_string();
                return Some(format!("{key}(%rip)"));
            }
            let off = self.var_offset(&name.name);
            return Some(format!("{off}(%r13)"));
        }
        // 指针解引用/成员/下标/转型: 求值地址到 r12, 返回 (%r12)
        // 注意 Deref/Cast 求值得到的是指针值, Member/Index 求值得到元素地址
        match expr {
            Expr::Deref { e, .. } | Expr::Cast { e, .. } => {
                self.gen_expr(e, 12);
                Some("(%r12)".to_string())
            },
            Expr::Member { .. } | Expr::Index { .. } => {
                self.gen_expr(expr, 12);
                Some("(%r12)".to_string())
            },
            _ => None,
        }
    }
    pub(crate) fn asm_mmx_name(idx: usize) -> &'static str {
        const NAMES: [&str; 8] = ["mm0", "mm1", "mm2", "mm3", "mm4", "mm5", "mm6", "mm7"];
        NAMES[idx % 8]
    }
    pub(crate) fn asm_operand_has_non_gpr_reg(op: &AsmOperand) -> bool {
        matches!(op, AsmOperand::Reg(_, sz) if !Self::asm_reg_is_gpr(*sz))
    }
    pub(crate) fn asm_operands_have_non_gpr_regs(ops: &[&AsmOperand]) -> bool {
        ops.iter().any(|op| Self::asm_operand_has_non_gpr_reg(op))
    }
    pub(crate) fn asm_parse_cc_constraint(constraint: &str) -> Option<&str> {
        let s = constraint.trim_start_matches(['=', '+', '&']);
        s.strip_prefix("@cc").filter(|rest| !rest.is_empty())
    }
    pub(crate) fn asm_parse_explicit_reg(constraint: &str) -> Option<ExplicitRegInfo> {
        let start = constraint.find('{')?;
        let end = constraint.find('}')?;
        if end <= start + 1 {
            return None;
        }
        let name = &constraint[start + 1..end];
        Self::parse_explicit_reg_name(name)
    }
    pub(crate) fn asm_pick_sub_constraint(constraint: &str, prefer_memory: bool) -> &'static str {
        let letters: Vec<char> = constraint
            .chars()
            .filter(|c| c.is_ascii_alphabetic())
            .collect();
        if prefer_memory && letters.contains(&'m') {
            return "m";
        }
        if letters.contains(&'r') {
            return "r";
        }
        if letters.contains(&'m') {
            return "m";
        }
        // 立即数约束: i (任意) / n / I/J/K/L/M/N/O (位移量) / X / E/F/G/H (浮点常量)
        if letters.iter().any(|c| {
            matches!(
                c,
                'i' | 'n'
                    | 'I'
                    | 'J'
                    | 'K'
                    | 'L'
                    | 'M'
                    | 'N'
                    | 'O'
                    | 'X'
                    | 'E'
                    | 'F'
                    | 'G'
                    | 'H'
                    | 's'
            )
        }) {
            return "i";
        }
        if letters.contains(&'g') {
            return "g";
        }
        "r"
    }
    pub(crate) fn asm_raw_operand(val: &str) -> String {
        if let Some(stripped) = val.strip_prefix('%') {
            stripped.to_string()
        } else if let Some(stripped) = val.strip_prefix('$') {
            stripped.to_string()
        } else {
            val.to_string()
        }
    }
    pub(crate) fn asm_reg_is_gpr(sz: u8) -> bool {
        sz <= 64
    }
    pub(crate) fn asm_reg_modifier_subst(val: &str, modifier: char) -> Option<String> {
        let (idx, sz) = Self::parse_asm_reg(val)?;
        if sz >= 128 {
            return None; // 向量/mask 寄存器无 b/w/k/q 修饰符
        }
        if modifier == 'h' {
            // 高 8 位寄存器: 仅 ax/bx/cx/dx
            let name = match idx {
                0 => "%ah",
                1 => "%ch",
                2 => "%dh",
                3 => "%bh",
                _ => return None,
            };
            return Some(name.to_string());
        }
        let bits = match modifier {
            'b' => 8,
            'w' => 16,
            'k' => 32,
            'q' => 64,
            'a' => sz,
            _ => return None,
        };
        Self::gp_reg_name_bits(idx, bits).map(|s| s.to_string())
    }
    pub(crate) fn asm_rel32_symbol(&mut self, label: &str, opcode_len: u64) {
        let disp_off = self.pos() - 4;
        let at = disp_off + 4 - opcode_len;
        self.pending.push(PendingJump {
            at,
            target: label.to_string(),
            kind: JumpKind::Jmp,
            disp_offset: disp_off,
            next_ip: disp_off + 4,
            is_push_imm: false,
        });
    }
    pub(crate) fn asm_src_pos(&self) -> (String, u32) {
        let line = self.current_asm_src_line;
        if line == 0 || self.line_map.is_empty() {
            return (self.source_name.clone(), line);
        }
        // 二分查找最后一个 out_start <= line 的区间
        let idx = self.line_map.partition_point(|s| s.out_start <= line);
        if let Some(ls) = idx.checked_sub(1).and_then(|i| self.line_map.get(i)) {
            if line < ls.out_start + ls.count {
                return (ls.file.clone(), ls.src_start + (line - ls.out_start));
            }
            // 未严格命中 (宏展开/缓存回放): 保留行号, 回填文件归属
            return (ls.file.clone(), line);
        }
        // line 小于 line_map[0].out_start: 预处理输出起始的预写入行
        if let Some(first) = self.line_map.first() {
            return (first.file.clone(), first.src_start.max(1));
        }
        (self.source_name.clone(), line)
    }
    pub(crate) fn asm_symbolic_operand(val: &str) -> bool {
        val.chars()
            .next()
            .is_some_and(|c| c == '.' || c == '_' || c.is_ascii_alphabetic())
            && Self::parse_asm_reg(&format!("%{val}")).is_none()
    }
    pub(crate) fn asm_symbol_is_function(&self, name: &str) -> bool {
        self.asm_func_symbols.contains(name) || self.global_asm_exports.contains(name)
    }
    pub(crate) fn asm_symbol_name(raw: &str) -> String {
        raw.trim()
            .trim_end_matches(',')
            .trim_matches('"')
            .to_string()
    }
    /// 内联汇编某一行的编码被本后端拒收。
    ///
    /// 旧实现叫 `asm_warn` 但实际 push 的是 `E0311` Error, 把“这个缺陷意味着
    /// 什么”的判定留在最底层。现在只记事实: 诊断由
    /// `asm_source::passthrough_diagnostics` 统一生成, 是否降级由
    /// `codegen::emit()` 根据 `--asm-fallback` 统一决定。
    pub(crate) fn asm_reject(&mut self, msg: &str) {
        let (file, line) = self.asm_src_pos();
        let ordinal = self.current_asm_line;
        crate::codegen::shared::asm_source::note_backend_reject(
            self,
            crate::codegen::shared::asm_source::AsmIsa::X86_64,
            msg,
            ordinal,
            &file,
            line,
        );
    }
    pub(crate) fn asm_xmm_name(idx: usize) -> &'static str {
        const NAMES: [&str; 16] = [
            "xmm0", "xmm1", "xmm2", "xmm3", "xmm4", "xmm5", "xmm6", "xmm7", "xmm8", "xmm9",
            "xmm10", "xmm11", "xmm12", "xmm13", "xmm14", "xmm15",
        ];
        NAMES[idx % 16]
    }
    pub(crate) fn parse_asm_constraint_flags(constraint: &str) -> (bool, bool, bool, Option<u32>) {
        let mut is_output = false;
        let mut is_inout = false;
        let mut early = false;
        let mut match_digit: Option<u32> = None;
        let mut s = constraint.trim();
        while !s.is_empty() {
            match s.chars().next() {
                Some('=') => {
                    is_output = true;
                    s = &s[1..];
                },
                Some('+') => {
                    is_output = true;
                    is_inout = true;
                    s = &s[1..];
                },
                Some('&') => {
                    early = true;
                    s = &s[1..];
                },
                Some('%') => {
                    s = &s[1..];
                },
                Some(c) if c.is_ascii_digit() => {
                    let mut val: u32 = 0;
                    while let Some(d) = s.chars().next() {
                        if !d.is_ascii_digit() {
                            break;
                        }
                        val = val.wrapping_mul(10).wrapping_add(d.to_digit(10).unwrap());
                        s = &s[1..];
                    }
                    match_digit = Some(val);
                },
                _ => break,
            }
        }
        (is_output, is_inout, early, match_digit)
    }
    pub(crate) fn parse_asm_data_expr(
        expr: &str,
        labels: &HashMap<String, u64>,
        locals: &AsmLocalLabels,
        line_idx: usize,
        numeric_label_map: &std::collections::HashMap<String, String>,
    ) -> AsmDataExprValue {
        let expr = expr.trim();
        if expr.is_empty() {
            return (0, None, None);
        }
        // 查找 '-' 和 '+' 操作符 (不在引号/括号内)
        let (expr_before, op, expr_after) = Self::find_expr_operator(expr);
        if let Some(op) = op {
            let left = expr_before.trim();
            let right = expr_after.trim();
            // symbol - . → PC32, addend=0
            if right == "." {
                let sym = Self::resolve_asm_data_symbol(left, locals, line_idx, numeric_label_map);
                return (0, Some((sym, shared_elf::R_X86_64_PC32, 0)), None);
            }
            // symbol + N → PC32, addend=N (PIC-compatible)
            if op == '+' {
                if let Ok(n) = right.parse::<i64>() {
                    let sym =
                        Self::resolve_asm_data_symbol(left, locals, line_idx, numeric_label_map);
                    return (0, Some((sym, shared_elf::R_X86_64_PC32, n)), None);
                }
            }
            // symbol - number → PC32 with negative addend
            // symbol - symbol → compute difference if both labels are known
            if op == '-' {
                if let Ok(n) = right.parse::<i64>() {
                    let sym =
                        Self::resolve_asm_data_symbol(left, locals, line_idx, numeric_label_map);
                    return (0, Some((sym, shared_elf::R_X86_64_PC32, -n)), None);
                }
                // Both operands are symbols: resolve and compute difference
                let left_sym =
                    Self::resolve_asm_data_symbol(left, locals, line_idx, numeric_label_map);
                let right_sym =
                    Self::resolve_asm_data_symbol(right, locals, line_idx, numeric_label_map);
                if let (Some(&lv), Some(&rv)) = (labels.get(&left_sym), labels.get(&right_sym)) {
                    return ((lv as i64) - (rv as i64), None, None);
                }
                // Forward reference: defer resolution
                return (0, None, Some((left_sym, right_sym)));
            }
        }
        // 纯符号引用 (无算术) → PC32 (PIC-compatible, 避免 R_X86_64_32 绝对重定位)
        let (val, is_sym) = Self::parse_asm_expr_atom(expr, locals, line_idx, numeric_label_map);
        if is_sym {
            let sym = Self::resolve_asm_data_symbol(expr, locals, line_idx, numeric_label_map);
            return (0, Some((sym, shared_elf::R_X86_64_PC32, 0)), None);
        }
        (val, None, None)
    }
    fn parse_asm_expr_atom(
        expr: &str,
        _locals: &AsmLocalLabels,
        _line_idx: usize,
        _numeric_label_map: &std::collections::HashMap<String, String>,
    ) -> (i64, bool) {
        let expr = expr.trim();
        if expr == "." {
            return (0, false);
        }
        // 数字标签引用: Nb 或 Nf
        if expr.len() >= 2 {
            let last = expr.as_bytes()[expr.len() - 1];
            if last == b'b' || last == b'f' {
                let num_str = &expr[..expr.len() - 1];
                if num_str.chars().all(|c| c.is_ascii_digit()) && !num_str.is_empty() {
                    return (0, true); // 符号引用
                }
            }
        }
        // 纯数字
        if let Some(hex) = expr.strip_prefix("0x").or_else(|| expr.strip_prefix("0X")) {
            if let Ok(v) = i64::from_str_radix(hex, 16) {
                return (v, false);
            }
        }
        if let Ok(v) = expr.parse::<i64>() {
            return (v, false);
        }
        // 其他符号名
        if !expr.is_empty() {
            return (0, true);
        }
        (0, false)
    }
    pub(crate) fn parse_asm_imm_token(s: &str) -> Option<i64> {
        let s = s.trim().trim_end_matches(',');
        if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            i64::from_str_radix(hex, 16).ok()
        } else {
            s.parse().ok()
        }
    }
    fn parse_asm_mem(s: &str) -> AsmOperand {
        // 格式: [disp](%base[,%idx[,scale]])
        let paren_pos = s.find('(').unwrap_or(s.len());
        let disp_str = s[..paren_pos].trim();
        let rest = &s[paren_pos..];
        // 检测 disp 是否为符号 (而不是立即数): symbol(%rip) / sym(%base)
        let mut disp: i32 = 0;
        if !disp_str.is_empty() {
            if let Some(hex) = disp_str
                .strip_prefix("0x")
                .or_else(|| disp_str.strip_prefix("0X"))
            {
                disp = i32::from_str_radix(hex, 16).unwrap_or(0);
            } else if disp_str.parse::<i32>().is_err() {
                // 符号名: 转 SymDisp 携带符号名 (RIP-relative 默认)
                return AsmOperand::SymDisp(disp_str.to_string(), 0);
            } else {
                disp = disp_str.parse::<i32>().unwrap_or(0);
            }
        }
        // 提取括号内的内容
        let inner = if rest.starts_with('(') && rest.ends_with(')') {
            &rest[1..rest.len() - 1]
        } else {
            return AsmOperand::None;
        };
        let parts: Vec<&str> = inner.split(',').collect();
        let base_str = parts.first().map(|s| s.trim()).unwrap_or("");
        if base_str == "%rip" || base_str == "rip" {
            // 检查是否有 index/scale: disp(%rip,%idx,scale)
            let mut idx: Option<u8> = None;
            let mut scale: u8 = 1;
            if parts.len() > 1 {
                let idx_str = parts[1].trim();
                if !idx_str.is_empty() {
                    idx = Self::parse_asm_reg(idx_str).map(|(r, _)| r);
                }
            }
            if parts.len() > 2 {
                scale = parts[2].trim().parse::<u8>().unwrap_or(1);
            }
            return AsmOperand::Mem {
                disp,
                base: ASM_BASE_RIP,
                index: idx,
                scale,
            };
        }
        let (base, _bsz) = Self::parse_asm_reg(base_str).unwrap_or((5, 64)); // 默认 rbp
        let mut idx: Option<u8> = None;
        let mut scale: u8 = 1;
        if parts.len() > 1 {
            let idx_str = parts[1].trim();
            if !idx_str.is_empty() {
                idx = Self::parse_asm_reg(idx_str).map(|(r, _)| r);
            }
        }
        if parts.len() > 2 {
            scale = parts[2].trim().parse::<u8>().unwrap_or(1);
        }
        AsmOperand::Mem {
            disp,
            base,
            index: idx,
            scale,
        }
    }
    pub(crate) fn parse_asm_named_label(line: &str) -> Option<&str> {
        let line = AsmLocalLabels::strip_asm_comment(line).trim();
        let (label, _) = line.split_once(':')?;
        let label = label.trim();
        if label.is_empty()
            || label.chars().all(|c| c.is_ascii_digit())
            || Self::is_asm_directive(label)
            || label.contains([' ', '\t', '%', '(', ')'])
            || label
                .chars()
                .next()
                .map(|c| c.is_ascii_digit())
                .unwrap_or(false)
        {
            return None;
        }
        Some(label)
    }
    pub(crate) fn parse_asm_operand_str(s: &str) -> AsmOperand {
        let s = s.trim();
        if s.is_empty() {
            return AsmOperand::None;
        }
        // 段前缀剥离: %gs:... / gs:... / 单独的 %gs / gs (必须在其他检查之前)
        let (s, seg) = Self::strip_asm_seg_prefix(s);
        let s = s.trim();
        if s.is_empty() {
            return if let Some(sc) = seg {
                AsmOperand::Seg(sc)
            } else {
                AsmOperand::None
            };
        }
        // 间接前缀 *: jmp *%rax, call *symbol, jmp *(%rax) 等
        if let Some(rest) = s.strip_prefix('*') {
            let inner = Self::parse_asm_operand_str(rest);
            return Self::wrap_indirect(inner);
        }
        // 立即数: $N
        if let Some(rest) = s.strip_prefix('$') {
            let rest = rest.trim();
            if let Some(hex) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
                if let Ok(v) = i64::from_str_radix(hex, 16) {
                    return AsmOperand::Imm(v);
                }
            } else if let Ok(v) = rest.parse::<i64>() {
                return AsmOperand::Imm(v);
            }
            // $label (e.g. $1f, $1b) — local label reference
            return AsmOperand::Sym(rest.to_string());
        }
        // 寄存器: %reg 或裸 rax/rbx 等
        if s.starts_with('%') {
            if let Some((idx, sz)) = Self::parse_asm_reg(s) {
                return AsmOperand::Reg(idx, sz);
            }
            return AsmOperand::None;
        }
        if let Some((idx, sz)) = Self::parse_asm_reg(&format!("%{s}")) {
            return AsmOperand::Reg(idx, sz);
        }
        // 内存: disp(%base,%idx,scale) 或 (%reg) 或 symbol(%rip)
        if s.contains('(') {
            let inner = Self::parse_asm_mem(s);
            return if let (
                Some(sc),
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
            ) = (seg, &inner)
            {
                AsmOperand::SegMem {
                    seg: sc,
                    disp: *disp,
                    base: *base,
                    index: *index,
                    scale: *scale,
                }
            } else {
                inner
            };
        }
        // 段前缀 + 符号 (例如 `gs:symbol` 实际罕见; 当作符号)
        // 符号名 (全局标签引用) + 段前缀 → 段前缀跟随到 emit 阶段
        let sym = AsmLocalLabels::strip_asm_comment(s).trim().to_string();
        if let Some(sc) = seg {
            return AsmOperand::SegMem {
                seg: sc,
                disp: 0,
                base: ASM_BASE_RIP,
                index: None,
                scale: 1,
            };
        }
        AsmOperand::Sym(sym)
    }
    pub(crate) fn parse_asm_pc_relative_long(expr: &str) -> Option<(String, i64)> {
        let expr = expr.trim();
        let (left, right) = expr.split_once('-')?;
        let symbol = left.trim();
        if symbol.is_empty() || symbol == "." {
            return None;
        }
        let right = right.trim().replace(' ', "");
        let addend = match right.as_str() {
            "." => 0,
            "(.+4)" => -4,
            r if r.starts_with('.') => {
                let n = r.strip_prefix(".+")?.parse::<i64>().ok()?;
                -n
            },
            r => -r.parse::<i64>().ok()?,
        };
        Some((symbol.to_string(), addend))
    }
    pub(crate) fn parse_asm_quoted_string(s: &str) -> String {
        let s = s.trim();
        if let Some(stripped) = s.strip_prefix('"') {
            let end = stripped.find('"').unwrap_or(stripped.len());
            let inner = &stripped[..end];
            let mut result = String::new();
            let mut chars = inner.chars();
            while let Some(c) = chars.next() {
                if c == '\\' {
                    match chars.next() {
                        Some('n') => result.push('\n'),
                        Some('t') => result.push('\t'),
                        Some('\\') => result.push('\\'),
                        Some('"') => result.push('"'),
                        Some('0') => result.push('\0'),
                        Some(other) => {
                            result.push('\\');
                            result.push(other);
                        },
                        None => result.push('\\'),
                    }
                } else {
                    result.push(c);
                }
            }
            return result;
        }
        s.to_string()
    }
    fn parse_asm_reg(name: &str) -> Option<(u8, u8)> {
        let n = name.strip_prefix('%').unwrap_or(name);
        match n {
            "rax" => Some((0, 64)),
            "eax" => Some((0, 32)),
            "ax" => Some((0, 16)),
            "al" => Some((0, 8)),
            "ah" => Some((0, 8)),
            "rbx" => Some((3, 64)),
            "ebx" => Some((3, 32)),
            "bx" => Some((3, 16)),
            "bl" => Some((3, 8)),
            "bh" => Some((3, 8)),
            "rcx" => Some((1, 64)),
            "ecx" => Some((1, 32)),
            "cx" => Some((1, 16)),
            "cl" => Some((1, 8)),
            "ch" => Some((1, 8)),
            "rdx" => Some((2, 64)),
            "edx" => Some((2, 32)),
            "dx" => Some((2, 16)),
            "dl" => Some((2, 8)),
            "dh" => Some((2, 8)),
            "rsi" => Some((6, 64)),
            "esi" => Some((6, 32)),
            "si" => Some((6, 16)),
            "sil" => Some((6, 8)),
            "rdi" => Some((7, 64)),
            "edi" => Some((7, 32)),
            "di" => Some((7, 16)),
            "dil" => Some((7, 8)),
            "rsp" => Some((4, 64)),
            "esp" => Some((4, 32)),
            "sp" => Some((4, 16)),
            "spl" => Some((4, 8)),
            "rbp" => Some((5, 64)),
            "ebp" => Some((5, 32)),
            "bp" => Some((5, 16)),
            "bpl" => Some((5, 8)),
            "r8" => Some((8, 64)),
            "r8d" => Some((8, 32)),
            "r8w" => Some((8, 16)),
            "r8b" => Some((8, 8)),
            "r9" => Some((9, 64)),
            "r9d" => Some((9, 32)),
            "r9w" => Some((9, 16)),
            "r9b" => Some((9, 8)),
            "r10" => Some((10, 64)),
            "r10d" => Some((10, 32)),
            "r10w" => Some((10, 16)),
            "r10b" => Some((10, 8)),
            "r11" => Some((11, 64)),
            "r11d" => Some((11, 32)),
            "r11w" => Some((11, 16)),
            "r11b" => Some((11, 8)),
            "r12" => Some((12, 64)),
            "r12d" => Some((12, 32)),
            "r12w" => Some((12, 16)),
            "r12b" => Some((12, 8)),
            "r13" => Some((13, 64)),
            "r13d" => Some((13, 32)),
            "r13w" => Some((13, 16)),
            "r13b" => Some((13, 8)),
            "r14" => Some((14, 64)),
            "r14d" => Some((14, 32)),
            "r14w" => Some((14, 16)),
            "r14b" => Some((14, 8)),
            "r15" => Some((15, 64)),
            "r15d" => Some((15, 32)),
            "r15w" => Some((15, 16)),
            "r15b" => Some((15, 8)),
            // APX EGPR r16-r31
            "r16" => Some((16, 64)),
            "r16d" => Some((16, 32)),
            "r16w" => Some((16, 16)),
            "r16b" => Some((16, 8)),
            "r17" => Some((17, 64)),
            "r17d" => Some((17, 32)),
            "r17w" => Some((17, 16)),
            "r17b" => Some((17, 8)),
            "r18" => Some((18, 64)),
            "r18d" => Some((18, 32)),
            "r18w" => Some((18, 16)),
            "r18b" => Some((18, 8)),
            "r19" => Some((19, 64)),
            "r19d" => Some((19, 32)),
            "r19w" => Some((19, 16)),
            "r19b" => Some((19, 8)),
            "r20" => Some((20, 64)),
            "r20d" => Some((20, 32)),
            "r20w" => Some((20, 16)),
            "r20b" => Some((20, 8)),
            "r21" => Some((21, 64)),
            "r21d" => Some((21, 32)),
            "r21w" => Some((21, 16)),
            "r21b" => Some((21, 8)),
            "r22" => Some((22, 64)),
            "r22d" => Some((22, 32)),
            "r22w" => Some((22, 16)),
            "r22b" => Some((22, 8)),
            "r23" => Some((23, 64)),
            "r23d" => Some((23, 32)),
            "r23w" => Some((23, 16)),
            "r23b" => Some((23, 8)),
            "r24" => Some((24, 64)),
            "r24d" => Some((24, 32)),
            "r24w" => Some((24, 16)),
            "r24b" => Some((24, 8)),
            "r25" => Some((25, 64)),
            "r25d" => Some((25, 32)),
            "r25w" => Some((25, 16)),
            "r25b" => Some((25, 8)),
            "r26" => Some((26, 64)),
            "r26d" => Some((26, 32)),
            "r26w" => Some((26, 16)),
            "r26b" => Some((26, 8)),
            "r27" => Some((27, 64)),
            "r27d" => Some((27, 32)),
            "r27w" => Some((27, 16)),
            "r27b" => Some((27, 8)),
            "r28" => Some((28, 64)),
            "r28d" => Some((28, 32)),
            "r28w" => Some((28, 16)),
            "r28b" => Some((28, 8)),
            "r29" => Some((29, 64)),
            "r29d" => Some((29, 32)),
            "r29w" => Some((29, 16)),
            "r29b" => Some((29, 8)),
            "r30" => Some((30, 64)),
            "r30d" => Some((30, 32)),
            "r30w" => Some((30, 16)),
            "r30b" => Some((30, 8)),
            "r31" => Some((31, 64)),
            "r31d" => Some((31, 32)),
            "r31w" => Some((31, 16)),
            "r31b" => Some((31, 8)),
            "rip" => Some((ASM_BASE_RIP, 64)),
            "xmm0" => Some((0, ASM_REG_XMM)),
            "xmm1" => Some((1, ASM_REG_XMM)),
            "xmm2" => Some((2, ASM_REG_XMM)),
            "xmm3" => Some((3, ASM_REG_XMM)),
            "xmm4" => Some((4, ASM_REG_XMM)),
            "xmm5" => Some((5, ASM_REG_XMM)),
            "xmm6" => Some((6, ASM_REG_XMM)),
            "xmm7" => Some((7, ASM_REG_XMM)),
            "xmm8" => Some((8, ASM_REG_XMM)),
            "xmm9" => Some((9, ASM_REG_XMM)),
            "xmm10" => Some((10, ASM_REG_XMM)),
            "xmm11" => Some((11, ASM_REG_XMM)),
            "xmm12" => Some((12, ASM_REG_XMM)),
            "xmm13" => Some((13, ASM_REG_XMM)),
            "xmm14" => Some((14, ASM_REG_XMM)),
            "xmm15" => Some((15, ASM_REG_XMM)),
            // ymm/zmm：VEX/EVEX 256/512 位向量寄存器
            "ymm0" => Some((0, ASM_REG_YMM)),
            "ymm1" => Some((1, ASM_REG_YMM)),
            "ymm2" => Some((2, ASM_REG_YMM)),
            "ymm3" => Some((3, ASM_REG_YMM)),
            "ymm4" => Some((4, ASM_REG_YMM)),
            "ymm5" => Some((5, ASM_REG_YMM)),
            "ymm6" => Some((6, ASM_REG_YMM)),
            "ymm7" => Some((7, ASM_REG_YMM)),
            "ymm8" => Some((8, ASM_REG_YMM)),
            "ymm9" => Some((9, ASM_REG_YMM)),
            "ymm10" => Some((10, ASM_REG_YMM)),
            "ymm11" => Some((11, ASM_REG_YMM)),
            "ymm12" => Some((12, ASM_REG_YMM)),
            "ymm13" => Some((13, ASM_REG_YMM)),
            "ymm14" => Some((14, ASM_REG_YMM)),
            "ymm15" => Some((15, ASM_REG_YMM)),
            "zmm0" => Some((0, ASM_REG_ZMM)),
            "zmm1" => Some((1, ASM_REG_ZMM)),
            "zmm2" => Some((2, ASM_REG_ZMM)),
            "zmm3" => Some((3, ASM_REG_ZMM)),
            "zmm4" => Some((4, ASM_REG_ZMM)),
            "zmm5" => Some((5, ASM_REG_ZMM)),
            "zmm6" => Some((6, ASM_REG_ZMM)),
            "zmm7" => Some((7, ASM_REG_ZMM)),
            "zmm8" => Some((8, ASM_REG_ZMM)),
            "zmm9" => Some((9, ASM_REG_ZMM)),
            "zmm10" => Some((10, ASM_REG_ZMM)),
            "zmm11" => Some((11, ASM_REG_ZMM)),
            "zmm12" => Some((12, ASM_REG_ZMM)),
            "zmm13" => Some((13, ASM_REG_ZMM)),
            "zmm14" => Some((14, ASM_REG_ZMM)),
            "zmm15" => Some((15, ASM_REG_ZMM)),
            "zmm16" => Some((16, ASM_REG_ZMM)),
            "zmm17" => Some((17, ASM_REG_ZMM)),
            "zmm18" => Some((18, ASM_REG_ZMM)),
            "zmm19" => Some((19, ASM_REG_ZMM)),
            "zmm20" => Some((20, ASM_REG_ZMM)),
            "zmm21" => Some((21, ASM_REG_ZMM)),
            "zmm22" => Some((22, ASM_REG_ZMM)),
            "zmm23" => Some((23, ASM_REG_ZMM)),
            "zmm24" => Some((24, ASM_REG_ZMM)),
            "zmm25" => Some((25, ASM_REG_ZMM)),
            "zmm26" => Some((26, ASM_REG_ZMM)),
            "zmm27" => Some((27, ASM_REG_ZMM)),
            "zmm28" => Some((28, ASM_REG_ZMM)),
            "zmm29" => Some((29, ASM_REG_ZMM)),
            "zmm30" => Some((30, ASM_REG_ZMM)),
            "zmm31" => Some((31, ASM_REG_ZMM)),
            // AMX tile 寄存器 (tmm0–tmm7)
            "tmm0" => Some((0, ASM_REG_TMM)),
            "tmm1" => Some((1, ASM_REG_TMM)),
            "tmm2" => Some((2, ASM_REG_TMM)),
            "tmm3" => Some((3, ASM_REG_TMM)),
            "tmm4" => Some((4, ASM_REG_TMM)),
            "tmm5" => Some((5, ASM_REG_TMM)),
            "tmm6" => Some((6, ASM_REG_TMM)),
            "tmm7" => Some((7, ASM_REG_TMM)),
            // AVX-512 mask 寄存器 (k0–k7)
            "k0" => Some((0, ASM_REG_K)),
            "k1" => Some((1, ASM_REG_K)),
            "k2" => Some((2, ASM_REG_K)),
            "k3" => Some((3, ASM_REG_K)),
            "k4" => Some((4, ASM_REG_K)),
            "k5" => Some((5, ASM_REG_K)),
            "k6" => Some((6, ASM_REG_K)),
            "k7" => Some((7, ASM_REG_K)),
            // 控制寄存器 (内核 read_crN/write_crN)
            "cr0" => Some((0, ASM_REG_CR)),
            "cr1" => Some((1, ASM_REG_CR)),
            "cr2" => Some((2, ASM_REG_CR)),
            "cr3" => Some((3, ASM_REG_CR)),
            "cr4" => Some((4, ASM_REG_CR)),
            "cr5" => Some((5, ASM_REG_CR)),
            "cr6" => Some((6, ASM_REG_CR)),
            "cr7" => Some((7, ASM_REG_CR)),
            "cr8" => Some((8, ASM_REG_CR)),
            // 调试寄存器 (GAS 用 drN 与 dbN 皆可)
            "dr0" => Some((0, ASM_REG_DR)),
            "dr1" => Some((1, ASM_REG_DR)),
            "dr2" => Some((2, ASM_REG_DR)),
            "dr3" => Some((3, ASM_REG_DR)),
            "dr4" => Some((4, ASM_REG_DR)),
            "dr5" => Some((5, ASM_REG_DR)),
            "dr6" => Some((6, ASM_REG_DR)),
            "dr7" => Some((7, ASM_REG_DR)),
            "db0" => Some((0, ASM_REG_DR)),
            "db1" => Some((1, ASM_REG_DR)),
            "db2" => Some((2, ASM_REG_DR)),
            "db3" => Some((3, ASM_REG_DR)),
            "db4" => Some((4, ASM_REG_DR)),
            "db5" => Some((5, ASM_REG_DR)),
            "db6" => Some((6, ASM_REG_DR)),
            "db7" => Some((7, ASM_REG_DR)),
            // MMX 寄存器 (mm0–mm7)
            "mm0" => Some((0, ASM_REG_MMX)),
            "mm1" => Some((1, ASM_REG_MMX)),
            "mm2" => Some((2, ASM_REG_MMX)),
            "mm3" => Some((3, ASM_REG_MMX)),
            "mm4" => Some((4, ASM_REG_MMX)),
            "mm5" => Some((5, ASM_REG_MMX)),
            "mm6" => Some((6, ASM_REG_MMX)),
            "mm7" => Some((7, ASM_REG_MMX)),
            // x87 FPU 栈寄存器 (AT&T 语法)
            "st" => Some((0, ASM_REG_ST)),
            "st(0)" => Some((0, ASM_REG_ST)),
            "st(1)" => Some((1, ASM_REG_ST)),
            "st(2)" => Some((2, ASM_REG_ST)),
            "st(3)" => Some((3, ASM_REG_ST)),
            "st(4)" => Some((4, ASM_REG_ST)),
            "st(5)" => Some((5, ASM_REG_ST)),
            "st(6)" => Some((6, ASM_REG_ST)),
            "st(7)" => Some((7, ASM_REG_ST)),
            _ => None,
        }
    }
    pub(crate) fn parse_asm_seg(name: &str) -> Option<(u8, u8)> {
        let n = name.strip_prefix('%').unwrap_or(name);
        match n {
            "es" => Some((0, 0x26)),
            "cs" => Some((1, 0x2E)),
            "ss" => Some((2, 0x36)),
            "ds" => Some((3, 0x3E)),
            "fs" => Some((4, 0x64)),
            "gs" => Some((5, 0x65)),
            _ => None,
        }
    }
    pub(crate) fn parse_clobber_registers(
        clobbers: &[String],
    ) -> (
        std::collections::HashSet<u8>, // GP 寄存器索引
        std::collections::HashSet<u8>, // XMM 寄存器索引
        u8,                            // x87 clobber 计数
    ) {
        let mut gp_set = std::collections::HashSet::new();
        let mut xmm_set = std::collections::HashSet::new();
        let mut x87_count: u8 = 0;

        for c in clobbers {
            let name = c.trim().to_lowercase();
            // cc / memory: 特殊 clobber, 不进寄存器池
            if name == "cc" || name == "memory" {
                continue;
            }
            // x87: "st" / "st(0)".."st(7)"
            if name == "st" || name.starts_with("st(") {
                x87_count = x87_count.saturating_add(1);
                continue;
            }
            // xmm0..xmm15
            if let Some(idx) = name.strip_prefix("xmm") {
                if let Ok(n) = idx.parse::<u8>() {
                    if n < 16 {
                        xmm_set.insert(n);
                    }
                }
                continue;
            }
            // GP 寄存器 (所有子宽度: eax/rax/ax/al/ah 等都映射到同一物理寄存器)
            if let Some((reg_idx, _bits, _sname)) = Self::parse_reg_name_bits(&name) {
                gp_set.insert(reg_idx);
                continue;
            }
            // 未知 clobber: 静默忽略 (GCC 兼容: 允许未知 clobber 字符串)
        }

        (gp_set, xmm_set, x87_count)
    }
    fn resolve_asm_data_symbol(
        expr: &str,
        locals: &AsmLocalLabels,
        line_idx: usize,
        numeric_label_map: &std::collections::HashMap<String, String>,
    ) -> String {
        let expr = expr.trim();
        // 数字标签: Nb 或 Nf
        if expr.len() >= 2 {
            let last = expr.as_bytes()[expr.len() - 1];
            if last == b'b' || last == b'f' {
                let num_str = &expr[..expr.len() - 1];
                if num_str.chars().all(|c| c.is_ascii_digit()) && !num_str.is_empty() {
                    // 通过 locals 查找内部名
                    let num: u32 = num_str.parse().unwrap_or(0);
                    if let Some(name) = locals.resolve(num, last as char, line_idx) {
                        return name.to_string();
                    }
                    // 回退到 numeric_label_map
                    if let Some(name) = numeric_label_map.get(num_str) {
                        return name.clone();
                    }
                    return format!(".Lasm_local_0_{}_0", num_str);
                }
            }
        }
        expr.to_string()
    }
}

// asm_source 的数据容器接入 (x86_64 只实现基础面)。
//
// x86_64 的 -S 文本由反汇编自己刚编码的字节得到, 不保存用户 asm 原文容器,
// 因此只实现 AsmOutcomeSink 而不实现 AsmStmtSink —— 能力差异留在类型上,
// 不用空实现掩盖。
impl crate::codegen::shared::asm_source::AsmOutcomeSink for Codegen {
    fn pc(&self) -> u64 {
        self.code.len() as u64
    }
    fn push_code(&mut self, bytes: &[u8]) {
        self.code.extend_from_slice(bytes);
    }
    fn save_passthrough_region(&mut self, pc: u64, lines: Vec<String>) {
        self.asm_passthrough_regions.push((pc, lines));
    }
    fn add_passthrough_records(&mut self, recs: Vec<AsmPassthroughRecord>) {
        self.asm_passthrough.extend(recs);
    }
}
