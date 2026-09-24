// src/tokens.rs - 词法分析
// 第一性原理: 词法是"字符流"到"语义单元"的映射,不是"语法"。
// suckless 准则: 词法状态机尽可能少。关键字/标识符/数字/字符串/符号,五大类。
// 关键字采用排序表 + 二分查找识别 (O(log N) 比较, 替代长 match 链)。

use crate::diagnostics::Span;
use std::sync::Arc;

/// 关键字查找表 (按字符串排序, 供二分查找使用)
const KEYWORD_TABLE: &[(&str, TokKind)] = &[
    ("_Accum", TokKind::KwAccum),
    ("_Alignof", TokKind::KwAlignof),
    ("_Atomic", TokKind::KwAtomic),
    ("_Bool", TokKind::KwBool),
    ("_Complex", TokKind::KwComplex),
    ("_Decimal128", TokKind::KwDecimal128),
    ("_Decimal32", TokKind::KwDecimal32),
    ("_Decimal64", TokKind::KwDecimal64),
    ("_Fract", TokKind::KwFract),
    ("_Imaginary", TokKind::KwImaginary),
    ("__alignof", TokKind::KwAlignof),
    ("__alignof__", TokKind::KwAlignof),
    ("__complex", TokKind::KwComplex),
    ("__complex__", TokKind::KwComplex),
    ("__const", TokKind::KwConst),
    ("__const__", TokKind::KwConst),
    ("__imag", TokKind::KwImag),
    ("__imag__", TokKind::KwImag),
    ("__inline", TokKind::KwInline),
    ("__inline__", TokKind::KwInline),
    ("__int128", TokKind::KwInt128),
    ("__int128_t", TokKind::KwInt128),
    ("__real", TokKind::KwReal),
    ("__real__", TokKind::KwReal),
    ("__restrict", TokKind::KwRestrict),
    ("__restrict__", TokKind::KwRestrict),
    ("__signed__", TokKind::KwSigned),
    ("__uint128_t", TokKind::KwUInt128),
    ("__volatile", TokKind::KwVolatile),
    ("__volatile__", TokKind::KwVolatile),
    ("alignof", TokKind::KwAlignof),
    ("auto", TokKind::KwAuto),
    ("break", TokKind::KwBreak),
    ("case", TokKind::KwCase),
    ("char", TokKind::KwChar),
    ("char16_t", TokKind::KwChar16),
    ("char32_t", TokKind::KwChar32),
    ("char8_t", TokKind::KwChar8),
    ("const", TokKind::KwConst),
    ("continue", TokKind::KwContinue),
    ("decltype", TokKind::KwDecltype),
    ("default", TokKind::KwDefault),
    ("do", TokKind::KwDo),
    ("double", TokKind::KwDouble),
    ("else", TokKind::KwElse),
    ("enum", TokKind::KwEnum),
    ("extern", TokKind::KwExtern),
    ("float", TokKind::KwFloat),
    ("for", TokKind::KwFor),
    ("goto", TokKind::KwGoto),
    ("if", TokKind::KwIf),
    ("inline", TokKind::KwInline),
    ("int", TokKind::KwInt),
    ("long", TokKind::KwLong),
    ("register", TokKind::KwRegister),
    ("restrict", TokKind::KwRestrict),
    ("return", TokKind::KwReturn),
    ("short", TokKind::KwShort),
    ("signed", TokKind::KwSigned),
    ("sizeof", TokKind::KwSizeof),
    ("static", TokKind::KwStatic),
    ("struct", TokKind::KwStruct),
    ("switch", TokKind::KwSwitch),
    ("typedef", TokKind::KwTypedef),
    ("union", TokKind::KwUnion),
    ("unsigned", TokKind::KwUnsigned),
    ("void", TokKind::KwVoid),
    ("volatile", TokKind::KwVolatile),
    ("while", TokKind::KwWhile),
];

/// 关键字二分查找: O(log N) 次字符串比较, 替代长 match 链
#[inline]
fn keyword_lookup(text: &str) -> TokKind {
    KEYWORD_TABLE
        .binary_search_by_key(&text, |&(k, _)| k)
        .map(|i| KEYWORD_TABLE[i].1)
        .unwrap_or(TokKind::Ident)
}

