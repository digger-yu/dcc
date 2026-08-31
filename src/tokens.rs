// src/tokens.rs - 词法分析
// 第一性原理: 词法是"字符流"到"语义单元"的映射,不是"语法"。
// suckless 准则: 词法状态机尽可能少。关键字/标识符/数字/字符串/符号,五大类。
// 关键字采用完美 hash 识别 (这里用 if-else 链,简单优先;后续可换 phf)。

use crate::diagnostics::Span;

#[derive(Debug, Clone, PartialEq)]
pub struct Tok {
    pub kind: TokKind,
    pub span: Span,
    pub text: String, // 原始文本(关键字/标识符/数字/字符串存原始,符号存 1 字节)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokKind {
    // 字面量
    Number,          // 123 0x7b 0b101 0o17
    FloatLit,        // 1.5 2.0f 3.14L
    StringLit,       // "abc"
    WideStringLit,   // L"abc" (wchar_t string, UTF-32 on Linux)
    Char16StringLit, // u"abc" (char16_t string)
    Char32StringLit, // U"abc" (char32_t string)
    CharLit,         // 'a'
    WideCharLit,     // L'a' (wchar_t char literal, wchar_t on Linux = int/UTF-32)
    Char16Lit,       // u'a' (char16_t char literal)
    Char32Lit,       // U'a' (char32_t char literal)
    Ident,           // foo _x
    // 关键字
    KwInt,
    KwChar,
    KwShort,
    KwLong,
    KwVoid,
    KwFloat,
    KwDouble,
    KwSigned,
    KwUnsigned,
    KwStruct,
    KwUnion,
    KwEnum,
    KwTypedef,
    KwIf,
    KwElse,
    KwWhile,
    KwFor,
    KwDo,
    KwSwitch,
    KwCase,
    KwDefault,
    KwBreak,
    KwContinue,
    KwReturn,
    KwGoto,
    KwIfDefFake, // 后者用于 #ifdef 兼容 (M0 不解析)
    KwStatic,
    KwExtern,
    KwConst,
    KwVolatile,
    KwInline,
    KwRestrict,
    KwAuto,
    KwRegister,
    KwAtomic, // _Atomic
    KwBool,   // _Bool (C99)
    KwChar8,  // char8_t (C23)
    KwChar16, // char16_t (C11)
    KwChar32, // char32_t (C11)
    KwFract,
    KwAccum,   // _Fract, _Accum (定点数)
    KwComplex,   // _Complex
    KwImaginary, // _Imaginary (C99)
    KwReal,      // __real__ / __real (取复数实部)
    KwImag,      // __imag__ / __imag (取复数虚部)
    KwDecimal32,
    KwDecimal64,
    KwDecimal128, // _Decimal32/64/128
    KwInt128,     // __int128 / __int128_t
    KwUInt128,    // __uint128_t
    KwSizeof,
    KwAlignof,
    KwDecltype, // decltype  (C++11 类型推导, M17.0)
    // 符号
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Semi,
    Colon,
    Question,
    Assign, // =
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Amp,
    Pipe,
    Caret,
    Tilde,
    Lt,
    Gt,
    Le,
    Ge,
    EqEq,
    Ne,
    AndAnd,
    OrOr,
    Not,
    Shl,
    Shr, // << >>
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
    AmpEq,
    PipeEq,
    CaretEq,
    ShlEq,
    ShrEq,
    PlusPlus,
    MinusMinus,
    Arrow,    // ->
    Dot,      // .
    DotStar,  // .*   (C++ 成员指针访问, M17.0)
    Ellipsis, // ...
    Hash,     // #
    // 特殊
    Invalid,
    Eof,
}

