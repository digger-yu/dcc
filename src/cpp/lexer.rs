//! C++ token 流 (M7 最小扩展).
//!
//! dcc 的 C 词法 (src/tokens.rs) 足以识别 C++ 绝大部分标点 / 标识符
//! / 数字 / 字符串 / 字符字面量. 真正的 C++ 扩展点仅在关键字:
//!   class, public, private, protected, this, new, delete,
//!   operator, virtual, inline, explicit, namespace, using,
//!   template, typename, try, catch, throw, ...
//!
//! 本模块定义 CppToken, 与 C 词法保持兼容:
//!   - C 关键字仍走 C 的 TokKind
//!   - C++ 关键字映射到新的 CppKw 包装, parser 优先匹配
//!   - :: 双冒号被 lex 为单个 ColonColon 标点

use crate::tokens::{Tok, TokKind};
use crate::diagnostics::Span;

/// C++ 关键字枚举 (仅 C 关键字之外的 C++ 扩展)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CppKw {
    Class, Public, Private, Protected,
    This, New, Delete,
    Operator, Virtual, Inline, Explicit,
    Namespace, Using,
    True, False, Nullptr,
    Static, Const, Friend,
    // M8 新增
    Template, Typename,
    Override, Final,
    Try, Catch, Throw,
    Constexpr, Consteval,
    Decltype, Typeid,
    DynamicCast, StaticCast, ReinterpretCast, ConstCast,
    Mutable, Volatile, Noexcept, ExplicitSpec,
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
}

impl CppTok {
    pub fn span(&self) -> Span {
        match self {
            Self::C(t) => t.span.clone(),
            Self::Kw(_, s) | Self::ColonColon(s) => s.clone(),
        }
    }

    pub fn is_access_spec(&self) -> bool {
        matches!(self, Self::Kw(CppKw::Public | CppKw::Private | CppKw::Protected, _))
    }
}

/// C++ token 流
#[derive(Clone)]
pub struct CppLexer<'a> {
    pub src: &'a [u8],
    pub file: String,
    pub pos: usize,
    pub line: u32,
    pub col: u32,
}

impl<'a> CppLexer<'a> {
    pub fn new(src: &'a [u8], file: &str) -> Self {
        Self { src, file: file.to_string(), pos: 0, line: 1, col: 1 }
    }

    /// 当前 span
    fn mk_span(&self, len: u32) -> Span {
        Span { file: self.file.clone(), line: self.line, col: self.col, len }
    }

    /// 跳过空白 / 注释 (C 风格 // 与 /* */)
    fn skip_trivia(&mut self) {
        loop {
            while self.pos < self.src.len() {
                let b = self.src[self.pos];
                if b == b'\n' { self.line += 1; self.col = 1; self.pos += 1; }
                else if b == b' ' || b == b'\t' || b == b'\r' { self.pos += 1; self.col += 1; }
                else { break; }
            }
            if self.pos + 1 < self.src.len() && &self.src[self.pos..self.pos+2] == b"//" {
                while self.pos < self.src.len() && self.src[self.pos] != b'\n' { self.pos += 1; }
                continue;
            }
            if self.pos + 1 < self.src.len() && &self.src[self.pos..self.pos+2] == b"/*" {
                self.pos += 2; self.col += 2;
                while self.pos + 1 < self.src.len()
                    && &self.src[self.pos..self.pos+2] != b"*/"
                {
                    if self.src[self.pos] == b'\n' { self.line += 1; self.col = 1; }
                    self.pos += 1;
                }
                if self.pos + 1 < self.src.len() { self.pos += 2; }
                continue;
            }
            break;
        }
    }