// ============================================================================
// char_class: 单字节 ASCII 字符分类查表 (Phase 3 优化基础)
// ============================================================================
// 第一性原理: 词法状态机在每次 next_tok 起始处都要识别字符类别 (空白 /
//   字母 / 数字 / 标点 / 引号 ...). 原实现直接调用 `is_ascii_*` 系列函数,
//   每次都进行若干分支比较. 用 256 字节查表代替之: 单次数组索引 + 表项比对,
//   在热路径上分支数显著下降.
//
// suckless 准则: 不引入第三方依赖; 256 字节静态表 + const fn 即可; 编译期
//   求值, 运行时零成本. 表项 0..=7 对应 8 类; 用 u8 表示足够, 占 1 字节.
//
// 类别编号:
//   0 WHITESPACE      — 空格 / 制表 / 回车 (跳过)
//   1 LETTER          — ASCII 字母 (标识符首字符的子集)
//   2 DIGIT           — ASCII 十进制数字
//   3 IDENT_CONT      — 标识符后续字符 (字母 + 数字 + 下划线)
//   4 QUOTE           — '"' 或 '\'' (字符串/字符字面量)
//   5 PUNCT           — 运算符/分隔符 (lex 主要派发到多字符 token)
//   6 OTHER           — 未分类字节 (非 ASCII 高字节 / 罕见符号, 走 Invalid)
//   7 NEWLINE         — '\n' (单独识别便于行号维护)
//
// 注: 类别 1 LETTER 与 3 IDENT_CONT 的拆分是为了让 is_id_start 只需一次
//   比较 (== LETTER), 而 is_id_continue 用类别 3 直接索引即可, 不必 OR
//   多个类别. 这是 LLVM 难以自动合成的小优化.
mod char_class {
    pub const WHITESPACE: u8 = 0;
    pub const LETTER: u8 = 1;
    pub const DIGIT: u8 = 2;
    pub const IDENT_CONT: u8 = 3;
    pub const QUOTE: u8 = 4;
    pub const PUNCT: u8 = 5;
    pub const OTHER: u8 = 6;
    pub const NEWLINE: u8 = 7;

    /// 对单个字节做分类 (const fn, 编译期可求值).
    const fn classify_byte(b: u8) -> u8 {
        match b {
            b' ' | b'\t' | b'\r' | 0x0b | 0x0c => WHITESPACE,
            b'\n' => NEWLINE,
            b'"' | b'\'' => QUOTE,
            b'A'..=b'Z' | b'a'..=b'z' => LETTER,
            b'0'..=b'9' => DIGIT,
            b'_' => IDENT_CONT,
            b'(' | b')' | b'{' | b'}' | b'[' | b']' | b',' | b';' | b':' | b'?' | b'~' | b'#'
            | b'+' | b'-' | b'*' | b'/' | b'%' | b'&' | b'|' | b'^' | b'!' | b'=' | b'<' | b'>'
            | b'.' | b'@' | b'$' | b'\\' | b'`' => PUNCT,
            _ => OTHER,
        }
    }

    /// 256 项查表, 编译期用 classify_byte 填充. 在热路径上单次数组加载
    /// + 一次比较即可完成字符分类, 优于多次 ASCII 谓词的链式调用.
    pub const CLASS_TABLE: [u8; 256] = {
        let mut table = [OTHER; 256];
        let mut i: usize = 0;
        while i < 256 {
            table[i] = classify_byte(i as u8);
            i += 1;
        }
        table
    };

    /// 编译期可调用的字符分类入口.
    #[inline]
    pub const fn classify(c: u8) -> u8 {
        CLASS_TABLE[c as usize]
    }

    /// 标识符首字符: 字母 (LETTER) 或下划线 (IDENT_CONT 中的 '_').
    /// 下划线在 CLASS_TABLE 中归类为 IDENT_CONT, 故需单独 OR.
    #[inline]
    pub const fn is_id_start(c: u8) -> bool {
        classify(c) == LETTER || c == b'_'
    }

