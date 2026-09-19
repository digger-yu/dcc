//! C++ token 流 (M7 最小扩展).
//!
//! dcc 的 C 词法 (src/tokens.rs) 足以识别 C++ 绝大部分标点 / 标识符
//! / 数字 / 字符串 / 字符字面量. 真正的 C++ 扩展点仅在关键字:
//!   class, public, private, protected, this, new, delete,
//!   operator, virtual, inline, explicit, namespace, using,
//!   template, typename, try, catch, throw, ...
//!
//! 本模块定义 CppToken, 与 C 词法保持兼容:
//!   - C 关键字 (int, return, if, while, ...) 仍走 C 的 TokKind
//!   - C++ 关键字映射到新的 CppKw 包装, parser 优先匹配
//!   - :: 双冒号被 lex 为单个 ColonColon 标点

use crate::diagnostics::Span;
use crate::tokens::{Tok, TokKind};
use std::sync::Arc;

/// C 关键字 → TokKind (M9 共享, 避免 C++ lexer 漏识别 return 等关键字)
fn c_keyword_kind(s: &str) -> Option<TokKind> {
    Some(match s {
        "int" => TokKind::KwInt,
        "char" => TokKind::KwChar,
        "short" => TokKind::KwShort,
        "long" => TokKind::KwLong,
        "void" => TokKind::KwVoid,
        "float" => TokKind::KwFloat,
        "double" => TokKind::KwDouble,
        "signed" | "__signed__" => TokKind::KwSigned,
        "unsigned" => TokKind::KwUnsigned,
        "struct" => TokKind::KwStruct,
        "union" => TokKind::KwUnion,
        "enum" => TokKind::KwEnum,
        "typedef" => TokKind::KwTypedef,
        "if" => TokKind::KwIf,
        "else" => TokKind::KwElse,
        "while" => TokKind::KwWhile,
        "for" => TokKind::KwFor,
        "do" => TokKind::KwDo,
        "switch" => TokKind::KwSwitch,
        "case" => TokKind::KwCase,
        "default" => TokKind::KwDefault,
        "break" => TokKind::KwBreak,
        "continue" => TokKind::KwContinue,
        "return" => TokKind::KwReturn,
        "goto" => TokKind::KwGoto,
        "static" => TokKind::KwStatic,
        "extern" => TokKind::KwExtern,
        "const" | "__const__" | "__const" => TokKind::KwConst,
        "volatile" | "__volatile__" | "__volatile" => TokKind::KwVolatile,
        "inline" | "__inline__" | "__inline" => TokKind::KwInline,
        "restrict" | "__restrict__" | "__restrict" => TokKind::KwRestrict,
        "auto" => TokKind::KwAuto,
        "register" => TokKind::KwRegister,
        "sizeof" => TokKind::KwSizeof,
        "_Alignof" | "alignof" | "__alignof" | "__alignof__" => TokKind::KwAlignof,
        "_Atomic" => TokKind::KwAtomic,
        "_Bool" => TokKind::KwBool,
        "__int128" | "__int128_t" => TokKind::KwInt128,
        "__uint128_t" => TokKind::KwUInt128,
        _ => return None,
    })
}

/// C++ 关键字枚举 (仅 C 关键字之外的 C++ 扩展)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CppKw {
    Class,
    Public,
    Private,
    Protected,
    This,
    New,
    Delete,
    Operator,
    Virtual,
    Inline,
    Explicit,
    Namespace,
    Using,
    True,
    False,
    Nullptr,
    Static,
    Const,
    Friend,
    // M8 新增
    Template,
    Typename,
    Override,
    Final,
    Try,
    Catch,
    Throw,
    Constexpr,
    Consteval,
    Decltype,
    Typeid,
    DynamicCast,
    StaticCast,
    ReinterpretCast,
    ConstCast,
    Mutable,
    Volatile,
    Noexcept,
    ExplicitSpec,
    // M9 新增 (C++20)
    Concept,
    Requires,
    CoAwait,
    CoYield,
    CoReturn,
    Module,
    Import,
    Export,
    // C++20 杂项
    Char8T,
    ConstevalAuto,
    Constinit,  // C++20: constinit 说明符
    // G3: User-defined literal operator
    UserDefinedLiteral,
}

