// src/tokens.rs - 词法分析
// 第一性原理: 词法是"字符流"到"语义单元"的映射,不是"语法"。
// suckless 准则: 词法状态机尽可能少。关键字/标识符/数字/字符串/符号,五大类。
// 关键字采用完美 hash 识别 (这里用 if-else 链,简单优先;后续可换 phf)。

use crate::diagnostics::Span;

#[derive(Debug, Clone, PartialEq)]
pub struct Tok {
    pub kind: TokKind,
    pub span: Span,
    pub text: String,   // 原始文本(关键字/标识符/数字/字符串存原始,符号存 1 字节)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokKind {
    // 字面量
    Number,        // 123 0x7b 0b101 0o17
    StringLit,     // "abc"
    CharLit,       // 'a'
    Ident,         // foo _x
    // 关键字
    KwInt, KwChar, KwShort, KwLong, KwVoid, KwFloat, KwDouble, KwSigned, KwUnsigned,
    KwStruct, KwUnion, KwEnum, KwTypedef,
    KwIf, KwElse, KwWhile, KwFor, KwDo, KwSwitch, KwCase, KwDefault, KwBreak, KwContinue,
    KwReturn, KwGoto, KwIfDefFake,        // 后者用于 #ifdef 兼容 (M0 不解析)
    KwStatic, KwExtern, KwConst, KwVolatile, KwInline, KwRestrict, KwAuto, KwRegister,
    KwSizeof, KwAlignof,
    // 符号
    LParen, RParen, LBrace, RBrace, LBracket, RBracket,
    Comma, Semi, Colon, Question,
    Assign,                  // =
    Plus, Minus, Star, Slash, Percent,
    Amp, Pipe, Caret, Tilde,
    Lt, Gt, Le, Ge, EqEq, Ne,
    AndAnd, OrOr, Not,
    Shl, Shr,                 // << >>
    PlusEq, MinusEq, StarEq, SlashEq, PercentEq,
    AmpEq, PipeEq, CaretEq, ShlEq, ShrEq,
    PlusPlus, MinusMinus,
    Arrow,                    // ->
    Dot,                      // .
    Ellipsis,                 // ...
    Hash,                     // #
    // 特殊
    Eof,
}