    /// 读一个 C++ token
    pub fn next_tok(&mut self) -> Option<CppTok> {
        self.skip_trivia();
        if self.pos >= self.src.len() { return None; }
        let b = self.src[self.pos];
        if b == b':' && self.pos + 1 < self.src.len() && self.src[self.pos + 1] == b':' {
            self.pos += 2; self.col += 2;
            return Some(CppTok::ColonColon(self.mk_span(2)));
        }
        if b.is_ascii_alphabetic() || b == b'_' {
            let start = self.pos;
            while self.pos < self.src.len()
                && (self.src[self.pos].is_ascii_alphanumeric() || self.src[self.pos] == b'_')
            { self.pos += 1; self.col += 1; }
            let s = std::str::from_utf8(&self.src[start..self.pos]).unwrap_or("");
            let len = (self.pos - start) as u32;
            let span = self.mk_span(len);
            if let Some(kw) = CppKw::from_ident(s) {
                return Some(CppTok::Kw(kw, span));
            }
            let tok = Tok { kind: TokKind::Ident, span, text: s.to_string() };
            return Some(CppTok::C(tok));
        }
        if b.is_ascii_digit() {
            let start = self.pos;
            while self.pos < self.src.len()
                && (self.src[self.pos].is_ascii_alphanumeric() || self.src[self.pos] == b'.')
            { self.pos += 1; self.col += 1; }
            let len = (self.pos - start) as u32;
            let span = self.mk_span(len);
            let tok = Tok { kind: TokKind::Number, span,
                            text: std::str::from_utf8(&self.src[start..self.pos])
                                .unwrap_or("").to_string() };
            return Some(CppTok::C(tok));
        }
        if b == b'"' || b == b'\'' {
            let q = b;
            let start = self.pos;
            self.pos += 1; self.col += 1;
            while self.pos < self.src.len() && self.src[self.pos] != q {
                if self.src[self.pos] == b'\\' && self.pos + 1 < self.src.len() {
                    self.pos += 1;
                }
                self.pos += 1;
            }
            if self.pos < self.src.len() { self.pos += 1; }
            let len = (self.pos - start) as u32;
            let span = self.mk_span(len);
            let tok = Tok {
                kind: if q == b'"' { TokKind::StringLit } else { TokKind::CharLit },
                span,
                text: std::str::from_utf8(&self.src[start..self.pos])
                    .unwrap_or("").to_string(),
            };
            return Some(CppTok::C(tok));
        }
        self.pos += 1; self.col += 1;
        let kind = match b {
            b'(' => TokKind::LParen, b')' => TokKind::RParen,
            b'{' => TokKind::LBrace, b'}' => TokKind::RBrace,
            b'[' => TokKind::LBracket, b']' => TokKind::RBracket,
            b';' => TokKind::Semi, b',' => TokKind::Comma,
            b'?' => TokKind::Question, b'~' => TokKind::Tilde,
            b'.' => TokKind::Dot, b'&' => TokKind::Amp,
            b'*' => TokKind::Star, b'+' => TokKind::Plus,
            b'-' => TokKind::Minus, b'/' => TokKind::Slash,
            b'%' => TokKind::Percent, b'^' => TokKind::Caret,
            b'|' => TokKind::Pipe, b'!' => TokKind::Not,
            b'=' => TokKind::Assign, b'<' => TokKind::Lt,
            b'>' => TokKind::Gt, b':' => TokKind::Colon,
            _ => TokKind::Invalid,
        };
        let tok = Tok { kind, span: self.mk_span(1), text: (b as char).to_string() };
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
        while let Some(t) = lx.next_tok() { toks.push(t); }
        assert!(matches!(toks[0], CppTok::Kw(CppKw::Class, _)));
        assert!(matches!(toks[1], CppTok::C(Tok { kind: TokKind::Ident, .. })));
        assert!(matches!(toks[2], CppTok::C(Tok { kind: TokKind::LBrace, .. })));
        assert!(matches!(toks[3], CppTok::Kw(CppKw::Public, _)));
        assert!(matches!(toks[4], CppTok::C(Tok { kind: TokKind::Colon, .. })));
    }
    #[test]
    fn cpp_lex_colon_colon() {
        let mut lx = CppLexer::new(b"Foo::bar", "t.cpp");
        let mut toks = vec![];
        while let Some(t) = lx.next_tok() { toks.push(t); }
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
}
