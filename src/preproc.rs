// src/preproc.rs - C 预处理器
// 第一性原理: 预处理 = 文本变换 + 宏展开。在词法分析之前运行。
// suckless 准则: 行级处理 #include/#define/#if, 然后 token 级宏展开。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::fs;
use crate::diagnostics::{Diagnostic, Span};

pub struct Preprocessor {
    macros: HashMap<String, MacroDef>,
    include_paths: Vec<PathBuf>,
    /// 已处理的文件 (防递归 include)
    seen: HashSet<String>,
    diags: Vec<Diagnostic>,
    /// 条件编译栈: true = 当前分支激活, false = 跳过
    cond_stack: Vec<CondState>,
    /// 输出缓冲
    out: String,
    /// 当前文件名 (用于诊断 + __FILE__)
    file: String,
    /// 搜索路径基准目录
    base_dir: PathBuf,
    /// 当前行号 (用于 __LINE__)
    cur_line: u32,
    /// 编译日期/时间 (用于 __DATE__ / __TIME__)
    compile_date: String,
    compile_time: String,
}

struct CondState {
    /// 是否已有一个分支为 true
    any_true: bool,
    /// 当前分支是否激活
    active: bool,
    /// 是否见过 else
    saw_else: bool,
}

#[derive(Clone)]
struct MacroDef {
    params: Option<Vec<String>>,  // None = object-like, Some = function-like
    body: String,
    /// 可变参数: true 表示末尾有 ...
    variadic: bool,
}

pub fn preprocess(src: &[u8], file: &str, include_paths: &[PathBuf],
                  defines: &[(String, Option<String>)]) -> (String, Vec<Diagnostic>) {
    let text = String::from_utf8_lossy(src);
    let base = Path::new(file).parent().unwrap_or(Path::new(".")).to_path_buf();
    // 编译日期/时间
    let (compile_date, compile_time) = Preprocessor::compile_datetime();
    let mut pp = Preprocessor {
        macros: HashMap::new(),
        include_paths: include_paths.to_vec(),
        seen: HashSet::new(),
        diags: vec![],
        cond_stack: vec![],
        out: String::with_capacity(text.len()),
        file: file.to_string(),
        base_dir: base,
        cur_line: 0,
        compile_date,
        compile_time,
    };
    // 预定义宏
    pp.define_builtin("__DCC__", "1");
    pp.define_builtin("__x86_64__", "1");
    pp.define_builtin("__linux__", "1");
    pp.define_builtin("__LP64__", "1");
    pp.define_builtin("__STDC__", "1");
    pp.define_builtin("__STDC_VERSION__", "202311L");
    pp.define_builtin("__STDC_HOSTED__", "1");
    pp.define_builtin("NULL", "((void*)0)");
    // 用户 -D 定义
    for (name, val) in defines {
        let v = val.as_deref().unwrap_or("1");
        pp.define_builtin(name, v);
    }

    pp.process_text(&text, file);
    (pp.out, pp.diags)
}

impl Preprocessor {
    fn is_active(&self) -> bool {
        self.cond_stack.iter().all(|c| c.active)
    }

    fn define_builtin(&mut self, name: &str, val: &str) {
        self.macros.insert(name.to_string(), MacroDef {
            params: None, body: val.to_string(), variadic: false,
        });
    }

    fn compile_datetime() -> (String, String) {
        // 简化: 使用固定格式, 实际项目可用 chrono
        let date = "Jul  9 2026";
        let time = "00:00:00";
        (date.to_string(), time.to_string())
    }

    fn process_text(&mut self, text: &str, file: &str) {
        let saved_file = self.file.clone();
        self.file = file.to_string();

        // 行续接: 合并以 \ 结尾的行
        let joined = Self::join_continued_lines(text);

        for (line_no, line) in joined.lines().enumerate() {
            let line_no = (line_no + 1) as u32;
            self.cur_line = line_no;
            let trimmed = line.trim();

            if trimmed.starts_with('#') {
                self.process_directive(trimmed, line_no);
                // 输出空行保持行号对齐
                self.out.push('\n');
            } else if self.is_active() {
                // 宏展开
                let expanded = self.expand_macros(line);
                self.out.push_str(&expanded);
                self.out.push('\n');
            } else {
                self.out.push('\n');
            }
        }

        self.file = saved_file;
    }