pub struct Lexer<'a> {
    pub src:    &'a [u8],
    pub file:   String,
    pub pos:    usize,
    pub line:   u32,
    pub col:    u32,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a [u8], file: &str) -> Self {
        Self { src, file: file.into(), pos: 0, line: 1, col: 1 }
    }

    fn peek(&self) -> Option<u8> { self.src.get(self.pos).copied() }
    fn peek_at(&self, off: usize) -> Option<u8> { self.src.get(self.pos + off).copied() }
    fn bump(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.pos += 1;
        if b == b'\n' { self.line += 1; self.col = 1; } else { self.col += 1; }
        Some(b)
    }

    fn span_here(&self, len: u32) -> Span {
        Span { file: self.file.clone(), line: self.line, col: self.col, len }
    }

    fn skip_ws_and_comments(&mut self) {
        loop {
            match self.peek() {
                Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n') => { self.bump(); }
                Some(b'/') if self.peek_at(1) == Some(b'/') => {
                    while self.peek().is_some() && self.peek() != Some(b'\n') { self.bump(); }
                }
                Some(b'/') if self.peek_at(1) == Some(b'*') => {
                    self.bump(); self.bump();
                    while let Some(c) = self.peek() {
                        if c == b'*' && self.peek_at(1) == Some(b'/') {
                            self.bump(); self.bump(); break;
                        }
                        self.bump();
                    }
                }
                _ => break,
            }
        }
    }

    pub fn next(&mut self) -> Tok {
        self.skip_ws_and_comments();
        let line = self.line; let col = self.col;
        let start = self.pos;
        let Some(b) = self.peek() else {
            return Tok { kind: TokKind::Eof, span: self.span_here(0), text: "".into() };
        };
        // 标识符 / 关键字
        if b.is_ascii_alphabetic() || b == b'_' {
            while let Some(c) = self.peek() {
                if c.is_ascii_alphanumeric() || c == b'_' { self.bump(); } else { break; }
            }
            let text = std::str::from_utf8(&self.src[start..self.pos]).unwrap().to_string();
            let kind = match text.as_str() {
                "int" => TokKind::KwInt, "char" => TokKind::KwChar,
                "short" => TokKind::KwShort, "long" => TokKind::KwLong,
                "void" => TokKind::KwVoid, "float" => TokKind::KwFloat,
                "double" => TokKind::KwDouble, "signed" => TokKind::KwSigned,
                "unsigned" => TokKind::KwUnsigned,
                "struct" => TokKind::KwStruct, "union" => TokKind::KwUnion,
                "enum" => TokKind::KwEnum, "typedef" => TokKind::KwTypedef,
                "if" => TokKind::KwIf, "else" => TokKind::KwElse,
                "while" => TokKind::KwWhile, "for" => TokKind::KwFor,
                "do" => TokKind::KwDo, "switch" => TokKind::KwSwitch,
                "case" => TokKind::KwCase, "default" => TokKind::KwDefault,
                "break" => TokKind::KwBreak, "continue" => TokKind::KwContinue,
                "return" => TokKind::KwReturn, "goto" => TokKind::KwGoto,
                "static" => TokKind::KwStatic, "extern" => TokKind::KwExtern,
                "const" => TokKind::KwConst, "volatile" => TokKind::KwVolatile,
                "inline" => TokKind::KwInline, "restrict" => TokKind::KwRestrict,
                "auto" => TokKind::KwAuto, "register" => TokKind::KwRegister,
                "sizeof" => TokKind::KwSizeof, "_Alignof" => TokKind::KwAlignof,
                _ => TokKind::Ident,
            };
            return Tok { kind, span: Span { file: self.file.clone(), line, col, len: (self.pos - start) as u32 }, text };
        }
        // 数字
        if b.is_ascii_digit() {
            if b == b'0' && matches!(self.peek_at(1), Some(b'x') | Some(b'X')) {
                self.bump(); self.bump();
                while let Some(c) = self.peek() {
                    if c.is_ascii_hexdigit() { self.bump(); } else { break; }
                }
            } else if b == b'0' && matches!(self.peek_at(1), Some(b'b') | Some(b'B')) {
                self.bump(); self.bump();
                while let Some(c) = self.peek() {
                    if c == b'0' || c == b'1' { self.bump(); } else { break; }
                }
            } else {
                while let Some(c) = self.peek() {
                    if c.is_ascii_digit() { self.bump(); } else { break; }
                }
            }
            let text = std::str::from_utf8(&self.src[start..self.pos]).unwrap().to_string();
            return Tok { kind: TokKind::Number, span: Span { file: self.file.clone(), line, col, len: (self.pos - start) as u32 }, text };
        }
        // 字符串
        if b == b'"' {
            self.bump();
            while let Some(c) = self.peek() {
                if c == b'"' { self.bump(); break; }
                if c == b'\\' { self.bump(); self.bump(); continue; }
                self.bump();
            }
            let text = std::str::from_utf8(&self.src[start..self.pos]).unwrap().to_string();
            return Tok { kind: TokKind::StringLit, span: Span { file: self.file.clone(), line, col, len: (self.pos - start) as u32 }, text };
        }
        if b == b'\'' {
            self.bump();
            while let Some(c) = self.peek() {
                if c == b'\'' { self.bump(); break; }
                if c == b'\\' { self.bump(); self.bump(); continue; }
                self.bump();
            }
            let text = std::str::from_utf8(&self.src[start..self.pos]).unwrap().to_string();
            return Tok { kind: TokKind::CharLit, span: Span { file: self.file.clone(), line, col, len: (self.pos - start) as u32 }, text };
        }
        // 符号
        self.bump();
        let (kind, len) = match b {
            b'(' => (TokKind::LParen, 1), b')' => (TokKind::RParen, 1),
            b'{' => (TokKind::LBrace, 1), b'}' => (TokKind::RBrace, 1),
            b'[' => (TokKind::LBracket, 1), b']' => (TokKind::RBracket, 1),
            b',' => (TokKind::Comma, 1), b';' => (TokKind::Semi, 1),
            b':' => (TokKind::Colon, 1), b'?' => (TokKind::Question, 1),
            b'~' => (TokKind::Tilde, 1),
            b'#' => (TokKind::Hash, 1),
            b'+' => if self.peek() == Some(b'+') { self.bump(); (TokKind::PlusPlus, 2) }
                    else if self.peek() == Some(b'=') { self.bump(); (TokKind::PlusEq, 2) }
                    else { (TokKind::Plus, 1) },
            b'-' => if self.peek() == Some(b'-') { self.bump(); (TokKind::MinusMinus, 2) }
                    else if self.peek() == Some(b'=') { self.bump(); (TokKind::MinusEq, 2) }
                    else if self.peek() == Some(b'>') { self.bump(); (TokKind::Arrow, 2) }
                    else { (TokKind::Minus, 1) },
            b'*' => if self.peek() == Some(b'=') { self.bump(); (TokKind::StarEq, 2) } else { (TokKind::Star, 1) },
            b'/' => if self.peek() == Some(b'=') { self.bump(); (TokKind::SlashEq, 2) } else { (TokKind::Slash, 1) },
            b'%' => if self.peek() == Some(b'=') { self.bump(); (TokKind::PercentEq, 2) } else { (TokKind::Percent, 1) },
            b'&' => if self.peek() == Some(b'&') { self.bump(); (TokKind::AndAnd, 2) }
                    else if self.peek() == Some(b'=') { self.bump(); (TokKind::AmpEq, 2) }
                    else { (TokKind::Amp, 1) },
            b'|' => if self.peek() == Some(b'|') { self.bump(); (TokKind::OrOr, 2) }
                    else if self.peek() == Some(b'=') { self.bump(); (TokKind::PipeEq, 2) }
                    else { (TokKind::Pipe, 1) },
            b'^' => if self.peek() == Some(b'=') { self.bump(); (TokKind::CaretEq, 2) } else { (TokKind::Caret, 1) },
            b'!' => if self.peek() == Some(b'=') { self.bump(); (TokKind::Ne, 2) } else { (TokKind::Not, 1) },
            b'=' => if self.peek() == Some(b'=') { self.bump(); (TokKind::EqEq, 2) } else { (TokKind::Assign, 1) },
            b'<' => if self.peek() == Some(b'=') { self.bump(); (TokKind::Le, 2) }
                    else if self.peek() == Some(b'<') {
                        self.bump();
                        if self.peek() == Some(b'=') { self.bump(); (TokKind::ShlEq, 3) } else { (TokKind::Shl, 2) }
                    } else { (TokKind::Lt, 1) },
            b'>' => if self.peek() == Some(b'=') { self.bump(); (TokKind::Ge, 2) }
                    else if self.peek() == Some(b'>') {
                        self.bump();
                        if self.peek() == Some(b'=') { self.bump(); (TokKind::ShrEq, 3) } else { (TokKind::Shr, 2) }
                    } else { (TokKind::Gt, 1) },
            b'.' => if self.peek() == Some(b'.') && self.peek_at(1) == Some(b'.') {
                self.bump(); self.bump(); (TokKind::Ellipsis, 3)
            } else { (TokKind::Dot, 1) },
            _ => { return Tok { kind: TokKind::Eof, span: self.span_here(1),
                                text: std::str::from_utf8(&[b]).unwrap().to_string() }; }
        };
        let text = std::str::from_utf8(&self.src[start..self.pos]).unwrap().to_string();
        Tok { kind, span: Span { file: self.file.clone(), line, col, len }, text }
    }
}