    /// 标识符后续字符: 字母 / 数字 / 下划线.
    ///
    /// LETTER / DIGIT / IDENT_CONT 三类都合法: 字母 + 数字 + 下划线
    /// 共同构成 identifier continuation. LLVM 通常能把此 OR 链优化为
    /// 一次 mask-and-compare, 等价于"class ∈ {LETTER, DIGIT, IDENT_CONT}".
    #[inline]
    pub const fn is_id_continue(c: u8) -> bool {
        let cls = classify(c);
        cls == LETTER || cls == DIGIT || cls == IDENT_CONT
    }
}

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
    KwAccum,     // _Fract, _Accum (定点数)
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
    pub file: Arc<str>,
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

    /// 当前字符的类别 (查 char_class 表), 无边界检查版本.
    ///
    /// 调用者必须保证 `self.pos < self.src.len()`. 源码输入是 UTF-8
    /// ASCII 子集 (C 源文件单字节字符都是 ASCII), lex 入口已经在
    /// `peek()` 处确认过非空, 故此处的越界访问是安全的.
    ///
    /// 与 `self.peek()` 不同, 此函数跳过 `Option<u8>` 的 None 分支与
    /// `slice::get` 的隐式长度比较, 适合作为热路径上多次分类的内联.
    #[inline]
    fn peek_char_class(&self) -> u8 {
        let b = unsafe { *self.src.get_unchecked(self.pos) };
        char_class::classify(b)
    }

    fn span_here(&self, len: u32) -> Span {
        Span {
            file: self.file.clone(),
            line: self.line,
            col: self.col,
            len,
        }
    }

    /// 判断给定行/列 (1-based) 的 `__attribute__((NAME))` 是否为 fallthrough.
    /// 仅在 `is_attribute_stmt` 判定为独立语句后才调用.
    /// 返回 `Some(NAME)` 如果是已识别属性, `None` 否则.
    pub fn gnu_attr_name_at(&self, line: usize, col: usize) -> Option<String> {
        // 找到目标行的起始字节偏移
        let mut p = 0;
        let mut cur_line = 1;
        while p < self.src.len() && cur_line < line {
            if self.src[p] == b'\n' {
                cur_line += 1;
            }
            p += 1;
        }
        // p 指向目标行的起始; col-1 得到 token 起始
        p += col.saturating_sub(1);
        // 跳过 __attribute__
        while p < self.src.len()
            && ((self.src[p] as char).is_ascii_alphanumeric() || self.src[p] == b'_')
        {
            p += 1;
        }
        // 跳过空白
        while p < self.src.len() && (self.src[p] as char).is_ascii_whitespace() {
            p += 1;
        }
        // 应该看到 ((
        if p + 1 >= self.src.len() || self.src[p] != b'(' || self.src[p + 1] != b'(' {
            return None;
        }
        p += 2;
        let mut depth = 2i32;
        while p < self.src.len() && depth > 0 {
            if self.src[p] == b'(' {
                depth += 1;
            } else if self.src[p] == b')' {
                depth -= 1;
            }
            p += 1;
        }
        // 跳过空白
        while p < self.src.len() && (self.src[p] as char).is_ascii_whitespace() {
            p += 1;
        }
        // 检查是否 ; 或 { (独立语句)
        if p >= self.src.len() {
            return None;
        }
        let ch = self.src[p] as char;
        if ch != ';' && ch != '{' {
            return None;
        }
        // 扫描 (( 内部第一个标识符作为属性名
        // 重新从 (( 之后开始
        // (上面已经走过 depth, 这里重做以便提取名称)
        let mut q = 0;
        let mut ql = 1;
        while q < self.src.len() && ql < line {
            if self.src[q] == b'\n' {
                ql += 1;
            }
            q += 1;
        }
        q += col.saturating_sub(1);
        while q < self.src.len()
            && ((self.src[q] as char).is_ascii_alphanumeric() || self.src[q] == b'_')
        {
            q += 1;
        }
        while q < self.src.len() && (self.src[q] as char).is_ascii_whitespace() {
            q += 1;
        }
        if q + 1 >= self.src.len() || self.src[q] != b'(' || self.src[q + 1] != b'(' {
            return None;
        }
        q += 2;
        // 跳过空白
        while q < self.src.len() && (self.src[q] as char).is_ascii_whitespace() {
            q += 1;
        }
        let start = q;
        while q < self.src.len()
            && ((self.src[q] as char).is_ascii_alphanumeric() || self.src[q] == b'_')
        {
            q += 1;
        }
        if start == q {
            return None;
        }
        std::str::from_utf8(&self.src[start..q])
            .ok()
            .map(|s| s.to_string())
    }

    fn skip_ws_and_comments(&mut self) {
        loop {
            match self.peek() {
                Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n') | Some(b'\x0c')
                | Some(b'\x0b') => {
                    self.bump();
                },
                Some(b'/') if self.peek_at(1) == Some(b'/') => {
                    while self.peek().is_some() && self.peek() != Some(b'\n') {
                        self.bump();
                    }
                },
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
                },
                _ => break,
            }
        }
    }

    pub fn next_tok(&mut self) -> Tok {
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
                && (self.src[self.pos + 1] == b'u' || self.src[self.pos + 1] == b'U'))
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
            let kind = keyword_lookup(&text);
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
                if (is_float || matches!(self.peek(), Some(b'p') | Some(b'P')))
                    && matches!(self.peek(), Some(b'p') | Some(b'P'))
                {
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
                // 支持 ISO _FloatN 后缀: f16/f32/f64/f128
                // 支持 Decimal 后缀: DF/DD/DL
                for _ in 0..2 {
                    match self.peek() {
                        Some(b'f' | b'F') => {
                            self.bump();
                            // _FloatN: f16/f32/f64/f128
                            if matches!(self.peek(), Some(b'1') | Some(b'3') | Some(b'6')) {
                                while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                                    self.bump();
                                }
                                // 可选的 x 后缀: f64x/f128x
                                if matches!(self.peek(), Some(b'x') | Some(b'X')) {
                                    self.bump();
                                }
                            }
                        },
                        Some(b'l' | b'L') => {
                            self.bump();
                        },
                        Some(b'd' | b'D')
                            if matches!(
                                self.peek_at(1),
                                Some(b'f')
                                    | Some(b'F')
                                    | Some(b'd')
                                    | Some(b'D')
                                    | Some(b'l')
                                    | Some(b'L')
                            ) =>
                        {
                            // Decimal float: DF/DD/DL
                            self.bump(); // d
                            self.bump(); // f/d/l
                        },
                        Some(b'i' | b'I' | b'j' | b'J') => {
                            self.bump();
                        },
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
            },
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
            },
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
            },
            b'*' => {
                if self.peek() == Some(b'=') {
                    self.bump();
                    (TokKind::StarEq, 2)
                } else {
                    (TokKind::Star, 1)
                }
            },
            b'/' => {
                if self.peek() == Some(b'=') {
                    self.bump();
                    (TokKind::SlashEq, 2)
                } else {
                    (TokKind::Slash, 1)
                }
            },
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
            },
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
            },
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
            },
            b'^' => {
                if self.peek() == Some(b'=') {
                    self.bump();
                    (TokKind::CaretEq, 2)
                } else {
                    (TokKind::Caret, 1)
                }
            },
            b'!' => {
                if self.peek() == Some(b'=') {
                    self.bump();
                    (TokKind::Ne, 2)
                } else {
                    (TokKind::Not, 1)
                }
            },
            b'=' => {
                if self.peek() == Some(b'=') {
                    self.bump();
                    (TokKind::EqEq, 2)
                } else {
                    (TokKind::Assign, 1)
                }
            },
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
            },
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
            },
            b'.' => {
                if self.peek() == Some(b'.') && self.peek_at(1) == Some(b'.') {
                    self.bump();
                    self.bump();
                    (TokKind::Ellipsis, 3)
                } else {
                    (TokKind::Dot, 1)
                }
            },
            _ => {
                // 非 ASCII 字节 (含 PUA 占位码点): 消费完整的 UTF-8 多字节
                // 序列, 产生 Invalid token。游离在字面量之外的非 ASCII
                // 字符即源码杂散字符, 类似 GCC "stray '\xxx' in program"。
                let extra = if b >= 0xF0 {
                    3
                } else if b >= 0xE0 {
                    2
                } else if b >= 0xC0 {
                    1
                } else {
                    0
                };
                for _ in 0..extra {
                    if self.peek().is_some_and(|c| c & 0xC0 == 0x80) {
                        self.bump();
                    } else {
                        break;
                    }
                }
                return Tok {
                    kind: TokKind::Invalid,
                    span: Span {
                        file: self.file.clone(),
                        line,
                        col,
                        len: (self.pos - start) as u32,
                    },
                    text: String::from_utf8_lossy(&self.src[start..self.pos]).into_owned(),
                };
            },
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