impl CppKw {
    pub fn from_ident(s: &str) -> Option<Self> {
        Some(match s {
            "class" => Self::Class,
            "public" => Self::Public,
            "private" => Self::Private,
            "protected" => Self::Protected,
            "this" => Self::This,
            "new" => Self::New,
            "delete" => Self::Delete,
            "operator" => Self::Operator,
            "virtual" => Self::Virtual,
            "inline" => Self::Inline,
            "explicit" => Self::Explicit,
            "namespace" => Self::Namespace,
            "using" => Self::Using,
            "true" => Self::True,
            "false" => Self::False,
            "nullptr" => Self::Nullptr,
            "static" => Self::Static,
            "const" => Self::Const,
            "friend" => Self::Friend,
            "template" => Self::Template,
            "typename" => Self::Typename,
            "override" => Self::Override,
            "final" => Self::Final,
            "try" => Self::Try,
            "catch" => Self::Catch,
            "throw" => Self::Throw,
            "constexpr" => Self::Constexpr,
            "consteval" => Self::Consteval,
            "decltype" => Self::Decltype,
            "typeid" => Self::Typeid,
            "dynamic_cast" => Self::DynamicCast,
            "static_cast" => Self::StaticCast,
            "reinterpret_cast" => Self::ReinterpretCast,
            "const_cast" => Self::ConstCast,
            "mutable" => Self::Mutable,
            "volatile" => Self::Volatile,
            "noexcept" => Self::Noexcept,
            "concept" => Self::Concept,
            "requires" => Self::Requires,
            "co_await" => Self::CoAwait,
            "co_yield" => Self::CoYield,
            "co_return" => Self::CoReturn,
            "module" => Self::Module,
            "import" => Self::Import,
            "export" => Self::Export,
            "char8_t" => Self::Char8T,
            "constinit" => Self::Constinit,
            "operator\"\"" => Self::UserDefinedLiteral,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Class => "class",
            Self::Public => "public",
            Self::Private => "private",
            Self::Protected => "protected",
            Self::This => "this",
            Self::New => "new",
            Self::Delete => "delete",
            Self::Operator => "operator",
            Self::Virtual => "virtual",
            Self::Inline => "inline",
            Self::Explicit => "explicit",
            Self::Namespace => "namespace",
            Self::Using => "using",
            Self::True => "true",
            Self::False => "false",
            Self::Nullptr => "nullptr",
            Self::Static => "static",
            Self::Const => "const",
            Self::Friend => "friend",
            Self::Template => "template",
            Self::Typename => "typename",
            Self::Override => "override",
            Self::Final => "final",
            Self::Try => "try",
            Self::Catch => "catch",
            Self::Throw => "throw",
            Self::Constexpr => "constexpr",
            Self::Consteval => "consteval",
            Self::Decltype => "decltype",
            Self::Typeid => "typeid",
            Self::DynamicCast => "dynamic_cast",
            Self::StaticCast => "static_cast",
            Self::ReinterpretCast => "reinterpret_cast",
            Self::ConstCast => "const_cast",
            Self::Mutable => "mutable",
            Self::Volatile => "volatile",
            Self::Noexcept => "noexcept",
            Self::ExplicitSpec => "explicit",
            Self::Concept => "concept",
            Self::Requires => "requires",
            Self::CoAwait => "co_await",
            Self::CoYield => "co_yield",
            Self::CoReturn => "co_return",
            Self::Module => "module",
            Self::Import => "import",
            Self::Export => "export",
            Self::Char8T => "char8_t",
            Self::ConstevalAuto => "consteval",
            Self::Constinit => "constinit",
            Self::UserDefinedLiteral => "operator\"\"",
        }
    }
}

/// C++ token (覆盖 C 词法的子集 + 扩展标点)
#[derive(Debug, Clone)]
pub enum CppTok {
    /// 透传 C token (标点 / 数字 / 字符串 / C 关键字)
    C(Tok),
    /// C++ 关键字
    Kw(CppKw, Span),
    /// `::` 作用域解析
    ColonColon(Span),
    /// G1: Raw string literal R"delim(...)delim"
    RawStringLit { span: Span, text: String, delimiter: Option<String> },
}

impl CppTok {
    pub fn span(&self) -> Span {
        match self {
            Self::C(t) => t.span.clone(),
            Self::Kw(_, s) | Self::ColonColon(s) => s.clone(),
            Self::RawStringLit { span: s, .. } => s.clone(),
        }
    }