    /// 行续接: 将行尾 \ 与下一行合并
    fn join_continued_lines(text: &str) -> String {
        let mut result = String::with_capacity(text.len());
        let mut continued = String::new();
        for line in text.lines() {
            if line.ends_with('\\') {
                continued.push_str(&line[..line.len()-1]);
                continued.push(' '); // \ 替换为空格
            } else {
                if !continued.is_empty() {
                    continued.push_str(line);
                    result.push_str(&continued);
                    result.push('\n');
                    continued.clear();
                } else {
                    result.push_str(line);
                    result.push('\n');
                }
            }
        }
        if !continued.is_empty() { result.push_str(&continued); result.push('\n'); }
        result
    }

    fn process_directive(&mut self, line: &str, line_no: u32) {
        let after_hash = line[1..].trim_start();
        // 处理行续接 (\ at end)
        let directive = after_hash;

        if directive.starts_with("include") {
            if self.is_active() {
                self.do_include(&directive[7..].trim(), line_no);
            }
        } else if directive.starts_with("define") {
            if self.is_active() {
                self.do_define(&directive[6..].trim());
            }
        } else if directive.starts_with("undef") {
            if self.is_active() {
                let name = directive[5..].trim();
                self.macros.remove(name);
            }
        } else if directive.starts_with("ifdef") {
            let name = directive[5..].trim();
            let active = self.is_active() && self.macros.contains_key(name);
            self.cond_stack.push(CondState {
                any_true: active, active, saw_else: false,
            });
        } else if directive.starts_with("ifndef") {
            let name = directive[6..].trim();
            let active = self.is_active() && !self.macros.contains_key(name);
            self.cond_stack.push(CondState {
                any_true: active, active, saw_else: false,
            });
        } else if directive.starts_with("if ") || directive.starts_with("if\t") {
            let expr = directive[2..].trim();
            let val = if self.is_active() { self.eval_const_expr(expr) } else { false };
            self.cond_stack.push(CondState {
                any_true: val, active: val, saw_else: false,
            });
        } else if directive.starts_with("elif ") || directive.starts_with("elif\t") {
            let len = self.cond_stack.len();
            if len > 0 {
                let parent_active = if len <= 1 { true }
                    else { self.cond_stack[..len-1].iter().all(|c| c.active) };
                let saw_else = self.cond_stack[len - 1].saw_else;
                let any_true = self.cond_stack[len - 1].any_true;
                if saw_else {
                    self.diags.push(Diagnostic::err("E0200", "#elif after #else",
                        Span::point(&self.file, line_no, 1)));
                    return;
                }
                let state = &mut self.cond_stack[len - 1];
                if any_true {
                    state.active = false;
                } else {
                    let expr = directive[4..].trim().to_string();
                    // eval 必须先于 cond_stack 的 mutable borrow 完成
                    drop(state);
                    let val = parent_active && self.eval_const_expr(&expr);
                    let state = &mut self.cond_stack[len - 1];
                    if val { state.any_true = true; }
                    state.active = val;
                }
            }
        } else if directive.starts_with("else") {
            let len = self.cond_stack.len();
            if len > 0 {
                let parent_active = if len <= 1 { true }
                    else { self.cond_stack[..len-1].iter().all(|c| c.active) };
                let state = &mut self.cond_stack[len - 1];
                if state.saw_else {
                    self.diags.push(Diagnostic::err("E0201", "duplicate #else",
                        Span::point(&self.file, line_no, 1)));
                    return;
                }
                state.saw_else = true;
                state.active = parent_active && !state.any_true;
                if state.active { state.any_true = true; }
            }
        } else if directive.starts_with("endif") {
            if self.cond_stack.pop().is_none() {
                self.diags.push(Diagnostic::err("E0202", "#endif without #if",
                    Span::point(&self.file, line_no, 1)));
            }
        } else if directive.starts_with("pragma") {
            // 忽略 #pragma
        } else if directive.starts_with("error") {
            if self.is_active() {
                let msg = directive[5..].trim();
                self.diags.push(Diagnostic::err("E0203", format!("#error {}", msg),
                    Span::point(&self.file, line_no, 1)));
            }
        } else if directive.starts_with("warning") {
            if self.is_active() {
                let msg = directive[7..].trim();
                self.diags.push(Diagnostic::warn("W0200", format!("#warning {}", msg),
                    Span::point(&self.file, line_no, 1)));
            }
        }
        // 忽略未知指令 (line markers 等)
    }

