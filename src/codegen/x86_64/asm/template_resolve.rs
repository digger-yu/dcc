//! asm/template_resolve.rs —— 自历史完整实现（git 28a9063 单文件 Codegen）按职责簇恢复的模块。
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
    pub(crate) fn asm_replace_label_idx(s: &str, idx: usize, label_name: &str) -> String {
        let pattern = format!("%l{}", idx);
        let mut result = String::with_capacity(s.len());
        let mut rest = s;
        while let Some(pos) = rest.find(&pattern) {
            let end = pos + pattern.len();
            // 边界检查: 后面不能跟数字
            let next_char = rest[end..].chars().next();
            if next_char.is_some_and(|c| c.is_ascii_digit()) {
                // 不是完整匹配, 跳过这个位置
                result.push_str(&rest[..end]);
                rest = &rest[end..];
            } else {
                result.push_str(&rest[..pos]);
                result.push_str(label_name);
                rest = &rest[end..];
            }
        }
        result.push_str(rest);
        result
    }
    /// 把 AST 里的标签名翻译成汇编标签。
    ///
    /// 必须带上**当前函数名**作为命名空间: SSA 回写的块标签 (`eh_lp.2` /
    /// `label_l1.5` / `for_cond.3`) 的序号是每函数从 0 重计的, 只用 `.Ll_<名>`
    /// 时不同函数会产生**完全同名**的标签。后果不是汇编报错 (GAS 允许重定义
    /// `.L` 标签) 而是 `Codegen::labels` 这张全局表被后写的函数覆盖:
    /// 先发的跳转与后建的表各自指向不同函数的同名标签, 实测零成本异常的 LSDA
    /// call-site landing pad 指到另一个函数的落地垫上。
    pub(crate) fn c_label_to_asm(&self, name: &str) -> String {
        if self.current_func_name.is_empty() {
            // 预注册期（`pre_register_static_locals`）没有"当前函数"，但 static local
            // 的初始化式可能含 `&&label`（computed-goto 跳转表）。此时命名空间由
            // `static_local_label_owner`（该 static local 的属主函数）给出 —— 缺了它，
            // 算出的 `.Ll_<label>` 与函数体发射时的 `.Ll_<func>_<label>` 不是同一个
            // 符号（旧代码正是因此把跳转表降级成运行时初始化）。
            match &self.static_local_label_owner {
                Some(owner) => format!(".Ll_{}_{}", owner, name),
                None => format!(".Ll_{}", name),
            }
        } else {
            format!(".Ll_{}_{}", self.current_func_name, name)
        }
    }
    /// Resolve a user-facing C label name to its asm symbol, taking nested
    /// function scoping into account.
    ///
    /// If the current function is a nested function and `name` is NOT a label
    /// defined in the current function's own body (`self.local_labels`),
    /// then the label belongs to the enclosing parent function. Using the
    /// current (child) function name as namespace would produce an asm symbol
    /// that is never defined (the parent emits `.Ll_<parent>_<name>`). Walk
    /// up one hop through `nested_func_parent` to find the right namespace.
    ///
    /// This is exactly the same rule as `Stmt::Goto` non-local resolution;
    /// shared here so `&&label` (LabelAddr) and `goto *&&label` agree.
    pub(crate) fn c_label_to_asm_resolve_nested(&self, name: &str) -> String {
        let is_nonlocal = self.current_func_is_nested && !self.local_labels.contains(name);
        if is_nonlocal {
            if let Some(parent) = self.nested_func_parent.get(&self.current_func_name) {
                return format!(".Ll_{}_{}", parent, name);
            }
        }
        self.c_label_to_asm(name)
    }
    pub(crate) fn merge_standalone_prefixes(lines: Vec<String>) -> Vec<String> {
        let prefixes = ["lock", "rep", "repz", "repnz", "repe"];
        let mut result = Vec::with_capacity(lines.len());
        let mut i = 0;
        while i < lines.len() {
            let trimmed = lines[i].trim().to_lowercase();
            if prefixes.contains(&trimmed.as_str()) && i + 1 < lines.len() {
                // 前缀独占一行: 与下一行合并
                let mut merged = lines[i].trim().to_string();
                merged.push(' ');
                merged.push_str(lines[i + 1].trim());
                result.push(merged);
                i += 2;
            } else {
                result.push(lines[i].clone());
                i += 1;
            }
        }
        result
    }
    pub(crate) fn resolve_asm_template(
        &self,
        line: &str,
        outputs: &[Expr],
        output_names: &[Option<String>],
        inputs: &[Expr],
        input_names: &[Option<String>],
        op_map: &HashMap<String, String>,
    ) -> String {
        let mut resolved = line.to_string();
        for (idx, op) in outputs.iter().chain(inputs.iter()).enumerate() {
            let key = format!("%{}", idx);
            if let Some(val) = op_map.get(&key) {
                // 先处理修饰符变体 (长模式), 再处理裸 %N, 避免 %0 子串
                // 匹配破坏 %q0 / %b0 等占位符.
                // 寄存器修饰符: %bN %wN %hN %kN %qN
                for m in ['b', 'w', 'h', 'k', 'q'] {
                    let mkey = format!("%{}{}", m, idx);
                    if resolved.contains(&mkey) {
                        if let Some(sub) = Self::asm_reg_modifier_subst(val, m) {
                            resolved = resolved.replace(&mkey, &sub);
                        }
                    }
                }
                // %cN: 常量形式 (去 $ 前缀, 同 %P 但对立即数)
                let ckey = format!("%c{}", idx);
                if resolved.contains(&ckey) {
                    resolved = resolved.replace(&ckey, &Self::asm_raw_operand(val));
                }
                // %aN: 地址形式 (寄存器同名, 立即数/内存去前缀)
                let akey = format!("%a{}", idx);
                if resolved.contains(&akey) {
                    if let Some(sub) = Self::asm_reg_modifier_subst(val, 'a') {
                        resolved = resolved.replace(&akey, &sub);
                    } else {
                        resolved = resolved.replace(&akey, &Self::asm_raw_operand(val));
                    }
                }
                // %PN: 原始符号名 (去 % / $ 前缀)
                let pkey = format!("%P{}", idx);
                if resolved.contains(&pkey) {
                    resolved = resolved.replace(&pkey, &Self::asm_raw_operand(val));
                }
                // 最后替换裸 %N
                resolved = resolved.replace(&key, &Self::asm_format_subst_safe(val));
            } else if let Some(val) = Self::eval_asm_operand(op) {
                // 修饰符形式: 纯数值 const 求值结果, %a/%P/%c 直接输出数字
                for m in ['a', 'P', 'c'] {
                    let mkey = format!("%{}{}", m, idx);
                    if resolved.contains(&mkey) {
                        resolved = resolved.replace(&mkey, &val);
                    }
                }
                resolved = resolved.replace(&key, &val);
            }
        }
        for (i, name) in output_names.iter().chain(input_names.iter()).enumerate() {
            if let Some(n) = name {
                let key = format!("%[{n}]");
                let num_key = format!("%{}", i);
                if let Some(reg) = op_map.get(&num_key).or_else(|| op_map.get(&key)) {
                    resolved = resolved.replace(&key, &Self::asm_format_subst_safe(reg));
                    // 修饰符变体: %b[name] %w[name] %h[name] %k[name] %q[name]
                    for m in ['b', 'w', 'h', 'k', 'q'] {
                        let mkey = format!("%{}[{n}]", m);
                        if resolved.contains(&mkey) {
                            if let Some(sub) = Self::asm_reg_modifier_subst(reg, m) {
                                resolved = resolved.replace(&mkey, &sub);
                            }
                        }
                    }
                    // %c[name]: 常量形式 (去 $ 前缀)
                    let ckey = format!("%c[{n}]");
                    if resolved.contains(&ckey) {
                        resolved = resolved.replace(&ckey, &Self::asm_raw_operand(reg));
                    }
                    // %a[name]: 地址形式 (寄存器同名, 立即数/内存去前缀)
                    let akey = format!("%a[{n}]");
                    if resolved.contains(&akey) {
                        if let Some(sub) = Self::asm_reg_modifier_subst(reg, 'a') {
                            resolved = resolved.replace(&akey, &sub);
                        } else {
                            resolved = resolved.replace(&akey, &Self::asm_raw_operand(reg));
                        }
                    }
                    // %P[name]: 原始符号名
                    let pkey = format!("%P[{n}]");
                    if resolved.contains(&pkey) {
                        resolved = resolved.replace(&pkey, &Self::asm_raw_operand(reg));
                    }
                }
            }
        }
        for (key, reg) in op_map {
            if key.starts_with("%[") {
                resolved = resolved.replace(key, &Self::asm_format_subst_safe(reg));
            }
        }
        // %= : GCC 唯一数字标签替换 (每条 asm 语句一个唯一数字)
        resolved = Self::substitute_asm_unique_number(&resolved, self.next_label as u64);
        Self::unescape_asm_template(&resolved)
    }
    pub(crate) fn split_asm_args(s: &str) -> Vec<&str> {
        let mut args = Vec::new();
        let mut start = 0;
        let mut in_quote = false;
        for (i, c) in s.char_indices() {
            match c {
                '"' => in_quote = !in_quote,
                ',' if !in_quote => {
                    args.push(&s[start..i]);
                    start = i + 1;
                },
                _ => {},
            }
        }
        if start < s.len() {
            args.push(&s[start..]);
        }
        args
    }
    fn substitute_asm_unique_number(s: &str, unique_id: u64) -> String {
        s.replace("%=", &unique_id.to_string())
    }
    pub(crate) fn unescape_asm_template(s: &str) -> String {
        s.replace("%%", "%")
    }
}