    pub fn is_access_spec(&self) -> bool {
        matches!(
            self,
            Self::Kw(CppKw::Public | CppKw::Private | CppKw::Protected, _)
        )
    }
}

/// C++ token 流
#[derive(Clone)]
pub struct CppLexer<'a> {
    pub src: &'a [u8],
    pub file: Arc<str>,
    pub pos: usize,
    pub line: u32,
    pub col: u32,
}

impl<'a> CppLexer<'a> {
    pub fn new(src: &'a [u8], file: &str) -> Self {
        Self {
            src,
            file: Arc::from(file),
            pos: 0,
            line: 1,
            col: 1,
        }
    }

    /// 构造已扫描 token 的 span。
    ///
    /// 约定: 各 `next_tok` 分支先把 `pos`/`col` 前进 token 长度, 再调用
    /// 本函数, 因此 token 起始列 = `self.col - len`。之前直接取 `self.col`
    /// 导致所有 C++ 诊断的列号整体右移一个 token 长度 (例如 `case` 关键字
    /// 的 span 指向它后面的空白), 与 C 端诊断位置不一致。
    fn mk_span(&self, len: u32) -> Span {
        Span {
            file: self.file.clone(),
            line: self.line,
            col: self.col.saturating_sub(len),
            len,
        }
    }

    /// 跳过空白 / 注释 (C 风格 // 与 /* */)
    fn skip_trivia(&mut self) {
        // 一次性吃掉文件起始的 UTF-8 BOM (EF BB BF), C++ 标准视为
        // 行首无字符, 等价于无 BOM.
        if self.pos == 0
            && self.src.len() >= 3
            && self.src[0] == 0xEF
            && self.src[1] == 0xBB
            && self.src[2] == 0xBF
        {
            self.pos = 3;
        }
        loop {
            while self.pos < self.src.len() {
                let b = self.src[self.pos];
                if b == b'\n' {
                    self.line += 1;
                    self.col = 1;
                    self.pos += 1;
                } else if b == b' ' || b == b'\t' || b == b'\r' {
                    self.pos += 1;
                    self.col += 1;
                } else {
                    break;
                }
            }
            if self.pos + 1 < self.src.len() && &self.src[self.pos..self.pos + 2] == b"//" {
                while self.pos < self.src.len() && self.src[self.pos] != b'\n' {
                    self.pos += 1;
                    self.col += 1;
                }
                continue;
            }
            if self.pos + 1 < self.src.len() && &self.src[self.pos..self.pos + 2] == b"/*" {
                self.pos += 2;
                self.col += 2;
                while self.pos + 1 < self.src.len() && &self.src[self.pos..self.pos + 2] != b"*/" {
                    if self.src[self.pos] == b'\n' {
                        self.line += 1;
                        self.col = 1;
                    } else {
                        self.col += 1;
                    }
                    self.pos += 1;
                }
                if self.pos + 1 < self.src.len() {
                    self.pos += 2;
                    self.col += 2;
                }
                continue;
            }
            break;
        }
    }