// ============================================================================
// Phase 2: 紧凑词法单元 (interned + arena) — 与现有 Tok 并存, 不替换.
// ============================================================================
// 第一性原理: 现 Tok 携带 String + Span, 单 token 24+ 字节.
//   编译器单次 parse 数十万 token, 内存压力 + cache miss 显著.
//   紧凑表示: 用 SymbolId (4B) 取代 String, 用 arena 索引代替逐 token 分配.
// suckless 准则: 不引入第三方 intern crate; arena 是普通 Vec; 生命周期 'static
//   是符号池驻留字符串的简化 (id 解析需借助外部 Interner).
//
// 与现有 `Tok` 的关系:
//   * 不修改 `Tok` / `next_tok` / 任何现有签名.
//   * CompactTok / TokenArena 是供后续 pipeline 阶段 (例如 batch lexer)
//     使用的替代表示. 旧路径保持不变, 1498 既有测试无需改动.

use crate::symbol::SymbolId;

/// 整数后缀 (C99/C11/C23).
///
/// 设计为 1 字节 enum, 与 CompactTok::Int 紧凑对齐.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntSuffix {
    None,
    U,
    L,
    UL,
    LL,
    ULL,
    /// `unsigned __int128` / `_Int128` 等扩展整数.
    U128,
    /// `__int128` (有符号).
    I128,
    /// `_BitInt(N)` / `size_t` 等需要任意宽度指示的占位.
    /// (保留位, 暂未启用; 留作未来扩展, 不破坏变体序.)
    Z,
}