    fn do_define(&mut self, rest: &str) {
        // #define NAME body  或  #define NAME(args, ...) body
        let rest = rest.trim_start();
        // 提取宏名
        let name_end = rest.find(|c: char| !c.is_alphanumeric() && c != '_')
            .unwrap_or(rest.len());
        if name_end == 0 { return; }
        let name = &rest[..name_end];
        let after_name = &rest[name_end..];

        if after_name.starts_with('(') {
            // function-like macro
            let paren_end = after_name.find(')').unwrap_or(after_name.len());
            let params_str = &after_name[1..paren_end];
            let mut params: Vec<String> = vec![];
            let mut variadic = false;
            for s in params_str.split(',') {
                let s = s.trim();
                if s == "..." { variadic = true; }
                else if s.starts_with("...") {
                    // ...name 形式
                    variadic = true;
                    let va_name = &s[3..];
                    if !va_name.is_empty() { params.push(va_name.to_string()); }
                } else if !s.is_empty() {
                    params.push(s.to_string());
                }
            }
            // 可变参数宏自动添加 __VA_ARGS__ 参数名
            if variadic && !params.iter().any(|p| p == "__VA_ARGS__") {
                params.push("__VA_ARGS__".to_string());
            }
            let body = after_name[paren_end+1..].trim().to_string();
            self.macros.insert(name.to_string(), MacroDef {
                params: Some(params), body, variadic,
            });
        } else {
            // object-like macro
            let body = after_name.trim().to_string();
            self.macros.insert(name.to_string(), MacroDef {
                params: None, body, variadic: false,
            });
        }
    }

    fn do_include(&mut self, rest: &str, line_no: u32) {
        let rest = rest.trim();
        let (path, is_system) = if rest.starts_with('"') {
            let end = rest[1..].find('"').map(|i| i + 1).unwrap_or(rest.len());
            (&rest[1..end], false)
        } else if rest.starts_with('<') {
            let end = rest[1..].find('>').map(|i| i + 1).unwrap_or(rest.len());
            (&rest[1..end], true)
        } else {
            self.diags.push(Diagnostic::err("E0204", "expected \"file\" or <file>",
                Span::point(&self.file, line_no, 1)));
            return;
        };

        // 搜索文件
        let found = self.find_include(path, is_system);
        match found {
            Some(full_path) => {
                let canonical = full_path.canonicalize()
                    .unwrap_or(full_path.clone());
                let key = canonical.to_string_lossy().to_string();
                if self.seen.contains(&key) { return; } // include guard
                self.seen.insert(key);
                match fs::read_to_string(&full_path) {
                    Ok(text) => {
                        // 输出 #line 标记 (保持行号对齐)
                        self.out.push_str(&format!("/* #include \"{}\" */\n", path));
                        let saved_base = self.base_dir.clone();
                        self.base_dir = full_path.parent()
                            .unwrap_or(Path::new(".")).to_path_buf();
                        self.process_text(&text, &full_path.to_string_lossy());
                        self.base_dir = saved_base;
                    }
                    Err(e) => {
                        self.diags.push(Diagnostic::err("E0205",
                            format!("cannot read {}: {}", path, e),
                            Span::point(&self.file, line_no, 1)));
                    }
                }
            }
            None => {
                self.diags.push(Diagnostic::err("E0206",
                    format!("include file not found: {}", path),
                    Span::point(&self.file, line_no, 1)));
            }
        }
    }

    fn find_include(&self, name: &str, is_system: bool) -> Option<PathBuf> {
        // 1. 相对当前文件
        if !is_system {
            let p = self.base_dir.join(name);
            if p.exists() { return Some(p); }
        }
        // 2. include paths
        for dir in &self.include_paths {
            let p = dir.join(name);
            if p.exists() { return Some(p); }
        }
        // 3. 标准路径
        for dir in &["/usr/include", "/usr/local/include"] {
            let p = Path::new(dir).join(name);
            if p.exists() { return Some(p); }
        }
        None
    }

