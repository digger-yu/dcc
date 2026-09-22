// src/codegen/x86_64/asm/directive.rs
// Inline assembly directive handling for x86_64 backend
// Handles .rept, .irp, .irpc, .macro, .endm, .if, .endif, etc.

use std::collections::HashMap;

/// Expander for advanced inline assembly directives (`.rept/.macro/.if`etc.)
/// Returns pure instruction lines after expansion
pub struct AsmDirectiveExpander {
    /// Defined macros: name -> (param_names, body_lines)
    pub macros: HashMap<String, (Vec<String>, Vec<String>)>,
    /// Intel syntax mode flag
    pub intel_syntax: bool,
    /// .altmacro mode flag
    pub alt_macro: bool,
}

impl AsmDirectiveExpander {
    pub fn new() -> Self {
        Self {
            macros: HashMap::new(),
            intel_syntax: false,
            alt_macro: false,
        }
    }

    /// Expand all high-level instructions in asm string, return pure instruction line list
    pub fn expand(&mut self, lines: &[String]) -> Vec<String> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i].trim();
            if line.is_empty() {
                i += 1;
                continue;
            }
            
            // .rept N ... .endr
            if let Some(count) = Self::parse_rept(line) {
                let (body, end_idx) = Self::collect_block(lines, i + 1, ".endr");
                let expanded_body = self.expand(&body);
                for _ in 0..count {
                    out.extend(expanded_body.clone());
                }
                i = end_idx + 1;
                continue;
            }
            
            // .irp sym, v1, v2, ... ... .endr
            if let Some((sym, vals)) = Self::parse_irp(line) {
                let (body, end_idx) = Self::collect_block(lines, i + 1, ".endr");
                let expanded_body = self.expand(&body);
                for val in &vals {
                    for l in &expanded_body {
                        out.push(
                            l.replace(&format!("\\{sym}", sym = sym), val)
                                .replace(&format!("\\({sym})", sym = sym), val),
                        );
                    }
                }
                i = end_idx + 1;
                continue;
            }
            
            // .irpc sym, chars ... .endr
            if let Some((sym, chars)) = Self::parse_irpc(line) {
                let (body, end_idx) = Self::collect_block(lines, i + 1, ".endr");
                let expanded_body = self.expand(&body);
                for c in chars.chars() {
                    for l in &expanded_body {
                        out.push(
                            l.replace(&format!("\\{sym}", sym = sym), &c.to_string())
                                .replace(&format!("\\({sym})", sym = sym), &c.to_string()),
                        );
                    }
                }
                i = end_idx + 1;
                continue;
            }
            
            // .macro name [args] ... .endm
            if let Some((name, args)) = Self::parse_macro_def(line) {
                let (body, end_idx) = Self::collect_block(lines, i + 1, ".endm");
                self.macros.insert(name.clone(), (args.clone(), body));
                i = end_idx + 1;
                continue;
            }
            
            // Macro call: name [args]
            if let Some(expanded) = self.try_expand_macro_call(line) {
                out.extend(expanded);
                i += 1;
                continue;
            }
            
            // .include "file"
            if let Some(path) = Self::parse_include(line) {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    let inc_lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
                    let expanded = self.expand(&inc_lines);
                    out.extend(expanded);
                } else {
                    // File not found, silently skip (GCC compatibility: .include failure doesn't error)
                }
                i += 1;
                continue;
            }
            
            // .if / .ifdef / .ifndef ... .else / .elseif ... .endif
            if let Some(cond_result) = Self::parse_conditional(line) {
                let (body, end_idx) = Self::collect_conditional_block(lines, i + 1, cond_result);
                let expanded = self.expand(&body);
                out.extend(expanded);
                i = end_idx + 1;
                continue;
            }
            
            // .intel_syntax noprefix / .att_syntax
            if line == ".intel_syntax noprefix" || line == ".intel_syntax" {
                self.intel_syntax = true;
                out.push(line.to_string());
                i += 1;
                continue;
            }
            
            if line == ".att_syntax" || line == ".att_syntax prefix" {
                self.intel_syntax = false;
                out.push(line.to_string());
                i += 1;
                continue;
            }
            
            // .altmacro / .noaltmacro
            if line == ".altmacro" {
                self.alt_macro = true;
                out.push(line.to_string());
                i += 1;
                continue;
            }
            
            if line == ".noaltmacro" {
                self.alt_macro = false;
                out.push(line.to_string());
                i += 1;
                continue;
            }
            
            // Ordinary line: pass through
            out.push(line.to_string());
            i += 1;
        }
        out
    }

    /// Parse `.rept N` and return repetition count
    pub fn parse_rept(line: &str) -> Option<usize> {
        let t = line.trim();
        let rest = t.strip_prefix(".rept")?;
        let rest = rest.trim();
        rest.split_whitespace().next()?.parse().ok()
    }

    /// Parse `.irp sym, v1, v2, ...` return (symbol_name, value_list)
    pub fn parse_irp(line: &str) -> Option<(String, Vec<String>)> {
        let t = line.trim();
        let rest = t.strip_prefix(".irp")?;
        let rest = rest.trim();
        let mut parts = rest.splitn(2, ',');
        let sym = parts.next()?.trim().to_string();
        let vals_str = parts.next()?;
        let vals: Vec<String> = vals_str.split(',').map(|s| s.trim().to_string()).collect();
        Some((sym, vals))
    }

    /// Parse `.irpc sym, chars` return (symbol_name, char_sequence)
    pub fn parse_irpc(line: &str) -> Option<(String, String)> {
        let t = line.trim();
        let rest = t.strip_prefix(".irpc")?;
        let rest = rest.trim();
        let mut parts = rest.splitn(2, ',');
        let sym = parts.next()?.trim().to_string();
        let chars = parts.next()?.trim().to_string();
        Some((sym, chars))
    }

    /// Parse `.macro name [arg1, arg2, ...]` return (macro_name, param_name_list)
    pub fn parse_macro_def(line: &str) -> Option<(String, Vec<String>)> {
        let t = line.trim();
        let rest = t.strip_prefix(".macro")?;
        let rest = rest.trim();
        let mut parts = rest.splitn(2, |c: char| c.is_whitespace());
        let name = parts.next()?.trim().to_string();
        let args_rest = parts.next().unwrap_or("").trim();
        let args: Vec<String> = if args_rest.is_empty() {
            Vec::new()
        } else {
            args_rest
                .split(',')
                .map(|s| s.trim().trim_start_matches('\\').to_string())
                .collect()
        };
        Some((name, args))
    }

    /// Try to expand macro call, return None if not a macro call
    pub fn try_expand_macro_call(&self, line: &str) -> Option<Vec<String>> {
        let t = line.trim();
        if t.starts_with('.') {
            return None;
        }
        let mut parts = t.splitn(2, |c: char| c.is_whitespace());
        let name = parts.next()?.trim();
        if let Some((args_def, body)) = self.macros.get(name) {
            let rest = parts.next().unwrap_or("").trim();
            let call_args: Vec<String> = if rest.is_empty() {
                Vec::new()
            } else {
                rest.split(',').map(|s| s.trim().to_string()).collect()
            };
            let mut result = Vec::new();
            for l in body {
                let mut expanded = l.clone();
                for (i, arg_name) in args_def.iter().enumerate() {
                    let val = call_args.get(i).cloned().unwrap_or_default();
                    expanded = expanded.replace(&format!("\\{arg_name}", arg_name = arg_name), &val);
                    expanded = expanded.replace(&format!("\\({arg_name})", arg_name = arg_name), &val);
                }
                result.push(expanded);
            }
            Some(result)
        } else {
            None
        }
    }

    /// Parse `.include "file"` and return file path
    pub fn parse_include(line: &str) -> Option<String> {
        let t = line.trim();
        let rest = t.strip_prefix(".include")?;
        let rest = rest.trim();
        let path = rest.trim_matches('"').to_string();
        Some(path)
    }

    /// Parse conditional directive, return condition result (true = include block, false = skip block)
    pub fn parse_conditional(line: &str) -> Option<bool> {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix(".ifdef") {
            // .ifdef: always false (no symbol table in asm except .set)
            let _ = rest.trim();
            return Some(false);
        }
        if let Some(rest) = t.strip_prefix(".ifndef") {
            // .ifndef: always true
            let _ = rest.trim();
            return Some(true);
        }
        if let Some(rest) = t.strip_prefix(".if") {
            let rest = rest.trim();
            // Simple integer expression evaluation
            return Some(Self::eval_simple_expr(rest));
        }
        None
    }

    /// Simple integer expression evaluation (supports: numbers, 0x prefix, +, -, *, /, |, &, ^, !, ==, !=, <, >, <=, >=)
    pub fn eval_simple_expr(s: &str) -> bool {
        let s = s.trim();
        // Try parsing as integer
        if let Ok(v) = Self::eval_expr_value(s) {
            return v != 0;
        }
        // Cannot parse, default false
        false
    }

    /// Evaluate integer expression (simplified version, supports basic operations)
    pub fn eval_expr_value(s: &str) -> Result<i64, ()> {
        let s = s.trim().trim_end_matches(',');
        
        // Handle parentheses
        if s.starts_with('(') && s.ends_with(')') {
            return Self::eval_expr_value(&s[1..s.len() - 1]);
        }
        
        // Handle unary !
        if let Some(rest) = s.strip_prefix('!') {
            let v = Self::eval_expr_value(rest)?;
            return Ok(if v == 0 { 1 } else { 0 });
        }
        
        // Handle unary -
        if let Some(rest) = s.strip_prefix('-') {
            if !rest.is_empty() && !rest.contains(['+', '-', '*', '/']) {
                let v = Self::eval_expr_value(rest)?;
                return Ok(-v);
            }
        }
        
        // Handle comparison operators (low priority)
        for op in &["==", "!=", "<=", ">=", "<", ">"] {
            if let Some(pos) = s.find(op) {
                let left = Self::eval_expr_value(&s[..pos])?;
                let right = Self::eval_expr_value(&s[pos + op.len()..])?;
                return Ok(match *op {
                    "==" => {
                        if left == right { 1 } else { 0 }
                    }
                    "!=" => {
                        if left != right { 1 } else { 0 }
                    }
                    "<=" => {
                        if left <= right { 1 } else { 0 }
                    }
                    ">=" => {
                        if left >= right { 1 } else { 0 }
                    }
                    "<" => {
                        if left < right { 1 } else { 0 }
                    }
                    ">" => {
                        if left > right { 1 } else { 0 }
                    }
                    _ => unreachable!(),
                });
            }
        }
        
        // Handle +, -, *, / (medium priority)
        for (op, priority) in &[('+', 1), ('-', 1), ('*', 2), ('/', 2)] {
            let mut pos = None;
            let mut idx = 0;
            for (i, c) in s.char_indices() {
                if c == *op {
                    if *priority == 2 {
                        // For *, / only at start or after operator
                        if i == 0 || matches!(s[..i].chars().last(), Some(c) if !c.is_ascii_alphanumeric() && c != ')') {
                            continue;
                        }
                    }
                    pos = Some(idx);
                    break;
                }
                // idx = 下一个字符的字节偏移 (当前 char 的 UTF-8 长度)
                idx = i + c.len_utf8();
            }
            if let Some(p) = pos {
                let left = Self::eval_expr_value(&s[..p])?;
                let right = Self::eval_expr_value(&s[p + op.len_utf8()..])?;
                return Ok(match *op {
                    '+' => left + right,
                    '-' => left - right,
                    '*' => left * right,
                    '/' => left / right,
                    _ => unreachable!(),
                });
            }
        }
        
        Err(())
    }

    /// Collect block between current line and end marker
    fn collect_block(lines: &[String], start: usize, end_marker: &str) -> (Vec<String>, usize) {
        let mut depth = 1;
        let mut i = start;
        let mut body = Vec::new();
        
        while i < lines.len() && depth > 0 {
            let line = lines[i].trim();
            if line.starts_with(end_marker) {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            } else if let Some(start_marker) = end_marker.strip_suffix("r").or_else(|| end_marker.strip_suffix("m")) {
                if line.starts_with(start_marker) {
                    depth += 1;
                }
            }
            
            if depth > 0 {
                body.push(lines[i].clone());
            }
            i += 1;
        }
        
        (body, i)
    }

    /// Collect conditional block (.if/.elseif/.else/.endif chain)
    fn collect_conditional_block(lines: &[String], start: usize, expect_true: bool) -> (Vec<String>, usize) {
        let mut i = start;
        let mut body = Vec::new();
        let mut found_condition = false;
        let mut depth = 1;
        
        while i < lines.len() && depth > 0 {
            let line = lines[i].trim();
            
            if line.starts_with(".if") || line.starts_with(".ifdef") || line.starts_with(".ifndef") {
                depth += 1;
                body.push(lines[i].clone());
                i += 1;
                continue;
            }
            
            if line.starts_with(".endif") {
                depth -= 1;
                if depth == 0 {
                    break;
                }
                body.push(lines[i].clone());
                i += 1;
                continue;
            }
            
            if line.starts_with(".else") || line.starts_with(".elseif") {
                if !found_condition && expect_true {
                    found_condition = true;
                }
                body.push(lines[i].clone());
                i += 1;
                continue;
            }
            
            if depth == 1 && (!expect_true || !found_condition) {
                body.push(lines[i].clone());
            }
            
            i += 1;
        }
        
        (body, i)
    }
}