pub struct Lexer<'a> {
    pub src: &'a [u8],
    pub file: String,
    pub pos: usize,
    pub line: u32,
    pub col: u32,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a [u8], file: &str) -> Self {
        Self {
            src,
            file: file.into(),
            pos: 0,
            line: 1,
            col: 1,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }
    fn peek_at(&self, off: usize) -> Option<u8> {
        self.src.get(self.pos + off).copied()
    }
    fn bump(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.pos += 1;
        if b == b'\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(b)
    }

    fn span_here(&self, len: u32) -> Span {
        Span {
            file: self.file.clone(),
            line: self.line,
            col: self.col,
            len,
        }
    }

    fn skip_ws_and_comments(&mut self) {
        loop {
            match self.peek() {
                Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n') | Some(b'\x0c')
                | Some(b'\x0b') => {
                    self.bump();
                }
                Some(b'/') if self.peek_at(1) == Some(b'/') => {
                    while self.peek().is_some() && self.peek() != Some(b'\n') {
                        self.bump();
                    }
                }
                Some(b'/') if self.peek_at(1) == Some(b'*') => {
                    self.bump();
                    self.bump();
                    while let Some(c) = self.peek() {
                        if c == b'*' && self.peek_at(1) == Some(b'/') {
                            self.bump();
                            self.bump();
                            break;
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
        let line = self.line;
        let col = self.col;
        let start = self.pos;
        let Some(b) = self.peek() else {
            return Tok {
                kind: TokKind::Eof,
                span: self.span_here(0),
                text: "".into(),
            };
        };
        // 标识符 / 关键字 (C99: support universal character names \uXXXX / \UXXXXXXXX)
        if b.is_ascii_alphabetic()
            || b == b'_'
            || (b == b'\\'
                && self.pos + 1 < self.src.len()
                && (self.src[self.pos + 1] == b'u'
                    || self.src[self.pos + 1] == b'U'))
        {
            while let Some(c) = self.peek() {
                if c.is_ascii_alphanumeric() || c == b'_' {
                    self.bump();
                } else if c == b'\\' {
                    // C99 universal character name: \uXXXX or \UXXXXXXXX
                    let saved_pos = self.pos;
                    let saved_col = self.col;
                    self.bump(); // consume '\\'
                    if self.peek() == Some(b'u') {
                        self.bump();
                        let mut ok = true;
                        for _ in 0..4 {
                            if matches!(self.peek(), Some(c) if c.is_ascii_hexdigit()) {
                                self.bump();
                            } else {
                                ok = false;
                                break;
                            }
                        }
                        if !ok {
                            self.pos = saved_pos;
                            self.col = saved_col;
                            break;
                        }
                    } else if self.peek() == Some(b'U') {
                        self.bump();
                        let mut ok = true;
                        for _ in 0..8 {
                            if matches!(self.peek(), Some(c) if c.is_ascii_hexdigit()) {
                                self.bump();
                            } else {
                                ok = false;
                                break;
                            }
                        }
                        if !ok {
                            self.pos = saved_pos;
                            self.col = saved_col;
                            break;
                        }
                    } else {
                        self.pos = saved_pos;
                        self.col = saved_col;
                        break;
                    }
                } else {
                    break;
                }
            }
            let text = std::str::from_utf8(&self.src[start..self.pos])
                .unwrap()
                .to_string();
            let kind = match text.as_str() {
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
                "const" | "__const__" => TokKind::KwConst,
                "volatile" | "__volatile__" => TokKind::KwVolatile,
                "inline" | "__inline__" | "__inline" => TokKind::KwInline,
                "restrict" | "__restrict__" | "__restrict" => TokKind::KwRestrict,
                "auto" => TokKind::KwAuto,
                "register" => TokKind::KwRegister,
                "sizeof" => TokKind::KwSizeof,
                "_Alignof" | "alignof" | "__alignof" | "__alignof__" => TokKind::KwAlignof,
                "decltype" => TokKind::KwDecltype, // M17.0
                "_Atomic" => TokKind::KwAtomic,
                "_Bool" => TokKind::KwBool,
                "char8_t" => TokKind::KwChar8,
                "char16_t" => TokKind::KwChar16,
                "char32_t" => TokKind::KwChar32,
                "_Fract" => TokKind::KwFract,
                "_Accum" => TokKind::KwAccum,
                "_Complex" | "__complex__" | "__complex" => TokKind::KwComplex,
                "_Imaginary" | "__imaginary" => TokKind::KwImaginary,
                "__real__" | "__real" => TokKind::KwReal,
                "__imag__" | "__imag" => TokKind::KwImag,
                "_Decimal32" => TokKind::KwDecimal32,
                "_Decimal64" => TokKind::KwDecimal64,
                "_Decimal128" => TokKind::KwDecimal128,
                "__int128" | "__int128_t" => TokKind::KwInt128,
                "__uint128_t" => TokKind::KwUInt128,
                _ => TokKind::Ident,
            };
            // L"..." wide string literal
            if text == "L" && self.peek() == Some(b'"') {
                self.bump(); // consume opening '"'
                let mut terminated = false;
                while let Some(c) = self.peek() {
                    if c == b'"' {
                        self.bump();
                        terminated = true;
                        break;
                    }
                    if c == b'\n' || c == b'\r' {
                        break;
                    }
                    if c == b'\\' {
                        self.bump();
                        self.bump();
                        continue;
                    }
                    self.bump();
                }
                let text = std::str::from_utf8(&self.src[start..self.pos])
                    .unwrap()
                    .to_string();
                let kind = if terminated {
                    TokKind::WideStringLit
                } else {
                    TokKind::Invalid
                };
                return Tok {
                    kind,
                    span: Span {
                        file: self.file.clone(),
                        line,
                        col,
                        len: (self.pos - start) as u32,
                    },
                    text,
                };
            }
            // L'...' wide char literal
            if text == "L" && self.peek() == Some(b'\'') {
                self.bump(); // consume opening '\''
                let mut terminated = false;
                while let Some(c) = self.peek() {
                    if c == b'\'' {
                        self.bump();
                        terminated = true;
                        break;
                    }
                    if c == b'\n' || c == b'\r' {
                        break;
                    }
                    if c == b'\\' {
                        self.bump();
                        self.bump();
                        continue;
                    }
                    self.bump();
                }
                let text = std::str::from_utf8(&self.src[start..self.pos])
                    .unwrap()
                    .to_string();
                let kind = if terminated {
                    TokKind::WideCharLit
                } else {
                    TokKind::Invalid
                };
                return Tok {
                    kind,
                    span: Span {
                        file: self.file.clone(),
                        line,
                        col,
                        len: (self.pos - start) as u32,
                    },
                    text,
                };
            }
            // u"..." char16_t string literal
            if text == "u" && self.peek() == Some(b'"') {
                self.bump(); // consume opening '"'
                let mut terminated = false;
                while let Some(c) = self.peek() {
                    if c == b'"' {
                        self.bump();
                        terminated = true;
                        break;
                    }
                    if c == b'\n' || c == b'\r' {
                        break;
                    }
                    if c == b'\\' {
                        self.bump();
                        self.bump();
                        continue;
                    }
                    self.bump();
                }
                let text = std::str::from_utf8(&self.src[start..self.pos])
                    .unwrap()
                    .to_string();
                let kind = if terminated {
                    TokKind::Char16StringLit
                } else {
                    TokKind::Invalid
                };
                return Tok {
                    kind,
                    span: Span {
                        file: self.file.clone(),
                        line,
                        col,
                        len: (self.pos - start) as u32,
                    },
                    text,
                };
            }
            // u'...' char16_t char literal
            if text == "u" && self.peek() == Some(b'\'') {
                self.bump(); // consume opening '\''
                let mut terminated = false;
                while let Some(c) = self.peek() {
                    if c == b'\'' {
                        self.bump();
                        terminated = true;
                        break;
                    }
                    if c == b'\n' || c == b'\r' {
                        break;
                    }
                    if c == b'\\' {
                        self.bump();
                        self.bump();
                        continue;
                    }
                    self.bump();
                }
                let text = std::str::from_utf8(&self.src[start..self.pos])
                    .unwrap()
                    .to_string();
                let kind = if terminated {
                    TokKind::Char16Lit
                } else {
                    TokKind::Invalid
                };
                return Tok {
                    kind,
                    span: Span {
                        file: self.file.clone(),
                        line,
                        col,
                        len: (self.pos - start) as u32,
                    },
                    text,
                };
            }
            // U"..." char32_t string literal
            if text == "U" && self.peek() == Some(b'"') {
                self.bump(); // consume opening '"'
                let mut terminated = false;
                while let Some(c) = self.peek() {
                    if c == b'"' {
                        self.bump();
                        terminated = true;
                        break;
                    }
                    if c == b'\n' || c == b'\r' {
                        break;
                    }
                    if c == b'\\' {
                        self.bump();
                        self.bump();
                        continue;
                    }
                    self.bump();
                }
                let text = std::str::from_utf8(&self.src[start..self.pos])
                    .unwrap()
                    .to_string();
                let kind = if terminated {
                    TokKind::Char32StringLit
                } else {
                    TokKind::Invalid
                };
                return Tok {
                    kind,
                    span: Span {
                        file: self.file.clone(),
                        line,
                        col,
                        len: (self.pos - start) as u32,
                    },
                    text,
                };
            }
            // U'...' char32_t char literal
            if text == "U" && self.peek() == Some(b'\'') {
                self.bump(); // consume opening '\''
                let mut terminated = false;
                while let Some(c) = self.peek() {
                    if c == b'\'' {
                        self.bump();
                        terminated = true;
                        break;
                    }
                    if c == b'\n' || c == b'\r' {
                        break;
                    }
                    if c == b'\\' {
                        self.bump();
                        self.bump();
                        continue;
                    }
                    self.bump();
                }
                let text = std::str::from_utf8(&self.src[start..self.pos])
                    .unwrap()
                    .to_string();
                let kind = if terminated {
                    TokKind::Char32Lit
                } else {
                    TokKind::Invalid
                };
                return Tok {
                    kind,
                    span: Span {
                        file: self.file.clone(),
                        line,
                        col,
                        len: (self.pos - start) as u32,
                    },
                    text,
                };
            }
            return Tok {
                kind,
                span: Span {
                    file: self.file.clone(),
                    line,
                    col,
                    len: (self.pos - start) as u32,
                },
                text,
            };
        }
        // 数字
        if b.is_ascii_digit() || (b == b'.' && self.peek_at(1).is_some_and(|c| c.is_ascii_digit()))
        {
            let mut is_float = b == b'.';
            if b == b'0' && matches!(self.peek_at(1), Some(b'x') | Some(b'X')) {
                self.bump();
                self.bump();
                while let Some(c) = self.peek() {
                    if c == b'\'' && self.peek_at(1).is_some_and(|n| n.is_ascii_hexdigit()) {
                        self.bump();
                        continue;
                    }
                    if c.is_ascii_hexdigit() {
                        self.bump();
                    } else if c == b'.' && !is_float {
                        // hex float: 0x1.5p3
                        is_float = true;
                        self.bump();
                    } else {
                        break;
                    }
                }
                // hex float exponent: p/P followed by decimal exponent
                if is_float || matches!(self.peek(), Some(b'p') | Some(b'P')) {
                    if matches!(self.peek(), Some(b'p') | Some(b'P')) {
                        is_float = true;
                        self.bump();
                        if let Some(b'+') | Some(b'-') = self.peek() {
                            self.bump();
                        }
                        while let Some(c) = self.peek() {
                            if c.is_ascii_digit() {
                                self.bump();
                            } else {
                                break;
                            }
                        }
                    }
                }
            } else if b == b'0' && matches!(self.peek_at(1), Some(b'b') | Some(b'B')) {
                self.bump();
                self.bump();
                while let Some(c) = self.peek() {
                    if c == b'\'' && self.peek_at(1).is_some_and(|n| n == b'0' || n == b'1') {
                        self.bump();
                        continue;
                    }
                    if c == b'0' || c == b'1' {
                        self.bump();
                    } else {
                        break;
                    }
                }
            } else {
                // If the number starts with '.', consume it first
                if b == b'.' {
                    self.bump();
                }
                while let Some(c) = self.peek() {
                    if c == b'\'' && self.peek_at(1).is_some_and(|n| n.is_ascii_digit()) {
                        self.bump();
                        continue;
                    }
                    if c.is_ascii_digit() {
                        self.bump();
                    } else if c == b'.' && !is_float {
                        is_float = true;
                        self.bump();
                    } else {
                        break;
                    }
                }
                // 科学计数法
                if let Some(b'e') | Some(b'E') = self.peek() {
                    is_float = true;
                    self.bump();
                    if let Some(b'+') | Some(b'-') = self.peek() {
                        self.bump();
                    }
                    while let Some(c) = self.peek() {
                        if c.is_ascii_digit() {
                            self.bump();
                        } else {
                            break;
                        }
                    }
                }
            }
            // 数字后缀。整数允许 U/L/LL 的任意标准顺序组合；浮点允许 F/L。
            // 两者均可追加 C99 虚数后缀 i/I/j/J (如 4.0i、1.0iF、3j)。
            if is_float {
                // 浮点: 类型后缀 F/L 与虚数后缀 i/j 各最多一个, 顺序任意
                for _ in 0..2 {
                    match self.peek() {
                        Some(b'f' | b'F' | b'l' | b'L') => {
                            self.bump();
                        }
                        Some(b'i' | b'I' | b'j' | b'J') => {
                            self.bump();
                        }
                        _ => break,
                    }
                }
            } else {
                while matches!(self.peek(), Some(b'u' | b'U' | b'l' | b'L')) {
                    self.bump();
                }
                if matches!(self.peek(), Some(b'i' | b'I' | b'j' | b'J')) {
                    self.bump();
                }
            }
            // C23 数字分隔符: 移除 ' 字符
            let raw = &self.src[start..self.pos];
            let text: String = raw
                .iter()
                .filter(|&&b| b != b'\'')
                .map(|&b| b as char)
                .collect();
            let kind = if is_float { TokKind::FloatLit } else { TokKind::Number };
            return Tok {
                kind,
                span: Span {
                    file: self.file.clone(),
                    line,
                    col,
                    len: (self.pos - start) as u32,
                },
                text,
            };
        }
        // 字符串
        if b == b'"' {
            self.bump();
            let mut terminated = false;
            while let Some(c) = self.peek() {
                if c == b'"' {
                    self.bump();
                    terminated = true;
                    break;
                }
                if c == b'\n' || c == b'\r' {
                    break;
                }
                if c == b'\\' {
                    self.bump();
                    self.bump();
                    continue;
                }
                self.bump();
            }
            let text = std::str::from_utf8(&self.src[start..self.pos])
                .unwrap()
                .to_string();
            let kind = if terminated {
                TokKind::StringLit
            } else {
                TokKind::Invalid
            };
            return Tok {
                kind,
                span: Span {
                    file: self.file.clone(),
                    line,
                    col,
                    len: (self.pos - start) as u32,
                },
                text,
            };
        }
        if b == b'\'' {
            self.bump();
            let mut terminated = false;
            while let Some(c) = self.peek() {
                if c == b'\'' {
                    self.bump();
                    terminated = true;
                    break;
                }
                if c == b'\n' || c == b'\r' {
                    break;
                }
                if c == b'\\' {
                    self.bump();
                    self.bump();
                    continue;
                }
                self.bump();
            }
            let text = std::str::from_utf8(&self.src[start..self.pos])
                .unwrap()
                .to_string();
            let kind = if terminated { TokKind::CharLit } else { TokKind::Invalid };
            return Tok {
                kind,
                span: Span {
                    file: self.file.clone(),
                    line,
                    col,
                    len: (self.pos - start) as u32,
                },
                text,
            };
        }
        // 符号
        self.bump();
        let (kind, len) = match b {
            b'(' => (TokKind::LParen, 1),
            b')' => (TokKind::RParen, 1),
            b'{' => (TokKind::LBrace, 1),
            b'}' => (TokKind::RBrace, 1),
            b'[' => (TokKind::LBracket, 1),
            b']' => (TokKind::RBracket, 1),
            b',' => (TokKind::Comma, 1),
            b';' => (TokKind::Semi, 1),
            b':' => {
                if self.peek() == Some(b'>') {
                    // digraph :> → ]
                    self.bump();
                    (TokKind::RBracket, 2)
                } else {
                    (TokKind::Colon, 1)
                }
            }
            b'?' => (TokKind::Question, 1),
            b'~' => (TokKind::Tilde, 1),
            b'#' => (TokKind::Hash, 1),
            b'+' => {
                if self.peek() == Some(b'+') {
                    self.bump();
                    (TokKind::PlusPlus, 2)
                } else if self.peek() == Some(b'=') {
                    self.bump();
                    (TokKind::PlusEq, 2)
                } else {
                    (TokKind::Plus, 1)
                }
            }
            b'-' => {
                if self.peek() == Some(b'-') {
                    self.bump();
                    (TokKind::MinusMinus, 2)
                } else if self.peek() == Some(b'=') {
                    self.bump();
                    (TokKind::MinusEq, 2)
                } else if self.peek() == Some(b'>') {
                    self.bump();
                    (TokKind::Arrow, 2)
                } else {
                    (TokKind::Minus, 1)
                }
            }
            b'*' => {
                if self.peek() == Some(b'=') {
                    self.bump();
                    (TokKind::StarEq, 2)
                } else {
                    (TokKind::Star, 1)
                }
            }
            b'/' => {
                if self.peek() == Some(b'=') {
                    self.bump();
                    (TokKind::SlashEq, 2)
                } else {
                    (TokKind::Slash, 1)
                }
            }
            b'%' => {
                if self.peek() == Some(b'=') {
                    self.bump();
                    (TokKind::PercentEq, 2)
                } else if self.peek() == Some(b'>') {
                    // digraph %> → }
                    self.bump();
                    (TokKind::RBrace, 2)
                } else if self.peek() == Some(b':') {
                    // digraph %: → # (and %:%: → ## as two Hash tokens)
                    self.bump();
                    (TokKind::Hash, 2)
                } else {
                    (TokKind::Percent, 1)
                }
            }
            b'&' => {
                if self.peek() == Some(b'&') {
                    self.bump();
                    (TokKind::AndAnd, 2)
                } else if self.peek() == Some(b'=') {
                    self.bump();
                    (TokKind::AmpEq, 2)
                } else {
                    (TokKind::Amp, 1)
                }
            }
            b'|' => {
                if self.peek() == Some(b'|') {
                    self.bump();
                    (TokKind::OrOr, 2)
                } else if self.peek() == Some(b'=') {
                    self.bump();
                    (TokKind::PipeEq, 2)
                } else {
                    (TokKind::Pipe, 1)
                }
            }
            b'^' => {
                if self.peek() == Some(b'=') {
                    self.bump();
                    (TokKind::CaretEq, 2)
                } else {
                    (TokKind::Caret, 1)
                }
            }
            b'!' => {
                if self.peek() == Some(b'=') {
                    self.bump();
                    (TokKind::Ne, 2)
                } else {
                    (TokKind::Not, 1)
                }
            }
            b'=' => {
                if self.peek() == Some(b'=') {
                    self.bump();
                    (TokKind::EqEq, 2)
                } else {
                    (TokKind::Assign, 1)
                }
            }
            b'<' => {
                if self.peek() == Some(b'=') {
                    self.bump();
                    (TokKind::Le, 2)
                } else if self.peek() == Some(b'<') {
                    self.bump();
                    if self.peek() == Some(b'=') {
                        self.bump();
                        (TokKind::ShlEq, 3)
                    } else {
                        (TokKind::Shl, 2)
                    }
                } else if self.peek() == Some(b':') {
                    // digraph <: → [
                    self.bump();
                    (TokKind::LBracket, 2)
                } else if self.peek() == Some(b'%') {
                    // digraph <% → {
                    self.bump();
                    (TokKind::LBrace, 2)
                } else {
                    (TokKind::Lt, 1)
                }
            }
            b'>' => {
                if self.peek() == Some(b'=') {
                    self.bump();
                    (TokKind::Ge, 2)
                } else if self.peek() == Some(b'>') {
                    self.bump();
                    if self.peek() == Some(b'=') {
                        self.bump();
                        (TokKind::ShrEq, 3)
                    } else {
                        (TokKind::Shr, 2)
                    }
                } else {
                    (TokKind::Gt, 1)
                }
            }
            b'.' => {
                if self.peek() == Some(b'.') && self.peek_at(1) == Some(b'.') {
                    self.bump();
                    self.bump();
                    (TokKind::Ellipsis, 3)
                } else {
                    (TokKind::Dot, 1)
                }
            }
            _ => {
                return Tok {
                    kind: TokKind::Invalid,
                    span: self.span_here(1),
                    text: std::str::from_utf8(&[b]).unwrap().to_string(),
                };
            }
        };
        let text = std::str::from_utf8(&self.src[start..self.pos])
            .unwrap()
            .to_string();
        Tok {
            kind,
            span: Span {
                file: self.file.clone(),
                line,
                col,
                len,
            },
            text,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Lexer, TokKind};

    #[test]
    fn malformed_literals_and_unknown_bytes_are_not_eof() {
        for source in [b"\"unterminated\n".as_slice(), b"'unterminated\n", b"@"] {
            let mut lexer = Lexer::new(source, "invalid-token.c");
            assert_eq!(lexer.next().kind, TokKind::Invalid);
        }
    }

    #[test]
    fn integer_suffixes_are_part_of_the_number_token() {
        for source in ["1U", "1UL", "1LU", "1ULL", "0xffUL", "0X10llu"] {
            let mut lexer = Lexer::new(source.as_bytes(), "integer-suffix.c");
            assert_eq!(lexer.next().kind, TokKind::Number, "{source}");
            assert_eq!(lexer.next().kind, TokKind::Eof, "{source}");
        }
    }

    #[test]
    fn wide_string_and_char_literals() {
        // L"hello" 应产生 WideStringLit
        let mut lx = Lexer::new(br#"L"hello""#, "wide.c");
        let tok = lx.next();
        assert_eq!(tok.kind, TokKind::WideStringLit);
        assert_eq!(tok.text.as_str(), r#"L"hello""#);
        assert_eq!(lx.next().kind, TokKind::Eof);

        // L'a' 应产生 WideCharLit
        let mut lx = Lexer::new(b"L'a'", "wide.c");
        let tok = lx.next();
        assert_eq!(tok.kind, TokKind::WideCharLit);
        assert_eq!(tok.text.as_str(), "L'a'");
        assert_eq!(lx.next().kind, TokKind::Eof);

        // L'\u00e9' 宽字符 (UTF-8 字节序列 e9 在源里)
        let mut lx = Lexer::new("L'\u{e9}'".as_bytes(), "wide.c");
        let tok = lx.next();
        assert_eq!(tok.kind, TokKind::WideCharLit);

        // 普通 'a' 仍是 CharLit, 不被 L 前缀吞
        let mut lx = Lexer::new(b"'a'", "plain.c");
        assert_eq!(lx.next().kind, TokKind::CharLit);
        assert_eq!(lx.next().kind, TokKind::Eof);

        // L 后接 identifier (非 ' " 前缀) 应是普通 ident Lx
        let mut lx = Lexer::new(b"Lx", "ident.c");
        let tok = lx.next();
        assert_eq!(tok.kind, TokKind::Ident);
        assert_eq!(tok.text.as_str(), "Lx");
        assert_eq!(lx.next().kind, TokKind::Eof);
    }

    #[test]
    fn wide_char_unterminated_is_invalid() {
        let mut lx = Lexer::new(b"L'abc", "wide.c");
        assert_eq!(lx.next().kind, TokKind::Invalid);
    }
}