    /// G1: Lex raw string literal R"delim(...)delim"
    fn lex_raw_string(&mut self) -> Option<CppTok> {
        // Check if we have '"' after R
        if self.pos + 1 >= self.src.len() || self.src[self.pos + 1] != b'"' {
            return None; // Not a raw string
        }
            
        // Skip 'R' and first '"'
        self.pos += 2;
        self.col += 2;
                
        // Read optional delimiter (<=16 chars)
        let delim_start = self.pos;
        while self.pos < self.src.len() {
            let c = self.src[self.pos];
            // Delimiter ends at '(' for custom delimiter or '"' for no delimiter
            if c == b'(' || c == b'"' || c.is_ascii_whitespace()
                || c == b'/' || c == b'\\' || c > 127 {
                break;
            }
            if self.pos - delim_start >= 16 {
                break; // Max 16 chars for delimiter
            }
            self.pos += 1;
            self.col += 1;
        }
            
        // Check what kind of raw string this is
        let has_delim = self.pos > delim_start && self.src[self.pos] == b'"';
        let delimiter = if has_delim {
            // Custom delimiter: R"delim"(...)
            Some(std::str::from_utf8(&self.src[delim_start..self.pos])
                .unwrap_or("").to_string())
        } else {
            // No delimiter: R"(...) -- will use current position as opening paren
            None
        };
                    
        // Expect '(' before content (either after closing '"' for delimited, or current position for no-delim)
        if self.pos >= self.src.len() {
            return None;
        }
                
        if has_delim {
            // After delimiter, expect '"' then '('
            if self.src[self.pos] != b'"' {
                return None;
            }
            self.pos += 1;
            self.col += 1;
        }
                
        // Now expect '(' before content
        if self.src[self.pos] != b'(' {
            return None;
        }
        self.pos += 1;
        self.col += 1;
            
        // Now scan until we find ') + optional delimiter + '"'
        let content_start = self.pos;
        loop {
            if self.pos >= self.src.len() {
                // Unterminated raw string
                return None;
            }
                
            // Check for closing sequence: ')' + optional delimiter + '"'
            if self.src[self.pos] == b')' {
                let mut check_pos = self.pos + 1;
                    
                // If there's a delimiter, it must match
                if let Some(ref delim) = delimiter {
                    if check_pos + delim.len() > self.src.len() {
                        continue; // Keep looking
                    } else {
                        let mut matches_delim = true;
                        for delim_byte in delim.bytes() {
                            if self.src[check_pos] != delim_byte {
                                matches_delim = false;
                                break;
                            }
                            check_pos += 1;
                        }
                        if !matches_delim {
                            continue; // Keep looking
                        }
                    }
                }
                    
                // Check for closing '"'
                if check_pos < self.src.len() && self.src[check_pos] == b'"' {
                    // End of raw string!
                    let full_len = (check_pos + 1 - content_start) as u32;
                    let span = Span {
                        file: self.file.clone(),
                        line: self.line,
                        col: self.col.saturating_sub(full_len),
                        len: full_len,
                    };
                    let text = std::str::from_utf8(&self.src[content_start..self.pos])
                        .unwrap_or("")
                        .to_string();
                        
                    self.pos = check_pos + 1;
                    self.col += full_len + 1;
                    return Some(CppTok::RawStringLit { span, text, delimiter });
                }
            }
                
            self.pos += 1;
            if self.src[self.pos - 1] == b'\n' {
                self.line += 1;
                self.col = 1;
            } else {
                self.col += 1;
            }
        }
    }