/// 紧凑词法单元: 不持有 String/Span, 仅携带必要 payload.
///
/// 内存布局 (`size_of` 在大多数平台上 ≤ 16 字节):
///   * tag (discriminant) + 4~8B payload, 紧凑打包.
/// 字符串原文通过 `Interner` + `SymbolId` 解析; Span 由外部 arena 索引表达.
///
/// 注意: 该类型故意保留 `'static` 生命周期; 通过 `CompactTok<'static>`
///   推过 arena, 实际生命周期由 `TokenArena` 拥有.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactTok<'a> {
    /// 关键字 / 标点 / EOF 等无 payload 的变体.
    Kind(TokKind),
    /// 标识符; 通过 `Interner::resolve` 取回原文.
    Ident(SymbolId),
    /// 整数 (bit-pattern + 后缀); 解析时按后缀决定宽度.
    Int(u64, IntSuffix),
    /// 浮点 (按位重解释的 u64).
    Float(u64),
    /// 字符串字面量 (已 intern).
    Str(SymbolId),
    /// 字符字面量 (Unicode codepoint).
    Char(u32),
    // 抑制 'a 未使用告警; 该参数为未来 lifetime 扩展预留.
    #[allow(dead_code)]
    _Phantom(std::marker::PhantomData<&'a ()>),
}

impl<'a> CompactTok<'a> {
    /// 仅取 TokKind (其他变体的语义类别).
    ///
    /// 用于在不解析字面量值的前提下快速分发 token 类型.
    #[inline]
    pub fn kind(&self) -> TokKind {
        match self {
            CompactTok::Kind(k) => *k,
            CompactTok::Ident(_) => TokKind::Ident,
            CompactTok::Int(..) => TokKind::Number,
            CompactTok::Float(_) => TokKind::FloatLit,
            CompactTok::Str(_) => TokKind::StringLit,
            CompactTok::Char(_) => TokKind::CharLit,
            CompactTok::_Phantom(_) => TokKind::Invalid,
        }
    }
}

