// src/parser.rs - 语法分析
// 第一性原理: 解析器把 token 流构造成 AST。
// suckless: 递归下降 + Pratt 表达式解析。

use crate::diagnostics::{Diagnostic, Span};
use crate::tokens::{Lexer, Tok, TokKind};
use std::sync::Arc;
use crate::ast::*;
use crate::const_eval::{self, ConstEvalError};
use std::collections::HashMap;

/// 解析后的 GNU 属性
#[derive(Default, Clone)]
struct ParsedAttrs {
    section: Option<String>,
    alignment: Option<usize>,
    is_weak: bool,
    visibility: Option<String>,
    is_unused: bool,
    is_cold: bool,
    is_noinline: bool,
    is_always_inline: bool,
    register_name: Option<String>,
    cleanup_fn: Option<String>,
    is_constructor: bool,
    is_destructor: bool,
    alias_name: Option<String>,
    format_kind: Option<String>,
    format_string_idx: Option<u32>,
    format_first_arg: Option<u32>,
    /// __attribute__((no_sanitize("address"|"undefined"|"thread"|"kernel-address")))
    no_sanitize: Vec<String>,
    /// __attribute__((target_clones("default,avx2,...")))
    target_clones: Vec<String>,
    /// __attribute__((copy("fn")))
    copy_fn: Option<String>,
    /// __attribute__((returns_nonnull))
    returns_nonnull: bool,
}

/// 解析选项（Driver 传入）
#[derive(Debug, Default, Clone)]
pub struct ParseOptions {
    /// `-fpermissive`：未知 typedef 降级为 int 并告警（默认严格 E0009）
    pub permissive: bool,
    /// #pragma pack 变更历史: (行号, 新对齐). 供 struct 布局按位置查询
    pub pack_history: Vec<(u32, Option<usize>)>,
    /// #pragma weak 记录的弱符号集合
    pub weak_symbols: std::collections::HashSet<String>,
    /// `-Wint-truncation`: 整数截断警告
    pub warn_int_truncation: bool,
    /// `-Wint-conversion`: 整数↔指针隐式转换警告
    pub warn_int_conversion: bool,
}

impl ParsedAttrs {
    fn merge(&mut self, other: ParsedAttrs) {
        if other.section.is_some() { self.section = other.section; }
        if other.alignment.is_some() { self.alignment = other.alignment; }
        if other.is_weak { self.is_weak = true; }
        if other.visibility.is_some() { self.visibility = other.visibility; }
        if other.is_unused { self.is_unused = true; }
        if other.is_cold { self.is_cold = true; }
        if other.is_noinline { self.is_noinline = true; }
        if other.is_always_inline { self.is_always_inline = true; }
        if other.register_name.is_some() { self.register_name = other.register_name; }
        if other.cleanup_fn.is_some() { self.cleanup_fn = other.cleanup_fn; }
        if other.is_constructor { self.is_constructor = true; }
        if other.is_destructor { self.is_destructor = true; }
        if other.alias_name.is_some() { self.alias_name = other.alias_name; }
        if other.format_kind.is_some() { self.format_kind = other.format_kind; }
        if other.format_string_idx.is_some() { self.format_string_idx = other.format_string_idx; }
        if other.format_first_arg.is_some() { self.format_first_arg = other.format_first_arg; }
        if !other.no_sanitize.is_empty() { self.no_sanitize.extend(other.no_sanitize); }
        if !other.target_clones.is_empty() { self.target_clones.extend(other.target_clones); }
        if other.copy_fn.is_some() { self.copy_fn = other.copy_fn; }
        if other.returns_nonnull { self.returns_nonnull = true; }
    }
}

#[derive(Default, Clone, Copy)]
struct StorageClass {
    is_extern: bool,
    is_static: bool,
    is_register: bool,
    is_thread_local: bool,
}

pub struct Parser<'a> {
    lex: Lexer<'a>, cur: Tok, peek: Tok,
    diags: Vec<Diagnostic>, finished: bool,
    typedefs: HashMap<String, Type>,
    struct_defs: HashMap<String, (Arc<Vec<Field>>, bool)>,
    enum_defs: HashMap<String, Arc<Vec<(String, i64)>>>,
    enum_consts: HashMap<String, i64>,
    locals: HashMap<String, Type>, // 变量名 → 类型 (函数内会清除)
    globals: HashMap<String, Type>, // 全局变量名 → 类型 (永不清除)
    in_function: bool,              // 是否在函数体内
    current_function: Option<String>, // 当前函数名 (供 __func__/__builtin_FUNCTION 等使用)
    pub last_variadic: bool,        // parse_params 是否遇到 ...
    decl_storage: StorageClass,     // 最近一次声明说明符中的存储类
    decl_attrs: ParsedAttrs,        // 类型说明符之间出现的 GNU 属性
    decl_auto_type: bool,           // __auto_type：类型由初始化表达式推导
    allow_named_type: bool,         // typeof 等上下文允许 Type::Named
    opts: ParseOptions,
}

pub fn parse(src: &[u8], file: &str) -> (Program, Vec<Diagnostic>) {
    parse_with_opts(src, file, ParseOptions::default())
}

pub fn parse_with_opts(src: &[u8], file: &str, opts: ParseOptions) -> (Program, Vec<Diagnostic>) {
    let mut p = Parser::new(src, file, opts);
    p.program()
}

impl<'a> Parser<'a> {
    pub fn new(src: &'a [u8], file: &str, opts: ParseOptions) -> Self {
        let mut lex = Lexer::new(src, file);
        let cur = lex.next(); let peek = lex.next();
        Self { lex, cur, peek, diags: vec![], finished: false,
               typedefs: HashMap::new(), struct_defs: HashMap::new(),
               enum_defs: HashMap::new(), enum_consts: HashMap::new(),
               locals: HashMap::new(), globals: HashMap::new(), in_function: false,
               current_function: None,
               last_variadic: false, decl_storage: StorageClass::default(),
               decl_attrs: ParsedAttrs::default(), decl_auto_type: false,
               allow_named_type: false, opts }
    }
    fn advance(&mut self) { self.cur = std::mem::replace(&mut self.peek, self.lex.next()); }

    /// 给定源行号, 查找 #pragma pack 历史中的当前 packing 对齐
    /// pack_history 按行号升序, 找到最后一个 l <= line 的条目即生效值
    pub fn pack_at_line(&self, line: u32) -> Option<usize> {
        let mut current: Option<usize> = None;
        for &(l, pack) in &self.opts.pack_history {
            if l <= line {
                current = pack;
            } else {
                break;
            }
        }
        current
    }

    /// 当前行处是否标记为弱符号 (#pragma weak name)
    pub fn is_weak_symbol(&self, name: &str) -> bool {
        self.opts.weak_symbols.contains(name)
    }

    /// 保存解析器/词法位置，供试探性解析后回退
    fn bookmark(&self) -> (usize, u32, u32, Tok, Tok) {
        (self.lex.pos, self.lex.line, self.lex.col, self.cur.clone(), self.peek.clone())
    }
    fn restore(&mut self, (pos, line, col, cur, peek): (usize, u32, u32, Tok, Tok)) {
        self.lex.pos = pos;
        self.lex.line = line;
        self.lex.col = col;
        self.cur = cur;
        self.peek = peek;
    }