    // ===== 宏展开 =====
    fn expand_macros(&self, line: &str) -> String {
        if self.macros.is_empty() { return line.to_string(); }
        let mut result = line.to_string();
        // 最多展开 64 层, 防止递归
        for _ in 0..64 {
            let prev = result.clone();
            result = self.expand_once(&result);
            if result == prev { break; }
        }
        result
    }

    fn expand_once(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let chars: Vec<char> = text.chars().collect();
        let len = chars.len();
        let mut i = 0;

        while i < len {
            // 跳过字符串字面量
            if chars[i] == '"' {
                out.push('"');
                i += 1;
                while i < len && chars[i] != '"' {
                    if chars[i] == '\\' && i + 1 < len { out.push(chars[i]); i += 1; }
                    out.push(chars[i]); i += 1;
                }
                if i < len { out.push('"'); i += 1; }
                continue;
            }
            // 跳过字符字面量
            if chars[i] == '\'' {
                out.push('\'');
                i += 1;
                while i < len && chars[i] != '\'' {
                    if chars[i] == '\\' && i + 1 < len { out.push(chars[i]); i += 1; }
                    out.push(chars[i]); i += 1;
                }
                if i < len { out.push('\''); i += 1; }
                continue;
            }
            // 标识符
            if chars[i].is_alphabetic() || chars[i] == '_' {
                let start = i;
                while i < len && (chars[i].is_alphanumeric() || chars[i] == '_') { i += 1; }
                let ident: String = chars[start..i].iter().collect();

                // 特殊内建宏: __FILE__ / __LINE__ / __DATE__ / __TIME__
                if ident == "__FILE__" {
                    out.push_str(&format!("\"{}\"", self.file));
                    continue;
                }
                if ident == "__LINE__" {
                    out.push_str(&self.cur_line.to_string());
                    continue;
                }
                if ident == "__DATE__" {
                    out.push_str(&format!("\"{}\"", self.compile_date));
                    continue;
                }
                if ident == "__TIME__" {
                    out.push_str(&format!("\"{}\"", self.compile_time));
                    continue;
                }

                if let Some(def) = self.macros.get(&ident) {
                    if let Some(ref params) = def.params {
                        // function-like: 需要后跟 (
                        let mut j = i;
                        while j < len && chars[j].is_whitespace() { j += 1; }
                        if j < len && chars[j] == '(' {
                            // 解析参数
                            j += 1; // skip (
                            let args = self.parse_macro_args(&chars, &mut j);
                            // 可变参数: 多余参数合并为 __VA_ARGS__
                            let mut final_args = args.clone();
                            if def.variadic && args.len() >= params.len().saturating_sub(1) {
                                // 固定参数 + 可变参数
                                let fixed = params.len() - 1; // 减去 __VA_ARGS__
                                if args.len() > fixed {
                                    let va_args: Vec<String> = args[fixed..].to_vec();
                                    let mut normal = args[..fixed].to_vec();
                                    normal.push(va_args.join(", "));
                                    final_args = normal;
                                }
                            }
                            if final_args.len() == params.len() {
                                let expanded = self.substitute_params(&def.body, params, &final_args);
                                out.push_str(&expanded);
                            } else if args.len() == params.len() {
                                let expanded = self.substitute_params(&def.body, params, &args);
                                out.push_str(&expanded);
                            } else {
                                out.push_str(&ident);
                            }
                            i = j;
                        } else {
                            out.push_str(&ident);
                        }
                    } else {
                        // object-like
                        out.push_str(&def.body);
                    }
                } else {
                    out.push_str(&ident);
                }
                continue;
            }
            out.push(chars[i]);
            i += 1;
        }
        out
    }