/// Token arena: 预分配向量存储 CompactTok, 降低分配次数.
///
/// 默认 capacity 4096 适配中型翻译单元的典型 token 量;
/// 超出后自动扩容 (Vec 标准行为).
#[derive(Debug)]
pub struct TokenArena {
    buf: Vec<CompactTok<'static>>,
}

impl Default for TokenArena {
    fn default() -> Self {
        Self::new()
    }
}

impl TokenArena {
    /// 预分配 4096 槽位的 arena.
    pub fn new() -> Self {
        Self {
            buf: Vec::with_capacity(4096),
        }
    }

    /// 指定初始容量 (例如已知 token 数的批处理场景).
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            buf: Vec::with_capacity(cap),
        }
    }

    /// 追加 token, 返回其索引.
    #[inline]
    pub fn push(&mut self, tok: CompactTok<'static>) -> usize {
        let idx = self.buf.len();
        self.buf.push(tok);
        idx
    }

    /// 按索引读取 token.
    #[inline]
    pub fn get(&self, idx: usize) -> Option<&CompactTok<'static>> {
        self.buf.get(idx)
    }

    /// 当前已存入的 token 数.
    #[inline]
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// 是否为空.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// 剩余容量 (供决策是否复用 arena).
    #[inline]
    pub fn remaining_capacity(&self) -> usize {
        self.buf.capacity() - self.buf.len()
    }

    /// 清空内容但保留已分配的容量 (供批量重用).
    #[inline]
    pub fn clear(&mut self) {
        self.buf.clear();
    }

    /// 底层切片 (供迭代器使用).
    #[inline]
    pub fn as_slice(&self) -> &[CompactTok<'static>] {
        &self.buf
    }
}

#[cfg(test)]
mod arena_tests {
    use super::*;
    use crate::symbol::{Interner, SymbolId};

    #[test]
    fn compact_tok_kind_dispatch() {
        let it = Interner::new();
        let id = it.intern("foo");
        assert_eq!(CompactTok::Kind(TokKind::Plus).kind(), TokKind::Plus);
        assert_eq!(CompactTok::Ident(id).kind(), TokKind::Ident);
        assert_eq!(CompactTok::Int(42, IntSuffix::L).kind(), TokKind::Number);
        assert_eq!(CompactTok::Float(0x7ff0_0000_0000_0000).kind(), TokKind::FloatLit);
        let sid = it.intern("hello");
        assert_eq!(CompactTok::Str(sid).kind(), TokKind::StringLit);
        assert_eq!(CompactTok::Char(b'a' as u32).kind(), TokKind::CharLit);
    }

    #[test]
    fn arena_push_returns_index_and_get_roundtrip() {
        let mut arena = TokenArena::new();
        let i0 = arena.push(CompactTok::Kind(TokKind::LParen));
        let i1 = arena.push(CompactTok::Kind(TokKind::RParen));
        assert_eq!(i0, 0);
        assert_eq!(i1, 1);
        assert_eq!(arena.get(0).unwrap().kind(), TokKind::LParen);
        assert_eq!(arena.get(1).unwrap().kind(), TokKind::RParen);
        assert!(arena.get(2).is_none());
    }

    #[test]
    fn arena_preallocates_capacity_4096() {
        let arena = TokenArena::new();
        assert_eq!(arena.len(), 0);
        assert!(arena.is_empty());
        assert!(arena.remaining_capacity() >= 4096);
    }

    #[test]
    fn arena_clear_preserves_capacity() {
        let mut arena = TokenArena::new();
        for _ in 0..16 {
            arena.push(CompactTok::Kind(TokKind::Semi));
        }
        let total_cap = arena.remaining_capacity() + arena.len();
        arena.clear();
        assert_eq!(arena.len(), 0);
        assert!(arena.is_empty());
        // clear 不应丢弃已分配的容量, 剩余容量恢复到原始容量.
        assert_eq!(arena.remaining_capacity(), total_cap);
    }

    #[test]
    fn arena_with_capacity_respects_request() {
        let arena = TokenArena::with_capacity(1024);
        assert!(arena.remaining_capacity() >= 1024);
    }