    /// 读一个 C++ token
    pub fn next_tok(&mut self) -> Option<CppTok> {
        self.skip_trivia();
        if self.pos >= self.src.len() {
            return None;
        }
        let b = self.src[self.pos];
        // G1: Raw string literal check (only if followed by ")
        if (b == b'r' || b == b'R') && self.pos + 1 < self.src.len() && self.src[self.pos + 1] == b'"' {
            return self.lex_raw_string();
        }
        if b == b':' && self.pos + 1 < self.src.len() && self.src[self.pos + 1] == b':' {
            self.pos += 2;
            self.col += 2;
            return Some(CppTok::ColonColon(self.mk_span(2)));
        }
        // C++ 复合操作符: ->, ->*, --, -=, <<=, >>=
        if b == b'-' && self.pos + 1 < self.src.len() {
            let n = self.src[self.pos + 1];
            if n == b'>' {
                self.pos += 2;
                self.col += 2;
                // ->* (成员指针访问) 也归并到 Arrow, 后续解析按上下文区分
                return Some(CppTok::C(Tok {
                    kind: TokKind::Arrow,
                    span: self.mk_span(2),
                    text: "->".to_string(),
                }));
            }
            if n == b'-' {
                self.pos += 2;
                self.col += 2;
                return Some(CppTok::C(Tok {
                    kind: TokKind::MinusMinus,
                    span: self.mk_span(2),
                    text: "--".to_string(),
                }));
            }
            if n == b'=' {
                self.pos += 2;
                self.col += 2;
                return Some(CppTok::C(Tok {
                    kind: TokKind::MinusEq,
                    span: self.mk_span(2),
                    text: "-=".to_string(),
                }));
            }
        }
        if b == b'+' && self.pos + 1 < self.src.len() {
            let n = self.src[self.pos + 1];
            if n == b'+' {
                self.pos += 2;
                self.col += 2;
                return Some(CppTok::C(Tok {
                    kind: TokKind::PlusPlus,
                    span: self.mk_span(2),
                    text: "++".to_string(),
                }));
            }
            if n == b'=' {
                self.pos += 2;
                self.col += 2;
                return Some(CppTok::C(Tok {
                    kind: TokKind::PlusEq,
                    span: self.mk_span(2),
                    text: "+=".to_string(),
                }));
            }
        }
        if b == b'<' && self.pos + 1 < self.src.len() && self.src[self.pos + 1] == b'<' {
            self.pos += 2;
            self.col += 2;
            if self.pos < self.src.len() && self.src[self.pos] == b'=' {
                self.pos += 1;
                self.col += 1;
                return Some(CppTok::C(Tok {
                    kind: TokKind::ShlEq,
                    span: self.mk_span(3),
                    text: "<<=".to_string(),
                }));
            }
            return Some(CppTok::C(Tok {
                kind: TokKind::Shl,
                span: self.mk_span(2),
                text: "<<".to_string(),
            }));
        }
        if b == b'>' && self.pos + 1 < self.src.len() && self.src[self.pos + 1] == b'>' {
            self.pos += 2;
            self.col += 2;
            if self.pos < self.src.len() && self.src[self.pos] == b'=' {
                self.pos += 1;
                self.col += 1;
                return Some(CppTok::C(Tok {
                    kind: TokKind::ShrEq,
                    span: self.mk_span(3),
                    text: ">>=".to_string(),
                }));
            }
            return Some(CppTok::C(Tok {
                kind: TokKind::Shr,
                span: self.mk_span(2),
                text: ">>".to_string(),
            }));
        }
        if b == b'=' && self.pos + 1 < self.src.len() && self.src[self.pos + 1] == b'=' {
            self.pos += 2;
            self.col += 2;
            return Some(CppTok::C(Tok {
                kind: TokKind::EqEq,
                span: self.mk_span(2),
                text: "==".to_string(),
            }));
        }
        if b == b'!' && self.pos + 1 < self.src.len() && self.src[self.pos + 1] == b'=' {
            self.pos += 2;
            self.col += 2;
            return Some(CppTok::C(Tok {
                kind: TokKind::Ne,
                span: self.mk_span(2),
                text: "!=".to_string(),
            }));
        }
        if b == b'<' && self.pos + 1 < self.src.len() && self.src[self.pos + 1] == b'=' {
            self.pos += 2;
            self.col += 2;
            return Some(CppTok::C(Tok {
                kind: TokKind::Le,
                span: self.mk_span(2),
                text: "<=".to_string(),
            }));
        }
        if b == b'>' && self.pos + 1 < self.src.len() && self.src[self.pos + 1] == b'=' {
            self.pos += 2;
            self.col += 2;
            return Some(CppTok::C(Tok {
                kind: TokKind::Ge,
                span: self.mk_span(2),
                text: ">=".to_string(),
            }));
        }
        if b == b'&' && self.pos + 1 < self.src.len() && self.src[self.pos + 1] == b'&' {
            self.pos += 2;
            self.col += 2;
            return Some(CppTok::C(Tok {
                kind: TokKind::AndAnd,
                span: self.mk_span(2),
                text: "&&".to_string(),
            }));
        }
        if b == b'|' && self.pos + 1 < self.src.len() && self.src[self.pos + 1] == b'|' {
            self.pos += 2;
            self.col += 2;
            return Some(CppTok::C(Tok {
                kind: TokKind::OrOr,
                span: self.mk_span(2),
                text: "||".to_string(),
            }));
        }
        if b == b'*' && self.pos + 1 < self.src.len() && self.src[self.pos + 1] == b'=' {
            self.pos += 2;
            self.col += 2;
            return Some(CppTok::C(Tok {
                kind: TokKind::StarEq,
                span: self.mk_span(2),
                text: "*=".to_string(),
            }));
        }
        if b == b'/' && self.pos + 1 < self.src.len() && self.src[self.pos + 1] == b'=' {
            self.pos += 2;
            self.col += 2;
            return Some(CppTok::C(Tok {
                kind: TokKind::SlashEq,
                span: self.mk_span(2),
                text: "/=".to_string(),
            }));
        }
        if b == b'%' && self.pos + 1 < self.src.len() && self.src[self.pos + 1] == b'=' {
            self.pos += 2;
            self.col += 2;
            return Some(CppTok::C(Tok {
                kind: TokKind::PercentEq,
                span: self.mk_span(2),
                text: "%=".to_string(),
            }));
        }
        if b == b'^' && self.pos + 1 < self.src.len() && self.src[self.pos + 1] == b'=' {
            self.pos += 2;
            self.col += 2;
            return Some(CppTok::C(Tok {
                kind: TokKind::CaretEq,
                span: self.mk_span(2),
                text: "^=".to_string(),
            }));
        }
        if b == b'|' && self.pos + 1 < self.src.len() && self.src[self.pos + 1] == b'=' {
            self.pos += 2;
            self.col += 2;
            return Some(CppTok::C(Tok {
                kind: TokKind::PipeEq,
                span: self.mk_span(2),
                text: "|=".to_string(),
            }));
        }
        if b == b'&' && self.pos + 1 < self.src.len() && self.src[self.pos + 1] == b'=' {
            self.pos += 2;
            self.col += 2;
            return Some(CppTok::C(Tok {
                kind: TokKind::AmpEq,
                span: self.mk_span(2),
                text: "&=".to_string(),
            }));
        }
        if b == b'.' && self.pos + 1 < self.src.len() && self.src[self.pos + 1] == b'*' {
            // C++ 成员指针访问: `obj.*ptr_to_member` (M17.0)
            // `->*` 已被 `->` Arrow + `*` Star 组合表示, parser 端组合处理
            self.pos += 2;
            self.col += 2;
            return Some(CppTok::C(Tok {
                kind: TokKind::DotStar,
                span: self.mk_span(2),
                text: ".*".to_string(),
            }));
        }
        if b == b'.'
            && self.pos + 2 < self.src.len()
            && self.src[self.pos + 1] == b'.'
            && self.src[self.pos + 2] == b'.'
        {
            // ... 用于异常 catch (...), 变参模板, C 变参
            self.pos += 3;
            self.col += 3;
            return Some(CppTok::C(Tok {
                kind: TokKind::Ellipsis,
                span: self.mk_span(3),
                text: "...".to_string(),
            }));
        }
        if b.is_ascii_alphabetic() || b == b'_' {
            let start = self.pos;
            while self.pos < self.src.len()
                && (self.src[self.pos].is_ascii_alphanumeric() || self.src[self.pos] == b'_')
            {
                self.pos += 1;
                self.col += 1;
            }
            let s = std::str::from_utf8(&self.src[start..self.pos]).unwrap_or("");
            let len = (self.pos - start) as u32;
            let span = self.mk_span(len);
            if let Some(kw) = CppKw::from_ident(s) {
                return Some(CppTok::Kw(kw, span));
            }
            // M9: 也识别 C 关键字 (return, if, while, ...)
            if let Some(kind) = c_keyword_kind(s) {
                let tok = Tok {
                    kind,
                    span,
                    text: s.to_string(),
                };
                return Some(CppTok::C(tok));
            }
            let tok = Tok {
                kind: TokKind::Ident,
                span,
                text: s.to_string(),
            };
            return Some(CppTok::C(tok));
        }
        if b.is_ascii_digit() {
            // G4: 数字分隔符 - 扫描时允许 ' 作为分隔符
            let start = self.pos;
            while self.pos < self.src.len()
                && (self.src[self.pos].is_ascii_alphanumeric() || self.src[self.pos] == b'.' || self.src[self.pos] == b'\'' || self.src[self.pos] == b'p' || self.src[self.pos] == b'P')
            {
                self.pos += 1;
                self.col += 1;
            }
            let len = (self.pos - start) as u32;
            let span = self.mk_span(len);
            let tok = Tok {
                kind: TokKind::Number,
                span,
                text: std::str::from_utf8(&self.src[start..self.pos])
                    .unwrap_or("")
                    .to_string(),
            };
            return Some(CppTok::C(tok));
        }
        // G1: Raw string literal R"delim(...)delim"
        if b == b'R' || b == b'r' {
            // 检查是否有双引号紧跟着
            if self.pos + 1 < self.src.len() && self.src[self.pos + 1] == b'"' {
                return self.lex_raw_string();
            }
        }
        if b == b'"' || b == b'\'' {
            let q = b;
            let start = self.pos;
            self.pos += 1;
            self.col += 1;
            while self.pos < self.src.len() && self.src[self.pos] != q {
                if self.src[self.pos] == b'\\' && self.pos + 1 < self.src.len() {
                    self.pos += 1;
                    self.col += 1;
                }
                self.pos += 1;
                self.col += 1;
            }
            if self.pos < self.src.len() {
                self.pos += 1;
                self.col += 1;
            }
            let len = (self.pos - start) as u32;
            let span = self.mk_span(len);
            let tok = Tok {
                kind: if q == b'"' {
                    TokKind::StringLit
                } else {
                    TokKind::CharLit
                },
                span,
                text: std::str::from_utf8(&self.src[start..self.pos])
                    .unwrap_or("")
                    .to_string(),
            };
            return Some(CppTok::C(tok));
        }
        self.pos += 1;
        self.col += 1;
        let kind = match b {
            b'(' => TokKind::LParen,
            b')' => TokKind::RParen,
            b'{' => TokKind::LBrace,
            b'}' => TokKind::RBrace,
            b'[' => TokKind::LBracket,
            b']' => TokKind::RBracket,
            b';' => TokKind::Semi,
            b',' => TokKind::Comma,
            b'?' => TokKind::Question,
            b'~' => TokKind::Tilde,
            b'.' => TokKind::Dot,
            b'&' => TokKind::Amp,
            b'*' => TokKind::Star,
            b'+' => TokKind::Plus,
            b'-' => TokKind::Minus,
            b'/' => TokKind::Slash,
            b'%' => TokKind::Percent,
            b'^' => TokKind::Caret,
            b'|' => TokKind::Pipe,
            b'!' => TokKind::Not,
            b'=' => TokKind::Assign,
            b'<' => TokKind::Lt,
            b'>' => TokKind::Gt,
            b':' => TokKind::Colon,
            _ => TokKind::Invalid,
        };
        let tok = Tok {
            kind,
            span: self.mk_span(1),
            text: (b as char).to_string(),
        };
        Some(CppTok::C(tok))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cpp_kw_recognized() {
        assert_eq!(CppKw::from_ident("class"), Some(CppKw::Class));
        assert_eq!(CppKw::from_ident("this"), Some(CppKw::This));
        assert_eq!(CppKw::from_ident("Foo"), None);
    }
    #[test]
    fn cpp_lex_basic() {
        let mut lx = CppLexer::new(b"class Foo { public: int x; };", "t.cpp");
        let mut toks = vec![];
        while let Some(t) = lx.next_tok() {
            toks.push(t);
        }
        assert!(matches!(toks[0], CppTok::Kw(CppKw::Class, _)));
        assert!(matches!(
            toks[1],
            CppTok::C(Tok {
                kind: TokKind::Ident,
                ..
            })
        ));
        assert!(matches!(
            toks[2],
            CppTok::C(Tok {
                kind: TokKind::LBrace,
                ..
            })
        ));
        assert!(matches!(toks[3], CppTok::Kw(CppKw::Public, _)));
        assert!(matches!(
            toks[4],
            CppTok::C(Tok {
                kind: TokKind::Colon,
                ..
            })
        ));
    }
    #[test]
    fn cpp_lex_colon_colon() {
        let mut lx = CppLexer::new(b"Foo::bar", "t.cpp");
        let mut toks = vec![];
        while let Some(t) = lx.next_tok() {
            toks.push(t);
        }
        assert!(matches!(toks[1], CppTok::ColonColon(_)));
    }
    #[test]
    fn cpp_lex_line_comment_skipped() {
        let mut lx = CppLexer::new(b"// hi\nclass X;", "t.cpp");
        let t = lx.next_tok().unwrap();
        assert!(matches!(t, CppTok::Kw(CppKw::Class, _)));
    }
    #[test]
    fn cpp_lex_block_comment_skipped() {
        let mut lx = CppLexer::new(b"/* a\nb */ class X;", "t.cpp");
        let t = lx.next_tok().unwrap();
        assert!(matches!(t, CppTok::Kw(CppKw::Class, _)));
    }
    #[test]
    fn cpp_lex_arrow_ptr_member() {
        // this->n_ 应被识别为 this Kw, -> Tok(Arrow), n_ Ident
        let mut lx = CppLexer::new(b"this->n_", "t.cpp");
        let t0 = lx.next_tok().unwrap();
        let t1 = lx.next_tok().unwrap();
        let t2 = lx.next_tok().unwrap();
        assert!(matches!(t0, CppTok::Kw(CppKw::This, _)));
        assert!(matches!(
            t1,
            CppTok::C(Tok {
                kind: TokKind::Arrow,
                ..
            })
        ));
        assert!(matches!(
            t2,
            CppTok::C(Tok {
                kind: TokKind::Ident,
                ..
            })
        ));
    }
    #[test]
    fn cpp_lex_compound_assigns() {
        // + - * / % & | ^ << >> == != <= >= && ||
        // (这里每个字符后接 = 或复合, 不应误判 +=/-= 等等)
        let mut lx = CppLexer::new(b"+ - * / % & | ^ << >> == != <= >= && ||", "t.cpp");
        let expect = [
            TokKind::Plus,
            TokKind::Minus,
            TokKind::Star,
            TokKind::Slash,
            TokKind::Percent,
            TokKind::Amp,
            TokKind::Pipe,
            TokKind::Caret,
            TokKind::Shl,
            TokKind::Shr,
            TokKind::EqEq,
            TokKind::Ne,
            TokKind::Le,
            TokKind::Ge,
            TokKind::AndAnd,
            TokKind::OrOr,
        ];
        for k in expect.iter() {
            let t = lx.next_tok().unwrap();
            match t {
                CppTok::C(Tok { kind, .. }) => assert_eq!(&kind, k, "token mismatch"),
                _ => panic!("expected C tok, got {:?}", t),
            }
        }
    }
    #[test]
    fn cpp_lex_compound_assigns_with_eq() {
        // 单独验证 += -= *= /= %= &= |= ^= <<= >>=
        let mut lx = CppLexer::new(b"+=-=*=/=%=&=|=^=<<=>>=", "t.cpp");
        let expect = [
            TokKind::PlusEq,
            TokKind::MinusEq,
            TokKind::StarEq,
            TokKind::SlashEq,
            TokKind::PercentEq,
            TokKind::AmpEq,
            TokKind::PipeEq,
            TokKind::CaretEq,
            TokKind::ShlEq,
            TokKind::ShrEq,
        ];
        for k in expect.iter() {
            let t = lx.next_tok().unwrap();
            match t {
                CppTok::C(Tok { kind, .. }) => assert_eq!(&kind, k, "token mismatch"),
                _ => panic!("expected C tok, got {:?}", t),
            }
        }
    }
    #[test]
    fn cpp_lex_incr_decr() {
        let mut lx = CppLexer::new(b"i++ j--", "t.cpp");
        // i Ident
        assert!(matches!(
            lx.next_tok(),
            Some(CppTok::C(Tok {
                kind: TokKind::Ident,
                ..
            }))
        ));
        // ++
        assert!(matches!(
            lx.next_tok(),
            Some(CppTok::C(Tok {
                kind: TokKind::PlusPlus,
                ..
            }))
        ));
        // j Ident
        assert!(matches!(
            lx.next_tok(),
            Some(CppTok::C(Tok {
                kind: TokKind::Ident,
                ..
            }))
        ));
        // --
        assert!(matches!(
            lx.next_tok(),
            Some(CppTok::C(Tok {
                kind: TokKind::MinusMinus,
                ..
            }))
        ));
    }
    #[test]
    fn cpp_lex_ellipsis() {
        let mut lx = CppLexer::new(b"...", "t.cpp");
        assert!(matches!(
            lx.next_tok(),
            Some(CppTok::C(Tok {
                kind: TokKind::Ellipsis,
                ..
            }))
        ));
    }
    #[test]
    fn cpp_lex_raw_string() {
        // G1: Raw string with no delimiter R"(content)"
        let mut lx = CppLexer::new(b"R\"(hello world)\"", "t.cpp");
        if let Some(CppTok::RawStringLit { text, delimiter, .. }) = lx.next_tok() {
            assert_eq!(text, "hello world");
            assert_eq!(delimiter, None);
        } else {
            panic!("expected RawStringLit, got {:?}", lx.next_tok());
        }
    }
    #[test]
    fn cpp_lex_raw_string_delim() {
        // G1: Raw string with custom delimiter
        let mut lx = CppLexer::new(b"R\"mydelim\"(hello world)mydelim\"", "t.cpp");
        if let Some(CppTok::RawStringLit { text, delimiter, .. }) = lx.next_tok() {
            assert_eq!(text, "hello world");
            assert_eq!(delimiter.as_deref(), Some("mydelim"));
        } else {
            panic!("expected RawStringLit, got {:?}", lx.next_tok());
        }
    }
    #[test]
    fn cpp_lex_number_with_separators() {
        // G4: Number separators (C++14)
        let mut lx = CppLexer::new(b"1'000'000",
        "t.cpp");
        let t = lx.next_tok().unwrap();
        match t {
            CppTok::C(Tok { kind: TokKind::Number, text, .. }) => {
                assert_eq!(text, "1'000'000");
            }
            _ => panic!("expected Number, got {:?}", t),
        }
    }
}