    fn parse_macro_args(&self, chars: &[char], pos: &mut usize) -> Vec<String> {
        let mut args = vec![];
        let mut current = String::new();
        let mut depth = 1;
        let mut closed = false;
        while *pos < chars.len() && depth > 0 {
            let c = chars[*pos];
            if c == '(' { depth += 1; current.push(c); }
            else if c == ')' {
                depth -= 1;
                if depth == 0 { closed = true; args.push(current.trim().to_string()); }
                else { current.push(c); }
            }
            else if c == ',' && depth == 1 {
                args.push(current.trim().to_string());
                current = String::new();
            }
            else { current.push(c); }
            *pos += 1;
        }
        if !closed && !current.trim().is_empty() { args.push(current.trim().to_string()); }
        args
    }

    fn substitute_params(&self, body: &str, params: &[String], args: &[String]) -> String {
        // 第一步: 处理 # (字符串化) — 必须在 ## 之前, 否则参数名被替换后 # 找不到模式
        let result = self.handle_stringify(body, params, args);
        // 第二步: 处理 ## (令牌拼接)
        let result = self.handle_paste(&result, params, args);
        // 第三步: 普通参数替换
        let mut result = result;
        for (i, param) in params.iter().enumerate() {
            if i >= args.len() { continue; }
            // 替换独立的参数名 (不拆分单词, 避免破坏 ## 结果)
            result = Self::replace_ident(&result, param, &args[i]);
        }
        result
    }

    /// ## 令牌拼接: 替换参数后直接拼接文本
    fn handle_paste(&self, body: &str, params: &[String], args: &[String]) -> String {
        // 先替换参数名, 然后处理 ##
        let mut result = body.to_string();
        // 替换参数名 (用于 ## 上下文)
        for (i, param) in params.iter().enumerate() {
            if i >= args.len() { continue; }
            result = Self::replace_ident(&result, param, &args[i]);
        }
        // 处理 ##: 拼接两侧 token
        while let Some(pos) = result.find("##") {
            let before = result[..pos].trim_end();
            let after = result[pos+2..].trim_start();
            // 取 before 最后一个 token
            let left_start = before.rfind(|c: char| !c.is_alphanumeric() && c != '_')
                .map(|p| p + 1).unwrap_or(0);
            let left = &before[left_start..];
            // 取 after 第一个 token
            let right_end = after.find(|c: char| !c.is_alphanumeric() && c != '_')
                .unwrap_or(after.len());
            let right = &after[..right_end];
            let pasted = format!("{}{}", left, right);
            result = format!("{} {} {}", &before[..left_start], pasted, &after[right_end..]);
        }
        result
    }

    /// # 字符串化: # param → "param_value"
    fn handle_stringify(&self, body: &str, params: &[String], args: &[String]) -> String {
        let mut result = body.to_string();
        for (i, param) in params.iter().enumerate() {
            if i >= args.len() { continue; }
            // 查找 # param (注意不是 ##)
            let pattern = format!("# {}", param);
            let pattern2 = format!("#{}", param);
            for pat in &[pattern, pattern2] {
                while let Some(pos) = result.find(pat.as_str()) {
                    // 确保不是 ##
                    if pos > 0 && result.as_bytes()[pos-1] == b'#' { break; }
                    let stringified = Self::stringify_arg(&args[i]);
                    let end = pos + pat.len();
                    result = format!("{}{}{}", &result[..pos], stringified, &result[end..]);
                }
            }
        }
        result
    }