    #[test]
    fn arena_as_slice_iterates_in_order() {
        let mut arena = TokenArena::new();
        arena.push(CompactTok::Kind(TokKind::Plus));
        arena.push(CompactTok::Kind(TokKind::Minus));
        arena.push(CompactTok::Kind(TokKind::Star));
        let kinds: Vec<TokKind> = arena.as_slice().iter().map(|t| t.kind()).collect();
        assert_eq!(kinds, vec![TokKind::Plus, TokKind::Minus, TokKind::Star]);
    }

    #[test]
    fn compact_tok_with_interner_round_trip() {
        // 验证 CompactTok::Ident 与 Interner 协同.
        let it = Interner::new();
        let mut arena = TokenArena::new();
        let id_x = it.intern("x");
        let id_y = it.intern("y");
        arena.push(CompactTok::Ident(id_x));
        arena.push(CompactTok::Ident(id_y));
        if let CompactTok::Ident(got) = arena.get(0).unwrap() {
            assert_eq!(it.resolve(*got), "x");
        } else {
            panic!("expected Ident variant");
        }
        if let CompactTok::Ident(got) = arena.get(1).unwrap() {
            assert_eq!(it.resolve(*got), "y");
        } else {
            panic!("expected Ident variant");
        }
    }

    #[test]
    fn empty_symbol_id_is_distinct_from_zero() {
        // SymbolId(0) 作为占位, 永远解析为空串. 但 CompactTok::Kind
        // 仍能区分 EOF 与占位 id; 单元测试仅锁定 interner 行为.
        let it = Interner::new();
        assert_eq!(it.resolve(SymbolId::EMPTY), "");
    }
}

#[cfg(test)]
mod tests {
    use super::{Lexer, TokKind};

    #[test]
    fn malformed_literals_and_unknown_bytes_are_not_eof() {
        for source in [b"\"unterminated\n".as_slice(), b"'unterminated\n", b"@"] {
            let mut lexer = Lexer::new(source, "invalid-token.c");
            assert_eq!(lexer.next_tok().kind, TokKind::Invalid);
        }
    }

    #[test]
    fn integer_suffixes_are_part_of_the_number_token() {
        for source in ["1U", "1UL", "1LU", "1ULL", "0xffUL", "0X10llu"] {
            let mut lexer = Lexer::new(source.as_bytes(), "integer-suffix.c");
            assert_eq!(lexer.next_tok().kind, TokKind::Number, "{source}");
            assert_eq!(lexer.next_tok().kind, TokKind::Eof, "{source}");
        }
    }

