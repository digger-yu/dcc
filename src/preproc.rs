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
    /// 系统搜索路径 (-isystem, 在用户路径之后、标准路径之前)
    system_paths: Vec<PathBuf>,
    /// 已处理的文件 (防递归 include)
    seen: HashSet<String>,
    /// #pragma once 文件
    pragma_once: HashSet<String>,
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
    /// 诊断状态栈 (用于 #pragma GCC diagnostic push/pop)
    diag_stack: Vec<DiagState>,
    /// 当前诊断设置
    diag_state: DiagState,
}

/// 诊断状态: 控制特定警告的开关
#[derive(Clone, Default)]
struct DiagState {
    /// ignored 的警告列表
    ignored: HashSet<String>,
    /// 视为错误的警告列表
    as_error: HashSet<String>,
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
                  system_paths: &[PathBuf],
                  defines: &[(String, Option<String>)]) -> (String, Vec<Diagnostic>) {
    let text = String::from_utf8_lossy(src);
    let base = Path::new(file).parent().unwrap_or(Path::new(".")).to_path_buf();
    // 编译日期/时间
    let (compile_date, compile_time) = Preprocessor::compile_datetime();
    let mut pp = Preprocessor {
        macros: HashMap::new(),
        include_paths: include_paths.to_vec(),
        system_paths: system_paths.to_vec(),
        seen: HashSet::new(),
        pragma_once: HashSet::new(),
        diags: vec![],
        cond_stack: vec![],
        out: String::with_capacity(text.len()),
        file: file.to_string(),
        base_dir: base,
        cur_line: 0,
        compile_date,
        compile_time,
        diag_stack: vec![],
        diag_state: DiagState::default(),
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
    // GCC 兼容宏 (Linux 内核头文件大量使用)
    pp.define_builtin("__GNUC__", "13");
    pp.define_builtin("__GNUC_MINOR__", "2");
    pp.define_builtin("__GNUC_PATCHLEVEL__", "0");
    pp.define_builtin("__GNUC_STDC_INLINE__", "1");
    pp.define_builtin("__GXX_ABI_VERSION", "1017");
    pp.define_builtin("__CHAR_BIT__", "8");
    pp.define_builtin("__ORDER_LITTLE_ENDIAN__", "1234");
    pp.define_builtin("__ORDER_BIG_ENDIAN__", "4321");
    pp.define_builtin("__BYTE_ORDER__", "__ORDER_LITTLE_ENDIAN__");
    pp.define_builtin("__SIZEOF_POINTER__", "8");
    pp.define_builtin("__SIZEOF_LONG__", "8");
    pp.define_builtin("__SIZEOF_INT__", "4");
    pp.define_builtin("__SIZEOF_SHORT__", "2");
    pp.define_builtin("__SIZEOF_LONG_LONG__", "8");
    pp.define_builtin("__SIZEOF_SIZE_T__", "8");
    pp.define_builtin("__SIZEOF_PTRDIFF_T__", "8");
    pp.define_builtin("__SIZE_TYPE__", "long unsigned int");
    pp.define_builtin("__PTRDIFF_TYPE__", "long int");
    pp.define_builtin("__WCHAR_TYPE__", "int");
    pp.define_builtin("__WINT_TYPE__", "unsigned int");
    pp.define_builtin("__INTMAX_TYPE__", "long int");
    pp.define_builtin("__UINTMAX_TYPE__", "long unsigned int");
    pp.define_builtin("__INT64_TYPE__", "long int");
    pp.define_builtin("__UINT64_TYPE__", "long unsigned int");
    pp.define_builtin("__INT32_TYPE__", "int");
    pp.define_builtin("__UINT32_TYPE__", "unsigned int");
    pp.define_builtin("__INT16_TYPE__", "short int");
    pp.define_builtin("__UINT16_TYPE__", "short unsigned int");
    pp.define_builtin("__INT8_TYPE__", "signed char");
    pp.define_builtin("__UINT8_TYPE__", "unsigned char");
    pp.define_builtin("__ELF__", "1");
    pp.define_builtin("__unix__", "1");
    // 原子内存序常量 (GCC __atomic 内置函数用)
    pp.define_builtin("__ATOMIC_RELAXED", "0");
    pp.define_builtin("__ATOMIC_CONSUME", "1");
    pp.define_builtin("__ATOMIC_ACQUIRE", "2");
    pp.define_builtin("__ATOMIC_RELEASE", "3");
    pp.define_builtin("__ATOMIC_ACQ_REL", "4");
    pp.define_builtin("__ATOMIC_SEQ_CST", "5");
    pp.define_builtin("memory_order_relaxed", "0");
    pp.define_builtin("memory_order_consume", "1");
    pp.define_builtin("memory_order_acquire", "2");
    pp.define_builtin("memory_order_release", "3");
    pp.define_builtin("memory_order_acq_rel", "4");
    pp.define_builtin("memory_order_seq_cst", "5");
    // __attribute__: 不定义为宏, 让 token 透传到解析器处理
    // 解析器会识别 __attribute__ 并处理 vector_size 等属性
    // __has_attribute / __has_builtin / __has_feature (函数式宏, 返回 0)
    // 注意: __has_include 不在宏表中, 在 #if 求值时特殊处理 (需要文件系统访问)
    pp.macros.insert("__has_attribute".to_string(), MacroDef {
        params: Some(vec!["x".to_string()]), body: "0".to_string(), variadic: false,
    });
    pp.macros.insert("__has_builtin".to_string(), MacroDef {
        params: Some(vec!["x".to_string()]), body: "0".to_string(), variadic: false,
    });
    pp.macros.insert("__has_feature".to_string(), MacroDef {
        params: Some(vec!["x".to_string()]), body: "0".to_string(), variadic: false,
    });
    // likely/unlikely 分支提示
    pp.macros.insert("likely".to_string(), MacroDef {
        params: Some(vec!["x".to_string()]), body: "(x)".to_string(), variadic: false,
    });
    pp.macros.insert("unlikely".to_string(), MacroDef {
        params: Some(vec!["x".to_string()]), body: "(x)".to_string(), variadic: false,
    });
    // barrier
    pp.macros.insert("barrier".to_string(), MacroDef {
        params: Some(vec![]), body: "".to_string(), variadic: false,
    });
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
            let pragma_text = directive[6..].trim();
            if pragma_text == "once" {
                // 记录当前文件为 pragma once
                let canonical = Path::new(&self.file).canonicalize()
                    .unwrap_or_else(|_| Path::new(&self.file).to_path_buf());
                self.pragma_once.insert(canonical.to_string_lossy().to_string());
            } else if pragma_text.starts_with("GCC diagnostic") {
                self.handle_gcc_diagnostic(&pragma_text[14..].trim(), line_no);
            }
            // 其他 #pragma 忽略
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
                if self.pragma_once.contains(&key) { return; } // pragma once
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
        // 1. 相对当前文件 (仅 "file" 形式)
        if !is_system {
            let p = self.base_dir.join(name);
            if p.exists() { return Some(p); }
        }
        // 2. 用户 -I 路径
        for dir in &self.include_paths {
            let p = dir.join(name);
            if p.exists() { return Some(p); }
        }
        // 3. 系统 -isystem 路径
        for dir in &self.system_paths {
            let p = dir.join(name);
            if p.exists() { return Some(p); }
        }
        // 4. 标准系统路径 (含多架构支持)
        let system_dirs = [
            "/usr/local/include",
            "/usr/include",
            // Debian/Ubuntu 多架构路径
            "/usr/include/x86_64-linux-gnu",
            "/usr/include/aarch64-linux-gnu",
            // Linux 内核头文件
            "/usr/include/linux",
            "/usr/include/asm",
            "/usr/include/asm-generic",
        ];
        for dir in &system_dirs {
            let p = Path::new(dir).join(name);
            if p.exists() { return Some(p); }
        }
        None
    }

    /// 处理 #pragma GCC diagnostic
    fn handle_gcc_diagnostic(&mut self, text: &str, _line_no: u32) {
        let text = text.trim();
        if text.starts_with("push") {
            self.diag_stack.push(self.diag_state.clone());
        } else if text.starts_with("pop") {
            if let Some(prev) = self.diag_stack.pop() {
                self.diag_state = prev;
            }
        } else if text.starts_with("ignored") {
            let diag_name = text[7..].trim().trim_start_matches("-W");
            if !diag_name.is_empty() {
                self.diag_state.ignored.insert(diag_name.to_string());
                self.diag_state.as_error.remove(diag_name);
            }
        } else if text.starts_with("warning") {
            let diag_name = text[7..].trim().trim_start_matches("-W");
            if !diag_name.is_empty() {
                self.diag_state.ignored.remove(diag_name);
                self.diag_state.as_error.remove(diag_name);
            }
        } else if text.starts_with("error") {
            let diag_name = text[5..].trim().trim_start_matches("-W");
            if !diag_name.is_empty() {
                self.diag_state.ignored.remove(diag_name);
                self.diag_state.as_error.insert(diag_name.to_string());
            }
        }
    }

    // ===== 宏展开 =====
    /// 顶层入口: 对一行文本做完全宏展开
    fn expand_macros(&self, line: &str) -> String {
        if self.macros.is_empty() { return line.to_string(); }
        let disabled = HashSet::new();
        self.expand_with_disabled(line, &disabled)
    }

    /// 用禁用集展开文本
    fn expand_with_disabled(&self, text: &str, disabled: &HashSet<String>) -> String {
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

                // 特殊内建宏
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

                // 检查是否在禁用集中 (paint blue)
                if disabled.contains(&ident) {
                    out.push_str(&ident);
                    continue;
                }

                if let Some(def) = self.macros.get(&ident).cloned() {
                    if let Some(ref params) = def.params {
                        // function-like: 需要后跟 (
                        let mut j = i;
                        while j < len && chars[j].is_whitespace() { j += 1; }
                        if j < len && chars[j] == '(' {
                            j += 1;
                            let raw_args = self.parse_macro_args(&chars, &mut j);
                            // 确定哪些参数被 # 或 ## 操作 (这些参数不预展开)
                            let (stringify_params, paste_params) = self.find_special_params(&def.body, params);
                            // 参数预展开: 未被 # / ## 引用的参数先展开
                            let mut expanded_args: Vec<String> = Vec::with_capacity(raw_args.len());
                            for (idx, arg) in raw_args.iter().enumerate() {
                                if stringify_params.contains(&idx) || paste_params.contains(&idx) {
                                    expanded_args.push(arg.clone());
                                } else {
                                    let arg_disabled = HashSet::new();
                                    expanded_args.push(self.expand_with_disabled(arg, &arg_disabled));
                                }
                            }
                            // 可变参数处理
                            let mut final_args = expanded_args.clone();
                            if def.variadic && raw_args.len() >= params.len().saturating_sub(1) {
                                let fixed = params.len() - 1;
                                if raw_args.len() > fixed {
                                    let va_args: Vec<String> = raw_args[fixed..].to_vec();
                                    let mut normal = raw_args[..fixed].to_vec();
                                    // 可变参数部分也要展开
                                    let va_expanded: Vec<String> = va_args.iter().map(|a| {
                                        let d = HashSet::new();
                                        self.expand_with_disabled(a, &d)
                                    }).collect();
                                    normal.push(va_expanded.join(", "));
                                    final_args = normal;
                                }
                            }
                            if final_args.len() == params.len() {
                                let substituted = self.substitute_params(&def.body, params, &final_args);
                                // 创建新禁用集 = 原禁用集 + 当前宏名
                                let mut new_disabled = disabled.clone();
                                new_disabled.insert(ident.clone());
                                // 重新扫描替换结果
                                let rescanned = self.expand_with_disabled(&substituted, &new_disabled);
                                out.push_str(&rescanned);
                            } else if raw_args.len() == params.len() {
                                let substituted = self.substitute_params(&def.body, params, &raw_args);
                                let mut new_disabled = disabled.clone();
                                new_disabled.insert(ident.clone());
                                let rescanned = self.expand_with_disabled(&substituted, &new_disabled);
                                out.push_str(&rescanned);
                            } else {
                                out.push_str(&ident);
                            }
                            i = j;
                        } else {
                            out.push_str(&ident);
                        }
                    } else {
                        // object-like: 替换后重新扫描
                        let mut new_disabled = disabled.clone();
                        new_disabled.insert(ident.clone());
                        let rescanned = self.expand_with_disabled(&def.body, &new_disabled);
                        out.push_str(&rescanned);
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

    /// 找出哪些参数被 # 或 ## 引用 (这些参数不做预展开)
    fn find_special_params(&self, body: &str, params: &[String]) -> (Vec<usize>, Vec<usize>) {
        let mut stringify = vec![];
        let mut paste = vec![];
        // 检查 #: # param
        for (idx, param) in params.iter().enumerate() {
            let p1 = format!("# {}", param);
            let p2 = format!("#{}", param);
            if body.contains(&p1) || body.contains(&p2) {
                stringify.push(idx);
            }
        }
        // 检查 ##: param ## 或 ## param
        for (idx, param) in params.iter().enumerate() {
            // 简单检查: 参数名两侧是否有 ##
            let patterns = [
                format!("{} ##", param),
                format!("{}##", param),
                format!("## {}", param),
                format!("##{}", param),
            ];
            for pat in &patterns {
                if body.contains(pat.as_str()) {
                    if !paste.contains(&idx) { paste.push(idx); }
                    break;
                }
            }
        }
        (stringify, paste)
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
        // 处理 __has_include/__has_attribute/__has_builtin/__has_feature
        let expr = self.replace_has_macros(&expr);
        // 替换已知宏
        let expr = self.expand_macros(&expr);
        // 简单求值: 非零 = true
        self.eval_int_expr(&expr) != 0
    }

    fn replace_defined(&self, expr: &str) -> String {
        let mut result = expr.to_string();
        // defined(NAME) or defined NAME
        while let Some(pos) = result.find("defined") {
            // 确保 defined 不在标识符中间
            if pos > 0 {
                let prev = result.as_bytes()[pos - 1];
                if prev.is_ascii_alphanumeric() || prev == b'_' { break; }
            }
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
        // 替换未知标识符为 0
        let expr = self.replace_unknown_idents(&expr);
        // 递归下降求值
        let mut pos = 0;
        let bytes = expr.as_bytes();
        let val = Self::parse_ternary(bytes, &mut pos);
        val
    }

    fn replace_unknown_idents(&self, expr: &str) -> String {
        let mut result = String::new();
        let chars: Vec<char> = expr.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            // 字符常量: 直接保留给 parse_number 处理
            if chars[i] == '\'' {
                result.push('\'');
                i += 1;
                while i < chars.len() && chars[i] != '\'' {
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        result.push(chars[i]); i += 1;
                    }
                    result.push(chars[i]); i += 1;
                }
                if i < chars.len() { result.push('\''); i += 1; }
                continue;
            }
            if chars[i].is_alphabetic() || chars[i] == '_' {
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') { i += 1; }
                let ident: String = chars[start..i].iter().collect();
                match ident.as_str() {
                    "defined" | "true" => result.push_str("1"),
                    "false" => result.push_str("0"),
                    _ => { result.push_str("0"); }
                }
            } else {
                result.push(chars[i]);
                i += 1;
            }
        }
        result
    }

    // 三元: cond ? then : else
    fn parse_ternary(bytes: &[u8], pos: &mut usize) -> i64 {
        let cond = Self::parse_or_expr(bytes, pos);
        Self::skip_ws(bytes, pos);
        if *pos < bytes.len() && bytes[*pos] == b'?' {
            *pos += 1;
            let then_val = Self::parse_ternary(bytes, pos);
            Self::skip_ws(bytes, pos);
            if *pos < bytes.len() && bytes[*pos] == b':' {
                *pos += 1;
            }
            let else_val = Self::parse_ternary(bytes, pos);
            if cond != 0 { then_val } else { else_val }
        } else {
            cond
        }
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
        let mut val = Self::parse_bit_or(bytes, pos);
        Self::skip_ws(bytes, pos);
        while *pos + 1 < bytes.len() && bytes[*pos] == b'&' && bytes[*pos+1] == b'&' {
            *pos += 2;
            let rhs = Self::parse_bit_or(bytes, pos);
            val = if val != 0 && rhs != 0 { 1 } else { 0 };
            Self::skip_ws(bytes, pos);
        }
        val
    }

    fn parse_bit_or(bytes: &[u8], pos: &mut usize) -> i64 {
        let mut val = Self::parse_bit_xor(bytes, pos);
        loop {
            Self::skip_ws(bytes, pos);
            // 排除 ||
            if *pos < bytes.len() && bytes[*pos] == b'|'
                && (*pos + 1 >= bytes.len() || bytes[*pos+1] != b'|') {
                *pos += 1;
                val |= Self::parse_bit_xor(bytes, pos);
            } else { break; }
        }
        val
    }

    fn parse_bit_xor(bytes: &[u8], pos: &mut usize) -> i64 {
        let mut val = Self::parse_bit_and(bytes, pos);
        loop {
            Self::skip_ws(bytes, pos);
            if *pos < bytes.len() && bytes[*pos] == b'^' {
                *pos += 1;
                val ^= Self::parse_bit_and(bytes, pos);
            } else { break; }
        }
        val
    }

    fn parse_bit_and(bytes: &[u8], pos: &mut usize) -> i64 {
        let mut val = Self::parse_eq_expr(bytes, pos);
        loop {
            Self::skip_ws(bytes, pos);
            // 排除 &&
            if *pos < bytes.len() && bytes[*pos] == b'&'
                && (*pos + 1 >= bytes.len() || bytes[*pos+1] != b'&') {
                *pos += 1;
                val &= Self::parse_eq_expr(bytes, pos);
            } else { break; }
        }
        val
    }

    fn parse_eq_expr(bytes: &[u8], pos: &mut usize) -> i64 {
        let mut val = Self::parse_cmp_expr(bytes, pos);
        loop {
            Self::skip_ws(bytes, pos);
            if *pos + 1 < bytes.len() {
                if bytes[*pos] == b'=' && bytes[*pos+1] == b'=' {
                    *pos += 2; val = (val == Self::parse_cmp_expr(bytes, pos)) as i64;
                } else if bytes[*pos] == b'!' && bytes[*pos+1] == b'=' {
                    *pos += 2; val = (val != Self::parse_cmp_expr(bytes, pos)) as i64;
                } else { break; }
            } else { break; }
        }
        val
    }

    fn parse_cmp_expr(bytes: &[u8], pos: &mut usize) -> i64 {
        let mut val = Self::parse_shift_expr(bytes, pos);
        loop {
            Self::skip_ws(bytes, pos);
            if *pos < bytes.len() {
                let (op, len) = match bytes[*pos..] {
                    [b'<', b'=', ..] => ("le", 2),
                    [b'>', b'=', ..] => ("ge", 2),
                    [b'<', ..] => ("lt", 1),
                    [b'>', ..] => ("gt", 1),
                    _ => ("none", 0),
                };
                if op != "none" {
                    *pos += len;
                    let rhs = Self::parse_shift_expr(bytes, pos);
                    val = match op {
                        "lt" => (val < rhs) as i64,
                        "gt" => (val > rhs) as i64,
                        "le" => (val <= rhs) as i64,
                        "ge" => (val >= rhs) as i64,
                        _ => unreachable!(),
                    };
                } else { break; }
            } else { break; }
        }
        val
    }

    fn parse_shift_expr(bytes: &[u8], pos: &mut usize) -> i64 {
        let mut val = Self::parse_add_expr(bytes, pos);
        loop {
            Self::skip_ws(bytes, pos);
            if *pos + 1 < bytes.len() {
                if bytes[*pos] == b'<' && bytes[*pos+1] == b'<' {
                    *pos += 2; val <<= Self::parse_add_expr(bytes, pos);
                } else if bytes[*pos] == b'>' && bytes[*pos+1] == b'>' {
                    *pos += 2; val >>= Self::parse_add_expr(bytes, pos);
                } else { break; }
            } else { break; }
        }
        val
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
                let val = Self::parse_ternary(bytes, pos);
                Self::skip_ws(bytes, pos);
                if *pos < bytes.len() && bytes[*pos] == b')' { *pos += 1; }
                return val;
            }
        }
        Self::parse_primary(bytes, pos)
    }

    fn parse_primary(bytes: &[u8], pos: &mut usize) -> i64 {
        Self::skip_ws(bytes, pos);
        if *pos < bytes.len() && bytes[*pos] == b'\'' {
            return Self::parse_char_const(bytes, pos);
        }
        Self::parse_number(bytes, pos)
    }

    /// 解析字符常量: 'A', '\n', '\x41', '\0' 等
    fn parse_char_const(bytes: &[u8], pos: &mut usize) -> i64 {
        debug_assert!(bytes[*pos] == b'\'');
        *pos += 1; // skip '
        let ch = if *pos < bytes.len() && bytes[*pos] == b'\\' {
            *pos += 1;
            if *pos < bytes.len() {
                let c = bytes[*pos]; *pos += 1;
                match c {
                    b'n' => '\n' as i64,
                    b't' => '\t' as i64,
                    b'r' => '\r' as i64,
                    b'0' => 0,
                    b'\\' => '\\' as i64,
                    b'\'' => '\'' as i64,
                    b'"' => '"' as i64,
                    b'a' => 7,  // bell
                    b'b' => 8,  // backspace
                    b'f' => 12, // form feed
                    b'v' => 11, // vertical tab
                    b'x' => {
                        // \xNN
                        let mut val: i64 = 0;
                        for _ in 0..2 {
                            if *pos < bytes.len() {
                                let d = bytes[*pos];
                                if d.is_ascii_hexdigit() {
                                    val = val * 16 + (d as char).to_digit(16).unwrap() as i64;
                                    *pos += 1;
                                } else { break; }
                            }
                        }
                        val
                    }
                    d if d.is_ascii_digit() => {
                        // \NNN 八进制
                        let mut val = (d - b'0') as i64;
                        for _ in 0..2 {
                            if *pos < bytes.len() && bytes[*pos].is_ascii_digit() && bytes[*pos] < b'8' {
                                val = val * 8 + (bytes[*pos] - b'0') as i64;
                                *pos += 1;
                            } else { break; }
                        }
                        val
                    }
                    _ => c as i64,
                }
            } else { 0 }
        } else if *pos < bytes.len() {
            let c = bytes[*pos] as i64; *pos += 1; c
        } else { 0 };
        // skip closing '
        if *pos < bytes.len() && bytes[*pos] == b'\'' { *pos += 1; }
        ch
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

    // ===== __has_* 特殊宏求值 (在宏展开之前执行) =====
    /// 替换 __has_include/__has_attribute/__has_builtin/__has_feature 为实际值
    fn replace_has_macros(&self, expr: &str) -> String {
        let mut result = expr.to_string();
        result = self.replace_has_include(&result);
        result = self.replace_has_attr_like(&result, "__has_attribute", |attr| Self::is_known_attribute(attr));
        result = self.replace_has_attr_like(&result, "__has_builtin", |name| Self::is_known_builtin(name));
        result = self.replace_has_attr_like(&result, "__has_feature", |_| false);
        result
    }

    fn replace_has_include(&self, expr: &str) -> String {
        let mut result = String::with_capacity(expr.len());
        let bytes = expr.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            // 检查 __has_include
            if i + 14 <= bytes.len() && &bytes[i..i+14] == b"__has_include"
                && (i == 0 || !bytes[i-1].is_ascii_alphanumeric() && bytes[i-1] != b'_')
                && (i + 14 >= bytes.len() || !bytes[i+14].is_ascii_alphanumeric() && bytes[i+14] != b'_')
            {
                let mut j = i + 14;
                while j < bytes.len() && bytes[j] == b' ' { j += 1; }
                if j < bytes.len() && bytes[j] == b'(' {
                    j += 1;
                    while j < bytes.len() && bytes[j] == b' ' { j += 1; }
                    let path_start = j;
                    // 跳过到 )
                    let mut depth = 1;
                    while j < bytes.len() && depth > 0 {
                        if bytes[j] == b'(' { depth += 1; }
                        else if bytes[j] == b')' { depth -= 1; }
                        if depth > 0 { j += 1; }
                    }
                    let path = expr[path_start..j].trim()
                        .trim_matches(|c| c == '<' || c == '>' || c == '"');
                    let found = self.find_has_include(path);
                    result.push_str(if found { "1" } else { "0" });
                    if j < bytes.len() { j += 1; } // skip )
                    i = j;
                    continue;
                }
            }
            result.push(bytes[i] as char);
            i += 1;
        }
        result
    }

    fn replace_has_attr_like<F>(&self, expr: &str, macro_name: &str, check: F) -> String
    where F: Fn(&str) -> bool {
        let mut result = String::with_capacity(expr.len());
        let bytes = expr.as_bytes();
        let name_len = macro_name.len();
        let mut i = 0;
        while i < bytes.len() {
            if i + name_len <= bytes.len()
                && &bytes[i..i+name_len] == macro_name.as_bytes()
                && (i == 0 || !bytes[i-1].is_ascii_alphanumeric() && bytes[i-1] != b'_')
                && (i + name_len >= bytes.len() || !bytes[i+name_len].is_ascii_alphanumeric() && bytes[i+name_len] != b'_')
            {
                let mut j = i + name_len;
                while j < bytes.len() && bytes[j] == b' ' { j += 1; }
                if j < bytes.len() && bytes[j] == b'(' {
                    j += 1;
                    while j < bytes.len() && bytes[j] == b' ' { j += 1; }
                    let arg_start = j;
                    let mut depth = 1;
                    while j < bytes.len() && depth > 0 {
                        if bytes[j] == b'(' { depth += 1; }
                        else if bytes[j] == b')' { depth -= 1; }
                        if depth > 0 { j += 1; }
                    }
                    let arg = expr[arg_start..j].trim();
                    let val = if check(arg) { "1" } else { "0" };
                    result.push_str(val);
                    if j < bytes.len() { j += 1; }
                    i = j;
                    continue;
                }
            }
            result.push(bytes[i] as char);
            i += 1;
        }
        result
    }

    fn find_has_include(&self, path: &str) -> bool {
        // 相对当前文件
        let p = self.base_dir.join(path);
        if p.exists() { return true; }
        // -I 路径
        for dir in &self.include_paths {
            if dir.join(path).exists() { return true; }
        }
        // -isystem 路径
        for dir in &self.system_paths {
            if dir.join(path).exists() { return true; }
        }
        // 标准系统路径
        let system_dirs = [
            "/usr/local/include", "/usr/include",
            "/usr/include/x86_64-linux-gnu", "/usr/include/aarch64-linux-gnu",
            "/usr/include/linux", "/usr/include/asm", "/usr/include/asm-generic",
        ];
        for dir in &system_dirs {
            if Path::new(dir).join(path).exists() { return true; }
        }
        false
    }

    fn is_known_attribute(attr: &str) -> bool {
        matches!(attr,
            "packed" | "aligned" | "noreturn" | "unused" | "used"
            | "section" | "visibility" | "always_inline" | "noinline"
            | "format" | "nonnull" | "cleanup" | "deprecated"
            | "fallthrough" | "weak" | "constructor" | "destructor"
            | "vector_size" | "may_alias" | "transparent_union"
            | "malloc" | "returns_twice" | "pure" | "const"
            | "warn_unused_result" | "sentinel" | "format_arg"
        )
    }

    fn is_known_builtin(name: &str) -> bool {
        let name = name.trim();
        matches!(name,
            "__builtin_expect" | "__builtin_memcpy" | "__builtin_memset"
            | "__builtin_memmove" | "__builtin_memcmp"
            | "__builtin_malloc" | "__builtin_free"
            | "__builtin_offsetof" | "__builtin_va_start" | "__builtin_va_end"
            | "__builtin_va_arg" | "__builtin_va_copy"
            | "__builtin_add_overflow" | "__builtin_sub_overflow"
            | "__builtin_mul_overflow"
            | "__builtin_types_compatible_p"
            | "__builtin_choose_expr" | "__builtin_constant_p"
            | "__builtin_abs" | "__builtin_unreachable" | "__builtin_trap"
            | "__builtin_strcpy" | "__builtin_strncpy"
            | "__builtin_strcmp" | "__builtin_strlen"
            | "__builtin_expect" | "__builtin_prefetch"
            | "__builtin_clz" | "__builtin_ctz" | "__builtin_popcount"
        )
    }
}