    fn err_here(&mut self, code: &'static str, msg: impl Into<String>, hint: Option<&str>) {
        let mut d = Diagnostic::err(code, msg, self.cur.span.clone());
        if let Some(h) = hint {
            d.hint = Some(h.into());
        }
        self.diags.push(d);
    }
    fn warn_here(&mut self, code: &'static str, msg: impl Into<String>, hint: Option<&str>) {
        let mut d = Diagnostic::warn(code, msg, self.cur.span.clone());
        if let Some(h) = hint {
            d.hint = Some(h.into());
        }
        self.diags.push(d);
    }
    /// 严格类型转换检查: 返回 Some(diag) 当出现以下情况:
    ///   1. 整型截断 (例如 (char)0x100, 由 warn_int_truncation 控制)
    ///   2. 整数 ↔ 指针隐式转换 (由 warn_int_conversion 控制)
    /// 仅在显式 (type)expr 强制转换路径触发; 隐式 cast 走 IR。
    fn check_strict_conversion(&self, from: &Type, to: &Type, sp: crate::diagnostics::Span) -> Option<Diagnostic> {
        use crate::ast::Type::*;
        let from_int_bits = match from {
            Bool | Char | SChar | UChar => Some(8),
            Short | UShort => Some(16),
            Int | UInt | Enum(_, _) => Some(32),
            Long | ULong => Some(64),
            LongLong | ULongLong => Some(64),
            Int128 | UInt128 => Some(128),
            _ => None,
        };
        let to_int_bits = match to {
            Bool | Char | SChar | UChar => Some(8),
            Short | UShort => Some(16),
            Int | UInt | Enum(_, _) => Some(32),
            Long | ULong => Some(64),
            LongLong | ULongLong => Some(64),
            Int128 | UInt128 => Some(128),
            _ => None,
        };
        // 整数截断: 仅当 const_eval 已可算 + 超出范围
        if let (Some(from_b), Some(to_b)) = (from_int_bits, to_int_bits) {
            if to_b < from_b && self.opts.warn_int_truncation {
                // 看是否紧跟字面量
                return Some(Diagnostic::warn(
                    "W0301",
                    format!("integer conversion from {} to {} bits may lose information",
                            from_b, to_b),
                    sp,
                ));
            }
        }
        // 整数 ↔ 指针转换
        let from_is_ptr = matches!(from, Ptr(_) | Func(_, _));
        let to_is_ptr = matches!(to, Ptr(_) | Func(_, _));
        if from_is_ptr != to_is_ptr && self.opts.warn_int_conversion {
            let dir = if from_is_ptr { "pointer to integer" } else { "integer to pointer" };
            return Some(Diagnostic::warn(
                "W0302",
                format!("implicit conversion from {} (use an explicit cast if intended)", dir),
                sp,
            ));
        }
        None
    }
    /// 登记 typedef；GCC 允许兼容或整型别名的重复 typedef（宿主头文件常见）
    fn register_typedef(&mut self, name: &Ident, ty: Type) -> bool {
        if let Some(existing) = self.typedefs.get(&name.name) {
            if Self::types_compatible(existing, &ty) {
                return true;
            }
            if Self::is_integer_typedef(existing) && Self::is_integer_typedef(&ty) {
                self.typedefs.insert(name.name.clone(), ty);
                return true;
            }
            if Self::typedef_redef_allowed(existing, &ty) {
                self.typedefs.insert(name.name.clone(), ty);
                return true;
            }
            self.diags.push(Diagnostic::err(
                "E0005",
                format!("duplicate typedef '{}'", name.name),
                name.span.clone(),
            ).with_hint("each typedef name may only be defined once"));
            return false;
        }
        self.typedefs.insert(name.name.clone(), ty);
        true
    }
    /// 变量/参数重声明类型冲突报 E0007
    /// M7 修复：locals 是扁平 HashMap，无法区分同函数内嵌套块的
    /// 重声明（合法 C 语义：内层遮蔽外层）。原实现在嵌套块内
    /// 重声明同名变量会误报 E0007。libdecnumber 的 decBasic.c
    /// 在外层 `uInt div[]` 之内的内层块又声明 `uint64_t div=...`
    /// 就属于这种情况。规则：
    ///   - 同类型: 允许
    ///   - 函数体内: 允许 (嵌套块遮蔽 / 遮蔽全局)
    ///   - 文件作用域且不同类型: 报 E0007
    fn check_var_redecl(&mut self, name: &str, ty: &Type, span: Span) -> bool {
        if let Some(existing) = self.locals.get(name) {
            if existing == ty {
                return true;
            }
            // 函数体内允许遮蔽 (嵌套块 / 遮蔽全局)
            if self.in_function {
                return true;
            }
            // 文件作用域且类型不匹配: 报 E0007
            self.diags.push(Diagnostic::err(
                "E0007",
                format!("conflicting types for redeclaration of '{}'", name),
                span,
            ).with_hint("use the same type as the previous declaration"));
            return false;
        }
        true
    }
    /// 声明说明符后是否像声明符（非类型名）
    fn peek_decl_context(&self) -> bool {
        matches!(
            self.peek.kind,
            TokKind::Semi | TokKind::Comma | TokKind::Assign | TokKind::LParen | TokKind::Star
        )
    }
    /// typeof 括号内是类型形式还是表达式形式
    fn typeof_operand_is_type_form(&self) -> bool {
        if self.cur.kind == TokKind::Ident {
            match self.cur.text.as_str() {
                "_Generic" | "nullptr" | "sizeof" | "__builtin_choose_expr" => return false,
                _ => {}
            }
            let n = self.cur.text.clone();
            if self.typedefs.contains_key(&n) {
                return true;
            }
            if self.locals.contains_key(&n) || self.globals.contains_key(&n) {
                return false;
            }
        }
        if self.is_type_start() {
            return true;
        }
        if self.cur.kind == TokKind::Ident {
            return true;
        }
        false
    }
    fn has_storage_without_base(&self, base: &Option<Type>) -> bool {
        base.is_none()
            && (self.decl_storage.is_static
                || self.decl_storage.is_extern
                || self.decl_storage.is_register)
    }
    fn expect(&mut self, k: TokKind, what: &str) -> bool {
        if self.cur.kind == k { self.advance(); return true; }
        self.err_here("E0001", format!("expected {}, got {:?}", what, self.cur.kind), Some("check the syntax near this position")); false
    }
    /// 静默剥离 __attribute__((...)): GNU 扩展属性不影响代码生成
    fn skip_attributes(&mut self) {
        while self.cur.kind == TokKind::Ident && self.cur.text == "__attribute__" {
            self.advance(); // __attribute__
            if self.cur.kind == TokKind::LParen { self.advance(); } // (
            if self.cur.kind == TokKind::LParen { self.advance(); } // (
            let mut depth = 2i32;
            while depth > 0 && self.cur.kind != TokKind::Eof {
                if self.cur.kind == TokKind::LParen { depth += 1; }
                if self.cur.kind == TokKind::RParen { depth -= 1; }
                self.advance();
            }
        }
    }
    /// 去除字符串字面量两端的引号
    fn strip_quotes(s: &str) -> String {
        let s = s.trim();
        if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
            s[1..s.len()-1].to_string()
        } else { s.to_string() }
    }
    fn decode_string_literal(s: &str) -> String {
        let mut bytes = parse_str(s);
        if bytes.last() == Some(&0) { bytes.pop(); }
        String::from_utf8_lossy(&bytes).into_owned()
    }
    /// 读取一个或多个相邻字符串字面量并拼接（GCC section 等属性常见）
    fn parse_concatenated_string_lit(&mut self) -> Option<String> {
        if self.cur.kind != TokKind::StringLit {
            return None;
        }
        let mut s = Self::strip_quotes(&self.cur.text);
        self.advance();
        while self.cur.kind == TokKind::StringLit {
            s.push_str(&Self::strip_quotes(&self.cur.text));
            self.advance();
        }
        Some(s)
    }
    /// 解析 __attribute__((...)) 并提取属性信息
    fn parse_attributes(&mut self) -> ParsedAttrs {
        let mut attrs = ParsedAttrs::default();
        while self.cur.kind == TokKind::Ident && self.cur.text == "__attribute__" {
            self.advance(); // __attribute__
            if self.cur.kind == TokKind::LParen { self.advance(); } // (
            if self.cur.kind == TokKind::LParen { self.advance(); } // (
            // 解析内部属性列表
            while self.cur.kind != TokKind::RParen && self.cur.kind != TokKind::Eof {
                let name = self.cur.text.trim_matches('_').to_string();
                match name.as_str() {
                    "section" => {
                        self.advance();
                        if self.cur.kind == TokKind::LParen {
                            self.advance();
                            if let Some(section) = self.parse_concatenated_string_lit() {
                                attrs.section = Some(section);
                            }
                            if self.cur.kind == TokKind::RParen { self.advance(); }
                        }
                    }
                    "aligned" => {
                        self.advance();
                        if self.cur.kind == TokKind::LParen {
                            let mut depth = 0i32;
                            loop {
                                if self.cur.kind == TokKind::LParen {
                                    depth += 1;
                                } else if self.cur.kind == TokKind::RParen {
                                    depth -= 1;
                                    self.advance();
                                    if depth == 0 { break; }
                                    continue;
                                } else if depth == 1 && self.cur.kind == TokKind::Number {
                                    let value = parse_int_lit(&self.cur.text);
                                    if value > 0 { attrs.alignment = Some(value as usize); }
                                }
                                self.advance();
                                if self.cur.kind == TokKind::Eof { break; }
                            }
                        } else {
                            attrs.alignment = Some(16); // __attribute__((aligned)) 无参数默认16
                        }
                    }
                    "weak" => { attrs.is_weak = true; self.advance(); }
                    "visibility" => {
                        self.advance();
                        if self.cur.kind == TokKind::LParen {
                            self.advance();
                            if let Some(visibility) = self.parse_concatenated_string_lit() {
                                attrs.visibility = Some(visibility);
                            }
                            if self.cur.kind == TokKind::RParen { self.advance(); }
                        }
                    }
                    "unused" => { attrs.is_unused = true; self.advance(); }
                    "cold" => { attrs.is_cold = true; self.advance(); }
                    "noinline" => { attrs.is_noinline = true; self.advance(); }
                    "always_inline" => { attrs.is_always_inline = true; self.advance(); }
                    "cleanup" => {
                        self.advance();
                        if self.cur.kind == TokKind::LParen {
                            self.advance();
                            if self.cur.kind == TokKind::Ident {
                                attrs.cleanup_fn = Some(self.cur.text.clone());
                                self.advance();
                            }
                            if self.cur.kind == TokKind::RParen { self.advance(); }
                        }
                    }
                    "constructor" => {
                        attrs.is_constructor = true;
                        self.advance();
                        // optional priority: __attribute__((constructor(100)))
                        if self.cur.kind == TokKind::LParen {
                            self.advance();
                            // skip priority number
                            if self.cur.kind == TokKind::Number { self.advance(); }
                            if self.cur.kind == TokKind::RParen { self.advance(); }
                        }
                    }
                    "destructor" => {
                        attrs.is_destructor = true;
                        self.advance();
                        if self.cur.kind == TokKind::LParen {
                            self.advance();
                            if self.cur.kind == TokKind::Number { self.advance(); }
                            if self.cur.kind == TokKind::RParen { self.advance(); }
                        }
                    }
                    "alias" => {
                        self.advance();
                        if self.cur.kind == TokKind::LParen {
                            self.advance();
                            if let Some(alias) = self.parse_concatenated_string_lit() {
                                attrs.alias_name = Some(alias);
                            }
                            if self.cur.kind == TokKind::RParen { self.advance(); }
                        }
                    }
                    "format" => {
                        self.advance();
                        if self.cur.kind == TokKind::LParen {
                            self.advance();
                            if self.cur.kind == TokKind::Ident {
                                attrs.format_kind = Some(self.cur.text.clone());
                                self.advance();
                            }
                            if self.cur.kind == TokKind::Comma { self.advance(); }
                            if self.cur.kind == TokKind::Number {
                                attrs.format_string_idx =
                                    parse_int_lit(&self.cur.text).try_into().ok();
                                self.advance();
                            }
                            if self.cur.kind == TokKind::Comma { self.advance(); }
                            if self.cur.kind == TokKind::Number {
                                attrs.format_first_arg =
                                    parse_int_lit(&self.cur.text).try_into().ok();
                                self.advance();
                            }
                            while self.cur.kind != TokKind::RParen && self.cur.kind != TokKind::Eof {
                                self.advance();
                            }
                            if self.cur.kind == TokKind::RParen { self.advance(); }
                        }
                    }
                    "no_sanitize" | "no_sanitize_address" | "no_address_safety_analysis" => {
                        // __attribute__((no_sanitize("address", "undefined")))
                        // __attribute__((no_sanitize_address))
                        // __attribute__((no_address_safety_analysis))
                        if name == "no_sanitize" {
                            self.advance();
                            if self.cur.kind == TokKind::LParen {
                                self.advance();
                                while self.cur.kind != TokKind::RParen && self.cur.kind != TokKind::Eof {
                                    if self.cur.kind == TokKind::StringLit {
                                        let s = Self::strip_quotes(&self.cur.text);
                                        if !s.is_empty() {
                                            attrs.no_sanitize.push(s);
                                        }
                                        self.advance();
                                    } else {
                                        self.advance();
                                    }
                                    if self.cur.kind == TokKind::Comma { self.advance(); }
                                }
                                if self.cur.kind == TokKind::RParen { self.advance(); }
                            }
                        } else {
                            attrs.no_sanitize.push(name.clone());
                            self.advance();
                        }
                    }
                    "target_clones" => {
                        // __attribute__((target_clones("default,avx2,avx512f")))
                        self.advance();
                        if self.cur.kind == TokKind::LParen {
                            self.advance();
                            while self.cur.kind != TokKind::RParen && self.cur.kind != TokKind::Eof {
                                if self.cur.kind == TokKind::StringLit {
                                    let s = Self::strip_quotes(&self.cur.text);
                                    if !s.is_empty() {
                                        // split by comma
                                        for part in s.split(',') {
                                            let part = part.trim();
                                            if !part.is_empty() {
                                                attrs.target_clones.push(part.to_string());
                                            }
                                        }
                                    }
                                    self.advance();
                                } else {
                                    self.advance();
                                }
                                if self.cur.kind == TokKind::Comma { self.advance(); }
                            }
                            if self.cur.kind == TokKind::RParen { self.advance(); }
                        }
                    }
                    "copy" => {
                        // __attribute__((copy("fn"))) — 函数/变量符号别名
                        self.advance();
                        if self.cur.kind == TokKind::LParen {
                            self.advance();
                            if let Some(s) = self.parse_concatenated_string_lit() {
                                attrs.copy_fn = Some(s);
                            } else if self.cur.kind == TokKind::Ident {
                                attrs.copy_fn = Some(self.cur.text.clone());
                                self.advance();
                            }
                            if self.cur.kind == TokKind::RParen { self.advance(); }
                        }
                    }
                    "returns_nonnull" => {
                        attrs.returns_nonnull = true;
                        self.advance();
                        // optional (): __attribute__((returns_nonnull))
                        if self.cur.kind == TokKind::LParen {
                            let mut depth = 0i32;
                            loop {
                                if self.cur.kind == TokKind::LParen { depth += 1; }
                                else if self.cur.kind == TokKind::RParen {
                                    depth -= 1;
                                    self.advance();
                                    if depth == 0 { break; }
                                    continue;
                                }
                                self.advance();
                                if self.cur.kind == TokKind::Eof { break; }
                            }
                        }
                    }
                    _ => {
                        self.advance();
                        // 跳过未知属性的完整参数列表，不能把其内部右括号
                        // 误当成 __attribute__((...)) 的结束。
                        if self.cur.kind == TokKind::LParen {
                            let mut depth = 0i32;
                            loop {
                                if self.cur.kind == TokKind::LParen { depth += 1; }
                                else if self.cur.kind == TokKind::RParen {
                                    depth -= 1;
                                    self.advance();
                                    if depth == 0 { break; }
                                    continue;
                                }
                                self.advance();
                                if self.cur.kind == TokKind::Eof { break; }
                            }
                        }
                    }
                }
                if self.cur.kind == TokKind::Comma { self.advance(); }
            }
            if self.cur.kind == TokKind::RParen { self.advance(); } // )
            if self.cur.kind == TokKind::RParen { self.advance(); } // )
        }
        attrs
    }
    /// 合并两组属性 (b 覆盖 a 中的同名字段)
    fn merge_attrs(a: ParsedAttrs, b: ParsedAttrs) -> ParsedAttrs {
        let mut r = a;
        r.merge(b);
        r
    }
    /// 跳过 C23 标准属性 [[attr]]: 不影响代码生成，静默跳过
    fn skip_std_attributes(&mut self) {
        while self.cur.kind == TokKind::LBracket && self.peek.kind == TokKind::LBracket {
            self.advance(); // [
            self.advance(); // [
            let mut depth = 2i32;
            while depth > 0 && self.cur.kind != TokKind::Eof {
                if self.cur.kind == TokKind::LBracket && self.peek.kind == TokKind::LBracket {
                    depth += 2;
                    self.advance(); self.advance();
                    continue;
                }
                if self.cur.kind == TokKind::RBracket && self.peek.kind == TokKind::RBracket {
                    depth -= 2;
                    if depth == 0 { self.advance(); break; }
                    self.advance(); self.advance();
                    continue;
                }
                self.advance();
            }
            if self.cur.kind == TokKind::RBracket { self.advance(); } // ]
        }
    }

    /// 剥离 __asm__("label"): GNU asm 标签, 不影响代码生成
    fn skip_asm_label(&mut self) -> Option<String> {
        if self.cur.kind == TokKind::Ident && (self.cur.text == "__asm__" || self.cur.text == "__asm" || self.cur.text == "asm") {
            if self.peek.kind == TokKind::LParen {
                self.advance(); // asm
                self.advance(); // (
                let mut label = String::new();
                while self.cur.kind == TokKind::StringLit {
                    label.push_str(&Self::decode_string_literal(&self.cur.text));
                    self.advance();
                }
                if label.is_empty() {
                if self.cur.kind == TokKind::Ident {
                    // 宏尚未展开为字符串: asm(__ASM_FORM_RAW) / asm(rsp)
                    label = self.cur.text.clone();
                    self.advance();
                } else {
                    // 复杂实参 (如 __stringify(rsp)): 跳过平衡括号
                    let mut depth = 1i32;
                    while depth > 0 && self.cur.kind != TokKind::Eof {
                        if self.cur.kind == TokKind::LParen { depth += 1; }
                        if self.cur.kind == TokKind::RParen {
                            depth -= 1;
                            if depth == 0 { self.advance(); break; }
                        }
                        self.advance();
                    }
                }
                }
                if self.cur.kind == TokKind::RParen { self.advance(); }
                return Some(label);
            }
        }
        None
    }

    fn skip_post_decl_modifiers(&mut self) -> ParsedAttrs {
        let mut attrs = ParsedAttrs::default();
        loop {
            if self.skip_asm_label().is_some() { continue; }
            if self.cur.kind == TokKind::Ident && self.cur.text == "__attribute__" {
                attrs = Self::merge_attrs(attrs, self.parse_attributes());
                continue;
            }
            break;
        }
        attrs
    }

    fn lookup_ident_type(&self, name: &str) -> Type {
        if let Some(ty) = self.locals.get(name) {
            return ty.clone();
        }
        if let Some(ty) = self.globals.get(name) {
            return ty.clone();
        }
        if let Some(ty) = self.typedefs.get(name) {
            return ty.clone();
        }
        int_t()
    }

    fn call_result_type(&self, callee: &Expr) -> Type {
        match callee.ty() {
            Type::Func(ret, _) => (*ret).clone(),
            Type::Ptr(inner) => match *inner {
                Type::Func(ret, _) => (*ret).clone(),
                other => other,
            },
            other => other,
        }
    }

    fn type_of_member(&self, base: &Expr, field: &Ident, is_arrow: bool) -> Type {
        let aggregate = if is_arrow {
            match base.ty() {
                Type::Ptr(inner) => inner.as_ref().clone(),
                other => other,
            }
        } else {
            base.ty()
        };
        match aggregate {
            Type::Struct(ref name, ref fields) | Type::Union(ref name, ref fields) => {
                // 仅当嵌入字段为空（前向声明）时才查 struct_defs；
                // 匿名 struct 名同为 ""，不能覆盖已嵌入的正确字段。
                let resolved = if fields.is_empty() {
                    self.struct_defs.get(name).map(|(f, _)| f.as_ref()).unwrap_or(fields.as_ref())
                } else {
                    fields.as_ref()
                };
                find_field(resolved, &field.name)
                    .map(|(found, _)| found.ty.clone())
                    .unwrap_or_else(int_t)
            }
            _ => int_t(),
        }
    }

    fn index_result_type(base: &Expr) -> Type {
        match base.ty() {
            Type::Ptr(inner) | Type::Array(inner, _) => (*inner).clone(),
            _ => int_t(),
        }
    }

    /// 探测 `asm [qualifiers]* goto` 而不消耗 token
    fn probe_asm_goto(&mut self) -> bool {
        if !matches!(self.cur.text.as_str(), "__asm__" | "__asm" | "asm") {
            return false;
        }
        let bm = self.bookmark();
        self.advance();
        self.skip_asm_qualifiers();
        let is_goto = self.cur.kind == TokKind::KwGoto
            || (self.cur.kind == TokKind::Ident && self.cur.text == "goto");
        self.restore(bm);
        is_goto
    }

    /// 跳过 asm/__asm__ 与括号之间的限定符（volatile、inline 等）
    fn skip_asm_qualifiers(&mut self) {
        loop {
            match self.cur.kind {
                TokKind::KwVolatile | TokKind::KwConst | TokKind::KwInline => {
                    self.advance();
                }
                TokKind::Ident
                    if matches!(
                        self.cur.text.as_str(),
                        "__volatile__" | "__const__" | "__inline__" | "__inline" | "inline"
                    ) =>
                {
                    self.advance();
                }
                _ => break,
            }
        }
    }

    /// 解析内联汇编表达式: asm("..." : outs : ins : clobbers)
    fn parse_asm_expr(&mut self, sp: Span) -> Option<Expr> {
        if self.cur.kind == TokKind::LParen {
            self.advance();
        } else {
            self.err_here("E0001", "expected '(' after asm", Some("asm requires parentheses: asm(\"instructions\" : ...)"));
            return None;
        }
        let mut asm_str = String::new();
        while self.cur.kind == TokKind::StringLit {
            asm_str.push_str(&Self::decode_string_literal(&self.cur.text));
            self.advance();
        }
        let mut outputs = vec![];
        let mut output_names = vec![];
        let mut output_constraints = vec![];
        let mut inputs = vec![];
        let mut input_names = vec![];
        let mut input_constraints = vec![];
        let mut clobbers = vec![];
        if self.cur.kind == TokKind::Colon {
            self.advance();
            let (o, n, c) = self.parse_asm_operand_list();
            outputs = o;
            output_names = n;
            output_constraints = c;
        }
        if self.cur.kind == TokKind::Colon {
            self.advance();
            let (o, n, c) = self.parse_asm_operand_list();
            inputs = o;
            input_names = n;
            input_constraints = c;
        }
        if self.cur.kind == TokKind::Colon { self.advance(); clobbers = self.parse_asm_clobber_list(); }
        if self.cur.kind == TokKind::RParen { self.advance(); }
        Some(Expr::AsmExpr {
            asm_str, outputs, output_names, output_constraints, inputs, input_names,
            input_constraints, clobbers, span: sp, ty: Type::Void,
        })
    }
    /// 解析 asm 操作数列表 (逗号分隔的表达式, 到冒号或右括号结束)
    fn parse_asm_operand_list(&mut self) -> (Vec<Expr>, Vec<Option<String>>, Vec<String>) {
        let mut ops = vec![];
        let mut names = vec![];
        let mut constraints = vec![];
        while self.cur.kind != TokKind::Colon && self.cur.kind != TokKind::RParen && self.cur.kind != TokKind::Eof {
            let name = if self.cur.kind == TokKind::LBracket {
                self.advance();
                let mut n = String::new();
                while self.cur.kind != TokKind::RBracket && self.cur.kind != TokKind::Eof {
                    if !n.is_empty() {
                        n.push(' ');
                    }
                    n.push_str(&self.cur.text);
                    self.advance();
                }
                if self.cur.kind == TokKind::RBracket {
                    self.advance();
                }
                Some(n.trim().to_string())
            } else {
                None
            };
            names.push(name);
            // GCC 约束字符串，实际表达式位于随后的括号中。
            let constraint = if self.cur.kind == TokKind::StringLit {
                let c = Self::decode_string_literal(&self.cur.text);
                self.advance();
                c
            } else {
                String::new()
            };
            constraints.push(constraint);
            let expr = if self.cur.kind == TokKind::LParen {
                self.advance();
                let e = self.parse_expr_bp(2);
                if self.cur.kind == TokKind::RParen { self.advance(); }
                e
            } else {
                self.parse_expr_bp(2)
            };
            if let Some(e) = expr { ops.push(e); } else { break; }
            if self.cur.kind == TokKind::Comma { self.advance(); }
        }
        (ops, names, constraints)
    }
    /// 解析 asm clobber 列表 (逗号分隔的字符串)
    fn parse_asm_clobber_list(&mut self) -> Vec<String> {
        let mut clobbers = vec![];
        while self.cur.kind != TokKind::Colon && self.cur.kind != TokKind::RParen && self.cur.kind != TokKind::Eof {
            clobbers.push(Self::strip_quotes(&self.cur.text));
            self.advance();
            if self.cur.kind == TokKind::Comma { self.advance(); }
        }
        clobbers
    }
    /// 解析 asm goto label 列表 (逗号分隔的标识符)
    fn parse_asm_label_list(&mut self) -> Vec<Ident> {
        let mut labels = vec![];
        while self.cur.kind != TokKind::RParen && self.cur.kind != TokKind::Eof {
            if let Some(id) = self.parse_ident() { labels.push(id); }
            if self.cur.kind == TokKind::Comma { self.advance(); }
        }
        labels
    }

    pub fn program(&mut self) -> (Program, Vec<Diagnostic>) {
        let mut decls = vec![];
        while self.cur.kind != TokKind::Eof {
            if self.cur.kind == TokKind::Semi { self.advance(); continue; }
            match self.parse_decl() {
                Some(d) => decls.extend(d),
                None => { if !self.finished { self.advance(); } else { break; } }
            }
        }
        (Program { decls }, std::mem::take(&mut self.diags))
    }

    fn parse_decl(&mut self) -> Option<Vec<Decl>> {
        // __extension__: GNU 扩展前缀，直接跳过
        if self.cur.kind == TokKind::Ident && self.cur.text == "__extension__" {
            self.advance();
        }
        // C23 [[attributes]]: 静默跳过
        self.skip_std_attributes();
        // constexpr: C23 编译期常量, 不影响代码生成, 静默跳过
        while self.cur.kind == TokKind::Ident && self.cur.text == "constexpr" {
            self.advance();
            self.skip_std_attributes();
        }
        // _Static_assert(expr, msg): C11 编译期断言
        if self.cur.kind == TokKind::Ident && self.cur.text == "_Static_assert" {
            return self.parse_static_assert().map(|_| vec![]);
        }
        // 预处理未完全展开的静态分支 key 声明 → 语义补全为 extern struct static_key_{false,true}
        if self.cur.kind == TokKind::Ident && self.cur.text.starts_with("_DECLARE_STATIC_KEY_") {
            return self.parse_static_key_decl();
        }
        // 解析声明开头的 __attribute__: GCC 扩展, 如 __attribute__((section(".init.text"))) int foo()
        let leading_attrs = self.parse_attributes();
        self.decl_storage = StorageClass::default();
        self.decl_attrs = ParsedAttrs::default();
        // 文件级 asm: 必须在 skip_asm_label 之前, 因为 __asm__("...") 与 asm 标签共享相同 token 模式
        if self.cur.kind == TokKind::Ident && (self.cur.text == "__asm__" || self.cur.text == "asm") {
            let sp = self.cur.span.clone();
            self.advance();
            // 可选的 volatile/const
            while matches!(self.cur.kind, TokKind::KwVolatile | TokKind::KwConst)
                || (self.cur.kind == TokKind::Ident && (self.cur.text == "__volatile__" || self.cur.text == "__const__")) {
                self.advance();
            }
            // 解析带约束的内联汇编表达式
            if self.cur.kind == TokKind::LParen {
                if let Some(expr) = self.parse_asm_expr(sp.clone()) {
                    if let Expr::AsmExpr { asm_str, outputs, output_names, output_constraints,
                        inputs, input_names, input_constraints, clobbers, span, .. } = expr {
                        if !self.expect(TokKind::Semi, "';'") { return None; }
                        return Some(vec![Decl::GlobalAsm {
                            asm_str, outputs, output_names, output_constraints,
                            inputs, input_names, input_constraints, clobbers, span,
                        }]);
                    }
                }
                return None;
            } else {
                self.err_here("E0001", "expected '(' after asm", Some("asm requires parentheses: asm(\"instructions\" : ...)"));
                return None;
            }
        }
        // 跳过声明开头的 __asm__("label") (在文件级 asm 之后, 避免误吞)
        self.skip_asm_label();
        // typedef
        if self.cur.kind == TokKind::KwTypedef {
            self.advance();
            let mut ty = self.parse_type_mode(true)?;
            ty = self.parse_ptr_suffix(ty);
            // typedef ret_type (name)(params) — 无 * 的函数 typedef
            if self.cur.kind == TokKind::LParen && self.peek.kind == TokKind::Ident {
                self.advance(); // (
                let name = self.parse_typedef_name()?;
                if !self.expect(TokKind::RParen, "')'") { return None; }
                if self.cur.kind == TokKind::LParen {
                    self.advance();
                    let params = self.parse_params()?;
                    if !self.expect(TokKind::RParen, "')'") { return None; }
                    let func_ty = Type::Func(Box::new(ty), params.iter().map(|p| p.ty.clone()).collect());
                    let sp = name.span.clone();
                    if !self.expect(TokKind::Semi, "';'") { return None; }
                    if !self.register_typedef(&name, func_ty.clone()) { return None; }
                    return Some(vec![Decl::Typedef { name, ty: func_ty, span: sp }]);
                }
            }
            // 函数指针 typedef: typedef ret_type (*name)(params)
            if self.cur.kind == TokKind::LParen && self.peek.kind == TokKind::Star {
                self.advance(); // (
                self.advance(); // *
                while matches!(self.cur.kind, TokKind::KwConst | TokKind::KwVolatile | TokKind::KwRestrict) { self.advance(); }
                let name = self.parse_typedef_name()?;
                if !self.expect(TokKind::RParen, "')'") { return None; }
                if self.cur.kind == TokKind::LParen {
                    self.advance();
                    let params = self.parse_params()?;
                    if !self.expect(TokKind::RParen, "')'") { return None; }
                    let func_ty = Type::Func(Box::new(ty), params.iter().map(|p| p.ty.clone()).collect());
                    let ptr_ty = Type::Ptr(Box::new(func_ty));
                    let sp = name.span.clone();
                    if !self.expect(TokKind::Semi, "';'") { return None; }
                    if !self.register_typedef(&name, ptr_ty.clone()) { return None; }
                    return Some(vec![Decl::Typedef { name, ty: ptr_ty, span: sp }]);
                }
            }
            let name = self.parse_typedef_name()?;
            let sp = name.span.clone();
            // 函数 typedef: typedef ret_type name(params)
            if self.cur.kind == TokKind::LParen {
                self.advance();
                let params = self.parse_params()?;
                if !self.expect(TokKind::RParen, "')'") { return None; }
                let func_ty = Type::Func(Box::new(ty.clone()), params.iter().map(|p| p.ty.clone()).collect());
                if !self.expect(TokKind::Semi, "';'") { return None; }
                if !self.register_typedef(&name, func_ty.clone()) { return None; }
                return Some(vec![Decl::Typedef { name, ty: func_ty, span: sp }]);
            }
            // 数组 typedef: typedef T name[N] / name[]
            let mut ty = ty;
            while self.cur.kind == TokKind::LBracket {
                self.advance();
                let arr_size = if self.cur.kind != TokKind::RBracket {
                    let e = self.parse_expr()?;
                    match &e {
                        Expr::Int { v, .. } => ArraySize::Const(*v as usize),
                        _ => ArraySize::Var(Box::new(e)),
                    }
                } else {
                    ArraySize::Const(0)
                };
                if !self.expect(TokKind::RBracket, "']'") { return None; }
                ty = Type::Array(Box::new(ty), arr_size);
            }
            let ty = self.try_parse_vector_attr(ty);
            if !self.expect(TokKind::Semi, "';'") { return None; }
            if !self.register_typedef(&name, ty.clone()) { return None; }
            return Some(vec![Decl::Typedef { name, ty, span: sp }]);
        }
        // struct/union/enum 定义或声明
        if matches!(self.cur.kind, TokKind::KwStruct | TokKind::KwUnion | TokKind::KwEnum) {
            // 只用声明说明符（不含 *）：每个声明符各自 parse_ptr_suffix
            // 否则 `struct T *a, *b` 会把第一个 * 吃进 ty，导致 b 变成 T**
            let ty = self.parse_decl_specifiers()?;
            let storage = self.decl_storage;
            let attrs = Self::merge_attrs(leading_attrs, std::mem::take(&mut self.decl_attrs));
            if self.cur.kind == TokKind::Semi { self.advance(); return Some(vec![]); }
            // 支持 `struct T *a, *b;` 多声明符
            let mut decls = vec![];
            loop {
                match self.parse_var_or_func(ty.clone(), attrs.clone(), storage) {
                    Some(ds) => decls.extend(ds),
                    None => break,
                }
                if self.cur.kind == TokKind::Comma {
                    self.advance();
                    continue;
                }
                break;
            }
            if decls.is_empty() { None } else { Some(decls) }
        } else {
        let ty = self.parse_decl_specifiers()?;
        // 单独的 struct/union/enum 定义: `__declspec(...) struct S { int x; };`
        // 解析说明符后类型是 struct/union/enum, 而当前 token 是分号,
        // 直接吃掉分号返回空 decl, 避免后面误调 parse_var_or_func.
        if matches!(ty, Type::Struct(_, _) | Type::Union(_, _) | Type::Enum(_, _))
            && self.cur.kind == TokKind::Semi
        {
            self.advance();
            return Some(vec![]);
        }
        let storage = self.decl_storage;
        let attrs = Self::merge_attrs(leading_attrs, std::mem::take(&mut self.decl_attrs));
        let mut decls = vec![];
        loop {
            match self.parse_var_or_func(ty.clone(), attrs.clone(), storage) {
                Some(ds) => decls.extend(ds),
                None => break,
            }
            if self.cur.kind == TokKind::Comma {
                self.advance();
                continue;
            }
            break;
        }
        if decls.is_empty() { None } else { Some(decls) }
        }
    }

    fn parse_static_assert(&mut self) -> Option<()> {
        let span = self.cur.span.clone();
        self.advance();
        if !self.expect(TokKind::LParen, "'('") { return None; }
        let condition = self.parse_expr_bp(2)?;
        // C23 允许省略消息; 默认为 "static assertion failed"
        let mut message = "static assertion failed".to_string();
        if self.cur.kind == TokKind::Comma {
            self.advance();
            if self.cur.kind == TokKind::StringLit {
                message = String::from_utf8_lossy(&parse_str(&self.cur.text))
                    .trim_end_matches('\0').to_string();
                self.advance();
            } else {
                self.err_here("E0012", "expected string literal in _Static_assert",
                    Some("the second argument must be a diagnostic string"));
                return None;
            }
        }
        if !self.expect(TokKind::RParen, "')'") { return None; }
        if !self.expect(TokKind::Semi, "';'") { return None; }
        // 在求值失败时附加条件表达式文本 (GCC 兼容: 错误消息后追加 "evaluates to <text>")
        let cond_text = format!("{}", const_eval::ExprDebug(&condition));
        match const_eval::eval(&condition) {
            Ok(0) => {
                if cond_text.len() > 0 && cond_text != "<expr>" {
                    self.diags.push(Diagnostic::err(
                        "E0010", format!("{} (evaluates to: {})", message, cond_text), span));
                } else {
                    self.diags.push(Diagnostic::err("E0010", message, span));
                }
            }
            Ok(_) => {}
            Err(ConstEvalError::DivisionByZero) => self.diags.push(Diagnostic::err(
                "E0011", format!("division by zero in static assertion (evaluates to: {})", cond_text), span)),
            Err(_) => self.diags.push(Diagnostic::err(
                "E0011", format!("{} (not an integer constant expression: {})", message, cond_text), span)),
        }
        Some(())
    }

    /// 将 `_DECLARE_STATIC_KEY_0(name)` / `_1(name)` 及
    /// `_DECLARE_STATIC_KEY_IS_ENABLED(cfg)(name)` 补全为 Linux jump_label 外部声明。
    fn parse_static_key_decl(&mut self) -> Option<Vec<Decl>> {
        let span = self.cur.span.clone();
        let macro_name = self.cur.text.clone();
        self.advance();
        // __PASTE 未完全展开时残留 _DECLARE_STATIC_KEY_IS_ENABLED(cfg)(name)
        let enabled = if macro_name == "_DECLARE_STATIC_KEY_IS_ENABLED" {
            if self.cur.kind == TokKind::LParen {
                self.advance();
                while self.cur.kind != TokKind::RParen && self.cur.kind != TokKind::Eof {
                    self.advance();
                }
                if self.cur.kind == TokKind::RParen { self.advance(); }
            }
            false
        } else {
            let suffix = macro_name.strip_prefix("_DECLARE_STATIC_KEY_")?;
            match suffix {
                "0" => false,
                "1" => true,
                _ => {
                    self.err_here(
                        "E0001",
                        format!("unknown static key macro '{macro_name}'"),
                        Some("expected _DECLARE_STATIC_KEY_0 or _DECLARE_STATIC_KEY_1"),
                    );
                    return None;
                }
            }
        };
        let struct_name = if enabled { "static_key_true" } else { "static_key_false" };
        if !self.struct_defs.contains_key(struct_name) {
            self.struct_defs.insert(
                struct_name.to_string(),
                (Arc::new(vec![]), false),
            );
        }
        if !self.expect(TokKind::LParen, "'('") { return None; }
        let name = if self.cur.kind == TokKind::Ident {
            let ident = Ident {
                name: self.cur.text.clone(),
                span: self.cur.span.clone(),
            };
            self.advance();
            ident
        } else {
            self.err_here("E0001", "expected identifier in static key declaration", None);
            return None;
        };
        if !self.expect(TokKind::RParen, "')'") { return None; }
        if !self.expect(TokKind::Semi, "';'") { return None; }
        let ty = Type::Struct(struct_name.to_string(), Arc::new(vec![]));
        self.globals.insert(name.name.clone(), ty.clone());
        Some(vec![Decl::Var(VarDecl {
            ty,
            name,
            init: None,
            span,
            is_extern: true,
            is_static: false,
            section: None,
            alignment: None,
            is_weak: false,
            visibility: None,
            is_unused: false,
            register_name: None,
            cleanup_fn: None,
            is_thread_local: false,
            no_sanitize: vec![],
            copy_fn: None,
        })])
    }

    fn parse_var_or_func(&mut self, ty: Type, leading_attrs: ParsedAttrs, storage: StorageClass) -> Option<Vec<Decl>> {
        // 函数指针声明/定义: type (*name)(params)
        // 第一性原理: (* 表示这不是普通变量名, 而是函数指针声明
        if self.cur.kind == TokKind::LParen && self.peek.kind == TokKind::Star {
            self.advance(); // (
            self.advance(); // *
            // 可选的 const/volatile/restrict 限定符
            while matches!(self.cur.kind, TokKind::KwConst | TokKind::KwVolatile | TokKind::KwRestrict) {
                self.advance();
            }
            let name = self.parse_ident()?;
            // (*name(params)): 返回指针的函数
            // (*name(params1))(params2): 返回函数指针的函数
            // (*name)(params) / (*name[N])(params): 函数指针（数组）
            if self.cur.kind == TokKind::LParen {
                self.advance();
                let inner_params = self.parse_params()?;
                if !self.expect(TokKind::RParen, "')'") { return None; }
                // 关闭 (*...) 分组
                if !self.expect(TokKind::RParen, "')'") { return None; }
                // 外层函数参数列表 (params2)
                if self.cur.kind == TokKind::LParen {
                    self.advance();
                    let outer_params = self.parse_params()?;
                    if !self.expect(TokKind::RParen, "')'") { return None; }
                    let inner_func = Type::Func(Box::new(ty), inner_params.iter().map(|p| p.ty.clone()).collect());
                    let ptr_ty = Type::Ptr(Box::new(inner_func));
                    let outer_func = Type::Func(Box::new(ptr_ty), outer_params.iter().map(|p| p.ty.clone()).collect());
                    self.skip_attributes();
                    self.skip_asm_label();
                    self.locals = self.globals.clone();
                    self.in_function = true;
                    let saved_func = std::mem::replace(&mut self.current_function, Some(name.name.clone()));
                    for p in &outer_params {
                        if let Some(nm) = &p.name { self.locals.insert(nm.name.clone(), p.ty.clone()); }
                    }
                    let body = if self.cur.kind == TokKind::LBrace {
                        self.advance();
                        let b = self.parse_block();
                        if !self.expect(TokKind::RBrace, "'}'" ) { return None; }
                        Some(b)
                    } else { if !self.expect(TokKind::Semi, "';' or '{'") { return None; } None };
                    self.skip_attributes();
                    self.locals = self.globals.clone();
                    self.in_function = false;
                    self.current_function = saved_func;
                    let sp = name.span.clone();
                    return Some(vec![Decl::Func(FuncDecl { ret_ty: outer_func, name, params: outer_params, body, span: sp, is_extern: storage.is_extern, is_static: storage.is_static, section: None, alignment: None, is_weak: false, visibility: None, is_unused: false, is_cold: false, is_noinline: false, is_always_inline: false, is_variadic: self.last_variadic, is_constructor: false, is_destructor: false, alias_name: None, is_nested: false, upvars: vec![], is_old_style: false, format_kind: None, format_string_idx: None, format_first_arg: None, no_sanitize: vec![], target_clones: vec![], returns_nonnull: false })]);
                }
                // (*name(params)) — 返回指针的函数，无外层参数表
                let ptr_ty = Type::Ptr(Box::new(ty));
                let func_ty = Type::Func(Box::new(ptr_ty.clone()), inner_params.iter().map(|p| p.ty.clone()).collect());
                self.skip_attributes();
                self.skip_asm_label();
                if !self.in_function {
                    self.globals.insert(name.name.clone(), func_ty.clone());
                }
                let body = if self.cur.kind == TokKind::LBrace {
                    self.advance();
                    self.locals = self.globals.clone();
                    self.in_function = true;
                    let saved_func = std::mem::replace(&mut self.current_function, Some(name.name.clone()));
                    for p in &inner_params {
                        if let Some(nm) = &p.name { self.locals.insert(nm.name.clone(), p.ty.clone()); }
                    }
                    let b = self.parse_block();
                    if !self.expect(TokKind::RBrace, "'}'") { return None; }
                    self.locals = self.globals.clone();
                    self.in_function = false;
                    self.current_function = saved_func;
                    Some(b)
                } else {
                    if !self.expect(TokKind::Semi, "';' or '{'") { return None; }
                    None
                };
                let sp = name.span.clone();
                return Some(vec![Decl::Func(FuncDecl {
                    ret_ty: ptr_ty, name, params: inner_params, body, span: sp,
                    is_extern: storage.is_extern, is_static: storage.is_static,
                    section: None, alignment: None, is_weak: false, visibility: None,
                    is_unused: false, is_cold: false, is_noinline: false, is_always_inline: false,
                    is_variadic: self.last_variadic, is_constructor: false, is_destructor: false,
                    alias_name: None, is_nested: false, upvars: vec![],
                    is_old_style: false, format_kind: None, format_string_idx: None, format_first_arg: None,
                    no_sanitize: vec![], target_clones: vec![], returns_nonnull: false,
                })]);
            }
            // 标准函数指针变量: type (*name)(params) 或 type (*name[N])(params)
            let array_sizes = {
                let mut sizes = Vec::new();
                while self.cur.kind == TokKind::LBracket {
                    self.advance();
                    let arr_size = if self.cur.kind != TokKind::RBracket {
                        let e = self.parse_expr()?;
                        match &e {
                            Expr::Int { v, .. } => ArraySize::Const(*v as usize),
                            _ => ArraySize::Var(Box::new(e)),
                        }
                    } else {
                        ArraySize::Const(0)
                    };
                    if !self.expect(TokKind::RBracket, "']'") { return None; }
                    sizes.push(arr_size);
                }
                sizes
            };
            if !self.expect(TokKind::RParen, "')'") { return None; }
            if self.cur.kind == TokKind::LParen {
                self.advance();
                let params = self.parse_params()?;
                if !self.expect(TokKind::RParen, "')'") { return None; }
                let func_ty = Type::Func(Box::new(ty.clone()), params.iter().map(|p| p.ty.clone()).collect());
                let mut ptr_ty = Type::Ptr(Box::new(func_ty));
                for sz in array_sizes.into_iter().rev() {
                    ptr_ty = Type::Array(Box::new(ptr_ty), sz);
                }
                self.skip_attributes();
                self.skip_asm_label();
                // 注册到变量表
                self.locals.insert(name.name.clone(), ptr_ty.clone());
                if !self.in_function { self.globals.insert(name.name.clone(), ptr_ty.clone()); }
                // 可选初始化器
                let init = if self.cur.kind == TokKind::Assign { self.advance(); Some(self.parse_expr_bp(2)?) } else { None };
                if !self.expect(TokKind::Semi, "';'") { return None; }
                let sp = name.span.clone();
                return Some(vec![Decl::Var(VarDecl { ty: ptr_ty, name, init, span: sp, is_extern: storage.is_extern, is_static: storage.is_static, section: None, alignment: None, is_weak: false, visibility: None, is_unused: false, register_name: None, cleanup_fn: None, is_thread_local: storage.is_thread_local, no_sanitize: vec![], copy_fn: None })]);
            }
            // 不是函数指针, 只是带括号的指针变量 (罕见)
            self.skip_attributes();
            self.skip_asm_label();
            let ptr_ty = Type::Ptr(Box::new(ty));
            let init = if self.cur.kind == TokKind::Assign { self.advance(); Some(self.parse_expr_bp(2)?) } else { None };
            if !self.expect(TokKind::Semi, "';'") { return None; }
            let sp = name.span.clone();
            return Some(vec![Decl::Var(VarDecl { ty: ptr_ty, name, init, span: sp, is_extern: storage.is_extern, is_static: storage.is_static, section: None, alignment: None, is_weak: false, visibility: None, is_unused: false, register_name: None, cleanup_fn: None, is_thread_local: storage.is_thread_local, no_sanitize: vec![], copy_fn: None })]);
        }
        // `char const *p`：const 在基类型之后、* 之前，须先于 parse_ptr_suffix 跳过
        while matches!(self.cur.kind, TokKind::KwConst | TokKind::KwVolatile | TokKind::KwRestrict)
            || (self.cur.kind == TokKind::Ident
                && matches!(self.cur.text.as_str(), "__const__" | "__volatile__" | "__restrict__"))
        {
            self.advance();
        }
        // void __attribute__(...) *fn(): 基本类型后的属性在 specifiers_only 路径下会残留
        self.skip_attributes();
        let ty = self.parse_ptr_suffix(ty);
        // 解析变量/函数名前的 __attribute__: 如 int __attribute__((unused)) x;
        let pre_attrs = Self::merge_attrs(leading_attrs, self.parse_attributes());
        self.skip_asm_label();
        // 括号包函数名防宏展开: size_t (htab_size) (htab_t h)
        let name = if self.cur.kind == TokKind::LParen && self.peek.kind == TokKind::Ident {
            self.advance(); // (
            let id = self.parse_ident()?;
            if !self.expect(TokKind::RParen, "')'") { return None; }
            id
        } else if self.cur.kind == TokKind::LParen && self.peek.kind != TokKind::Star {
            // 无名函数定义: int () { ... } — IR 阶段报 E1002
            Ident { name: String::new(), span: self.cur.span.clone() }
        } else {
            self.parse_ident()?
        };
        // C23 [[attributes]]: 在变量/函数名后出现，静默跳过
        self.skip_std_attributes();
        let sp = name.span.clone();
        let name_for_decl = name.clone();
        if self.cur.kind == TokKind::LParen {
            self.advance();
            let params = self.parse_params()?;
            if !self.expect(TokKind::RParen, "')'") { return None; }
            // K&R 风格函数: 参数后跟类型声明, 然后是函数体
            let mut is_old_style = false;
            let params = if !params.is_empty() && params.iter().all(|p| p.ty == Type::Int)
                && self.cur.kind != TokKind::LBrace && self.cur.kind != TokKind::Semi
                && self.is_type_start() {
                is_old_style = true;
                // 解析 K&R 参数类型声明
                let mut kr_params = params;
                while self.is_type_start() && self.cur.kind != TokKind::LBrace {
                    let ty = self.parse_type()?;
                    // 可能有多个参数名共享同一类型
                    while self.cur.kind == TokKind::Ident {
                        let name = self.cur.text.clone();
                        self.advance();
                        // 更新对应参数的类型
                        for p in &mut kr_params {
                            if let Some(n) = &p.name {
                                if n.name == name { p.ty = ty.clone(); }
                            }
                        }
                        if self.cur.kind == TokKind::Comma { self.advance(); }
                    }
                    if self.cur.kind == TokKind::Semi { self.advance(); }
                }
                kr_params
            } else { params };
            // 函数声明后: 解析 __attribute__ 和 __asm__ 标签
            let post_attrs = self.skip_post_decl_modifiers();
            let attrs = Self::merge_attrs(pre_attrs, post_attrs);
            // 检测是否是嵌套函数 (在外层函数体内)
            let is_nested_func = self.in_function;
            let parent_locals = if is_nested_func { self.locals.clone() } else { HashMap::new() };
            // 进入函数: 从全局变量表恢复, 清除局部变量
            let func_ty = Type::Func(
                Box::new(ty.clone()),
                params.iter().map(|p| p.ty.clone()).collect(),
            );
            if !is_nested_func {
                self.globals.insert(name_for_decl.name.clone(), func_ty);
            }
            self.locals = self.globals.clone();
            self.in_function = true;
            let saved_func = std::mem::replace(&mut self.current_function, Some(name_for_decl.name.clone()));
            for p in &params {
                if let Some(nm) = &p.name { self.locals.insert(nm.name.clone(), p.ty.clone()); }
            }
            let body = if self.cur.kind == TokKind::LBrace {
                self.advance();
                let b = self.parse_block();
                if !self.expect(TokKind::RBrace, "'}'") { return None; }
                Some(b)
            } else { if !self.expect(TokKind::Semi, "';' or '{'") { return None; } None };
            let child_locals = self.locals.clone();
            // 恢复外层函数的 in_function 状态
            if is_nested_func {
                self.locals = parent_locals.clone();
                self.in_function = true;
            } else {
                self.locals = self.globals.clone();
                self.in_function = false;
            }
            // 恢复外层 current_function (嵌套函数时为 outer；顶层函数时为 None)
            self.current_function = saved_func;
            // 检测嵌套函数捕获的外层变量
            let upvars = if is_nested_func {
                let mut captured = Vec::new();
                if let Some(ref b) = body {
                    Self::collect_upvars(&b.items, &child_locals, &parent_locals, &mut captured);
                }
                captured
            } else {
                vec![]
            };
            // 函数声明后不再解析额外属性 (避免吞掉下一个声明的属性)
            let attrs = attrs;
            // #pragma weak name 在 codegen 体现为 STB_WEAK 绑定, 此处合并
            let pragma_weak = self.is_weak_symbol(&name_for_decl.name);
            Some(vec![Decl::Func(FuncDecl { ret_ty: ty, name: name_for_decl, params, body, span: sp, is_extern: storage.is_extern, is_static: storage.is_static,
                section: attrs.section, alignment: attrs.alignment, is_weak: attrs.is_weak || pragma_weak,
                visibility: attrs.visibility, is_unused: attrs.is_unused,
                is_cold: attrs.is_cold, is_noinline: attrs.is_noinline, is_always_inline: attrs.is_always_inline,
                is_variadic: self.last_variadic, is_constructor: attrs.is_constructor,
                is_destructor: attrs.is_destructor, alias_name: attrs.alias_name.clone().or(attrs.copy_fn.clone()),
                is_nested: is_nested_func, upvars, is_old_style,
                format_kind: attrs.format_kind, format_string_idx: attrs.format_string_idx,
                format_first_arg: attrs.format_first_arg,
                no_sanitize: attrs.no_sanitize.clone(), target_clones: attrs.target_clones.clone(),
                returns_nonnull: attrs.returns_nonnull })])
        } else if self.cur.kind == TokKind::LBracket {
            let mut arr_ty = ty;
            while self.cur.kind == TokKind::LBracket {
                self.advance();
                let arr_size = if self.cur.kind != TokKind::RBracket {
                    let e = self.parse_expr()?;
                    match &e {
                        Expr::Int { v, .. } => ArraySize::Const(*v as usize),
                        _ => ArraySize::Var(Box::new(e)),
                    }
                } else { ArraySize::Const(0) };
                if !self.expect(TokKind::RBracket, "']'") { return None; }
                arr_ty = Type::Array(Box::new(arr_ty), arr_size);
            }
            let mut arr_attrs = pre_attrs.clone();
            arr_attrs.merge(self.parse_attributes());
            self.skip_asm_label(); // asm 标签
            let init = if self.cur.kind == TokKind::Assign { self.advance(); Some(self.parse_expr_bp(2)?) } else { None };
            // 从 InitList 推断最外层数组大小: data[] = {1,2,3} → data[3]
            if let (Type::Array(elem, ArraySize::Const(0)), Some(Expr::InitList { inits, .. })) = (&arr_ty, &init) {
                arr_ty = Type::Array(elem.clone(), ArraySize::Const(inits.len()));
            }
            self.locals.insert(name_for_decl.name.clone(), arr_ty.clone());
            if !self.in_function { self.globals.insert(name_for_decl.name.clone(), arr_ty.clone()); }
            if self.cur.kind != TokKind::Comma && !self.expect(TokKind::Semi, "';'") { return None; }
            Some(vec![Decl::Var(VarDecl { ty: arr_ty, name: name_for_decl, init, span: sp, is_extern: storage.is_extern, is_static: storage.is_static,
                section: arr_attrs.section, alignment: arr_attrs.alignment, is_weak: arr_attrs.is_weak,
                visibility: arr_attrs.visibility, is_unused: arr_attrs.is_unused, register_name: arr_attrs.register_name, cleanup_fn: arr_attrs.cleanup_fn, is_thread_local: storage.is_thread_local,
                no_sanitize: arr_attrs.no_sanitize.clone(), copy_fn: arr_attrs.copy_fn.clone() })])
        } else {
            if matches!(ty, Type::Void) {
                self.err_here(
                    "E0008",
                    "variable declared with void type",
                    Some("variables must have a complete object type; use 'void *' for generic pointers"),
                );
                return None;
            }
            if !name_for_decl.name.is_empty()
                && !self.check_var_redecl(&name_for_decl.name, &ty, sp.clone())
            {
                return None;
            }
            // 变量名后可能有 __attribute__((unused)) 和 __asm__("label") 等
            let var_attrs = self.parse_attributes();
            let mut attrs = Self::merge_attrs(pre_attrs, var_attrs);
            // __asm__("label"): 可能是 asm 标签或 register asm("rN")
            if let Some(asm_label) = self.skip_asm_label() {
                if storage.is_register && attrs.register_name.is_none() {
                    attrs.register_name = Some(asm_label);
                }
            }
            if self.cur.kind == TokKind::Assign {
                self.advance();
                // 初始化器可引用当前声明符（sizeof(*v)、typeof(v) 等）
                if !name_for_decl.name.is_empty() {
                    self.locals.insert(name_for_decl.name.clone(), ty.clone());
                }
                let init = self.parse_expr_bp(2)?;
                let decl_ty = if self.decl_auto_type {
                    self.decl_auto_type = false;
                    init.ty()
                } else {
                    ty.clone()
                };
                // 允许多声明符: T *a = 0, *b = 0;
                if self.cur.kind != TokKind::Comma && !self.expect(TokKind::Semi, "';'") { return None; }
                self.locals.insert(name_for_decl.name.clone(), decl_ty.clone());
                if !self.in_function { self.globals.insert(name_for_decl.name.clone(), decl_ty.clone()); }
                let pragma_weak = self.is_weak_symbol(&name_for_decl.name);
                Some(vec![Decl::Var(VarDecl { ty: decl_ty, name: name_for_decl, init: Some(init), span: sp, is_extern: storage.is_extern, is_static: storage.is_static,
                    section: attrs.section, alignment: attrs.alignment, is_weak: attrs.is_weak || pragma_weak,
                                        visibility: attrs.visibility, is_unused: attrs.is_unused, register_name: attrs.register_name, cleanup_fn: attrs.cleanup_fn, is_thread_local: storage.is_thread_local,
                                        no_sanitize: attrs.no_sanitize.clone(), copy_fn: attrs.copy_fn.clone() })])
            } else if self.cur.kind == TokKind::Semi {
                self.locals.insert(name_for_decl.name.clone(), ty.clone());
                if !self.in_function { self.globals.insert(name_for_decl.name.clone(), ty.clone()); }
                self.advance();
                let pragma_weak = self.is_weak_symbol(&name_for_decl.name);
                Some(vec![Decl::Var(VarDecl { ty, name: name_for_decl, init: None, span: sp, is_extern: storage.is_extern, is_static: storage.is_static,
                    section: attrs.section, alignment: attrs.alignment, is_weak: attrs.is_weak || pragma_weak,
                                        visibility: attrs.visibility, is_unused: attrs.is_unused, register_name: attrs.register_name, cleanup_fn: attrs.cleanup_fn, is_thread_local: storage.is_thread_local,
                                        no_sanitize: attrs.no_sanitize.clone(), copy_fn: attrs.copy_fn.clone() })])
            } else if self.cur.kind == TokKind::Comma {
                self.locals.insert(name_for_decl.name.clone(), ty.clone());
                if !self.in_function { self.globals.insert(name_for_decl.name.clone(), ty.clone()); }
                let pragma_weak = self.is_weak_symbol(&name_for_decl.name);
                Some(vec![Decl::Var(VarDecl { ty, name: name_for_decl, init: None, span: sp, is_extern: storage.is_extern, is_static: storage.is_static,
                    section: attrs.section, alignment: attrs.alignment, is_weak: attrs.is_weak || pragma_weak,
                                        visibility: attrs.visibility, is_unused: attrs.is_unused, register_name: attrs.register_name, cleanup_fn: attrs.cleanup_fn, is_thread_local: storage.is_thread_local,
                                        no_sanitize: attrs.no_sanitize.clone(), copy_fn: attrs.copy_fn.clone() })])
            } else {
                self.err_here("E0002", format!("expected ';', '=', '(', got {:?}", self.cur.kind), Some("declaration must end with ';' or have an initializer"));
                None
            }
        }
    }

    /// 判断 peek 是否像声明符起始（指针/标识符），但排除 __attribute__ 等后缀属性。
    fn peek_declarator_start(&self) -> bool {
        match self.peek.kind {
            TokKind::Star => true,
            TokKind::Ident => !matches!(
                self.peek.text.as_str(),
                "__attribute__" | "__asm__" | "__asm" | "__volatile__" | "__const__"
            ),
            _ => false,
        }
    }

    fn parse_decl_specifiers(&mut self) -> Option<Type> {
        self.parse_type_mode(true)
    }

    fn parse_type(&mut self) -> Option<Type> {
        self.parse_type_mode(false)
    }

    fn finish_type(&mut self, ty: Type, specifiers_only: bool) -> Type {
        let ty = self.try_parse_vector_attr(ty);
        if specifiers_only {
            return ty;
        }
        // `char const *p`：限定符可在基类型与 * 之间（C 声明符语法）
        while matches!(self.cur.kind, TokKind::KwConst | TokKind::KwVolatile | TokKind::KwRestrict)
            || (self.cur.kind == TokKind::Ident
                && matches!(self.cur.text.as_str(), "__const__" | "__volatile__" | "__restrict__"))
        {
            self.advance();
        }
        self.parse_ptr_suffix(ty)
    }

    fn parse_type_mode(&mut self, specifiers_only: bool) -> Option<Type> {
        let mut is_unsigned = false; let mut is_long = 0u8; let mut is_short = false;
        let mut base: Option<Type> = None;
        // 跳过存储类/const/volatile/_Atomic/__extension__/__attribute__
        loop {
            // C23 [[attributes]]: 在类型限定符之间出现，在 match 之前检查
            if self.cur.kind == TokKind::LBracket && self.peek.kind == TokKind::LBracket {
                self.skip_std_attributes();
                continue;
            }
            match self.cur.kind {
                TokKind::KwStatic => { self.decl_storage.is_static = true; self.advance(); }
                TokKind::KwExtern => { self.decl_storage.is_extern = true; self.advance(); }
                TokKind::KwRegister => { self.decl_storage.is_register = true; self.advance(); }
                TokKind::KwConst | TokKind::KwVolatile | TokKind::KwInline
                | TokKind::KwRestrict | TokKind::KwAuto => { self.advance(); }
                // _Noreturn: C11 函数限定符, 不影响代码生成, 静默跳过
                TokKind::Ident if self.cur.text == "_Noreturn" => { self.advance(); }
                // _Thread_local / __thread：线程局部存储
                TokKind::Ident if self.cur.text == "_Thread_local" || self.cur.text == "__thread" => {
                    self.decl_storage.is_thread_local = true;
                    self.advance();
                }
                TokKind::KwAtomic => {
                    self.advance();
                    // _Atomic(type) 形式: 解析内部类型并包装为 Atomic
                    if self.cur.kind == TokKind::LParen {
                        self.advance();
                        let inner = self.parse_type();
                        if self.cur.kind == TokKind::RParen { self.advance(); }
                        let ty = Type::Atomic(Box::new(inner?));
                        return Some(self.finish_type(ty, specifiers_only));
                    }
                    // _Atomic 作为类型限定符: 设置 is_atomic 标志
            // 注意: 不做类型包装, 让后续解析决定具体的类型上下文
            // 在 parse_ptr_suffix 中也会处理 _Atomic 限定符
            // 当前简化: 作为 qualifier 跳过, 不生成 Type::Atomic 包装
            // (详细的类型包装在 _Atomic(type) 语法中已完成)
                }
                // __extension__: GNU 扩展, 抑制警告, 直接跳过
                TokKind::Ident if self.cur.text == "__extension__" => { self.advance(); }
                // __attribute__: 在类型限定符之间出现, 跳过
                TokKind::Ident if self.cur.text == "__attribute__" => {
                    let attrs = self.parse_attributes();
                    self.decl_attrs.merge(attrs);
                }
                // __asm__("label"): 在类型限定符之间出现, 跳过
                TokKind::Ident if self.cur.text == "__asm__" || self.cur.text == "__asm" => { self.skip_asm_label(); }
                // __auto_type: GNU 扩展，类型由 = 后表达式推导
                TokKind::Ident if self.cur.text == "__auto_type" => {
                    self.advance();
                    self.decl_auto_type = true;
                    return Some(self.finish_type(Type::Int, specifiers_only));
                }
                // typeof(expr/type): GNU 扩展, 推导类型
                TokKind::Ident if self.cur.text == "typeof" || self.cur.text == "__typeof__" || self.cur.text == "__typeof" => {
                    self.advance();
                    if self.cur.kind == TokKind::LParen {
                        self.advance();
                        if self.typeof_operand_is_type_form() {
                            self.allow_named_type = true;
                            let ty = self.parse_type()?;
                            self.allow_named_type = false;
                            if self.cur.kind == TokKind::RParen { self.advance(); }
                            return Some(self.finish_type(ty, specifiers_only));
                        } else {
                            let e = self.parse_expr()?;
                            if self.cur.kind == TokKind::RParen { self.advance(); }
                            return Some(self.finish_type(e.ty(), specifiers_only));
                        }
                    }
                }
                // __const__/__volatile__/__restrict__: GNU 扩展拼写, 同 const/volatile/restrict
                TokKind::Ident if self.cur.text == "__const__" || self.cur.text == "__volatile__"
                    || self.cur.text == "__restrict__" => { self.advance(); }
                // MSVC 扩展 _ms_restrict: 视为 restrict 同义
                TokKind::Ident if self.cur.text == "_ms_restrict" => { self.advance(); }
                // MSVC 扩展 __declspec: 解析参数并转化为等价 GCC 属性
                // 形式: __declspec(align(8)) / __declspec(noreturn) / __declspec(thread)
                //       __declspec(dllimport) / __declspec(dllexport)
                TokKind::Ident if self.cur.text == "__declspec" => {
                    self.advance();
                    if self.cur.kind == TokKind::LParen {
                        self.advance();
                        // 解析 __declspec 的内容, 逐个 ident 应用等价属性
                        while self.cur.kind != TokKind::RParen && self.cur.kind != TokKind::Eof {
                            if self.cur.kind == TokKind::Ident {
                                let kw = self.cur.text.clone();
                                self.advance();
                                if kw == "thread" {
                                    // __declspec(thread) ≡ __thread (TLS 变量)
                                    self.decl_storage.is_thread_local = true;
                                } else if kw == "noreturn" {
                                    // __declspec(noreturn) 标记函数不返回, 用 cold + noinline 提示
                                    self.decl_attrs.is_cold = true;
                                    self.decl_attrs.is_noinline = true;
                                } else if kw == "align" {
                                    // __declspec(align(N)): 设置对齐
                                    if self.cur.kind == TokKind::LParen {
                                        self.advance();
                                        if self.cur.kind == TokKind::Number {
                                            if let Ok(n) = self.cur.text.parse::<usize>() {
                                                // 已有 alignment 则取较大者
                                                let cur = self.decl_attrs.alignment.unwrap_or(0);
                                                self.decl_attrs.alignment = Some(cur.max(n));
                                            }
                                            self.advance();
                                        }
                                        if self.cur.kind == TokKind::RParen { self.advance(); }
                                    }
                                }
                                // dllimport / dllexport / 其他: 接受但无 codegen 差异
                            } else if self.cur.kind == TokKind::Number {
                                self.advance();
                            } else if self.cur.kind == TokKind::LParen {
                                // 跳过一个带参数的子句 (例如 align(N))
                                self.advance();
                                let mut depth = 1i32;
                                while depth > 0 && self.cur.kind != TokKind::Eof {
                                    if self.cur.kind == TokKind::LParen { depth += 1; }
                                    else if self.cur.kind == TokKind::RParen { depth -= 1; }
                                    if depth > 0 { self.advance(); }
                                }
                                if self.cur.kind == TokKind::RParen { self.advance(); }
                            } else {
                                self.advance();
                            }
                        }
                        if self.cur.kind == TokKind::RParen { self.advance(); }
                    }
                }
                // MSVC 扩展 __pragma: 跳过参数
                TokKind::Ident if self.cur.text == "__pragma" => {
                    self.advance();
                    if self.cur.kind == TokKind::LParen {
                        self.advance();
                        let mut depth = 1i32;
                        while depth > 0 && self.cur.kind != TokKind::Eof {
                            if self.cur.kind == TokKind::LParen { depth += 1; }
                            else if self.cur.kind == TokKind::RParen { depth -= 1; }
                            self.advance();
                        }
                    }
                }
                // _Pragma("..."): C99 操作符
                TokKind::Ident if self.cur.text == "_Pragma" => {
                    self.advance();
                    if self.cur.kind == TokKind::LParen {
                        self.advance();
                        // 提取字符串字面量; 实际应用在 preproc 阶段已经完成
                        // (preproc 把 _Pragma(...) 整体收集到 pending_pragmas 后批量执行).
                        // 此处仅做语法消费, 不再额外处理.
                        if self.cur.kind == TokKind::StringLit {
                            self.advance();
                        }
                        while self.cur.kind != TokKind::RParen && self.cur.kind != TokKind::Eof {
                            self.advance();
                        }
                        if self.cur.kind == TokKind::RParen { self.advance(); }
                    }
                }
                // SysV AMD64 va_list: gp_offset, fp_offset,
                // overflow_arg_area, reg_save_area.  C ABI 中它是单元素数组；
                // AST 内保留等价的 24 字节对象，builtin 操作直接取得其地址。
                TokKind::Ident if self.cur.text == "__builtin_va_list" => {
                    self.advance();
                    base = Some(Type::Struct("__dcc_sysv_va_list".into(), Arc::new(vec![
                        Field { name: "gp_offset".into(), ty: Type::UInt, offset: 0, bit_width: None, bit_offset: 0 },
                        Field { name: "fp_offset".into(), ty: Type::UInt, offset: 4, bit_width: None, bit_offset: 0 },
                        Field { name: "overflow_arg_area".into(), ty: Type::Ptr(Box::new(Type::Void)), offset: 8, bit_width: None, bit_offset: 0 },
                        Field { name: "reg_save_area".into(), ty: Type::Ptr(Box::new(Type::Void)), offset: 16, bit_width: None, bit_offset: 0 },
                    ])));
                }
                // constexpr: C23 编译期常量, 静默跳过
                TokKind::Ident if self.cur.text == "constexpr" => { self.advance(); }
                // ISO/IEC TS 18661 / GCC：_FloatN 二进制浮点类型
                TokKind::Ident if matches!(
                    self.cur.text.as_str(),
                    "_Float32" | "_Float32x" | "_Float16"
                ) => {
                    base = Some(Type::Float);
                    self.advance();
                }
                TokKind::Ident if matches!(
                    self.cur.text.as_str(),
                    "_Float64" | "_Float64x"
                ) => {
                    base = Some(Type::Double);
                    self.advance();
                }
                TokKind::Ident if matches!(
                    self.cur.text.as_str(),
                    "_Float128" | "_Float128x"
                ) => {
                    base = Some(Type::LongDouble);
                    self.advance();
                }
                // _BitInt(N): C23 定宽整数类型
                // 用 base 类型 + 标志位, 让 resolve_base 统一处理 signed/unsigned
                TokKind::Ident if self.cur.text == "_BitInt" => {
                    self.advance();
                    let width = if self.cur.kind == TokKind::LParen {
                        self.advance();
                        let w = if self.cur.kind == TokKind::Number {
                            self.cur.text.parse::<u32>().unwrap_or(32)
                        } else { 32 };
                        if matches!(self.cur.kind, TokKind::Number) { self.advance(); }
                        if self.cur.kind == TokKind::RParen { self.advance(); }
                        w
                    } else { 32 };
                    // 映射到基础类型, signed/unsigned 由 is_unsigned 标志决定
                    match width {
                        0..=8  => { base = Some(Type::Char); }
                        9..=16 => { is_short = true; }
                        17..=32 => { base = Some(Type::Int); }
                        _ => { is_long = 2; }
                    }
                }
                // 内核属性宏 (__weak, __init 等): 仅在后面是标识符或指针时跳过
                TokKind::Ident if self.cur.text.starts_with("__")
                    && !self.typedefs.contains_key(&self.cur.text)
                    && self.peek_declarator_start() => { self.advance(); }
                _ => break,
            }
        }
        // 如果已从 _BitInt(typeof 等确定了 base 类型,立即解析避免后续 ident 误判
        // 注意: 必须放在 struct/union/enum/ident 检查之前
        if base.is_some() || is_unsigned || is_long > 0 || is_short {
            let ty = self.resolve_base(base, is_unsigned, is_long, is_short);
            return Some(self.finish_type(ty, specifiers_only));
        }
        // struct/union/enum
        if self.cur.kind == TokKind::KwStruct || self.cur.kind == TokKind::KwUnion {
            let is_u = self.cur.kind == TokKind::KwUnion;
            // struct 关键字所在行, 用于查询 #pragma pack
            let struct_line = self.cur.span.line;
            self.advance();
            // struct __attribute__((__may_alias__)) name
            self.skip_attributes();
            let nm = if self.cur.kind == TokKind::Ident { let n = self.cur.text.clone(); self.advance(); n } else { String::new() };
            if self.cur.kind == TokKind::LBrace {
                self.advance(); let fields = self.parse_fields();
                if !self.expect(TokKind::RBrace, "'}'") { return None; }
                let mut fields = fields;
                // 检查 struct 后的 __attribute__((packed/aligned(N)))
                // 注意: 使用 skip_attributes 正确处理嵌套括号, 如 section(".init.text")
                let mut pack = false; let align_n: Option<usize> = None;
                if self.cur.kind == TokKind::Ident && self.cur.text == "__attribute__" {
                    // 先跳过所有 __attribute__((...))，用深度计数支持嵌套括号
                    while self.cur.kind == TokKind::Ident && self.cur.text == "__attribute__" {
                        self.advance(); // __attribute__
                        if self.cur.kind == TokKind::LParen { self.advance(); } // (
                        if self.cur.kind == TokKind::LParen { self.advance(); } // (
                        let mut depth = 2i32;
                        while depth > 0 && self.cur.kind != TokKind::Eof {
                            // 识别属性名 (仅在 depth==2 时, 否则在嵌套括号内部)
                            if depth == 2 && self.cur.kind == TokKind::Ident && self.cur.text == "packed" {
                                pack = true;
                            }
                            if self.cur.kind == TokKind::LParen { depth += 1; }
                            if self.cur.kind == TokKind::RParen { depth -= 1; if depth == 0 { break; } }
                            self.advance();
                        }
                        if self.cur.kind == TokKind::RParen { self.advance(); } // 消耗外层的 )
                    }
                }
                if pack { Type::layout_struct_packed(&mut fields, is_u); }
                else {
                    // 优先级: __attribute__((packed)) > 当前 #pragma pack(n) > 自然对齐
                    let max_align = self.pack_at_line(struct_line);
                    Type::layout_struct_with_pack(&mut fields, is_u, max_align);
                }
                if let Some(a) = align_n { for f in fields.iter_mut() { f.offset = (f.offset + a - 1) & !(a - 1); } }
                let fields_rc = Arc::new(fields);
                self.struct_defs.insert(nm.clone(), (Arc::clone(&fields_rc), is_u));
                let ty = if is_u { Type::Union(nm, fields_rc) } else { Type::Struct(nm, fields_rc) };
                return Some(self.finish_type(ty, specifiers_only));
            } else if !nm.is_empty() {
                if let Some((f, _)) = self.struct_defs.get(&nm) {
                    let ty = if is_u { Type::Union(nm, Arc::clone(f)) } else { Type::Struct(nm, Arc::clone(f)) };
                    return Some(self.finish_type(ty, specifiers_only));
                }
                // 不完整类型: 登记占位，待后续定义补全字段
                let empty = Arc::new(vec![]);
                self.struct_defs.insert(nm.clone(), (Arc::clone(&empty), is_u));
                let ty = if is_u { Type::Union(nm, empty) } else { Type::Struct(nm, empty) };
                return Some(self.finish_type(ty, specifiers_only));
            }
            self.err_here("E0003", "expected struct name or '{'", Some("struct/union definition needs a name or body: struct Name { ... }")); return None;
        }
        if self.cur.kind == TokKind::KwEnum {
            self.advance();
            self.skip_attributes();
            let nm = if self.cur.kind == TokKind::Ident { let n = self.cur.text.clone(); self.advance(); n } else { String::new() };
            if self.cur.kind == TokKind::LBrace {
                self.advance(); let vs = self.parse_enum_body();
                if !self.expect(TokKind::RBrace, "'}'") { return None; }
                let vals: Vec<(String, i64)> = vs.iter().map(|(id, v)| (id.name.clone(), v.unwrap_or(0))).collect();
                let vals_rc = Arc::new(vals);
                self.enum_defs.insert(nm.clone(), Arc::clone(&vals_rc));
                let mut nv = 0i64;
                for (id, v) in &vs { let val = v.unwrap_or(nv); self.enum_consts.insert(id.name.clone(), val); nv = val + 1; }
                return Some(self.finish_type(Type::Enum(nm, vals_rc), specifiers_only));
            } else if !nm.is_empty() {
                if let Some(v) = self.enum_defs.get(&nm) { return Some(self.finish_type(Type::Enum(nm, Arc::clone(v)), specifiers_only)); }
                return Some(self.finish_type(Type::Enum(nm, Arc::new(vec![])), specifiers_only));
            }
        }
        // typedef 引用
        if self.cur.kind == TokKind::Ident {
            if let Some(ty) = self.typedefs.get(&self.cur.text).cloned() {
                self.advance();
                // 跳过 typedef 名后的 __attribute__: 如 asmlinkage = __attribute__((aligned(4)))
                self.skip_attributes();
                return Some(self.finish_type(ty, specifiers_only));
            }
            if self.allow_named_type {
                let name = self.cur.text.clone();
                self.advance();
                return Some(self.finish_type(Type::Named(name), specifiers_only));
            }
            if specifiers_only && self.has_storage_without_base(&base) && self.peek_decl_context() {
                self.warn_here(
                    "W0001",
                    "type defaults to 'int' in declaration without a type specifier",
                    Some("add an explicit type such as 'int'"),
                );
                base = Some(Type::Int);
            } else {
            let name = self.cur.text.clone();
            if self.opts.permissive {
                self.warn_here(
                    "W0002",
                    format!("unknown type '{}' treated as 'int' (-fpermissive)", name),
                    Some("declare the typedef or remove -fpermissive for strict checking"),
                );
                self.advance();
                return Some(self.finish_type(Type::Int, specifiers_only));
            }
            self.diags.push(Diagnostic::err("E0009",
                format!("unknown type or typedef '{}'", name),
                self.cur.span.clone()).with_hint("declare the typedef before using it"));
            self.advance();
            return None;
            }
        }
        // 基本类型
        let mut is_complex = false;
        loop {
            match self.cur.kind {
                TokKind::KwComplex => { is_complex = true; self.advance(); }
                TokKind::KwInt => { base = Some(Type::Int); self.advance(); }
                TokKind::KwChar => { base = Some(Type::Char); self.advance(); }
                TokKind::KwShort => { is_short = true; self.advance(); }
                TokKind::KwLong => { is_long += 1; self.advance(); }
                TokKind::KwVoid => { base = Some(Type::Void); self.advance(); break; }
                TokKind::KwBool => { base = Some(Type::Bool); self.advance(); break; }
                TokKind::KwFloat => { base = Some(Type::Float); self.advance(); break; }
                TokKind::KwDouble => { base = Some(Type::Double); self.advance(); break; }
                TokKind::KwDecimal32 => { base = Some(Type::Float); self.advance(); break; }
                TokKind::KwDecimal64 => { base = Some(Type::Double); self.advance(); break; }
                TokKind::KwDecimal128 => { base = Some(Type::LongDouble); self.advance(); break; }
                TokKind::KwInt128 => { base = Some(Type::Int128); self.advance(); break; }
                TokKind::KwUInt128 => { base = Some(Type::UInt128); self.advance(); break; }
                TokKind::KwFract => { base = Some(Type::Fract); self.advance(); break; }
                TokKind::KwAccum => { base = Some(Type::Accum); self.advance(); break; }
                TokKind::KwSigned => { self.advance(); }
                TokKind::KwUnsigned => { is_unsigned = true; self.advance(); }
                // _BitInt(N): C23 定宽整数类型 (也在第一个 loop 中处理,但 unsigned 在第二个 loop)
                TokKind::Ident if self.cur.text == "_BitInt" => {
                    self.advance();
                    let width = if self.cur.kind == TokKind::LParen {
                        self.advance();
                        let w = if self.cur.kind == TokKind::Number {
                            self.cur.text.parse::<u32>().unwrap_or(32)
                        } else { 32 };
                        if matches!(self.cur.kind, TokKind::Number) { self.advance(); }
                        if self.cur.kind == TokKind::RParen { self.advance(); }
                        w
                    } else { 32 };
                    match width {
                        0..=8  => { base = Some(Type::Char); }
                        9..=16 => { is_short = true; }
                        17..=32 => { base = Some(Type::Int); }
                        _ => { is_long = 2; }
                    }
                }
                // constexpr: 在第二个 loop 中也可能出现 (如 unsigned constexpr _BitInt)
                TokKind::Ident if self.cur.text == "constexpr" => { self.advance(); }
                _ => break,
            }
        }
        // 消耗类型后的内核属性宏 (仅在后面是标识符或指针时, 避免误吞函数名)
        while self.cur.kind == TokKind::Ident && self.cur.text.starts_with("__")
            && !self.typedefs.contains_key(&self.cur.text)
            && self.cur.text != "__attribute__" && self.cur.text != "__asm__" && self.cur.text != "__asm"
            && self.peek_declarator_start() {
            self.advance();
        }
        if base.is_some() || is_unsigned || is_long > 0 || is_short || is_complex {
            let ty = self.resolve_base(base, is_unsigned, is_long, is_short);
            let ty = if is_complex { Type::Complex(Box::new(ty)) } else { ty };
            Some(self.finish_type(ty, specifiers_only))
        } else {
            self.err_here("E0003", format!("expected type, got {:?}", self.cur.kind), Some("use a known type like int, void, or a typedef/struct/enum")); None
        }
    }

    fn resolve_base(&self, base: Option<Type>, uns: bool, long: u8, short: bool) -> Type {
        if let Some(ty @ (Type::Struct(_, _) | Type::Union(_, _) | Type::Enum(_, _) | Type::Atomic(_))) = base {
            return ty;
        }
        match (base, uns, long, short) {
            (Some(Type::Void), ..) => Type::Void,
            (Some(Type::Bool), ..) => Type::Bool,
            (Some(Type::Int128), true, ..) => Type::UInt128,
            (Some(Type::Int128), false, ..) => Type::Int128,
            (Some(Type::UInt128), ..) => Type::UInt128,
            (Some(Type::Float), ..) => Type::Float,
            (Some(Type::Double), _, 0, _) => Type::Double,
            (Some(Type::Double), _, _, _) => Type::LongDouble,
            (Some(Type::LongDouble), ..) => Type::LongDouble,  // _Decimal128 直接映射
            (Some(Type::Fract), ..) => Type::Fract,
            (Some(Type::Accum), ..) => Type::Accum,
            (Some(Type::Char), false, ..) => Type::SChar,
            (Some(Type::Char), true, ..) => Type::UChar,
            (_, _, _, true) if uns => Type::UShort,
            (_, _, _, true) => Type::Short,
            (_, true, 0, _) => Type::UInt,
            (_, true, 1, _) => Type::ULong,
            (_, true, _, _) => Type::ULongLong,
            (_, false, 0, false) => Type::Int,
            (_, false, 1, _) => Type::Long,
            (_, false, _, _) => Type::LongLong,
            // 仅有修饰符无基类型时, 默认基类型为 int (C 标准隐式 int 规则)
            _ => Type::Int,
        }
    }

    fn skip_ptr_qualifiers(&mut self) {
        while matches!(self.cur.kind, TokKind::KwConst | TokKind::KwVolatile | TokKind::KwAtomic | TokKind::KwRestrict)
            || (self.cur.kind == TokKind::Ident
                && matches!(self.cur.text.as_str(), "__restrict__" | "__const__" | "__volatile__" | "__attribute__" | "_ms_restrict" | "_restrict"))
        {
            if self.cur.text == "__attribute__" { self.skip_attributes(); }
            else { self.advance(); }
        }
    }

    fn parse_ptr_suffix(&mut self, mut ty: Type) -> Type {
        while self.cur.kind == TokKind::Star {
            self.advance();
            self.skip_ptr_qualifiers();
            ty = Type::Ptr(Box::new(ty));
        }
        ty
    }

    /// 解析 __attribute__((vector_size(N))) 等属性, 返回可能包装为 Vector 的类型
    fn try_parse_vector_attr(&mut self, ty: Type) -> Type {
        if self.cur.kind != TokKind::Ident || self.cur.text != "__attribute__" { return ty; }
        self.advance(); // __attribute__
        if self.cur.kind != TokKind::LParen { return ty; }
        self.advance(); // (
        if self.cur.kind != TokKind::LParen { return ty; }
        self.advance(); // (
        // 解析 vector_size(N)
        if self.cur.kind == TokKind::Ident && self.cur.text == "vector_size" {
            self.advance(); // vector_size
            if self.cur.kind == TokKind::LParen {
                self.advance(); // (
                let size = if self.cur.kind == TokKind::Number {
                    let v: usize = self.cur.text.parse().unwrap_or(16);
                    self.advance();
                    v
                } else { 16 };
                if self.cur.kind == TokKind::RParen { self.advance(); } // )
                let elem_size = ty.size().max(1);
                let count = size / elem_size;
                // 消耗 ))
                if self.cur.kind == TokKind::RParen { self.advance(); }
                if self.cur.kind == TokKind::RParen { self.advance(); }
                return Type::Vector(Box::new(ty), count);
            }
        }
        // 其他属性: 跳过直到 ))
        let mut depth = 2i32; // 已在两层括号内
        while depth > 0 && self.cur.kind != TokKind::Eof {
            if self.cur.kind == TokKind::LParen { depth += 1; }
            if self.cur.kind == TokKind::RParen { depth -= 1; }
            self.advance();
        }
        ty
    }

    fn parse_field_declarator(&mut self, spec: Type) -> (Type, String) {
        // 函数指针字段: type (*name)(params) / type (*name[N])(params)
        if self.cur.kind == TokKind::LParen && self.peek.kind == TokKind::Star {
            self.advance(); // (
            self.advance(); // *
            while matches!(self.cur.kind, TokKind::KwConst | TokKind::KwVolatile | TokKind::KwRestrict) {
                self.advance();
            }
            let nm = if self.cur.kind == TokKind::Ident {
                let n = self.cur.text.clone();
                self.advance();
                n
            } else {
                String::new()
            };
            // 可选数组维: (*fns[32])
            let mut array_sizes = Vec::new();
            while self.cur.kind == TokKind::LBracket {
                self.advance();
                let arr_size = if self.cur.kind != TokKind::RBracket {
                    let e = self.parse_expr().unwrap_or(Expr::Int { v: 0, span: Span::point("", 0, 0), ty: Type::Int });
                    match &e {
                        Expr::Int { v, .. } => ArraySize::Const(*v as usize),
                        _ => ArraySize::Var(Box::new(e)),
                    }
                } else {
                    ArraySize::Const(0)
                };
                let _ = self.expect(TokKind::RBracket, "']'");
                array_sizes.push(arr_size);
            }
            if self.cur.kind == TokKind::RParen {
                self.advance();
            }
            let ty = if self.cur.kind == TokKind::LParen {
                self.advance();
                let params = self.parse_params().unwrap_or_default();
                if self.cur.kind == TokKind::RParen {
                    self.advance();
                }
                let func_ty = Type::Func(
                    Box::new(spec),
                    params.iter().map(|p| p.ty.clone()).collect(),
                );
                let mut ty = Type::Ptr(Box::new(func_ty));
                for sz in array_sizes.into_iter().rev() {
                    ty = Type::Array(Box::new(ty), sz);
                }
                ty
            } else {
                let mut ty = Type::Ptr(Box::new(spec));
                for sz in array_sizes.into_iter().rev() {
                    ty = Type::Array(Box::new(ty), sz);
                }
                ty
            };
            (ty, nm)
        } else {
            let mut ty = self.parse_ptr_suffix(spec);
            // `void *(*name)(params)`：指针后再跟函数指针声明符
            if self.cur.kind == TokKind::LParen && self.peek.kind == TokKind::Star {
                self.advance(); // (
                self.advance(); // *
                while matches!(self.cur.kind, TokKind::KwConst | TokKind::KwVolatile | TokKind::KwRestrict) {
                    self.advance();
                }
                let nm = if self.cur.kind == TokKind::Ident {
                    let n = self.cur.text.clone();
                    self.advance();
                    n
                } else {
                    String::new()
                };
                if self.cur.kind == TokKind::RParen {
                    self.advance();
                }
                if self.cur.kind == TokKind::LParen {
                    self.advance();
                    let params = self.parse_params().unwrap_or_default();
                    if self.cur.kind == TokKind::RParen {
                        self.advance();
                    }
                    let func_ty = Type::Func(
                        Box::new(ty),
                        params.iter().map(|p| p.ty.clone()).collect(),
                    );
                    ty = Type::Ptr(Box::new(func_ty));
                } else {
                    ty = Type::Ptr(Box::new(ty));
                }
                return (ty, nm);
            }
            self.skip_attributes();
            let nm = if self.cur.kind == TokKind::Ident {
                let n = self.cur.text.clone();
                self.advance();
                n
            } else {
                String::new()
            };
            (ty, nm)
        }
    }

    fn parse_fields(&mut self) -> Vec<Field> {
        let mut fs = vec![];
        while self.cur.kind != TokKind::RBrace && self.cur.kind != TokKind::Eof {
            // GCC/内核宏偶发空字段: struct { int a; ; }
            if self.cur.kind == TokKind::Semi {
                self.advance();
                continue;
            }
            let spec = match self.parse_decl_specifiers() {
                Some(t) => t,
                None => { self.advance(); continue; }
            };
            // 同一声明说明符后可用逗号分隔多个声明符: *next, *prev / a, b
            loop {
                let (mut ty, nm) = self.parse_field_declarator(spec.clone());
                self.skip_attributes();
                let bw = if self.cur.kind == TokKind::Colon {
                    self.advance();
                    // 位域宽度必须是单个表达式，不能含逗号（同类型多个位域: a:8, b:4）
                    if let Some(e) = self.parse_expr_bp(2) {
                        if let Expr::Int { v, .. } = e {
                            Some(v as u32)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };
                ty = {
                    let mut arr_ty = ty;
                    while self.cur.kind == TokKind::LBracket {
                        self.advance();
                        let arr_size = if self.cur.kind != TokKind::RBracket {
                            let e = self.parse_expr().unwrap_or(Expr::Int { v: 0, span: Span::point("", 0, 0), ty: Type::Int });
                            if let Ok(v) = crate::const_eval::eval(&e) {
                                ArraySize::Const(v.max(0) as usize)
                            } else {
                                match &e {
                                    Expr::Int { v, .. } => ArraySize::Const(*v as usize),
                                    _ => ArraySize::Var(Box::new(e)),
                                }
                            }
                        } else {
                            ArraySize::Const(0)
                        };
                        let _ = self.expect(TokKind::RBracket, "']'");
                        arr_ty = Type::Array(Box::new(arr_ty), arr_size);
                    }
                    arr_ty
                };
                // 数组/名字后的 __attribute__((aligned(...))) 等
                self.skip_attributes();
                fs.push(Field { name: nm, ty, offset: 0, bit_width: bw, bit_offset: 0 });
                if self.cur.kind == TokKind::Comma {
                    self.advance();
                    continue;
                }
                if self.cur.kind == TokKind::Semi {
                    self.advance();
                }
                break;
            }
        }
        fs
    }

    fn parse_enum_body(&mut self) -> Vec<(Ident, Option<i64>)> {
        let mut vs = vec![];
        let mut seen = std::collections::HashSet::new();
        while self.cur.kind != TokKind::RBrace && self.cur.kind != TokKind::Eof {
            if self.cur.kind == TokKind::Comma { self.advance(); continue; }
            let nm = match self.parse_ident() { Some(id) => id, None => { self.advance(); continue; } };
            if !seen.insert(nm.name.clone()) {
                self.err_here(
                    "E0005",
                    format!("duplicate enum member '{}'", nm.name),
                    Some("each enumerator name must be unique"),
                );
            }
            let val = if self.cur.kind == TokKind::Assign {
                self.advance();
                // 枚举值必须是整型常量表达式，不能含逗号运算符 (A = 0, B 会误吞后续成员)
                if let Some(Expr::Int { v, .. }) = self.parse_expr_bp(2) { Some(v as i64) } else { None }
            } else { None };
            vs.push((nm, val));
            if self.cur.kind == TokKind::Comma { self.advance(); }
        }
        vs
    }

    fn parse_typedef_name(&mut self) -> Option<Ident> {
        match self.cur.kind {
            TokKind::Ident | TokKind::KwBool => {
                let id = Ident { name: self.cur.text.clone(), span: self.cur.span.clone() };
                self.advance();
                Some(id)
            }
            _ => {
                self.err_here(
                    "E0004",
                    format!("expected identifier, got {:?}", self.cur.kind),
                    Some("identifiers must start with a letter or '_'"),
                );
                None
            }
        }
    }

    fn parse_ident(&mut self) -> Option<Ident> {
        if self.cur.kind != TokKind::Ident { self.err_here("E0004", format!("expected identifier, got {:?}", self.cur.kind), Some("identifiers must start with a letter or '_'")); return None; }
        let id = Ident { name: self.cur.text.clone(), span: self.cur.span.clone() }; self.advance(); Some(id)
    }

    fn parse_params(&mut self) -> Option<Vec<Param>> {
        let mut ps = vec![];
        self.last_variadic = false;
        if self.cur.kind == TokKind::KwVoid && self.peek.kind == TokKind::RParen { self.advance(); return Some(ps); }
        if self.cur.kind == TokKind::RParen { return Some(ps); }
        if self.cur.kind == TokKind::Ellipsis { self.advance(); self.last_variadic = true; return Some(ps); }
        // K&R 风格函数: 参数只有标识符, 没有类型 (如 func(a, b, c))
        // 检测: 当前是 Ident 且不是类型起始 (不是 typedef/typeof 等)
        if self.cur.kind == TokKind::Ident && !self.is_type_start() {
            let mut kr_names = vec![];
            while self.cur.kind == TokKind::Ident {
                kr_names.push(self.cur.text.clone());
                self.advance();
                if self.cur.kind == TokKind::Comma { self.advance(); }
            }
            // 返回特殊标记: 参数名列表, 类型默认为 int
            for name in &kr_names {
                ps.push(Param { ty: Type::Int, name: Some(Ident { name: name.clone(), span: Span::point("", 0, 0) }), span: Span::point("", 0, 0) });
            }
            // 设置 is_kr 标志 (通过返回特殊 Param, 后续在 parse_var_or_func 中处理)
            return Some(ps);
        }
        loop {
            // 函数指针参数: ret_type (*name)(params)
            // 第一性原理: 当类型后跟 (* 时, 这是函数指针参数而非普通参数
            // 注意: _BitInt(N) 和 constexpr 的 ( 不是函数指针指示符, 跳过前瞻
            if self.is_type_start() && self.peek.kind == TokKind::LParen
                && !(self.cur.kind == TokKind::Ident && self.cur.text == "_BitInt")
                && !(self.cur.kind == TokKind::Ident && self.cur.text == "constexpr") {
                // 检查是否是函数指针参数模式: type (* ...)
                let saved_cur = self.cur.clone();
                let saved_peek = self.peek.clone();
                if let Some(ret_ty) = self.parse_type() {
                    if self.cur.kind == TokKind::LParen && self.peek.kind == TokKind::Star {
                        self.advance(); // (
                        self.advance(); // *
                        while matches!(self.cur.kind, TokKind::KwConst | TokKind::KwVolatile | TokKind::KwRestrict)
                            || (self.cur.kind == TokKind::Ident && self.cur.text == "_ms_restrict") { self.advance(); }
                        let nm = if self.cur.kind == TokKind::Ident {
                            let id = Ident { name: self.cur.text.clone(), span: self.cur.span.clone() };
                            self.advance(); Some(id)
                        } else { None };
                        self.skip_attributes();
                        if !self.expect(TokKind::RParen, "')'") { return None; }
                        if self.cur.kind == TokKind::LParen {
                            self.advance();
                            let inner_params = self.parse_params()?;
                            if !self.expect(TokKind::RParen, "')'") { return None; }
                            let func_ty = Type::Func(Box::new(ret_ty), inner_params.iter().map(|p| p.ty.clone()).collect());
                            let ptr_ty = Type::Ptr(Box::new(func_ty));
                            ps.push(Param { ty: ptr_ty, name: nm, span: saved_cur.span.clone() });
                            if self.cur.kind == TokKind::Comma { self.advance();
                                if self.cur.kind == TokKind::Ellipsis { self.advance(); self.last_variadic = true; break; }
                                continue;
                            }
                            if self.cur.kind == TokKind::Ellipsis { self.advance(); self.last_variadic = true; }
                            break;
                        }
                    }
                    // 不是函数指针参数, 恢复并走普通路径
                    self.cur = saved_cur;
                    self.peek = saved_peek;
                } else {
                    self.cur = saved_cur;
                    self.peek = saved_peek;
                }
            }
            let param_span = self.cur.span.clone();
            let mut ty = self.parse_type()?;
            // 普通路径可能漏掉指针: parse_type 已含 *, 这里再吞一层 qual
            self.skip_ptr_qualifiers();
            // 嵌套函数指针参数: char *(*pfn)(const char *)
            if self.cur.kind == TokKind::LParen && self.peek.kind == TokKind::Star {
                self.advance(); // (
                self.advance(); // *
                while matches!(self.cur.kind, TokKind::KwConst | TokKind::KwVolatile | TokKind::KwRestrict) {
                    self.advance();
                }
                let nm = if self.cur.kind == TokKind::Ident {
                    let id = Ident { name: self.cur.text.clone(), span: self.cur.span.clone() };
                    self.advance();
                    Some(id)
                } else {
                    None
                };
                self.skip_attributes();
                if !self.expect(TokKind::RParen, "')'") { return None; }
                if self.cur.kind == TokKind::LParen {
                    self.advance();
                    let inner_params = self.parse_params()?;
                    if !self.expect(TokKind::RParen, "')'") { return None; }
                    let func_ty = Type::Func(Box::new(ty), inner_params.iter().map(|p| p.ty.clone()).collect());
                    ty = Type::Ptr(Box::new(func_ty));
                } else {
                    ty = Type::Ptr(Box::new(ty));
                }
                ps.push(Param { ty, name: nm, span: param_span });
                if self.cur.kind == TokKind::Comma {
                    self.advance();
                    if self.cur.kind == TokKind::Ellipsis { self.advance(); self.last_variadic = true; break; }
                    continue;
                }
                if self.cur.kind == TokKind::Ellipsis { self.advance(); self.last_variadic = true; }
                break;
            }
            let nm = if self.cur.kind == TokKind::Ident { let id = Ident { name: self.cur.text.clone(), span: self.cur.span.clone() }; self.advance(); Some(id) } else { None };
            // 参数名后的 __attribute__((unused)) 等
            self.skip_attributes();
            let mut ty = ty;
            if self.cur.kind == TokKind::LBracket {
                self.advance();
                // 跳过 C99 数组参数限定符: [restrict] / [static N] / [const]
                while matches!(self.cur.kind, TokKind::KwConst | TokKind::KwVolatile | TokKind::KwRestrict | TokKind::KwStatic)
                    || (self.cur.kind == TokKind::Ident
                        && matches!(self.cur.text.as_str(), "__restrict__" | "__restrict" | "__const__" | "__volatile__"))
                {
                    self.advance();
                }
                let arr_size = if self.cur.kind != TokKind::RBracket {
                    let e = self.parse_expr()?;
                    match &e {
                        Expr::Int { v, .. } => ArraySize::Const(*v as usize),
                        _ => ArraySize::Var(Box::new(e)),
                    }
                } else {
                    ArraySize::Const(0)
                };
                if !self.expect(TokKind::RBracket, "']'") { return None; }
                ty = Type::Array(Box::new(ty), arr_size);
            }
            let ty = match ty { Type::Array(e, _) => Type::Ptr(e), o => o };
            ps.push(Param { ty, name: nm, span: self.cur.span.clone() });
            if self.cur.kind == TokKind::Comma { self.advance();
                if self.cur.kind == TokKind::Ellipsis { self.advance(); self.last_variadic = true; break; }
                continue;
            }
            if self.cur.kind == TokKind::Ellipsis { self.advance(); self.last_variadic = true; }
            break;
        }
        Some(ps)
    }

    fn parse_block(&mut self) -> Block {
        let mut items = vec![];
        loop {
            if self.cur.kind == TokKind::RBrace || self.cur.kind == TokKind::Eof { break; }
            // C23 [[attributes]]: 在块级作用域中出现，静默跳过
            if self.cur.kind == TokKind::LBracket && self.peek.kind == TokKind::LBracket {
                self.skip_std_attributes();
            }
            if self.cur.kind == TokKind::Ident && self.cur.text == "_Static_assert" {
                let _ = self.parse_static_assert();
            } else if self.is_decl_start() {
                match self.parse_decl() { Some(ds) => { for d in ds { items.push(BlockItem::Decl(d)); } } None => { self.advance(); } }
            } else {
                match self.parse_stmt() { Some(s) => items.push(BlockItem::Stmt(s)), None => { self.advance(); } }
            }
        }
        Block { items, span: self.cur.span.clone() }
    }

    fn is_type_start(&self) -> bool {
        matches!(self.cur.kind, TokKind::KwInt | TokKind::KwChar | TokKind::KwLong | TokKind::KwVoid
            | TokKind::KwShort | TokKind::KwUnsigned | TokKind::KwSigned | TokKind::KwFloat
            | TokKind::KwDouble | TokKind::KwStruct | TokKind::KwUnion | TokKind::KwEnum
            | TokKind::KwConst | TokKind::KwVolatile | TokKind::KwStatic | TokKind::KwExtern
            | TokKind::KwInline | TokKind::KwRegister | TokKind::KwAuto | TokKind::KwTypedef
            | TokKind::KwAtomic | TokKind::KwFract | TokKind::KwAccum | TokKind::KwComplex
            | TokKind::KwDecimal32 | TokKind::KwDecimal64 | TokKind::KwDecimal128 | TokKind::KwBool
            | TokKind::KwInt128 | TokKind::KwUInt128)
            || (self.cur.kind == TokKind::Ident
                && Self::is_type_specifier_ident(&self.cur.text, &self.typedefs))
    }

    /// 声明上下文允许把 `unknown_type value` 交给 parse_type 产生精确诊断，
    /// 但表达式中的 `(value * other)` 不能因此被误判为类型名。
    fn is_decl_start(&self) -> bool {
        self.is_type_start()
            || (self.cur.kind == TokKind::Ident
                && self.cur.text != "_Generic"
                && self.cur.text != "nullptr"
                && Self::is_type_specifier_ident(&self.cur.text, &self.typedefs)
                && matches!(self.peek.kind, TokKind::Ident | TokKind::Star))
    }

    fn is_peek_type_start(&self) -> bool {
        matches!(self.peek.kind, TokKind::KwInt | TokKind::KwChar | TokKind::KwLong | TokKind::KwVoid
            | TokKind::KwShort | TokKind::KwUnsigned | TokKind::KwSigned | TokKind::KwFloat
            | TokKind::KwDouble | TokKind::KwStruct | TokKind::KwUnion | TokKind::KwEnum
            | TokKind::KwConst | TokKind::KwVolatile | TokKind::KwStatic | TokKind::KwExtern
            | TokKind::KwInline | TokKind::KwRegister | TokKind::KwAuto | TokKind::KwTypedef
            | TokKind::KwAtomic | TokKind::KwFract | TokKind::KwAccum | TokKind::KwComplex
            | TokKind::KwDecimal32 | TokKind::KwDecimal64 | TokKind::KwDecimal128 | TokKind::KwBool
            | TokKind::KwInt128 | TokKind::KwUInt128)
            || (self.peek.kind == TokKind::Ident
                && Self::is_type_specifier_ident(&self.peek.text, &self.typedefs))
    }

    fn parse_stmt(&mut self) -> Option<Stmt> {
        let sp = self.cur.span.clone();
        match self.cur.kind {
            TokKind::KwReturn => {
                self.advance();
                let val = if self.cur.kind != TokKind::Semi { Some(self.parse_expr()?) } else { None };
                if !self.expect(TokKind::Semi, "';'") { return None; }
                Some(Stmt::Return { val, span: sp })
            }
            TokKind::Semi => { self.advance(); Some(Stmt::Expr { e: Expr::Int { v: 0, span: sp.clone(), ty: int_t() }, span: sp }) }
            TokKind::LBrace => { self.advance(); let b = self.parse_block(); if !self.expect(TokKind::RBrace, "'}'") { return None; } Some(Stmt::Block { b, span: sp }) }
            TokKind::KwIf => {
                self.advance(); if !self.expect(TokKind::LParen, "'('") { return None; }
                let cond = self.parse_expr()?; if !self.expect(TokKind::RParen, "')'") { return None; }
                let then_ = Box::new(self.parse_stmt()?);
                let else_ = if self.cur.kind == TokKind::KwElse { self.advance(); Some(Box::new(self.parse_stmt()?)) } else { None };
                Some(Stmt::If { cond, then_, else_, span: sp })
            }
            TokKind::KwWhile => {
                self.advance(); if !self.expect(TokKind::LParen, "'('") { return None; }
                let cond = self.parse_expr()?; if !self.expect(TokKind::RParen, "')'") { return None; }
                Some(Stmt::While { cond, body: Box::new(self.parse_stmt()?), span: sp })
            }
            TokKind::KwDo => {
                self.advance(); let body = Box::new(self.parse_stmt()?);
                if !self.expect(TokKind::KwWhile, "'while'") { return None; }
                if !self.expect(TokKind::LParen, "'('") { return None; }
                let cond = self.parse_expr()?; if !self.expect(TokKind::RParen, "')'") { return None; }
                if !self.expect(TokKind::Semi, "';'") { return None; }
                Some(Stmt::Do { body, cond, span: sp })
            }
            TokKind::KwFor => {
                self.advance(); if !self.expect(TokKind::LParen, "'('") { return None; }
                let init = if self.cur.kind == TokKind::Semi { self.advance(); None } else { Some(Box::new(self.parse_stmt()?)) };
                let cond = if self.cur.kind == TokKind::Semi { None } else { Some(self.parse_expr()?) };
                if !self.expect(TokKind::Semi, "';'") { return None; }
                let step = if self.cur.kind == TokKind::RParen { None } else { Some(self.parse_expr()?) };
                if !self.expect(TokKind::RParen, "')'") { return None; }
                Some(Stmt::For { init, cond, step, body: Box::new(self.parse_stmt()?), span: sp })
            }
            TokKind::KwBreak => { self.advance(); if !self.expect(TokKind::Semi, "';'") { return None; } Some(Stmt::Break { span: sp }) }
            TokKind::KwContinue => { self.advance(); if !self.expect(TokKind::Semi, "';'") { return None; } Some(Stmt::Continue { span: sp }) }
            TokKind::KwSwitch => {
                self.advance(); if !self.expect(TokKind::LParen, "'('") { return None; }
                let expr = self.parse_expr()?; if !self.expect(TokKind::RParen, "')'") { return None; }
                if !self.expect(TokKind::LBrace, "'{'") { return None; }
                let cases = self.parse_cases();
                if !self.expect(TokKind::RBrace, "'}'") { return None; }
                Some(Stmt::Switch { expr, cases, span: sp })
            }
            TokKind::KwGoto => {
                self.advance();
                // goto *expr: 间接跳转 (GCC 扩展)
                if self.cur.kind == TokKind::Star {
                    self.advance();
                    let expr = self.parse_expr()?;
                    if !self.expect(TokKind::Semi, "';'") { return None; }
                    Some(Stmt::GotoIndirect { expr: Box::new(expr), span: sp })
                } else {
                    let t = self.parse_ident()?;
                    if !self.expect(TokKind::Semi, "';'") { return None; }
                    Some(Stmt::Goto { target: t, span: sp })
                }
            }
            // asm goto 语句；普通 asm 在同 arm 内走 parse_expr
            TokKind::Ident if matches!(self.cur.text.as_str(), "__asm__" | "__asm" | "asm") => {
                if self.probe_asm_goto() {
                self.advance(); // asm
                self.skip_asm_qualifiers();
                self.advance(); // goto
                self.skip_asm_qualifiers();
                if self.cur.kind != TokKind::LParen { self.err_here("E0001", "expected '(' after asm goto", None); return None; }
                self.advance(); // (
                // 解析汇编字符串
                let mut asm_str = String::new();
                while self.cur.kind == TokKind::StringLit {
                    asm_str.push_str(&Self::decode_string_literal(&self.cur.text));
                    self.advance();
                }
                // 解析 : outputs : inputs : clobbers : goto_labels
                let mut outputs = vec![];
                let mut output_names = vec![];
                let mut output_constraints = vec![];
                let mut inputs = vec![];
                let mut input_names = vec![];
                let mut input_constraints = vec![];
                let mut clobbers = vec![];
                let mut goto_labels = vec![];
                if self.cur.kind == TokKind::Colon {
                    self.advance();
                    let (o, n, c) = self.parse_asm_operand_list();
                    outputs = o;
                    output_names = n;
                    output_constraints = c;
                }
                if self.cur.kind == TokKind::Colon {
                    self.advance();
                    let (o, n, c) = self.parse_asm_operand_list();
                    inputs = o;
                    input_names = n;
                    input_constraints = c;
                }
                if self.cur.kind == TokKind::Colon { self.advance(); clobbers = self.parse_asm_clobber_list(); }
                if self.cur.kind == TokKind::Colon { self.advance(); goto_labels = self.parse_asm_label_list(); }
                if self.cur.kind == TokKind::RParen { self.advance(); }
                if self.cur.kind == TokKind::Semi { self.advance(); }
                Some(Stmt::AsmGoto {
                    asm_str, outputs, output_names, output_constraints, inputs, input_names,
                    input_constraints, clobbers, goto_labels, span: sp,
                })
                } else {
                    let e = self.parse_expr()?;
                    if !self.expect(TokKind::Semi, "';'") { return None; }
                    Some(Stmt::Expr { e, span: sp })
                }
            }
            TokKind::Ident if self.peek.kind == TokKind::Colon => {
                let nm = self.parse_ident()?; self.advance();
                // switch 内 `label: case X:`：标签后直接跟 case/default
                if matches!(self.cur.kind, TokKind::KwCase | TokKind::KwDefault | TokKind::RBrace) {
                    Some(Stmt::Label {
                        name: nm,
                        stmt: Box::new(Stmt::Block {
                            b: Block { items: vec![], span: sp.clone() },
                            span: sp.clone(),
                        }),
                        span: sp,
                    })
                } else {
                    Some(Stmt::Label { name: nm, stmt: Box::new(self.parse_stmt()?), span: sp })
                }
            }
            _ if self.is_decl_start() => {
                let ds = self.parse_decl()?;
                let items = ds.into_iter().map(BlockItem::Decl).collect();
                Some(Stmt::Block { b: Block { items, span: sp.clone() }, span: sp })
            }
            _ => { let e = self.parse_expr()?; if !self.expect(TokKind::Semi, "';'") { return None; } Some(Stmt::Expr { e, span: sp }) }
        }
    }

    fn parse_cases(&mut self) -> Vec<Case> {
        let mut cases = vec![];
        while self.cur.kind != TokKind::RBrace && self.cur.kind != TokKind::Eof {
            let sp = self.cur.span.clone();
            let (val, range_end) = if self.cur.kind == TokKind::KwCase {
                self.advance();
                let low = self.parse_expr();
                let mut hi = None;
                if self.cur.kind == TokKind::Ellipsis {
                    self.advance();
                    hi = self.parse_expr();
                }
                let _ = self.expect(TokKind::Colon, "':'");
                (low, hi)
            } else if self.cur.kind == TokKind::KwDefault {
                self.advance();
                let _ = self.expect(TokKind::Colon, "':'");
                (None, None)
            } else {
                self.advance();
                continue;
            };
            let mut items = vec![];
            while !matches!(self.cur.kind, TokKind::KwCase | TokKind::KwDefault | TokKind::RBrace | TokKind::Eof) {
                if self.is_decl_start() { match self.parse_decl() { Some(ds) => { for d in ds { items.push(BlockItem::Decl(d)); } } None => { self.advance(); } } }
                else { match self.parse_stmt() { Some(s) => items.push(BlockItem::Stmt(s)), None => { self.advance(); } } }
            }
            cases.push(Case { val, range_end, body: Block { items, span: sp.clone() }, span: sp });
        }
        cases
    }

    // ===== Pratt 表达式 =====
    fn parse_expr(&mut self) -> Option<Expr> { self.parse_expr_bp(0) }

    fn parse_expr_bp(&mut self, min_bp: u8) -> Option<Expr> {
        let mut lhs = self.parse_unary()?;
        loop {
            // 逗号运算符: bp=1, 最低优先级
            if self.cur.kind == TokKind::Comma && min_bp <= 1 {
                let sp = self.cur.span.clone();
                self.advance();
                let r = self.parse_expr_bp(1)?;
                let ty = r.ty();
                lhs = Expr::Comma { l: Box::new(lhs), r: Box::new(r), span: sp, ty };
                continue;
            }
            if self.cur.kind == TokKind::Assign && min_bp <= 2 {
                self.advance(); let r = self.parse_expr_bp(2)?;
                let ty = lhs.ty();
                lhs = Expr::Assign { op: BinOp::Add, l: Box::new(lhs), r: Box::new(r), span: self.cur.span.clone(), ty }; continue;
            }
            if let Some(op) = self.compound_op() {
                if min_bp <= 2 { self.advance(); let r = self.parse_expr_bp(2)?;
                    // 展开 lhs += e → target = lhs + e
                    // 提取赋值目标 (如果 lhs 已是 Assign, 提取其目标)
                    let target = Self::assign_target(&lhs);
                    let read = Box::new(lhs.clone()); // 读取当前值
                    let binop_ty = if matches!(target.ty(), Type::Ptr(_)) && matches!(op, BinOp::Add | BinOp::Sub) {
                        target.ty()
                    } else if matches!(target.ty(), Type::Float | Type::Double | Type::LongDouble) {
                        target.ty()
                    } else if matches!(target.ty(), Type::Vector(_, _)) {
                        target.ty()
                    } else { int_t() };
                    let expanded = Expr::Binop { op, l: read, r: Box::new(r), span: self.cur.span.clone(), ty: binop_ty };
                    let assign_ty = target.ty();
                    lhs = Expr::Assign { op: BinOp::Add, l: Box::new(target), r: Box::new(expanded), span: self.cur.span.clone(), ty: assign_ty }; continue; }
            }
            if self.cur.kind == TokKind::Question {
                // 三元优先级高于逗号(bp=1)。else 分支是 conditional-expression，
                // 不得用 bp=0，否则会吞掉 `_Generic(a?b:c, type: x)` 中的逗号。
                if 2 < min_bp { break; }
                let cond = lhs;
                self.advance();
                // GNU 扩展: x ?: y 等价于 x ? x : y
                let then_ = if self.cur.kind == TokKind::Colon {
                    cond.clone()
                } else {
                    self.parse_expr()?
                };
                if !self.expect(TokKind::Colon, "':'") { return None; }
                let else_ = self.parse_expr_bp(2)?;
                let then_ty = then_.ty();
                let else_ty = else_.ty();
                let cond_ty = if then_ty == else_ty {
                    then_ty
                } else if matches!(then_ty, Type::Ptr(_)) {
                    then_ty
                } else if matches!(else_ty, Type::Ptr(_)) {
                    else_ty
                } else {
                    int_t()
                };
                lhs = Expr::Cond {
                    cond: Box::new(cond),
                    then_: Box::new(then_),
                    else_: Box::new(else_),
                    span: self.cur.span.clone(),
                    ty: cond_ty,
                };
                continue;
            }
            let (op, lbp, rbp) = match self.cur.kind {
                TokKind::OrOr => (BinOp::OrOr, 4u8, 5),
                TokKind::AndAnd => (BinOp::AndAnd, 5, 6),
                TokKind::Pipe => (BinOp::Or, 6, 7),
                TokKind::Caret => (BinOp::Xor, 7, 8),
                TokKind::Amp => (BinOp::And, 8, 9),
                TokKind::EqEq => (BinOp::EqEq, 9, 10), TokKind::Ne => (BinOp::Ne, 9, 10),
                TokKind::Lt => (BinOp::Lt, 10, 11), TokKind::Gt => (BinOp::Gt, 10, 11),
                TokKind::Le => (BinOp::Le, 10, 11), TokKind::Ge => (BinOp::Ge, 10, 11),
                TokKind::Shl => (BinOp::Shl, 11, 12), TokKind::Shr => (BinOp::Shr, 11, 12),
                TokKind::Plus => (BinOp::Add, 12u8, 13), TokKind::Minus => (BinOp::Sub, 12, 13),
                TokKind::Star => (BinOp::Mul, 13, 14), TokKind::Slash => (BinOp::Div, 13, 14),
                TokKind::Percent => (BinOp::Mod, 13, 14),
                _ => break,
            };
            if lbp < min_bp { break; }
            self.advance(); let r = self.parse_expr_bp(rbp)?;
            // 比较与逻辑运算结果恒为 int，即使操作数是浮点
            let binop_ty = {
                let lt = lhs.ty(); let rt = r.ty();
                // 数组在表达式中自动衰减为指向首元素的指针
                // (C99 6.3.2.1.3): 除 sizeof/&/字符串字面量初始化外, 数组名
                // 在表达式上下文一律退化为 T*.  此处实现 arr+int 形式,
                // 让 libdecnumber 等库能写 `*(bcdar+1)`.
                let lt = if let Type::Array(inner, _) = &lt {
                    Type::Ptr(inner.clone())
                } else { lt };
                let rt = if let Type::Array(inner, _) = &rt {
                    Type::Ptr(inner.clone())
                } else { rt };
                if matches!(op, BinOp::EqEq | BinOp::Ne | BinOp::Lt | BinOp::Le
                    | BinOp::Gt | BinOp::Ge | BinOp::AndAnd | BinOp::OrOr) {
                    int_t()
                } else {
                let is_ca = matches!(&lt, Type::Complex(_));
                let is_cb = matches!(&rt, Type::Complex(_));
                let is_va = matches!(&lt, Type::Vector(_, _));
                let is_vb = matches!(&rt, Type::Vector(_, _));
                let is_fa = matches!(&lt, Type::Float | Type::Double | Type::LongDouble);
                let is_fb = matches!(&rt, Type::Float | Type::Double | Type::LongDouble);
                if is_va || is_vb {
                    if is_va { lt } else { rt }
                } else if is_ca || is_cb {
                    // 复数算术结果仍为复数（供 Cast 等类型推导）
                    if is_ca { lt } else { rt }
                } else if is_fa || is_fb {
                    if matches!(&lt, Type::LongDouble) || matches!(&rt, Type::LongDouble) { Type::LongDouble }
                    else if matches!(&lt, Type::Double) || matches!(&rt, Type::Double) { Type::Double }
                    else { Type::Float }
                } else if matches!(op, BinOp::Add | BinOp::Sub) {
                    if matches!(&lt, Type::Ptr(_)) { lt }
                    else if matches!(op, BinOp::Add) && matches!(&rt, Type::Ptr(_)) { rt }
                    else if matches!(&lt, Type::Int128 | Type::UInt128)
                        || matches!(&rt, Type::Int128 | Type::UInt128) {
                        if matches!(&lt, Type::UInt128) || matches!(&rt, Type::UInt128) {
                            Type::UInt128
                        } else { Type::Int128 }
                    } else { int_t() }
                } else if matches!(&lt, Type::Int128 | Type::UInt128)
                    || matches!(&rt, Type::Int128 | Type::UInt128) {
                    if matches!(&lt, Type::UInt128) || matches!(&rt, Type::UInt128) {
                        Type::UInt128
                    } else { Type::Int128 }
                } else { int_t() }
                }
            };
            lhs = Expr::Binop { op, l: Box::new(lhs), r: Box::new(r), span: self.cur.span.clone(), ty: binop_ty };
        }
        Some(lhs)
    }

    fn compound_op(&self) -> Option<BinOp> {
        match self.cur.kind {
            TokKind::PlusEq => Some(BinOp::Add), TokKind::MinusEq => Some(BinOp::Sub),
            TokKind::StarEq => Some(BinOp::Mul), TokKind::SlashEq => Some(BinOp::Div),
            TokKind::PercentEq => Some(BinOp::Mod), TokKind::AmpEq => Some(BinOp::And),
            TokKind::PipeEq => Some(BinOp::Or), TokKind::CaretEq => Some(BinOp::Xor),
            TokKind::ShlEq => Some(BinOp::Shl), TokKind::ShrEq => Some(BinOp::Shr),
            _ => None,
        }
    }

    // 提取赋值目标: 如果 expr 是 Assign, 递归提取其目标; 否则克隆 expr
    fn assign_target(expr: &Expr) -> Expr {
        match expr {
            Expr::Assign { l, .. } => Self::assign_target(l),
            _ => expr.clone(),
        }
    }

    fn parse_unary(&mut self) -> Option<Expr> {
        let sp = self.cur.span.clone();
        match self.cur.kind {
            TokKind::Plus => { self.advance(); let e = self.parse_unary()?; let ty = e.ty(); Some(Expr::Unary { op: UnOp::Plus, e: Box::new(e), span: sp, ty }) }
            TokKind::Minus => { self.advance(); let e = self.parse_unary()?; let ty = e.ty(); Some(Expr::Unary { op: UnOp::Neg, e: Box::new(e), span: sp, ty }) }
            TokKind::Not => { self.advance(); let e = self.parse_unary()?; Some(Expr::Unary { op: UnOp::Not, e: Box::new(e), span: sp, ty: int_t() }) }
            TokKind::Tilde => { self.advance(); let e = self.parse_unary()?; Some(Expr::Unary { op: UnOp::BitNot, e: Box::new(e), span: sp, ty: int_t() }) }
            TokKind::Star => { self.advance(); let e = self.parse_unary()?;
                let ty = if let Type::Ptr(inner) = &e.ty() { *inner.clone() } else { int_t() };
                Some(Expr::Deref { e: Box::new(e), span: sp, ty }) }
            TokKind::Amp => {
                self.advance(); let e = self.parse_unary()?;
                let ty = Type::Ptr(Box::new(e.ty()));
                Some(Expr::AddrOf { e: Box::new(e), span: sp, ty })
            }
            // &&label: GCC 扩展 label 地址 (词法分析器把 && 识别为 AndAnd)
            TokKind::AndAnd => {
                self.advance(); // &&
                if self.cur.kind == TokKind::Ident {
                    let label = self.parse_ident()?;
                    let ptr_ty = Type::Ptr(Box::new(Type::Void));
                    Some(Expr::LabelAddr { label, span: sp, ty: ptr_ty })
                } else {
                    self.err_here("E0001", "expected label name after &&", Some("use &&label for GNU label address extension"));
                    None
                }
            }
            TokKind::PlusPlus => { self.advance(); let e = self.parse_unary()?; let ty = e.ty(); Some(Expr::PreInc { e: Box::new(e), span: sp, ty }) }
            TokKind::MinusMinus => { self.advance(); let e = self.parse_unary()?; let ty = e.ty(); Some(Expr::PreDec { e: Box::new(e), span: sp, ty }) }
            TokKind::KwSizeof => {
                self.advance();
                if self.cur.kind == TokKind::LParen && self.is_peek_type_start() {
                    self.advance(); let ty = self.parse_type()?;
                    if !self.expect(TokKind::RParen, "')'") { return None; }
                    Some(Expr::SizeofTy { ty, span: sp })
                } else { let e = self.parse_unary()?; Some(Expr::SizeofExpr { e: Box::new(e), span: sp }) }
            }
            // _Alignof(type) 或 _Alignof(expr)
            TokKind::KwAlignof => {
                self.advance();
                if self.cur.kind == TokKind::LParen {
                    self.advance();
                    if self.is_type_start() {
                        let ty = self.parse_type()?;
                        if !self.expect(TokKind::RParen, "')'") { return None; }
                        Some(Expr::AlignofTy { ty, span: sp })
                    } else {
                        let e = self.parse_expr()?;
                        if !self.expect(TokKind::RParen, "')'") { return None; }
                        Some(Expr::AlignofExpr { e: Box::new(e), span: sp })
                    }
                } else { self.err_here("E0001", "expected '(' after _Alignof", Some("_Alignof requires parentheses: _Alignof(type)")); None }
            }
            // __extension__: 跳过, 继续解析下一个一元表达式
            TokKind::Ident if self.cur.text == "__extension__" => {
                self.advance();
                self.parse_unary()
            }
            // _Generic(controlling_expr, type: expr, ..., default: expr): C11 泛型选择
            TokKind::Ident if self.cur.text == "_Generic" => {
                self.advance();
                if !self.expect(TokKind::LParen, "'('") { return None; }
                // 控制表达式: 不能用 parse_expr() (会消费逗号为逗号运算符)
                // 用 bp=2 跳过逗号运算符 (bp=1), 让逗号留给 association list
                let controlling = self.parse_expr_bp(2)?;
                if !self.expect(TokKind::Comma, "','") { return None; }
                let ctrl_ty = controlling.ty();
                let mut result: Option<Expr> = None;
                let mut default_result: Option<Expr> = None;
                // 解析 association-list: type-name: expr 或 default: expr
                loop {
                    if self.cur.kind == TokKind::RParen { break; }
                    // default: expr
                    if (self.cur.kind == TokKind::Ident && self.cur.text == "default")
                        || self.cur.kind == TokKind::KwDefault {
                        self.advance();
                        if !self.expect(TokKind::Colon, "':'") { return None; }
                        default_result = Some(self.parse_expr_bp(2)?);
                    } else {
                        // type-name: expr
                        let assoc_ty = self.parse_type()?;
                        if !self.expect(TokKind::Colon, "':'") { return None; }
                        let assoc_expr = self.parse_expr_bp(2)?;
                        // 类型匹配: 简化比较 (忽略 const/volatile 限定符)
                        if result.is_none() && Self::types_compatible(&ctrl_ty, &assoc_ty) {
                            result = Some(assoc_expr);
                        }
                    }
                    if self.cur.kind == TokKind::Comma { self.advance(); }
                }
                if !self.expect(TokKind::RParen, "')'") { return None; }
                // 优先使用精确匹配, 否则使用 default
                let chosen = result.or(default_result);
                match chosen {
                    Some(e) => Some(e),
                    None => {
                        self.err_here("E0001", "no matching generic selection", Some("add a 'default' association to _Generic"));
                        None
                    }
                }
            }
            // nullptr: C23 空指针常量, 等价于 (void*)0
            TokKind::Ident if self.cur.text == "nullptr" => {
                self.advance();
                let ptr_ty = Type::Ptr(Box::new(Type::Void));
                Some(Expr::Int { v: 0, span: sp, ty: ptr_ty })
            }
            // __asm__("..."): 内联汇编表达式
            TokKind::Ident if self.cur.text == "__asm__" || self.cur.text == "__asm" || self.cur.text == "asm" => {
                self.advance();
                self.skip_asm_qualifiers();
                self.parse_asm_expr(sp)
            }
            // 语句表达式: ({...})
            TokKind::LParen if self.peek.kind == TokKind::LBrace => {
                self.advance(); // (
                self.advance(); // {
                let b = self.parse_block();
                if !self.expect(TokKind::RBrace, "'}'") { return None; }
                if !self.expect(TokKind::RParen, "')'") { return None; }
                // 类型取最后一个表达式语句的类型
                let ty = b.items.last().map_or(Type::Void, |item| {
                    if let BlockItem::Stmt(Stmt::Expr { e, .. }) = item { e.ty() }
                    else if let BlockItem::Stmt(Stmt::Return { val: Some(e), .. }) = item { e.ty() }
                    else { Type::Void }
                });
                // 语句表达式后可跟后缀运算符，如 ({ ... })()
                let e = Expr::StmtExpr { block: Box::new(b), span: sp.clone(), ty };
                return Some(self.parse_postfix_suffix(e, sp)?);
            }
            TokKind::LParen if self.is_cast() => {
                self.advance(); let ty = self.parse_type()?;
                // 函数指针强制转换: (type (*)(params))expr
                // 解析完类型后, 检查是否是 (* 模式
                if self.cur.kind == TokKind::LParen && self.peek.kind == TokKind::Star {
                    self.advance(); // (
                    self.advance(); // *
                    while matches!(self.cur.kind, TokKind::KwConst | TokKind::KwVolatile | TokKind::KwRestrict) { self.advance(); }
                    if !self.expect(TokKind::RParen, "')'") { return None; }
                    if self.cur.kind == TokKind::LParen {
                        self.advance();
                        let _params = self.parse_params()?;
                        if !self.expect(TokKind::RParen, "')'") { return None; }
                    }
                    let ty = Type::Ptr(Box::new(ty));
                    if !self.expect(TokKind::RParen, "')'") { return None; }
                    let e = self.parse_unary()?;
                    return Some(Expr::Cast { ty, e: Box::new(e), span: sp });
                }
                if !self.expect(TokKind::RParen, "')'") { return None; }
                let e = self.parse_unary()?;
                // 严格类型检查: 整数截断 / 整数↔指针转换
                let src_ty = e.ty();
                if let Some(diag) = self.check_strict_conversion(&src_ty, &ty, sp.clone()) {
                    self.diags.push(diag);
                }
                Some(Expr::Cast { ty, e: Box::new(e), span: sp })
            }
            // 函数指针强制转换: 当 is_cast() 未检测到时, 尝试解析 (type (*...))expr
            TokKind::LParen if matches!(self.peek.kind,
                TokKind::KwInt | TokKind::KwChar | TokKind::KwLong | TokKind::KwVoid
                | TokKind::KwShort | TokKind::KwUnsigned | TokKind::KwSigned
                | TokKind::KwFloat | TokKind::KwDouble | TokKind::KwStruct
                | TokKind::KwUnion | TokKind::KwEnum | TokKind::KwInt128 | TokKind::KwUInt128) => {
                // 推测性解析: 保存状态, 尝试解析为函数指针cast
                let saved_cur = self.cur.clone();
                let saved_peek = self.peek.clone();
                self.advance(); // (
                if let Some(ty) = self.parse_type() {
                    if self.cur.kind == TokKind::LParen && self.peek.kind == TokKind::Star {
                        self.advance(); // (
                        self.advance(); // *
                        while matches!(self.cur.kind, TokKind::KwConst | TokKind::KwVolatile | TokKind::KwRestrict)
                            || (self.cur.kind == TokKind::Ident && self.cur.text == "_ms_restrict") { self.advance(); }
                        if self.cur.kind == TokKind::RParen {
                            self.advance(); // )
                            if self.cur.kind == TokKind::LParen {
                                self.advance();
                                let _params = self.parse_params();
                                if self.cur.kind == TokKind::RParen {
                                    self.advance();
                                    if self.cur.kind == TokKind::RParen {
                                        self.advance();
                                        let ty = Type::Ptr(Box::new(ty));
                                        let e = self.parse_unary()?;
                                        return Some(Expr::Cast { ty, e: Box::new(e), span: sp });
                                    }
                                }
                            }
                        }
                    }
                }
                // 不是函数指针cast, 恢复并作为普通括号表达式
                self.cur = saved_cur;
                self.peek = saved_peek;
                let e = self.parse_expr()?;
                if !self.expect(TokKind::RParen, "')'") { return None; }
                Some(e)
            }
            _ => self.parse_postfix(),
        }
    }

    /// GNU/标准类型说明符起始（用于 is_cast / typeof 等）
    fn is_type_specifier_ident(text: &str, typedefs: &std::collections::HashMap<String, Type>) -> bool {
        typedefs.contains_key(text)
            || matches!(
                text,
                "typeof" | "__typeof__" | "__typeof" | "__auto_type" | "_Atomic" | "__extension__"
                    | "constexpr" | "_BitInt" | "_Noreturn" | "_Thread_local" | "__thread"
                    | "__builtin_va_list"
                    | "_Float16" | "_Float32" | "_Float32x" | "_Float64" | "_Float64x"
                    | "_Float128" | "_Float128x"
                    | "_ms_restrict" | "__declspec" | "__pragma" | "_Pragma"
            )
    }

    fn is_cast(&self) -> bool {
        if self.cur.kind != TokKind::LParen {
            return false;
        }
        matches!(
            self.peek.kind,
            TokKind::KwInt
                | TokKind::KwChar
                | TokKind::KwLong
                | TokKind::KwVoid
                | TokKind::KwShort
                | TokKind::KwUnsigned
                | TokKind::KwSigned
                | TokKind::KwFloat
                | TokKind::KwDouble
                | TokKind::KwStruct
                | TokKind::KwUnion
                | TokKind::KwEnum
                | TokKind::KwConst
                | TokKind::KwVolatile
                | TokKind::KwAtomic
                | TokKind::KwFract
                | TokKind::KwAccum
                | TokKind::KwBool
                | TokKind::KwInt128
                | TokKind::KwUInt128
        ) || (self.peek.kind == TokKind::Ident
            && Self::is_type_specifier_ident(&self.peek.text, &self.typedefs))
    }

    fn parse_postfix(&mut self) -> Option<Expr> {
        let sp = self.cur.span.clone();
        let e = self.parse_primary()?;
        self.parse_postfix_suffix(e, sp)
    }

    /// 解析已得表达式上的后缀运算符（调用、下标、成员访问等）
    fn parse_postfix_suffix(&mut self, mut e: Expr, sp: Span) -> Option<Expr> {
        loop {
            match self.cur.kind {
                TokKind::LParen => {
                    // 函数调用: 支持标识符和函数指针表达式
                    let callee_expr = e.clone();
                    self.advance();
                    // __builtin_va_arg(ap, type): 第二个参数是类型名
                    if let Expr::Ident { name, .. } = &callee_expr {
                        if name.name == "__builtin_va_arg" {
                            let ap = self.parse_expr_bp(2)?;
                            if !self.expect(TokKind::Comma, "','") { return None; }
                            let arg_ty = self.parse_type()?;
                            if !self.expect(TokKind::RParen, "')'") { return None; }
                            e = Expr::VaArg { ap: Box::new(ap), ty: arg_ty, span: sp.clone() };
                            continue;
                        }
                        // __builtin_types_compatible_p(type1, type2): 编译期类型比较
                        if name.name == "__builtin_types_compatible_p" {
                            let ty1 = self.parse_type()?;
                            if !self.expect(TokKind::Comma, "','") { return None; }
                            let ty2 = self.parse_type()?;
                            if !self.expect(TokKind::RParen, "')'") { return None; }
                            // 解析 typedef 别名以比较真实类型
                            let ty1 = if let Type::Named(ref n) = ty1 {
                                self.typedefs.get(n).cloned().unwrap_or(ty1)
                            } else { ty1 };
                            let ty2 = if let Type::Named(ref n) = ty2 {
                                self.typedefs.get(n).cloned().unwrap_or(ty2)
                            } else { ty2 };
                            let v = if Self::types_compatible(&ty1, &ty2) { 1 } else { 0 };
                            e = Expr::Int { v: v as i128, span: sp.clone(), ty: int_t() };
                            continue;
                        }
                        // __builtin_choose_expr(cond, expr1, expr2): 编译期条件选择
                        if name.name == "__builtin_choose_expr" {
                            let cond = self.parse_expr_bp(2)?;
                            if !self.expect(TokKind::Comma, "','") { return None; }
                            let expr1 = self.parse_expr_bp(2)?;
                            if !self.expect(TokKind::Comma, "','") { return None; }
                            let expr2 = self.parse_expr_bp(2)?;
                            if !self.expect(TokKind::RParen, "')'") { return None; }
                            let chosen = match const_eval::eval(&cond) {
                                Ok(v) if v != 0 => expr1,
                                Ok(_) => expr2,
                                Err(_) => {
                                    self.diags.push(Diagnostic::err(
                                        "E0013",
                                        "__builtin_choose_expr condition is not constant",
                                        sp.clone(),
                                    ));
                                    expr2
                                }
                            };
                            e = chosen;
                            continue;
                        }
                        // __builtin_constant_p(expr): 只判断，不执行参数。
                        if name.name == "__builtin_constant_p" {
                            let arg = self.parse_expr_bp(2)?;
                            if !self.expect(TokKind::RParen, "')'") { return None; }
                            e = Expr::Int {
                                v: const_eval::is_constant(&arg) as i128,
                                span: sp.clone(),
                                ty: int_t(),
                            };
                            continue;
                        }
                        // __builtin_object_size(ptr, type): 编译期对象大小
                        if name.name == "__builtin_object_size" {
                            let ptr = self.parse_expr_bp(2)?;
                            if !self.expect(TokKind::Comma, "','") { return None; }
                            let type_arg = self.parse_expr_bp(2)?;
                            if !self.expect(TokKind::RParen, "')'") { return None; }
                            let mode = const_eval::eval(&type_arg).unwrap_or(0);
                            let size = const_eval::object_size(&ptr, mode).unwrap_or(-1);
                            e = Expr::Int { v: size, span: sp.clone(), ty: int_t() };
                            continue;
                        }
                        // __builtin_FILE / __builtin_LINE / __builtin_FUNCTION /
                        // __builtin_FUNCSIG: 编译期折叠为常量 (按 GCC 行为)。
                        if name.name == "__builtin_FILE" {
                            if !self.expect(TokKind::RParen, "')'") { return None; }
                            let file_str = self.cur.span.file.clone();
                            let mut data = file_str.into_bytes();
                            data.push(0);
                            e = Expr::String { data, span: sp.clone(),
                                               ty: Type::Ptr(Box::new(Type::Char)) };
                            continue;
                        }
                        if name.name == "__builtin_LINE" {
                            if !self.expect(TokKind::RParen, "')'") { return None; }
                            e = Expr::Int { v: self.cur.span.line.max(1) as i128,
                                            span: sp.clone(), ty: int_t() };
                            continue;
                        }
                        if name.name == "__builtin_FUNCTION"
                            || name.name == "__builtin_FUNCSIG"
                            || name.name == "__builtin_PRETTY_FUNCTION" {
                            if !self.expect(TokKind::RParen, "')'") { return None; }
                            // 函数外: GCC 返回空串; 函数内: 返回函数名
                            let name_str = self.current_function.clone().unwrap_or_default();
                            let mut data = name_str.into_bytes();
                            data.push(0);
                            e = Expr::String { data, span: sp.clone(),
                                               ty: Type::Ptr(Box::new(Type::Char)) };
                            continue;
                        }
                        // __builtin_classify_type(type): GCC type classification.
                        // 1=void, 2=integer, 3=pointer, 4=real_float,
                        // 5=complex, 6=struct_union, 7=array, 8=function,
                        // 9=vector, 10=enum, -1=lang_mask (unknown).
                        if name.name == "__builtin_classify_type" {
                            let ty = self.parse_type()?;
                            if !self.expect(TokKind::RParen, "')'") { return None; }
                            // 若仍为 Type::Named, 查 typedef 映射以获得真实类型
                            let ty = if let Type::Named(ref n) = ty {
                                self.typedefs.get(n).cloned().unwrap_or(ty)
                            } else { ty };
                            let class = match ty {
                                Type::Void => 1,
                                Type::Bool | Type::Char | Type::SChar | Type::UChar
                                | Type::Short | Type::UShort
                                | Type::Int | Type::UInt
                                | Type::Long | Type::ULong
                                | Type::LongLong | Type::ULongLong
                                | Type::Int128 | Type::UInt128
                                | Type::Fract | Type::Accum => 2,
                                Type::Ptr(_) | Type::Func(_, _) => 3,
                                Type::Float | Type::Double | Type::LongDouble => 4,
                                Type::Complex(_) => 5,
                                Type::Struct(_, _) | Type::Union(_, _) => 6,
                                Type::Array(_, _) => 7,
                                Type::Vector(_, _) => 9,
                                Type::Enum(_, _) => 10,
                                Type::Atomic(_) | Type::Named(_) => 1,
                            };
                            e = Expr::Int { v: class, span: sp.clone(), ty: int_t() };
                            continue;
                        }
                        // __builtin_offsetof(type, member): 结构体成员偏移
                        if name.name == "__builtin_offsetof" {
                            let base_ty = self.parse_type()?;
                            if !self.expect(TokKind::Comma, "','") { return None; }
                            // 解析成员设计器: field, field.sub, field[idx].sub 等
                            let mut member = String::new();
                            if self.cur.kind == TokKind::Ident {
                                member = self.cur.text.clone();
                                self.advance();
                            }
                            // 支持 .field 和 [expr] 后缀
                            loop {
                                if self.cur.kind == TokKind::Dot {
                                    self.advance();
                                    if self.cur.kind == TokKind::Ident {
                                        member.push('.');
                                        member.push_str(&self.cur.text);
                                        self.advance();
                                    }
                                } else if self.cur.kind == TokKind::LBracket {
                                    self.advance();
                                    let _idx = self.parse_expr_bp(2);
                                    if self.cur.kind == TokKind::RBracket { self.advance(); }
                                    member.push_str("[]");
                                } else {
                                    break;
                                }
                            }
                            if !self.expect(TokKind::RParen, "')'") { return None; }
                            e = Expr::Offsetof { ty: base_ty, member, span: sp.clone(), result_ty: int_t() };
                            continue;
                        }
                        // __builtin_strlen(s): 字符串字面量编译期求值
                        // 只对 String 字面量做折叠; 其它情况正常 Call
                        if name.name == "__builtin_strlen" {
                            let saved = self.cur.clone();
                            if self.cur.kind == TokKind::LParen {
                                self.advance();
                                if let TokKind::StringLit = self.cur.kind {
                                    let mut data = parse_str(&self.cur.text);
                                    self.advance();
                                    while self.cur.kind == TokKind::StringLit {
                                        let more = parse_str(&self.cur.text);
                                        data.extend(more);
                                        self.advance();
                                    }
                                    if self.expect(TokKind::RParen, "')'") {
                                        // 排除末尾 '\0'
                                        let len = if data.last() == Some(&0) {
                                            data.len() - 1
                                        } else {
                                            data.len()
                                        };
                                        e = Expr::Int { v: len as i128, span: sp.clone(), ty: int_t() };
                                        continue;
                                    }
                                }
                                // 回退
                                self.cur = saved.clone();
                            }
                        }
                        // __builtin_abs/__builtin_labs/__builtin_llabs:
                        // 整型字面量编译期求值
                        if matches!(name.name.as_str(),
                            "__builtin_abs" | "__builtin_labs" | "__builtin_llabs")
                        {
                            let saved = self.cur.clone();
                            if self.cur.kind == TokKind::LParen {
                                self.advance();
                                if let TokKind::Number = self.cur.kind {
                                    let lit = self.cur.text.clone();
                                    self.advance();
                                    if self.expect(TokKind::RParen, "')'") {
                                        if let Some(v) = crate::const_eval::eval_int_lit(&lit) {
                                            let r = v.wrapping_abs();
                                            e = Expr::Int { v: r as i128, span: sp.clone(), ty: int_t() };
                                            continue;
                                        }
                                    }
                                }
                                self.cur = saved.clone();
                            }
                        }
                        // __builtin_bswap16/32/64: 字节序反转 (编译期)
                        if matches!(name.name.as_str(),
                            "__builtin_bswap16" | "__builtin_bswap32" | "__builtin_bswap64")
                        {
                            let saved = self.cur.clone();
                            if self.cur.kind == TokKind::LParen {
                                self.advance();
                                if let TokKind::Number = self.cur.kind {
                                    let lit = self.cur.text.clone();
                                    self.advance();
                                    if self.expect(TokKind::RParen, "')'") {
                                        if let Some(v) = crate::const_eval::eval_int_lit(&lit) {
                                            let bits: u64 = v as u64;
                                            let result = match name.name.as_str() {
                                                "__builtin_bswap16" => (bits & 0xffff).swap_bytes() as i128,
                                                "__builtin_bswap32" => (bits as u32).swap_bytes() as i128,
                                                _ => bits.swap_bytes() as i128,
                                            };
                                            e = Expr::Int { v: result, span: sp.clone(), ty: int_t() };
                                            continue;
                                        }
                                    }
                                }
                                self.cur = saved.clone();
                            }
                        }
                        // __builtin_expect(cond, expect): 提示分支预测
                        // 编译期折叠为 cond 的值
                        if name.name == "__builtin_expect" {
                            let saved = self.cur.clone();
                            if self.cur.kind == TokKind::LParen {
                                self.advance();
                                if let TokKind::Number = self.cur.kind {
                                    let lit = self.cur.text.clone();
                                    self.advance();
                                    let _ = self.cur.clone();
                                    if self.cur.kind == TokKind::Comma { self.advance(); }
                                    // 跳过 expect 表达式 (简单形式: 标识符或数字)
                                    while self.cur.kind != TokKind::RParen && self.cur.kind != TokKind::Eof {
                                        self.advance();
                                    }
                                    if self.expect(TokKind::RParen, "')'") {
                                        if let Some(v) = crate::const_eval::eval_int_lit(&lit) {
                                            e = Expr::Int { v, span: sp.clone(), ty: int_t() };
                                            continue;
                                        }
                                    }
                                }
                                self.cur = saved.clone();
                            }
                        }
                        // __builtin_popcount / __builtin_popcountl / __builtin_popcountll
                        if matches!(name.name.as_str(),
                            "__builtin_popcount" | "__builtin_popcountl" | "__builtin_popcountll")
                        {
                            let saved = self.cur.clone();
                            if self.cur.kind == TokKind::LParen {
                                self.advance();
                                if let TokKind::Number = self.cur.kind {
                                    let lit = self.cur.text.clone();
                                    self.advance();
                                    if self.expect(TokKind::RParen, "')'") {
                                        if let Some(v) = crate::const_eval::eval_int_lit(&lit) {
                                            let n = if v >= 0 { v as u64 } else { !v as u64 };
                                            e = Expr::Int { v: n.count_ones() as i128, span: sp.clone(), ty: int_t() };
                                            continue;
                                        }
                                    }
                                }
                                self.cur = saved.clone();
                            }
                        }
                    }
                    let mut args = vec![];
                    if self.cur.kind != TokKind::RParen { loop { args.push(self.parse_expr_bp(2)?); if self.cur.kind == TokKind::Comma { self.advance(); continue; } break; } }
                    if !self.expect(TokKind::RParen, "')'") { return None; }
                    let call_ty = self.call_result_type(&callee_expr);
                    e = Expr::Call { callee: Box::new(callee_expr), args, span: sp.clone(), ty: call_ty };
                }
                TokKind::LBracket => {
                    self.advance(); let idx = self.parse_expr()?;
                    if !self.expect(TokKind::RBracket, "']'") { return None; }
                    let ty = Self::index_result_type(&e);
                    e = Expr::Index { arr: Box::new(e), idx: Box::new(idx), span: sp.clone(), ty };
                }
                TokKind::Dot => {
                    self.advance();
                    let f = self.parse_ident()?;
                    let ty = self.type_of_member(&e, &f, false);
                    e = Expr::Member { expr: Box::new(e), field: f, span: sp.clone(), ty, is_arrow: false };
                }
                TokKind::Arrow => {
                    self.advance();
                    let f = self.parse_ident()?;
                    let ty = self.type_of_member(&e, &f, true);
                    e = Expr::Member { expr: Box::new(e), field: f, span: sp.clone(), ty, is_arrow: true };
                }
                TokKind::PlusPlus => { self.advance(); let ty = e.ty(); e = Expr::PostInc { e: Box::new(e), span: sp.clone(), ty }; }
                TokKind::MinusMinus => { self.advance(); let ty = e.ty(); e = Expr::PostDec { e: Box::new(e), span: sp.clone(), ty }; }
                _ => break,
            }
        }
        Some(e)
    }

    fn parse_primary(&mut self) -> Option<Expr> {
        let sp = self.cur.span.clone();
        match self.cur.kind {
            TokKind::Number => {
                let s = &self.cur.text; let v = parse_int_lit(s);
                let suffix = s.trim_start_matches(|c: char| c.is_ascii_hexdigit()
                    || c == 'x' || c == 'X' || c == 'b' || c == 'B');
                let has_unsigned = suffix.chars().any(|c| c == 'u' || c == 'U');
                let long_count = suffix.chars().filter(|c| *c == 'l' || *c == 'L').count();
                let ty = match (has_unsigned, long_count) {
                    (true, 2..) => Type::ULongLong,
                    (true, 1) => Type::ULong,
                    (true, 0) => Type::UInt,
                    (false, 2..) => Type::LongLong,
                    (false, 1) => Type::Long,
                    (false, 0) => Type::Int,
                };
                self.advance(); Some(Expr::Int { v, span: sp, ty })
            }
            TokKind::FloatLit => {
                let s = &self.cur.text;
                let s_clean = s.trim_end_matches(|c| c == 'f' || c == 'F' || c == 'l' || c == 'L');
                let v: f64 = s_clean.parse().unwrap_or(0.0);
                let ty = if s.ends_with('f') || s.ends_with('F') { Type::Float }
                    else if s.ends_with('l') || s.ends_with('L') { Type::LongDouble }
                    else { Type::Double };
                self.advance(); Some(Expr::Float { v, span: sp, ty })
            }
            TokKind::StringLit => {
                let mut data = parse_str(&self.cur.text); self.advance();
                while self.cur.kind == TokKind::StringLit { data.extend(parse_str(&self.cur.text)); self.advance(); }
                Some(Expr::String { data, span: sp, ty: Type::Ptr(Box::new(Type::Char)) })
            }
            TokKind::WideStringLit => {
                let mut data = parse_wide_str(&self.cur.text); self.advance();
                while self.cur.kind == TokKind::WideStringLit { data.extend(parse_wide_str(&self.cur.text)); self.advance(); }
                // wchar_t = int (4 bytes) on Linux, pointer to wchar_t
                Some(Expr::String { data, span: sp, ty: Type::Ptr(Box::new(Type::Int)) })
            }
            TokKind::CharLit => { let v = parse_chr(&self.cur.text); self.advance(); Some(Expr::Char { v, span: sp, ty: Type::Int }) }
            TokKind::Ident => {
                let t = self.cur.text.clone();
                // C99 __func__ / GCC __FUNCTION__ / __PRETTY_FUNCTION__:
                // 编译期折叠为当前函数名字符串字面量.
                if t == "__func__" || t == "__FUNCTION__" || t == "__PRETTY_FUNCTION__" {
                    self.advance();
                    let name_str = self.current_function.clone().unwrap_or_default();
                    let mut data = name_str.into_bytes();
                    data.push(0);
                    return Some(Expr::String {
                        data, span: sp, ty: Type::Ptr(Box::new(Type::Char))
                    });
                }
                if let Some(&val) = self.enum_consts.get(&t) { self.advance(); return Some(Expr::Int { v: val as i128, span: sp, ty: Type::Int }); }
                let id = Ident { name: t, span: sp.clone() }; self.advance();
                let id_ty = self.lookup_ident_type(&id.name);
                Some(Expr::Ident { name: id, span: sp, ty: id_ty })
            }
            TokKind::LParen => {
                self.advance();
                // 检查是否是复合字面量: (type){...}
                if self.is_type_start() {
                    // 尝试解析类型
                    let saved_pos = self.cur.clone();
                    let saved_peek = self.peek.clone();
                    if let Some(ty) = self.parse_type() {
                        if self.cur.kind == TokKind::RParen && self.peek.kind == TokKind::LBrace {
                            // 复合字面量: (type){...}
                            self.advance(); // 跳过 )
                            self.advance(); // 跳过 {
                            let mut inits = Vec::new();
                            while self.cur.kind != TokKind::RBrace {
                                let item = if self.cur.kind == TokKind::Dot {
                                    self.advance();
                                    let field = self.parse_ident()?;
                                    if !self.expect(TokKind::Assign, "'='") { return None; }
                                    let value = self.parse_expr_bp(2)?;
                                    InitItem::Designated { designator: Designator::Field(field), value }
                                } else {
                                    let e = self.parse_expr_bp(2)?;
                                    InitItem::Expr(e)
                                };
                                inits.push(item);
                                if self.cur.kind == TokKind::Comma { self.advance(); } else { break; }
                            }
                            if !self.expect(TokKind::RBrace, "'}'") { return None; }
                            return Some(Expr::CompoundLit { ty, inits, span: sp });
                        }
                    }
                    // 不是复合字面量，恢复状态
                    self.cur = saved_pos;
                    self.peek = saved_peek;
                }
                let e = self.parse_expr()?;
                if !self.expect(TokKind::RParen, "')'") { return None; }
                Some(e)
            }
            TokKind::LBrace => {
                // 初始化列表: {1, 2, 3} 或 {.x = 1, .y = 2}
                self.advance();
                let mut inits = Vec::new();
                while self.cur.kind != TokKind::RBrace {
                    let item = if self.cur.kind == TokKind::Dot {
                        // 指定初始化器: .field = value
                        self.advance();
                        let field = self.parse_ident()?;
                        if !self.expect(TokKind::Assign, "'='") { return None; }
                        let value = self.parse_expr_bp(2)?;
                        InitItem::Designated { designator: Designator::Field(field), value }
                    } else if self.cur.kind == TokKind::LBracket {
                        // 数组指定初始化器: [idx] = value
                        self.advance();
                        let idx = self.parse_expr()?;
                        if !self.expect(TokKind::RBracket, "']'") { return None; }
                        if !self.expect(TokKind::Assign, "'='") { return None; }
                        let value = self.parse_expr_bp(2)?;
                        InitItem::Designated { designator: Designator::Index(idx), value }
                    } else {
                        // 普通初始化值
                        let e = self.parse_expr_bp(2)?;
                        InitItem::Expr(e)
                    };
                    inits.push(item);
                    if self.cur.kind == TokKind::Comma { self.advance(); } else { break; }
                }
                if !self.expect(TokKind::RBrace, "'}'") { return None; }
                Some(Expr::InitList { inits, span: sp, ty: Type::Int }) // 类型稍后由上下文确定
            }
            _ => { self.err_here("E0006", format!("expected expression, got {:?}", self.cur.kind), Some("expected a value, variable, or expression here")); None }
        }
    }

    /// 收集嵌套函数捕获的外层变量
    fn collect_upvars(items: &[BlockItem], local_vars: &HashMap<String, Type>, parent_locals: &HashMap<String, Type>, out: &mut Vec<(String, i32)>) {
        for item in items {
            match item {
                BlockItem::Stmt(s) => Self::collect_upvars_stmt(s, local_vars, parent_locals, out),
                BlockItem::Decl(Decl::Var(v)) => {
                    // 检查初始化表达式中的外层变量引用
                    if let Some(init) = &v.init {
                        Self::collect_upvars_expr(init, local_vars, parent_locals, out);
                    }
                    // 新声明的局部变量加入 local_vars (不修改原始 map, 只在递归时传递)
                }
                BlockItem::Decl(Decl::Func(f)) => {
                    // 递归检查嵌套函数体
                    if let Some(body) = &f.body {
                        Self::collect_upvars(&body.items, local_vars, parent_locals, out);
                    }
                }
                _ => {}
            }
        }
    }
    fn collect_upvars_stmt(s: &Stmt, local_vars: &HashMap<String, Type>, parent_locals: &HashMap<String, Type>, out: &mut Vec<(String, i32)>) {
        match s {
            Stmt::Expr { e, .. } | Stmt::Return { val: Some(e), .. } => Self::collect_upvars_expr(e, local_vars, parent_locals, out),
            Stmt::Return { val: None, .. } => {}
            Stmt::Block { b, .. } => Self::collect_upvars(&b.items, local_vars, parent_locals, out),
            Stmt::If { cond, then_, else_, .. } => {
                Self::collect_upvars_expr(cond, local_vars, parent_locals, out);
                Self::collect_upvars_stmt(then_, local_vars, parent_locals, out);
                if let Some(e) = else_ { Self::collect_upvars_stmt(e, local_vars, parent_locals, out); }
            }
            Stmt::While { cond, body, .. } | Stmt::Do { body, cond, .. } => {
                Self::collect_upvars_expr(cond, local_vars, parent_locals, out);
                Self::collect_upvars_stmt(body, local_vars, parent_locals, out);
            }
            Stmt::For { init, cond, step, body, .. } => {
                if let Some(i) = init { Self::collect_upvars_stmt(i, local_vars, parent_locals, out); }
                if let Some(c) = cond { Self::collect_upvars_expr(c, local_vars, parent_locals, out); }
                if let Some(s) = step { Self::collect_upvars_expr(s, local_vars, parent_locals, out); }
                Self::collect_upvars_stmt(body, local_vars, parent_locals, out);
            }
            Stmt::Switch { expr, cases, .. } => {
                Self::collect_upvars_expr(expr, local_vars, parent_locals, out);
                for c in cases { Self::collect_upvars(&c.body.items, local_vars, parent_locals, out); }
            }
            _ => {}
        }
    }
    fn collect_upvars_expr(e: &Expr, local_vars: &HashMap<String, Type>, parent_locals: &HashMap<String, Type>, out: &mut Vec<(String, i32)>) {
        match e {
            Expr::Ident { name, .. } => {
                let n = &name.name;
                // 如果变量不在当前局部变量表中, 但在父级局部变量表中, 则是捕获的 upvar
                if !local_vars.contains_key(n) && parent_locals.contains_key(n) {
                    if !out.iter().any(|(name, _)| name == n) {
                        // 偏移占位符 0, 实际偏移在 codegen 中计算
                        out.push((n.clone(), 0));
                    }
                }
            }
            Expr::Binop { l, r, .. } => {
                Self::collect_upvars_expr(l, local_vars, parent_locals, out);
                Self::collect_upvars_expr(r, local_vars, parent_locals, out);
            }
            Expr::Unary { e, .. } => Self::collect_upvars_expr(e, local_vars, parent_locals, out),
            Expr::Assign { l, r, .. } => {
                Self::collect_upvars_expr(l, local_vars, parent_locals, out);
                Self::collect_upvars_expr(r, local_vars, parent_locals, out);
            }
            Expr::Call { callee, args, .. } => {
                Self::collect_upvars_expr(callee, local_vars, parent_locals, out);
                for a in args { Self::collect_upvars_expr(a, local_vars, parent_locals, out); }
            }
            Expr::Member { expr, .. } => Self::collect_upvars_expr(expr, local_vars, parent_locals, out),
            Expr::Index { arr, idx, .. } => {
                Self::collect_upvars_expr(arr, local_vars, parent_locals, out);
                Self::collect_upvars_expr(idx, local_vars, parent_locals, out);
            }
            Expr::Cond { cond, then_, else_, .. } => {
                Self::collect_upvars_expr(cond, local_vars, parent_locals, out);
                Self::collect_upvars_expr(then_, local_vars, parent_locals, out);
                Self::collect_upvars_expr(else_, local_vars, parent_locals, out);
            }
            Expr::Cast { e, .. } | Expr::AddrOf { e, .. } | Expr::Deref { e, .. } => {
                Self::collect_upvars_expr(e, local_vars, parent_locals, out);
            }
            Expr::StmtExpr { block, .. } => Self::collect_upvars(&block.items, local_vars, parent_locals, out),
            _ => {}
        }
    }

    /// 宿主头文件中常见的整型 typedef 重定义（如 dev_t / uint64_t）
    fn is_integer_typedef(ty: &Type) -> bool {
        match ty {
            Type::Atomic(inner) => Self::is_integer_typedef(inner),
            Type::Bool | Type::Char | Type::SChar | Type::UChar
            | Type::Short | Type::UShort | Type::Int | Type::UInt
            | Type::Long | Type::ULong | Type::LongLong | Type::ULongLong
            | Type::Int128 | Type::UInt128 | Type::Enum(_, _) | Type::Named(_) => true,
            _ => false,
        }
    }

    /// glibc/linux 头文件对同一名字的重复 typedef（如 timer_t void* vs int）
    fn typedef_redef_allowed(old: &Type, new: &Type) -> bool {
        if matches!(old, Type::Named(_)) || matches!(new, Type::Named(_)) {
            return true;
        }
        matches!(old, Type::Ptr(_)) || matches!(new, Type::Ptr(_))
    }

    /// _Generic 类型兼容性检查 (C11 6.2.7): 指针按 pointee 匹配，void* 与任意对象指针兼容
    fn types_compatible(a: &Type, b: &Type) -> bool {
        let a = match a { Type::Atomic(inner) => inner.as_ref(), other => other };
        let b = match b { Type::Atomic(inner) => inner.as_ref(), other => other };
        match (a, b) {
            (Type::Bool, Type::Bool) => true,
            (Type::Char | Type::SChar | Type::UChar, Type::Char | Type::SChar | Type::UChar) => true,
            (Type::Short | Type::UShort, Type::Short | Type::UShort) => true,
            (Type::Int | Type::UInt, Type::Int | Type::UInt) => true,
            (Type::Long | Type::ULong, Type::Long | Type::ULong) => true,
            (Type::LongLong | Type::ULongLong, Type::LongLong | Type::ULongLong) => true,
            (Type::Float, Type::Float) => true,
            (Type::Double, Type::Double) => true,
            (Type::LongDouble, Type::LongDouble) => true,
            (Type::Ptr(pa), Type::Ptr(pb)) => {
                if matches!(pa.as_ref(), Type::Void) || matches!(pb.as_ref(), Type::Void) {
                    return true;
                }
                Self::types_compatible(pa, pb)
            }
            (Type::Void, Type::Void) => true,
            (Type::Fract, Type::Fract) => true,
            (Type::Accum, Type::Accum) => true,
            (Type::Struct(na, _), Type::Struct(nb, _)) => na == nb,
            (Type::Union(na, _), Type::Union(nb, _)) => na == nb,
            (Type::Enum(na, _), Type::Enum(nb, _)) => na == nb,
            _ => a == b,
        }
    }

}

fn parse_int_lit(s: &str) -> i128 {
    let s = s.trim_end_matches(|c: char| "lLuU".contains(c));
    if let Some(r) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) { i128::from_str_radix(r, 16).unwrap_or(0) }
    else if let Some(r) = s.strip_prefix("0b").or_else(|| s.strip_prefix("0B")) { i128::from_str_radix(r, 2).unwrap_or(0) }
    else if let Some(r) = s.strip_prefix("0").filter(|r| !r.is_empty() && r.chars().all(|c| c.is_ascii_digit())) { i128::from_str_radix(r, 8).unwrap_or(0) }
    else { s.parse::<i128>().unwrap_or(0) }
}