    #[test]
    fn wide_string_and_char_literals() {
        // L"hello" 应产生 WideStringLit
        let mut lx = Lexer::new(br#"L"hello""#, "wide.c");
        let tok = lx.next_tok();
        assert_eq!(tok.kind, TokKind::WideStringLit);
        assert_eq!(tok.text.as_str(), r#"L"hello""#);
        assert_eq!(lx.next_tok().kind, TokKind::Eof);

        // L'a' 应产生 WideCharLit
        let mut lx = Lexer::new(b"L'a'", "wide.c");
        let tok = lx.next_tok();
        assert_eq!(tok.kind, TokKind::WideCharLit);
        assert_eq!(tok.text.as_str(), "L'a'");
        assert_eq!(lx.next_tok().kind, TokKind::Eof);

        // L'\u00e9' 宽字符 (UTF-8 字节序列 e9 在源里)
        let mut lx = Lexer::new("L'\u{e9}'".as_bytes(), "wide.c");
        let tok = lx.next_tok();
        assert_eq!(tok.kind, TokKind::WideCharLit);

        // 普通 'a' 仍是 CharLit, 不被 L 前缀吞
        let mut lx = Lexer::new(b"'a'", "plain.c");
        assert_eq!(lx.next_tok().kind, TokKind::CharLit);
        assert_eq!(lx.next_tok().kind, TokKind::Eof);

        // L 后接 identifier (非 ' " 前缀) 应是普通 ident Lx
        let mut lx = Lexer::new(b"Lx", "ident.c");
        let tok = lx.next_tok();
        assert_eq!(tok.kind, TokKind::Ident);
        assert_eq!(tok.text.as_str(), "Lx");
        assert_eq!(lx.next_tok().kind, TokKind::Eof);
    }

    #[test]
    fn wide_char_unterminated_is_invalid() {
        let mut lx = Lexer::new(b"L'abc", "wide.c");
        assert_eq!(lx.next_tok().kind, TokKind::Invalid);
    }
}

// char_class 查表正确性单元测试. 这些断言锁定了 Phase 3 优化路径上
// 字符分类的核心不变量; 任何对分类表的改动都必须经过此模块验证.
#[cfg(test)]
mod char_class_tests {
    use super::char_class;

    #[test]
    fn letters_are_id_start() {
        // 大小写字母都是标识符首字符.
        assert!(char_class::is_id_start(b'a'));
        assert!(char_class::is_id_start(b'z'));
        assert!(char_class::is_id_start(b'A'));
        assert!(char_class::is_id_start(b'Z'));
        assert!(char_class::is_id_start(b'm'));
    }

    #[test]
    fn underscore_is_id_start() {
        // 下划线在 C 中是合法的标识符首字符.
        assert!(char_class::is_id_start(b'_'));
    }

    #[test]
    fn digits_are_not_id_start() {
        // 数字不能作为标识符首字符, 但可以是后续字符.
        for c in b'0'..=b'9' {
            assert!(!char_class::is_id_start(c), "digit {:?} should not start ident", c as char);
        }
    }

    #[test]
    fn operators_are_punct() {
        // 常见运算符应归类为 PUNCT (5).
        for &c in &[b'+', b'-', b'*', b'/'] {
            assert_eq!(char_class::classify(c), char_class::PUNCT, "{:?}", c as char);
        }
    }

    #[test]
    fn whitespace_classification() {
        // 空格 / 制表 / 回车归类为 WHITESPACE (0).
        assert_eq!(char_class::classify(b' '), char_class::WHITESPACE);
        assert_eq!(char_class::classify(b'\t'), char_class::WHITESPACE);
        assert_eq!(char_class::classify(b'\r'), char_class::WHITESPACE);
    }

    #[test]
    fn newline_is_separate_class() {
        // 换行单独一类 (NEWLINE = 7), 便于行号维护.
        assert_eq!(char_class::classify(b'\n'), char_class::NEWLINE);
    }

    #[test]
    fn id_continue_includes_letter_digit_underscore() {
        // 字母 / 数字 / 下划线都是合法 id_continue.
        assert!(char_class::is_id_continue(b'a'));
        assert!(char_class::is_id_continue(b'Z'));
        assert!(char_class::is_id_continue(b'_'));
        for c in b'0'..=b'9' {
            assert!(char_class::is_id_continue(c), "digit {:?} should continue ident", c as char);
        }
        // 标点和空白不应是 id_continue.
        assert!(!char_class::is_id_continue(b'+'));
        assert!(!char_class::is_id_continue(b' '));
        assert!(!char_class::is_id_continue(b'\n'));
    }

    #[test]
    fn quotes_are_classified() {
        assert_eq!(char_class::classify(b'"'), char_class::QUOTE);
        assert_eq!(char_class::classify(b'\''), char_class::QUOTE);
    }

    #[test]
    fn table_is_const_evaluable() {
        // 验证 CLASS_TABLE 是 const (编译期可读), 并且条目符合预期.
        // 这一断言同时保证 const fn 的初始化器在编译期已被求值.
        const _: () = {
            assert!(char_class::CLASS_TABLE[b'a' as usize] == char_class::LETTER);
            assert!(char_class::CLASS_TABLE[b'_' as usize] == char_class::IDENT_CONT);
            assert!(char_class::CLASS_TABLE[b'0' as usize] == char_class::DIGIT);
            assert!(char_class::CLASS_TABLE[b'+' as usize] == char_class::PUNCT);
            assert!(char_class::CLASS_TABLE[b'\n' as usize] == char_class::NEWLINE);
            assert!(char_class::CLASS_TABLE[0xFF] == char_class::OTHER);
        };
    }

    #[test]
    fn classify_high_bytes_default_to_other() {
        // 高字节 (UTF-8 后续字节) 不会单字节出现, 但表项应归类为 OTHER.
        for &b in &[0x80u8, 0xC0, 0xFF] {
            assert_eq!(char_class::classify(b), char_class::OTHER, "{:#x}", b);
        }
    }
}