    /// 将参数值转为字符串字面量 (转义 " 和 \)
    fn stringify_arg(arg: &str) -> String {
        let escaped = arg.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{}\"", escaped)
    }

    /// 替换文本中的独立标识符
    fn replace_ident(text: &str, from: &str, to: &str) -> String {
        let mut result = String::with_capacity(text.len());
        let chars: Vec<char> = text.chars().collect();
        let len = chars.len();
        let mut i = 0;
        while i < len {
            if chars[i].is_alphabetic() || chars[i] == '_' {
                let start = i;
                while i < len && (chars[i].is_alphanumeric() || chars[i] == '_') { i += 1; }
                let ident: String = chars[start..i].iter().collect();
                if ident == from { result.push_str(to); }
                else { result.push_str(&ident); }
            } else {
                result.push(chars[i]);
                i += 1;
            }
        }
        result
    }

    // ===== 常量表达式求值 (用于 #if) =====
    fn eval_const_expr(&self, expr: &str) -> bool {
        let expr = expr.trim();
        // 处理 defined(X) 和 defined X
        let expr = self.replace_defined(expr);
        // 替换已知宏
        let expr = self.expand_macros(&expr);
        // 简单求值: 非零 = true
        self.eval_int_expr(&expr) != 0
    }

    fn replace_defined(&self, expr: &str) -> String {
        let mut result = expr.to_string();
        // defined(NAME) or defined NAME
        while let Some(pos) = result.find("defined") {
            let after = &result[pos + 7..];
            let after = after.trim_start();
            if after.starts_with('(') {
                let end = after.find(')').unwrap_or(0);
                let name = after[1..end].trim();
                let val = if self.macros.contains_key(name) { "1" } else { "0" };
                result = format!("{}{}{}", &result[..pos], val, &after[end+1..]);
            } else {
                let name_end = after.find(|c: char| !c.is_alphanumeric() && c != '_')
                    .unwrap_or(after.len());
                let name = &after[..name_end];
                let val = if self.macros.contains_key(name) { "1" } else { "0" };
                result = format!("{}{}{}", &result[..pos], val, &after[name_end..]);
            }
        }
        result
    }

    fn eval_int_expr(&self, expr: &str) -> i64 {
        let expr = expr.trim();
        // 极简: 只处理数字, !, &&, ||, ==, !=, <, >, <=, >=, +, -, *, /
        // 替换未知标识符为 0
        let expr = self.replace_unknown_idents(&expr);
        // 用简单递归下降求值
        let mut pos = 0;
        let bytes = expr.as_bytes();
        Self::parse_or_expr(bytes, &mut pos)
    }

    fn replace_unknown_idents(&self, expr: &str) -> String {
        let mut result = String::new();
        let chars: Vec<char> = expr.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i].is_alphabetic() || chars[i] == '_' {
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') { i += 1; }
                let ident: String = chars[start..i].iter().collect();
                match ident.as_str() {
                    "defined" | "true" => result.push_str("1"),
                    "false" => result.push_str("0"),
                    _ => {
                        // 检查是否是数字后缀 (如 64L)
                        result.push_str("0");
                    }
                }
            } else {
                result.push(chars[i]);
                i += 1;
            }
        }
        result
    }

    fn parse_or_expr(bytes: &[u8], pos: &mut usize) -> i64 {
        let mut val = Self::parse_and_expr(bytes, pos);
        Self::skip_ws(bytes, pos);
        while *pos + 1 < bytes.len() && bytes[*pos] == b'|' && bytes[*pos+1] == b'|' {
            *pos += 2;
            let rhs = Self::parse_and_expr(bytes, pos);
            val = if val != 0 || rhs != 0 { 1 } else { 0 };
            Self::skip_ws(bytes, pos);
        }
        val
    }

    fn parse_and_expr(bytes: &[u8], pos: &mut usize) -> i64 {
        let mut val = Self::parse_cmp_expr(bytes, pos);
        Self::skip_ws(bytes, pos);
        while *pos + 1 < bytes.len() && bytes[*pos] == b'&' && bytes[*pos+1] == b'&' {
            *pos += 2;
            let rhs = Self::parse_cmp_expr(bytes, pos);
            val = if val != 0 && rhs != 0 { 1 } else { 0 };
            Self::skip_ws(bytes, pos);
        }
        val
    }

    fn parse_cmp_expr(bytes: &[u8], pos: &mut usize) -> i64 {
        let lhs = Self::parse_add_expr(bytes, pos);
        Self::skip_ws(bytes, pos);
        if *pos < bytes.len() {
            let (op, len) = match bytes[*pos..] {
                [b'=', b'=', ..] => ("eq", 2),
                [b'!', b'=', ..] => ("ne", 2),
                [b'<', b'=', ..] => ("le", 2),
                [b'>', b'=', ..] => ("ge", 2),
                [b'<', ..] => ("lt", 1),
                [b'>', ..] => ("gt", 1),
                _ => ("none", 0),
            };
            if op != "none" {
                *pos += len;
                let rhs = Self::parse_add_expr(bytes, pos);
                return match op {
                    "eq" => (lhs == rhs) as i64,
                    "ne" => (lhs != rhs) as i64,
                    "lt" => (lhs < rhs) as i64,
                    "gt" => (lhs > rhs) as i64,
                    "le" => (lhs <= rhs) as i64,
                    "ge" => (lhs >= rhs) as i64,
                    _ => unreachable!(),
                };
            }
        }
        lhs
    }

    fn parse_add_expr(bytes: &[u8], pos: &mut usize) -> i64 {
        let mut val = Self::parse_mul_expr(bytes, pos);
        loop {
            Self::skip_ws(bytes, pos);
            if *pos < bytes.len() {
                if bytes[*pos] == b'+' { *pos += 1; val += Self::parse_mul_expr(bytes, pos); }
                else if bytes[*pos] == b'-' { *pos += 1; val -= Self::parse_mul_expr(bytes, pos); }
                else { break; }
            } else { break; }
        }
        val
    }

    fn parse_mul_expr(bytes: &[u8], pos: &mut usize) -> i64 {
        let mut val = Self::parse_unary(bytes, pos);
        loop {
            Self::skip_ws(bytes, pos);
            if *pos < bytes.len() {
                if bytes[*pos] == b'*' { *pos += 1; val *= Self::parse_unary(bytes, pos); }
                else if bytes[*pos] == b'/' {
                    *pos += 1;
                    let d = Self::parse_unary(bytes, pos);
                    val = if d != 0 { val / d } else { 0 };
                }
                else if bytes[*pos] == b'%' {
                    *pos += 1;
                    let d = Self::parse_unary(bytes, pos);
                    val = if d != 0 { val % d } else { 0 };
                }
                else { break; }
            } else { break; }
        }
        val
    }

    fn parse_unary(bytes: &[u8], pos: &mut usize) -> i64 {
        Self::skip_ws(bytes, pos);
        if *pos < bytes.len() {
            if bytes[*pos] == b'!' { *pos += 1; return if Self::parse_unary(bytes, pos) == 0 { 1 } else { 0 }; }
            if bytes[*pos] == b'-' { *pos += 1; return -Self::parse_unary(bytes, pos); }
            if bytes[*pos] == b'+' { *pos += 1; return Self::parse_unary(bytes, pos); }
            if bytes[*pos] == b'~' { *pos += 1; return !Self::parse_unary(bytes, pos); }
            if bytes[*pos] == b'(' {
                *pos += 1;
                let val = Self::parse_or_expr(bytes, pos);
                Self::skip_ws(bytes, pos);
                if *pos < bytes.len() && bytes[*pos] == b')' { *pos += 1; }
                return val;
            }
        }
        Self::parse_number(bytes, pos)
    }

    fn parse_number(bytes: &[u8], pos: &mut usize) -> i64 {
        Self::skip_ws(bytes, pos);
        let start = *pos;
        if *pos < bytes.len() && bytes[*pos].is_ascii_digit() {
            while *pos < bytes.len() && (bytes[*pos].is_ascii_alphanumeric() || bytes[*pos] == b'\'') {
                *pos += 1;
            }
            let s = std::str::from_utf8(&bytes[start..*pos]).unwrap_or("0");
            // 去掉后缀 (L, U, LL 等)
            let s = s.trim_end_matches(|c: char| c == 'L' || c == 'l' || c == 'U' || c == 'u');
            if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                i64::from_str_radix(hex, 16).unwrap_or(0)
            } else if let Some(bin) = s.strip_prefix("0b").or_else(|| s.strip_prefix("0B")) {
                i64::from_str_radix(bin, 2).unwrap_or(0)
            } else if s.starts_with('0') && s.len() > 1 && s.chars().all(|c| c.is_ascii_digit()) {
                i64::from_str_radix(s, 8).unwrap_or(0)
            } else {
                s.parse::<i64>().unwrap_or(0)
            }
        } else { 0 }
    }

    fn skip_ws(bytes: &[u8], pos: &mut usize) {
        while *pos < bytes.len() && (bytes[*pos] == b' ' || bytes[*pos] == b'\t') {
            *pos += 1;
        }
    }
}