fn parse_str(raw: &str) -> Vec<u8> {
    let inner = &raw[1..raw.len()-1]; let mut d = vec![]; let mut it = inner.chars();
    while let Some(c) = it.next() {
        if c == '\\' { match it.next() { Some('n') => d.push(b'\n'), Some('t') => d.push(b'\t'), Some('r') => d.push(b'\r'), Some('0') => d.push(0), Some('\\') => d.push(b'\\'), Some('"') => d.push(b'"'), Some('\'') => d.push(b'\''), Some(c) => { d.push(b'\\'); d.push(c as u8); } None => d.push(b'\\'), } }
        else { d.extend_from_slice(c.to_string().as_bytes()); }
    }
    d.push(0); d
}

/// Parse wide string literal L"..." to UTF-32LE bytes (4 bytes per wchar_t)
fn parse_wide_str(raw: &str) -> Vec<u8> {
    // Strip L prefix and quotes: L"..." -> ...
    let inner = if raw.starts_with("L\"") || raw.starts_with("L'") {
        &raw[2..raw.len()-1]
    } else {
        &raw[1..raw.len()-1]
    };
    let mut d = vec![];
    let mut it = inner.chars();
    while let Some(c) = it.next() {
        let cp = if c == '\\' {
            match it.next() {
                Some('n') => '\n' as u32,
                Some('t') => '\t' as u32,
                Some('r') => '\r' as u32,
                Some('0') => 0,
                Some('\\') => '\\' as u32,
                Some('"') => '"' as u32,
                Some('\'') => '\'' as u32,
                Some('x') => {
                    let mut val = 0u32;
                    for _ in 0..2 {
                        if let Some(h) = it.next() {
                            val = val * 16 + h.to_digit(16).unwrap_or(0);
                        }
                    }
                    val
                }
                Some(c) => c as u32,
                None => '\\' as u32,
            }
        } else {
            c as u32
        };
        d.extend_from_slice(&cp.to_le_bytes());
    }
    // null terminator (4 bytes of zero)
    d.extend_from_slice(&0u32.to_le_bytes());
    d
}

fn parse_chr(raw: &str) -> u8 {
    let inner = &raw[1..raw.len()-1];
    if inner.starts_with('\\') { match inner.chars().nth(1) { Some('n') => b'\n', Some('t') => b'\t', Some('r') => b'\r', Some('0') => 0, Some('\\') => b'\\', Some('\'') => b'\'', _ => inner.as_bytes().get(1).copied().unwrap_or(0) } }
    else { inner.as_bytes().first().copied().unwrap_or(0) }
}
