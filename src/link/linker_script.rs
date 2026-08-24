// src/link/linker_script.rs - 链接器脚本解析与执行
//
// 支持 GNU ld 链接器脚本的核心子集，面向 Linux 内核 vmlinux.lds.S 预处理输出：
//   - OUTPUT_FORMAT / OUTPUT_ARCH / ENTRY
//   - PHDRS { ... }
//   - SECTIONS { ... } 含 AT() / ALIGN() / fill / :phdr
//   - 符号赋值 (sym = expr) / PROVIDE(sym = expr)
//   - 输入节通配 *(.text ...) / KEEP / SORT / SORT_BY_ALIGNMENT / SORT_BY_NAME
//   - ASSERT / SIZEOF / ADDR / ABSOLUTE / ALIGN / ADDR
//   - /DISCARD/ 与 (INFO) 虚拟节
//   - 表达式：算术、位运算、三元、比较
//
// 用法：
//   let script = LinkerScript::parse(&text)?;
//   script.apply(&mut layout, &bundle, &resolved)?;

use std::collections::HashMap;

// ─── Token ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Token {
    // 关键字
    OutputFormat,
    OutputArch,
    Entry,
    Phdrs,
    Sections,
    Keep,
    Sort,
    SortByAlignment,
    SortByName,
    Assert,
    Provide,
    ProvideHidden,
    Absolute,
    Addr,
    Sizeof,
    Align,
    Max,
    Min,
    Defined,
    Length,
    Constructors,
    Discard,
    Info,
    // 段类型
    PtLoad,
    PtDynamic,
    PtInterp,
    PtNote,
    PtGnuStack,
    PtGnuRelro,
    PtPhdr,
    // 字面量
    Int(u64),
    Str(String),
    Ident(String),
    // 符号
    LParen,
    RParen,
    LBrace,
    RBrace,
    Colon,
    Semicolon,
    Comma,
    Star,      // *
    Plus,
    Minus,
    Slash,
    Percent,
    Amp,
    Pipe,
    Caret,
    Tilde,
    LShift,
    RShift,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    And,   // &&
    Or,    // ||
    Question,
    Bang,
    Dot,   // .
    Assign, // =
    PlusAssign,  // +=
    MinusAssign, // -=
    AndAssign,   // &=
    OrAssign,    // |=
    LShiftAssign, // <<=
    RShiftAssign, // >>=
    // 特殊
    Eof,
}

// ─── Lexer ──────────────────────────────────────────────────────────────────

struct Lexer {
    chars: Vec<char>,
    pos: usize,
}

impl Lexer {
    fn new(input: &str) -> Self {
        Self {
            chars: input.chars().collect(),
            pos: 0,
        }
    }

    fn peek_char(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn next_char(&mut self) -> Option<char> {
        let c = self.chars.get(self.pos).copied();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn skip_whitespace_and_comments(&mut self) {
        loop {
            // skip whitespace
            while let Some(c) = self.peek_char() {
                if c.is_whitespace() {
                    self.next_char();
                } else {
                    break;
                }
            }
            // skip C-style comments
            if self.pos + 1 < self.chars.len()
                && self.chars[self.pos] == '/'
                && self.chars[self.pos + 1] == '*'
            {
                self.pos += 2;
                while self.pos + 1 < self.chars.len() {
                    if self.chars[self.pos] == '*' && self.chars[self.pos + 1] == '/' {
                        self.pos += 2;
                        break;
                    }
                    self.pos += 1;
                }
                continue;
            }
            // skip C++-style comments
            if self.pos + 1 < self.chars.len()
                && self.chars[self.pos] == '/'
                && self.chars[self.pos + 1] == '/'
            {
                while let Some(c) = self.next_char() {
                    if c == '\n' {
                        break;
                    }
                }
                continue;
            }
            // skip # line directives (from cpp output)
            if self.peek_char() == Some('#') {
                while let Some(c) = self.next_char() {
                    if c == '\n' {
                        break;
                    }
                }
                continue;
            }
            break;
        }
    }

    fn read_string(&mut self, quote: char) -> String {
        // 注意:next_token 已经消费了开头引号,这里不需要再次消费
        // quote 参数告诉我们应该匹配哪种引号（" 或 '）
        let mut s = String::new();
        loop {
            match self.next_char() {
                Some(c) if c == quote => break,
                Some('\\') => {
                    if let Some(c2) = self.next_char() {
                        s.push(c2);
                    }
                }
                Some(c) => s.push(c),
                None => break,
            }
        }
        s
    }

    fn read_number(&mut self, first: char) -> u64 {
        if first == '0' {
            match self.peek_char() {
                Some('x') | Some('X') => {
                    self.next_char();
                    let mut hex = String::new();
                    while let Some(c) = self.peek_char() {
                        if c.is_ascii_hexdigit() {
                            hex.push(c);
                            self.next_char();
                        } else {
                            break;
                        }
                    }
                    u64::from_str_radix(&hex, 16).unwrap_or(0)
                }
                Some(c) if c.is_ascii_digit() => {
                    // could be octal, but in linker scripts leading 0 is just decimal
                    let mut num = String::from("0");
                    while let Some(c) = self.peek_char() {
                        if c.is_ascii_digit() {
                            num.push(c);
                            self.next_char();
                        } else {
                            break;
                        }
                    }
                    num.parse().unwrap_or(0)
                }
                _ => 0,
            }
        } else {
            let mut num = String::from(first);
            while let Some(c) = self.peek_char() {
                if c.is_ascii_digit() {
                    num.push(c);
                    self.next_char();
                } else {
                    break;
                }
            }
            // handle K/M suffixes
            match self.peek_char() {
                Some('K') | Some('k') => {
                    self.next_char();
                    num.parse::<u64>().unwrap_or(0) * 1024
                }
                Some('M') | Some('m') => {
                    self.next_char();
                    num.parse::<u64>().unwrap_or(0) * 1024 * 1024
                }
                _ => num.parse().unwrap_or(0),
            }
        }
    }

    fn read_ident(&mut self, first: char) -> String {
        let mut s = String::from(first);
        while let Some(c) = self.peek_char() {
            // 允许字母、数字、下划线、方括号、连字符、加号作为标识符的一部分
            // 这是为了支持链接器脚本中的节名模式，如 ___ksymtab+*
            // 也允许 `.` 后跟字母/数字，以支持 .got.plt 这样的节名
            if c.is_ascii_alphanumeric() || c == '_' || c == '[' || c == ']' || c == '-' || c == '+' {
                s.push(c);
                self.next_char();
            } else if c == '.' {
                // 检查 `.` 后面是否是字母/数字，如果是则将其包含在标识符中
                if self.pos + 1 < self.chars.len() {
                    let next_c = self.chars[self.pos + 1];
                    if next_c.is_ascii_alphanumeric() || next_c == '_' {
                        s.push(c);
                        self.next_char();
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        s
    }

    fn next_token(&mut self) -> Token {
        self.skip_whitespace_and_comments();
        let c = match self.next_char() {
            Some(c) => c,
            None => return Token::Eof,
        };
        match c {
            '"' | '\'' => Token::Str(self.read_string(c)),
            '(' => Token::LParen,
            ')' => Token::RParen,
            '{' => Token::LBrace,
            '}' => Token::RBrace,
            ':' => Token::Colon,
            ';' => Token::Semicolon,
            ',' => Token::Comma,
            '*' => Token::Star,
            '+' => {
                if self.peek_char() == Some('=') {
                    self.next_char();
                    Token::PlusAssign
                } else {
                    Token::Plus
                }
            }
            '-' => {
                if self.peek_char() == Some('=') {
                    self.next_char();
                    Token::MinusAssign
                } else {
                    Token::Minus
                }
            }
            '/' => Token::Slash,
            '%' => Token::Percent,
            '&' => {
                if self.peek_char() == Some('&') {
                    self.next_char();
                    Token::And
                } else if self.peek_char() == Some('=') {
                    self.next_char();
                    Token::AndAssign
                } else {
                    Token::Amp
                }
            }
            '|' => {
                if self.peek_char() == Some('|') {
                    self.next_char();
                    Token::Or
                } else if self.peek_char() == Some('=') {
                    self.next_char();
                    Token::OrAssign
                } else {
                    Token::Pipe
                }
            }
            '^' => Token::Caret,
            '~' => Token::Tilde,
            '<' => {
                if self.peek_char() == Some('<') {
                    self.next_char();
                    if self.peek_char() == Some('=') {
                        self.next_char();
                        Token::LShiftAssign
                    } else {
                        Token::LShift
                    }
                } else if self.peek_char() == Some('=') {
                    self.next_char();
                    Token::Le
                } else {
                    Token::Lt
                }
            }
            '>' => {
                if self.peek_char() == Some('>') {
                    self.next_char();
                    if self.peek_char() == Some('=') {
                        self.next_char();
                        Token::RShiftAssign
                    } else {
                        Token::RShift
                    }
                } else if self.peek_char() == Some('=') {
                    self.next_char();
                    Token::Ge
                } else {
                    Token::Gt
                }
            }
            '=' => {
                if self.peek_char() == Some('=') {
                    self.next_char();
                    Token::Eq
                } else {
                    Token::Assign
                }
            }
            '!' => {
                if self.peek_char() == Some('=') {
                    self.next_char();
                    Token::Ne
                } else {
                    Token::Bang
                }
            }
            '?' => Token::Question,
            '.' => {
                // check if it's a section name starting with .
                if let Some(c2) = self.peek_char() {
                    if c2.is_ascii_alphanumeric() || c2 == '_' {
                        return Token::Ident(self.read_ident('.'));
                    }
                }
                Token::Dot
            }
            c if c.is_ascii_digit() => Token::Int(self.read_number(c)),
            c if c.is_ascii_alphabetic() || c == '_' => {
                let ident = self.read_ident(c);
                match ident.as_str() {
                    "OUTPUT_FORMAT" => Token::OutputFormat,
                    "OUTPUT_ARCH" => Token::OutputArch,
                    "ENTRY" => Token::Entry,
                    "PHDRS" => Token::Phdrs,
                    "SECTIONS" => Token::Sections,
                    "KEEP" => Token::Keep,
                    "SORT" | "SORT_BY_NAME" => Token::Sort,
                    "SORT_BY_ALIGNMENT" => Token::SortByAlignment,
                    "ASSERT" => Token::Assert,
                    "PROVIDE" => Token::Provide,
                    "PROVIDE_HIDDEN" => Token::ProvideHidden,
                    "ABSOLUTE" => Token::Absolute,
                    "ADDR" => Token::Addr,
                    "SIZEOF" => Token::Sizeof,
                    "ALIGN" => Token::Align,
                    "MAX" => Token::Max,
                    "MIN" => Token::Min,
                    "DEFINED" => Token::Defined,
                    "LENGTH" => Token::Length,
                    "CONSTRUCTORS" => Token::Constructors,
                    "PT_LOAD" => Token::PtLoad,
                    "PT_DYNAMIC" => Token::PtDynamic,
                    "PT_INTERP" => Token::PtInterp,
                    "PT_NOTE" => Token::PtNote,
                    "PT_GNU_STACK" => Token::PtGnuStack,
                    "PT_GNU_RELRO" => Token::PtGnuRelro,
                    "PT_PHDR" => Token::PtPhdr,
                    "INFO" => Token::Info,
                    _ => Token::Ident(ident),
                }
            }
            _ => {
                // skip unknown characters
                self.next_token()
            }
        }
    }
}

// ─── AST ────────────────────────────────────────────────────────────────────

/// 链接器脚本顶层结构
#[derive(Debug, Clone)]
pub struct LinkerScript {
    pub output_format: Option<String>,
    pub output_arch: Option<String>,
    pub entry: Option<String>,
    pub phdrs: Vec<PhdrDef>,
    pub sections: Vec<SectionCommand>,
    /// 顶层符号赋值 (SECTIONS 之外)
    pub top_assignments: Vec<SymbolAssignment>,
    /// SECTIONS 之后的 ASSERT/PROVIDE
    pub trailing_commands: Vec<TrailingCommand>,
}

/// PHDRS 块中的段定义
#[derive(Debug, Clone)]
pub struct PhdrDef {
    pub name: String,
    pub ty: PhdrType,
    pub flags: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhdrType {
    PtLoad,
    PtDynamic,
    PtInterp,
    PtNote,
    PtGnuStack,
    PtGnuRelro,
    PtPhdr,
}

impl PhdrType {
    pub fn to_pt(self) -> u32 {
        match self {
            PhdrType::PtLoad => 1,
            PhdrType::PtDynamic => 2,
            PhdrType::PtInterp => 3,
            PhdrType::PtNote => 4,
            PhdrType::PtGnuStack => 0x6474e551,
            PhdrType::PtGnuRelro => 0x6474e552,
            PhdrType::PtPhdr => 6,
        }
    }
}

/// SECTIONS 块中的命令
#[derive(Debug, Clone)]
pub enum SectionCommand {
    /// 位置计数器赋值: . = expr
    LocationAssign(Expr),
    /// 符号赋值: sym = expr 或 PROVIDE(sym = expr)
    SymbolAssign(SymbolAssignment),
    /// 输出节定义
    OutputSection(OutputSectionDef),
    /// ASSERT
    AssertCmd(Expr, String),
    /// PROVIDE at top level after SECTIONS
    ProvideCmd(String, Expr),
}

/// 符号赋值
#[derive(Debug, Clone)]
pub struct SymbolAssignment {
    pub name: String,
    pub expr: Expr,
    pub is_provide: bool,
    pub is_hidden: bool,
}

/// 输出节定义
#[derive(Debug, Clone)]
pub struct OutputSectionDef {
    pub name: String,
    pub address: Option<Expr>,
    pub lma: Option<Expr>,
    pub align: Option<Expr>,
    pub fill: Option<u64>,
    pub phdrs: Vec<String>,
    pub body: Vec<SectionBodyItem>,
    pub is_discard: bool,
    pub is_info: bool,
}

/// 输出节体内的项目
#[derive(Debug, Clone)]
pub enum SectionBodyItem {
    /// 位置计数器赋值: . = expr
    LocationAssign(Expr),
    /// 符号赋值
    SymbolAssign(SymbolAssignment),
    /// 输入节: *(.text .text.*) 或 KEEP(...)
    InputSection(InputSectionSpec),
    /// 数据填充: BYTE/SHORT/LONG/QUAD
    DataFill(u64, usize), // (value, size_in_bytes)
    /// CONSTRUCTORS
    Constructors,
}

/// 输入节规范
#[derive(Debug, Clone)]
pub struct InputSectionSpec {
    pub file_pattern: String,
    pub section_patterns: Vec<SectionPattern>,
    pub keep: bool,
}

/// 节模式（含排序方式）
#[derive(Debug, Clone)]
pub struct SectionPattern {
    pub pattern: String,
    pub sort: SortMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortMode {
    #[default]
    None,
    ByName,
    ByAlignment,
}

/// 表达式 AST
#[derive(Debug, Clone)]
pub enum Expr {
    Int(u64),
    Symbol(String),
    Dot,
    BinaryOp(Box<Expr>, BinOp, Box<Expr>),
    UnaryOp(UnaryOp, Box<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    BitAnd,
    BitOr,
    BitXor,
    LShift,
    RShift,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    LogAnd,
    LogOr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
    BitNot,
}

/// SECTIONS 之后的顶层命令
#[derive(Debug, Clone)]
pub enum TrailingCommand {
    Assert(Expr, String),
    Provide(String, Expr),
    LocationAssign(Expr),
}

// ─── Parser ─────────────────────────────────────────────────────────────────

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> &Token {
        self.tokens.get(self.pos).unwrap_or(&Token::Eof)
    }

    fn next(&mut self) -> Token {
        let t = self.tokens.get(self.pos).cloned().unwrap_or(Token::Eof);
        self.pos += 1;
        t
    }

    fn expect(&mut self, expected: &Token) -> Result<(), String> {
        let t = self.next();
        if std::mem::discriminant(&t) == std::mem::discriminant(expected) {
            Ok(())
        } else {
            // 添加更多上下文信息
            let context = if self.pos > 0 && self.pos <= self.tokens.len() {
                let prev = &self.tokens[self.pos - 1];
                // 显示前后的 token
                let mut ctx = format!(" (pos: {}, prev: {:?}", self.pos - 1, prev);
                // 显示前几个 token
                if self.pos >= 3 {
                    ctx.push_str(&format!(", tokens[-3..-1]: {:?}", &self.tokens[self.pos-3..self.pos-1]));
                }
                // 显示后几个 token
                let end = std::cmp::min(self.pos + 2, self.tokens.len());
                ctx.push_str(&format!(", next: {:?}", &self.tokens[self.pos..end]));
                ctx.push(')');
                ctx
            } else {
                String::new()
            };
            Err(format!("expected {:?}, got {:?}{}", expected, t, context))
        }
    }

    fn eat(&mut self, expected: &Token) -> bool {
        if std::mem::discriminant(self.peek()) == std::mem::discriminant(expected) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn parse(&mut self) -> Result<LinkerScript, String> {
        let mut script = LinkerScript {
            output_format: None,
            output_arch: None,
            entry: None,
            phdrs: Vec::new(),
            sections: Vec::new(),
            top_assignments: Vec::new(),
            trailing_commands: Vec::new(),
        };

        // 解析顶层命令
        loop {
            match self.peek().clone() {
                Token::Eof => break,
                Token::OutputFormat => {
                    self.next();
                    self.expect(&Token::LParen)?;
                    if let Token::Str(s) = self.next() {
                        script.output_format = Some(s);
                    }
                    self.expect(&Token::RParen)?;
                }
                Token::OutputArch => {
                    self.next();
                    self.expect(&Token::LParen)?;
                    // arch can be ident or string, and may contain colons (e.g., i386:x86-64)
                    let mut arch = String::new();
                    loop {
                        let tok = self.peek().clone();
                        match tok {
                            Token::RParen | Token::Eof => break,
                            Token::Colon => {
                                self.next();
                                arch.push(':');
                            }
                            Token::Ident(s) => {
                                self.next();
                                arch.push_str(&s);
                            }
                            Token::Str(s) => {
                                self.next();
                                arch.push_str(&s);
                            }
                            _ => {
                                let tok = self.next();
                                return Err(format!("OUTPUT_ARCH: unexpected {:?}", tok));
                            }
                        }
                    }
                    script.output_arch = Some(arch);
                    self.expect(&Token::RParen)?;
                }
                Token::Entry => {
                    self.next();
                    self.expect(&Token::LParen)?;
                    if let Token::Ident(s) = self.next() {
                        script.entry = Some(s);
                    }
                    self.expect(&Token::RParen)?;
                }
                Token::Phdrs => {
                    script.phdrs = self.parse_phdrs()?;
                }
                Token::Sections => {
                    script.sections = self.parse_sections()?;
                }
                Token::Ident(_) => {
                    // 可能是符号赋值: sym = expr
                    if let Some(assign) = self.try_parse_symbol_assignment(false)? {
                        script.top_assignments.push(assign);
                    } else {
                        self.next(); // skip unknown
                    }
                }
                Token::Dot => {
                    // . = expr at top level (after SECTIONS)
                    self.next();
                    if self.eat(&Token::Assign) {
                        let expr = self.parse_expr()?;
                        script.trailing_commands.push(TrailingCommand::LocationAssign(expr));
                        self.eat(&Token::Semicolon);
                    }
                }
                Token::Assert => {
                    self.next();
                    self.expect(&Token::LParen)?;
                    let expr = self.parse_expr()?;
                    self.expect(&Token::Comma)?;
                    let msg = if let Token::Str(s) = self.next() { s } else { String::new() };
                    self.expect(&Token::RParen)?;
                    script.trailing_commands.push(TrailingCommand::Assert(expr, msg));
                    self.eat(&Token::Semicolon);
                }
                Token::Provide => {
                    self.next();
                    self.expect(&Token::LParen)?;
                    let name = if let Token::Ident(s) = self.next() { s } else { String::new() };
                    self.expect(&Token::Assign)?;
                    let expr = self.parse_expr()?;
                    self.expect(&Token::RParen)?;
                    script.trailing_commands.push(TrailingCommand::Provide(name, expr));
                    self.eat(&Token::Semicolon);
                }
                _ => {
                    self.next(); // skip unknown tokens
                }
            }
        }

        Ok(script)
    }

    fn parse_phdrs(&mut self) -> Result<Vec<PhdrDef>, String> {
        self.next(); // consume PHDRS
        self.expect(&Token::LBrace)?;
        let mut phdrs = Vec::new();
        while !self.eat(&Token::RBrace) {
            let name = if let Token::Ident(s) = self.next() {
                s
            } else {
                return Err("expected phdr name".into());
            };
            let ty = match self.next() {
                Token::PtLoad => PhdrType::PtLoad,
                Token::PtDynamic => PhdrType::PtDynamic,
                Token::PtInterp => PhdrType::PtInterp,
                Token::PtNote => PhdrType::PtNote,
                Token::PtGnuStack => PhdrType::PtGnuStack,
                Token::PtGnuRelro => PhdrType::PtGnuRelro,
                Token::PtPhdr => PhdrType::PtPhdr,
                other => return Err(format!("unknown phdr type {:?}", other)),
            };
            let mut flags = None;
            // 检查 FLAGS(N)
            if let Token::Ident(ref s) = self.peek().clone() {
                if s == "FLAGS" {
                    self.next();
                    self.expect(&Token::LParen)?;
                    if let Token::Int(n) = self.next() {
                        flags = Some(n as u32);
                    }
                    self.expect(&Token::RParen)?;
                }
            }
            self.eat(&Token::Semicolon);
            phdrs.push(PhdrDef { name, ty, flags });
        }
        Ok(phdrs)
    }

    fn parse_sections(&mut self) -> Result<Vec<SectionCommand>, String> {
        self.next(); // consume SECTIONS
        self.expect(&Token::LBrace)?;
        let mut cmds = Vec::new();

        while !self.eat(&Token::RBrace) {
            match self.peek().clone() {
                Token::Eof => break,
                Token::Dot => {
                    self.next();
                    // . = expr (location counter assignment)
                    if self.eat(&Token::Assign) {
                        let expr = self.parse_expr()?;
                        cmds.push(SectionCommand::LocationAssign(expr));
                    }
                    // 可能是以 . 开头的节名
                    else if let Token::Ident(_) = self.peek() {
                        // 回退，当作输出节定义
                        // 但我们已经消费了 Dot，需要把名字拼起来
                        // 实际上 read_ident 已经处理了 .name 的情况
                        // 这里不会到达因为 .name 会被 lexer 识别为 Ident
                    }
                    self.eat(&Token::Semicolon);
                }
                Token::Assert => {
                    self.next();
                    self.expect(&Token::LParen)?;
                    let expr = self.parse_expr()?;
                    self.expect(&Token::Comma)?;
                    let msg = if let Token::Str(s) = self.peek().clone() {
                        self.next();
                        s
                    } else {
                        String::new()
                    };
                    self.expect(&Token::RParen)?;
                    cmds.push(SectionCommand::AssertCmd(expr, msg));
                    self.eat(&Token::Semicolon);
                }
                Token::Provide => {
                    self.next();
                    self.expect(&Token::LParen)?;
                    let name = if let Token::Ident(s) = self.next() { s } else { String::new() };
                    self.expect(&Token::Assign)?;
                    let expr = self.parse_expr()?;
                    self.expect(&Token::RParen)?;
                    cmds.push(SectionCommand::ProvideCmd(name, expr));
                    self.eat(&Token::Semicolon);
                }
                Token::Ident(_) => {
                    // 可能是输出节定义或符号赋值
                    if let Some(assign) = self.try_parse_symbol_assignment(false)? {
                        cmds.push(SectionCommand::SymbolAssign(assign));
                    } else {
                        // 尝试解析为输出节
                        if let Some(sec) = self.try_parse_output_section()? {
                            cmds.push(SectionCommand::OutputSection(sec));
                        } else {
                            self.next(); // skip
                        }
                    }
                }
                Token::Discard => {
                    // /DISCARD/ : { ... }
                    if let Some(sec) = self.try_parse_output_section()? {
                        cmds.push(SectionCommand::OutputSection(sec));
                    } else {
                        self.next();
                    }
                }
                _ => {
                    self.next(); // skip unknown
                }
            }
        }

        Ok(cmds)
    }

    fn try_parse_symbol_assignment(&mut self, _in_section: bool) -> Result<Option<SymbolAssignment>, String> {
        let saved = self.pos;
        // 尝试: name = expr 或 PROVIDE(name = expr)
        let (name, is_provide, is_hidden) = match self.peek().clone() {
            Token::Provide => {
                self.next();
                self.expect(&Token::LParen)?;
                let n = if let Token::Ident(s) = self.next() { s } else {
                    self.pos = saved;
                    return Ok(None);
                };
                (n, true, false)
            }
            Token::ProvideHidden => {
                self.next();
                self.expect(&Token::LParen)?;
                let n = if let Token::Ident(s) = self.next() { s } else {
                    self.pos = saved;
                    return Ok(None);
                };
                (n, true, true)
            }
            Token::Ident(name) => {
                self.next();
                (name, false, false)
            }
            _ => return Ok(None),
        };

        // 期望 = 或 += 等
        let assign_op = match self.next() {
            Token::Assign => None,
            Token::PlusAssign => Some(BinOp::Add),
            Token::MinusAssign => Some(BinOp::Sub),
            Token::AndAssign => Some(BinOp::BitAnd),
            Token::OrAssign => Some(BinOp::BitOr),
            Token::LShiftAssign => Some(BinOp::LShift),
            Token::RShiftAssign => Some(BinOp::RShift),
            _ => {
                self.pos = saved;
                return Ok(None);
            }
        };

        let mut expr = self.parse_expr()?;

        // 如果有复合赋值操作符，包装为二元操作
        if let Some(op) = assign_op {
            expr = Expr::BinaryOp(
                Box::new(Expr::Symbol(name.clone())),
                op,
                Box::new(expr),
            );
        }

        if is_provide {
            self.expect(&Token::RParen)?;
        }

        self.eat(&Token::Semicolon);

        Ok(Some(SymbolAssignment {
            name,
            expr,
            is_provide,
            is_hidden,
        }))
    }

    fn try_parse_output_section(&mut self) -> Result<Option<OutputSectionDef>, String> {
        let saved = self.pos;

        // 节名
        let (name, is_discard) = match self.next() {
            Token::Ident(s) if s == "/DISCARD/" => (s.clone(), true),
            Token::Ident(s) => (s, false),
            Token::Discard => ("/DISCARD/".to_string(), true),
            _ => {
                self.pos = saved;
                return Ok(None);
            }
        };

        // 可选的地址: .text ADDR_EXPR : ...
        let address = None; // 地址通常在 : 之前用 expr 表示，但内核脚本中不常见

        // 可选的 AT(LMA)
        // 实际上在内核脚本中格式是: .name : AT(ADDR(.name) - offset) { ... }
        // 所以 : 之后可能有 AT

        // 期望 :
        if !self.eat(&Token::Colon) {
            self.pos = saved;
            return Ok(None);
        }

        // 可选的 AT(LMA) 在 : 之后
        let mut lma = None;
        if let Token::Absolute = self.peek() {
            // 不是 AT，是其他
        }
        // 检查 AT(...)
        if let Token::Ident(ref s) = self.peek().clone() {
            if s == "AT" {
                self.next();
                self.expect(&Token::LParen)?;
                lma = Some(self.parse_expr()?);
                self.expect(&Token::RParen)?;
            }
        }

        // 可选的 ALIGN(N) 在 : 之后
        let mut align = None;
        if let Token::Align = self.peek() {
            self.next();
            self.expect(&Token::LParen)?;
            align = Some(self.parse_expr()?);
            self.expect(&Token::RParen)?;
        }

        // 期望 {
        if !self.eat(&Token::LBrace) {
            // 可能没有 body (如 .got.plt (INFO) : { ... })
            // 检查 (INFO)
            let is_info;
            if let Token::Info = self.peek() {
                self.next();
                is_info = true;
                if !self.eat(&Token::Colon) {
                    self.pos = saved;
                    return Ok(None);
                }
                if !self.eat(&Token::LBrace) {
                    self.pos = saved;
                    return Ok(None);
                }
            } else {
                self.pos = saved;
                return Ok(None);
            }

            let body = self.parse_section_body()?;

            // fill value
            let fill = self.try_parse_fill();

            // phdrs
            let phdrs = self.parse_section_phdrs();

            self.eat(&Token::Semicolon);

            return Ok(Some(OutputSectionDef {
                name,
                address,
                lma,
                align,
                fill,
                phdrs,
                body,
                is_discard,
                is_info,
            }));
        }

        let body = self.parse_section_body()?;

        // 可选的填充值: = 0xcccccccc
        let fill = self.try_parse_fill();

        // 可选的段映射: :text :data
        let phdrs = self.parse_section_phdrs();

        self.eat(&Token::Semicolon);

        Ok(Some(OutputSectionDef {
            name,
            address,
            lma,
            align,
            fill,
            phdrs,
            body,
            is_discard,
            is_info: false,
        }))
    }

    fn parse_section_body(&mut self) -> Result<Vec<SectionBodyItem>, String> {
        let mut items = Vec::new();

        while !self.eat(&Token::RBrace) {
            match self.peek().clone() {
                Token::Eof => break,
                Token::Dot => {
                    self.next();
                    if self.eat(&Token::Assign) {
                        let expr = self.parse_expr()?;
                        items.push(SectionBodyItem::LocationAssign(expr));
                    }
                    // . += expr
                    else if self.eat(&Token::PlusAssign) {
                        let expr = self.parse_expr()?;
                        items.push(SectionBodyItem::LocationAssign(Expr::BinaryOp(
                            Box::new(Expr::Dot),
                            BinOp::Add,
                            Box::new(expr),
                        )));
                    }
                    self.eat(&Token::Semicolon);
                }
                Token::Star => {
                    // *(.text ...)
                    let spec = self.parse_input_section_spec(false)?;
                    items.push(SectionBodyItem::InputSection(spec));
                }
                Token::Keep => {
                    // KEEP(*(.text ...))
                    self.next(); // consume KEEP
                    self.expect(&Token::LParen)?; // consume (
                    let spec = self.parse_input_section_spec(true)?;
                    self.expect(&Token::RParen)?; // consume )
                    items.push(SectionBodyItem::InputSection(spec));
                }
                Token::Constructors => {
                    self.next();
                    items.push(SectionBodyItem::Constructors);
                }
                Token::Ident(_) => {
                    // 可能是符号赋值
                    if let Some(assign) = self.try_parse_symbol_assignment(true)? {
                        items.push(SectionBodyItem::SymbolAssign(assign));
                    } else {
                        self.next(); // skip
                    }
                }
                _ => {
                    self.next(); // skip unknown
                }
            }
        }

        Ok(items)
    }

    fn parse_input_section_spec(&mut self, keep: bool) -> Result<InputSectionSpec, String> {
        self.expect(&Token::Star)?; // consume *
        if !self.eat(&Token::LParen) {
            return Ok(InputSectionSpec {
                file_pattern: "*".to_string(),
                section_patterns: Vec::new(),
                keep,
            });
        }

        let mut patterns = Vec::new();
        while !self.eat(&Token::RParen) {
            match self.peek().clone() {
                Token::Eof => break,
                Token::Keep => {
                    // KEEP inside input section
                    self.next();
                    self.expect(&Token::LParen)?;
                    while !self.eat(&Token::RParen) {
                        if let Some(pat) = self.parse_section_pattern()? {
                            patterns.push(pat);
                        }
                    }
                }
                Token::Sort => {
                    self.next();
                    self.expect(&Token::LParen)?;
                    while !self.eat(&Token::RParen) {
                        if let Some(mut pat) = self.parse_section_pattern()? {
                            pat.sort = SortMode::ByName;
                            patterns.push(pat);
                        }
                    }
                }
                Token::SortByAlignment => {
                    self.next();
                    self.expect(&Token::LParen)?;
                    while !self.eat(&Token::RParen) {
                        if let Some(mut pat) = self.parse_section_pattern()? {
                            pat.sort = SortMode::ByAlignment;
                            patterns.push(pat);
                        }
                    }
                }
                Token::RParen => break,
                _ => {
                    if let Some(pat) = self.parse_section_pattern()? {
                        patterns.push(pat);
                    }
                }
            }
        }

        Ok(InputSectionSpec {
            file_pattern: "*".to_string(),
            section_patterns: patterns,
            keep,
        })
    }

    fn parse_section_pattern(&mut self) -> Result<Option<SectionPattern>, String> {
        // 节名模式通常是一个单独的 Ident 令牌（如 .text 或 .text.*）
        // 或者由多个令牌组成（如 .text + * 等）
        let mut pattern = String::new();
        
        loop {
            match self.peek().clone() {
                Token::RParen | Token::Comma | Token::Eof => break,
                Token::Ident(s) => {
                    self.next();
                    pattern.push_str(&s);
                }
                Token::Dot => {
                    self.next();
                    pattern.push('.');
                }
                Token::Star => {
                    self.next();
                    pattern.push('*');
                }
                Token::Plus => {
                    self.next();
                    pattern.push('+');
                }
                Token::Minus => {
                    self.next();
                    pattern.push('-');
                }
                Token::Int(n) => {
                    self.next();
                    pattern.push_str(&n.to_string());
                }
                _ => {
                    // 其他令牌不是模式的一部分
                    break;
                }
            }
        }
        
        if pattern.is_empty() {
            return Ok(None);
        }
        
        Ok(Some(SectionPattern {
            pattern,
            sort: SortMode::None,
        }))
    }

    fn try_parse_fill(&mut self) -> Option<u64> {
        if self.eat(&Token::Assign) {
            if let Token::Int(n) = self.next() {
                return Some(n);
            }
        }
        None
    }

    fn parse_section_phdrs(&mut self) -> Vec<String> {
        let mut phdrs = Vec::new();
        while self.eat(&Token::Colon) {
            if let Token::Ident(s) = self.next() {
                phdrs.push(s);
            }
        }
        phdrs
    }

    // ─── Expression parser (precedence climbing) ─────────────────────────

    fn parse_expr(&mut self) -> Result<Expr, String> {
        self.parse_ternary()
    }

    fn parse_ternary(&mut self) -> Result<Expr, String> {
        let cond = self.parse_log_or()?;
        if self.eat(&Token::Question) {
            let then_expr = self.parse_expr()?;
            self.expect(&Token::Colon)?;
            let else_expr = self.parse_expr()?;
            Ok(Expr::Ternary(
                Box::new(cond),
                Box::new(then_expr),
                Box::new(else_expr),
            ))
        } else {
            Ok(cond)
        }
    }

    fn parse_log_or(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_log_and()?;
        while self.eat(&Token::Or) {
            let right = self.parse_log_and()?;
            left = Expr::BinaryOp(Box::new(left), BinOp::LogOr, Box::new(right));
        }
        Ok(left)
    }

    fn parse_log_and(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_bit_or()?;
        while self.eat(&Token::And) {
            let right = self.parse_bit_or()?;
            left = Expr::BinaryOp(Box::new(left), BinOp::LogAnd, Box::new(right));
        }
        Ok(left)
    }

    fn parse_bit_or(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_bit_xor()?;
        while matches!(self.peek(), Token::Pipe) {
            self.next();
            let right = self.parse_bit_xor()?;
            left = Expr::BinaryOp(Box::new(left), BinOp::BitOr, Box::new(right));
        }
        Ok(left)
    }

    fn parse_bit_xor(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_bit_and()?;
        while matches!(self.peek(), Token::Caret) {
            self.next();
            let right = self.parse_bit_and()?;
            left = Expr::BinaryOp(Box::new(left), BinOp::BitXor, Box::new(right));
        }
        Ok(left)
    }

    fn parse_bit_and(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_equality()?;
        while matches!(self.peek(), Token::Amp) {
            self.next();
            let right = self.parse_equality()?;
            left = Expr::BinaryOp(Box::new(left), BinOp::BitAnd, Box::new(right));
        }
        Ok(left)
    }

    fn parse_equality(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_comparison()?;
        loop {
            let op = match self.peek() {
                Token::Eq => BinOp::Eq,
                Token::Ne => BinOp::Ne,
                _ => break,
            };
            self.next();
            let right = self.parse_comparison()?;
            left = Expr::BinaryOp(Box::new(left), op, Box::new(right));
        }
        Ok(left)
    }

    fn parse_comparison(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_shift()?;
        loop {
            let op = match self.peek() {
                Token::Lt => BinOp::Lt,
                Token::Gt => BinOp::Gt,
                Token::Le => BinOp::Le,
                Token::Ge => BinOp::Ge,
                _ => break,
            };
            self.next();
            let right = self.parse_shift()?;
            left = Expr::BinaryOp(Box::new(left), op, Box::new(right));
        }
        Ok(left)
    }

    fn parse_shift(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_additive()?;
        loop {
            let op = match self.peek() {
                Token::LShift => BinOp::LShift,
                Token::RShift => BinOp::RShift,
                _ => break,
            };
            self.next();
            let right = self.parse_additive()?;
            left = Expr::BinaryOp(Box::new(left), op, Box::new(right));
        }
        Ok(left)
    }

    fn parse_additive(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_multiplicative()?;
        loop {
            let op = match self.peek() {
                Token::Plus => BinOp::Add,
                Token::Minus => BinOp::Sub,
                _ => break,
            };
            self.next();
            let right = self.parse_multiplicative()?;
            left = Expr::BinaryOp(Box::new(left), op, Box::new(right));
        }
        Ok(left)
    }

    fn parse_multiplicative(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_unary()?;
        loop {
            let op = match self.peek() {
                Token::Star => BinOp::Mul,
                Token::Slash => BinOp::Div,
                Token::Percent => BinOp::Mod,
                _ => break,
            };
            self.next();
            let right = self.parse_unary()?;
            left = Expr::BinaryOp(Box::new(left), op, Box::new(right));
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr, String> {
        match self.peek().clone() {
            Token::Minus => {
                self.next();
                let expr = self.parse_unary()?;
                Ok(Expr::UnaryOp(UnaryOp::Neg, Box::new(expr)))
            }
            Token::Tilde => {
                self.next();
                let expr = self.parse_unary()?;
                Ok(Expr::UnaryOp(UnaryOp::BitNot, Box::new(expr)))
            }
            Token::Bang => {
                self.next();
                let expr = self.parse_unary()?;
                Ok(Expr::UnaryOp(UnaryOp::Not, Box::new(expr)))
            }
            _ => self.parse_primary(),
        }
    }

    fn parse_primary(&mut self) -> Result<Expr, String> {
        match self.peek().clone() {
            Token::Int(n) => {
                self.next();
                Ok(Expr::Int(n))
            }
            Token::Dot => {
                self.next();
                // check for .name (section name)
                if let Token::Ident(_) = self.peek() {
                    // this is actually a section name, not the dot location counter
                    // but in expression context, . is the location counter
                    // and .name shouldn't appear in expressions
                }
                Ok(Expr::Dot)
            }
            Token::LParen => {
                self.next();
                let expr = self.parse_expr()?;
                self.expect(&Token::RParen)?;
                Ok(expr)
            }
            Token::Absolute | Token::Addr | Token::Sizeof | Token::Align
            | Token::Max | Token::Min | Token::Defined | Token::Length
            | Token::Assert => {
                let name = match self.next() {
                    Token::Absolute => "ABSOLUTE",
                    Token::Addr => "ADDR",
                    Token::Sizeof => "SIZEOF",
                    Token::Align => "ALIGN",
                    Token::Max => "MAX",
                    Token::Min => "MIN",
                    Token::Defined => "DEFINED",
                    Token::Length => "LENGTH",
                    Token::Assert => "ASSERT",
                    _ => unreachable!(),
                };
                self.expect(&Token::LParen)?;
                let mut args = Vec::new();
                if !matches!(self.peek(), Token::RParen) {
                    args.push(self.parse_expr()?);
                    while self.eat(&Token::Comma) {
                        args.push(self.parse_expr()?);
                    }
                }
                self.expect(&Token::RParen)?;
                Ok(Expr::Call(name.to_string(), args))
            }
            Token::Ident(name) => {
                self.next();
                // 检查是否是函数调用
                if self.eat(&Token::LParen) {
                    let mut args = Vec::new();
                    if !matches!(self.peek(), Token::RParen) {
                        args.push(self.parse_expr()?);
                        while self.eat(&Token::Comma) {
                            args.push(self.parse_expr()?);
                        }
                    }
                    self.expect(&Token::RParen)?;
                    Ok(Expr::Call(name, args))
                } else {
                    Ok(Expr::Symbol(name))
                }
            }
            Token::Str(s) => {
                self.next();
                // 字符串在表达式中当作符号名
                Ok(Expr::Symbol(s))
            }
            other => {
                Err(format!("unexpected token in expression: {:?}", other))
            }
        }
    }
}

// ─── Expression Evaluator ───────────────────────────────────────────────────

/// 表达式求值上下文
pub struct EvalContext {
    /// 位置计数器 (.)
    pub location_counter: u64,
    /// 符号值表
    pub symbols: HashMap<String, u64>,
    /// 节地址表 (section name -> VMA)
    pub section_addrs: HashMap<String, u64>,
    /// 节大小表 (section name -> size)
    pub section_sizes: HashMap<String, u64>,
}

impl EvalContext {
    pub fn new() -> Self {
        Self {
            location_counter: 0,
            symbols: HashMap::new(),
            section_addrs: HashMap::new(),
            section_sizes: HashMap::new(),
        }
    }

    pub fn eval(&self, expr: &Expr) -> Result<u64, String> {
        match expr {
            Expr::Int(n) => Ok(*n),
            Expr::Symbol(name) => {
                self.symbols.get(name).copied().ok_or_else(|| {
                    format!("undefined symbol '{}' in linker script expression", name)
                })
            }
            Expr::Dot => Ok(self.location_counter),
            Expr::BinaryOp(left, op, right) => {
                let l = self.eval(left)?;
                let r = self.eval(right)?;
                Ok(match op {
                    BinOp::Add => l.wrapping_add(r),
                    BinOp::Sub => l.wrapping_sub(r),
                    BinOp::Mul => l.wrapping_mul(r),
                    BinOp::Div => {
                        if r == 0 { 0 } else { l / r }
                    }
                    BinOp::Mod => {
                        if r == 0 { 0 } else { l % r }
                    }
                    BinOp::BitAnd => l & r,
                    BinOp::BitOr => l | r,
                    BinOp::BitXor => l ^ r,
                    BinOp::LShift => l << r,
                    BinOp::RShift => l >> r,
                    BinOp::Eq => if l == r { 1 } else { 0 },
                    BinOp::Ne => if l != r { 1 } else { 0 },
                    BinOp::Lt => if l < r { 1 } else { 0 },
                    BinOp::Gt => if l > r { 1 } else { 0 },
                    BinOp::Le => if l <= r { 1 } else { 0 },
                    BinOp::Ge => if l >= r { 1 } else { 0 },
                    BinOp::LogAnd => if l != 0 && r != 0 { 1 } else { 0 },
                    BinOp::LogOr => if l != 0 || r != 0 { 1 } else { 0 },
                })
            }
            Expr::UnaryOp(op, expr) => {
                let v = self.eval(expr)?;
                Ok(match op {
                    UnaryOp::Neg => v.wrapping_neg(),
                    UnaryOp::Not => if v == 0 { 1 } else { 0 },
                    UnaryOp::BitNot => !v,
                })
            }
            Expr::Ternary(cond, then_e, else_e) => {
                let c = self.eval(cond)?;
                if c != 0 {
                    self.eval(then_e)
                } else {
                    self.eval(else_e)
                }
            }
            Expr::Call(name, args) => {
                match name.as_str() {
                    "ABSOLUTE" => {
                        if args.is_empty() {
                            return Err("ABSOLUTE() requires 1 argument".into());
                        }
                        self.eval(&args[0])
                    }
                    "ADDR" => {
                        if args.is_empty() {
                            return Err("ADDR() requires 1 argument".into());
                        }
                        if let Expr::Symbol(sec_name) = &args[0] {
                            self.section_addrs.get(sec_name).copied().ok_or_else(|| {
                                format!("ADDR: section '{}' not found", sec_name)
                            })
                        } else {
                            Err("ADDR() requires section name".into())
                        }
                    }
                    "SIZEOF" => {
                        if args.is_empty() {
                            return Err("SIZEOF() requires 1 argument".into());
                        }
                        if let Expr::Symbol(sec_name) = &args[0] {
                            self.section_sizes.get(sec_name).copied().ok_or_else(|| {
                                format!("SIZEOF: section '{}' not found", sec_name)
                            })
                        } else {
                            Err("SIZEOF() requires section name".into())
                        }
                    }
                    "ALIGN" => {
                        if args.is_empty() {
                            return Err("ALIGN() requires 1 argument".into());
                        }
                        let align = self.eval(&args[0])?;
                        if align == 0 {
                            return Ok(self.location_counter);
                        }
                        Ok((self.location_counter + align - 1) & !(align - 1))
                    }
                    "MAX" => {
                        if args.len() < 2 {
                            return Err("MAX() requires 2 arguments".into());
                        }
                        let a = self.eval(&args[0])?;
                        let b = self.eval(&args[1])?;
                        Ok(a.max(b))
                    }
                    "MIN" => {
                        if args.len() < 2 {
                            return Err("MIN() requires 2 arguments".into());
                        }
                        let a = self.eval(&args[0])?;
                        let b = self.eval(&args[1])?;
                        Ok(a.min(b))
                    }
                    "DEFINED" => {
                        if args.is_empty() {
                            return Err("DEFINED() requires 1 argument".into());
                        }
                        if let Expr::Symbol(name) = &args[0] {
                            Ok(if self.symbols.contains_key(name) { 1 } else { 0 })
                        } else {
                            Ok(0)
                        }
                    }
                    other => Err(format!("unknown function '{}' in linker script", other)),
                }
            }
        }
    }
}

// ─── Public API ─────────────────────────────────────────────────────────────

impl LinkerScript {
    /// 解析预处理后的链接器脚本文本
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut lexer = Lexer::new(text);
        let mut tokens = Vec::new();
        loop {
            let t = lexer.next_token();
            if t == Token::Eof {
                tokens.push(Token::Eof);
                break;
            }
            tokens.push(t);
        }
        let mut parser = Parser::new(tokens);
        parser.parse()
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_script() {
        let text = r#"
            OUTPUT_FORMAT("elf64-x86-64")
            OUTPUT_ARCH(i386:x86-64)
            ENTRY(phys_startup_64)
            SECTIONS {
                . = 0xffffffff80000000;
                .text : { *(.text) }
                .data : { *(.data) }
                .bss : { *(.bss) }
            }
        "#;
        let script = LinkerScript::parse(text).unwrap();
        assert_eq!(script.output_format.as_deref(), Some("elf64-x86-64"));
        assert_eq!(script.entry.as_deref(), Some("phys_startup_64"));
        assert!(!script.sections.is_empty());
    }

    #[test]
    fn parse_expression() {
        let text = r#"
            SECTIONS {
                . = (0xffffffff80000000 + 0x200000);
                .text : AT(ADDR(.text) - 0xffffffff80000000) {
                    _text = .;
                    *(.text)
                    _etext = .;
                }
            }
        "#;
        let script = LinkerScript::parse(text).unwrap();
        assert!(!script.sections.is_empty());
    }

    #[test]
    fn eval_expression() {
        let ctx = EvalContext::new();
        let expr = Expr::BinaryOp(
            Box::new(Expr::Int(0x1000)),
            BinOp::Add,
            Box::new(Expr::Int(0x200)),
        );
        assert_eq!(ctx.eval(&expr).unwrap(), 0x1200);
    }

    #[test]
    fn eval_align() {
        let mut ctx = EvalContext::new();
        ctx.location_counter = 0x1001;
        let expr = Expr::Call("ALIGN".into(), vec![Expr::Int(0x1000)]);
        assert_eq!(ctx.eval(&expr).unwrap(), 0x2000);
    }

    #[test]
    fn parse_phdrs() {
        let text = r#"
            PHDRS {
                text PT_LOAD FLAGS(5);
                data PT_LOAD FLAGS(6);
                note PT_NOTE FLAGS(0);
            }
            SECTIONS {
                .text : { *(.text) } :text
                .data : { *(.data) } :data
            }
        "#;
        let script = LinkerScript::parse(text).unwrap();
        assert_eq!(script.phdrs.len(), 3);
    }

    #[test]
    fn parse_assert_provide() {
        let text = r#"
            SECTIONS {
                .text : { *(.text) }
                ASSERT(SIZEOF(.got) == 0, "no GOT allowed")
            }
            PROVIDE(__hidden = __visible);
        "#;
        let script = LinkerScript::parse(text).unwrap();
        assert!(!script.trailing_commands.is_empty());
    }

    #[test]
    fn parse_kernel_like_script() {
        // 测试类似内核 vmlinux.lds 的脚本
        let text = r#"
            OUTPUT_FORMAT("elf64-x86-64")
            ENTRY(phys_startup_64)
            jiffies = jiffies_64;
            PHDRS {
                text PT_LOAD FLAGS(5);
                data PT_LOAD FLAGS(6);
                note PT_NOTE FLAGS(0);
            }
            SECTIONS {
                . = (0xffffffff80000000 + 0x200000);
                .text : AT(ADDR(.text) - 0xffffffff80000000) {
                    _text = .;
                    _stext = .;
                    *(.text)
                    *(.text.*)
                } :text = 0xcccccccc
                _etext = .;
                . = ALIGN(4096);
                .rodata : AT(ADDR(.rodata) - 0xffffffff80000000) {
                    __start_rodata = .;
                    *(.rodata)
                    *(.rodata.*)
                }
                .data : AT(ADDR(.data) - 0xffffffff80000000) {
                    _sdata = .;
                    *(.data)
                    *(.data.*)
                    _edata = .;
                } :data
                .bss : AT(ADDR(.bss) - 0xffffffff80000000) {
                    __bss_start = .;
                    *(.bss)
                    *(.bss.*)
                    __bss_stop = .;
                }
                _end = .;
            }
        "#;
        let script = LinkerScript::parse(text).unwrap();
        assert_eq!(script.output_format.as_deref(), Some("elf64-x86-64"));
        assert_eq!(script.entry.as_deref(), Some("phys_startup_64"));
        assert_eq!(script.phdrs.len(), 3);
        assert!(!script.sections.is_empty());
        // 检查顶层符号赋值
        assert!(!script.top_assignments.is_empty());
    }

    #[test]
    fn parse_complex_section_patterns() {
        // 测试复杂的节名模式，包括字符类和通配符
        let text = r#"
            SECTIONS {
                .text : {
                    *(.text.s[_0-9A-Za-su-z]* .text.s .text.s.*)
                }
            }
        "#;
        let script = LinkerScript::parse(text).unwrap();
        assert!(!script.sections.is_empty());
    }

    #[test]
    fn parse_location_counter_with_complex_expr() {
        // 测试位置计数器赋值与复杂表达式
        let text = r#"
            SECTIONS {
                .text : {
                    . = srso_alias_untrain_ret | (1 << 2) | (1 << 8);
                    *(.text)
                }
            }
        "#;
        let script = LinkerScript::parse(text).unwrap();
        assert!(!script.sections.is_empty());
    }

    #[test]
    fn parse_sort_with_plus_pattern() {
        // 测试 SORT 与包含 + 的模式
        let text = r#"
            SECTIONS {
                __ksymtab : {
                    KEEP(*(SORT(___ksymtab+*)))
                }
            }
        "#;
        let script = LinkerScript::parse(text).unwrap();
        assert!(!script.sections.is_empty());
    }

    #[test]
    fn parse_real_kernel_vmlinux_lds() {
        // 测试解析真实的内核 vmlinux.lds
        let path = "/root/linux-7.1/arch/x86/kernel/vmlinux.lds";
        if !std::path::Path::new(path).exists() {
            println!("Skipping test: {} not found", path);
            return;
        }
        let text = std::fs::read_to_string(path).expect("read vmlinux.lds");
        let script = match LinkerScript::parse(&text) {
            Ok(s) => s,
            Err(e) => {
                panic!("parse vmlinux.lds failed: {}", e);
            }
        };
        
        // 验证基本结构
        assert_eq!(script.output_format.as_deref(), Some("elf64-x86-64"));
        assert!(script.output_arch.is_some());
        assert_eq!(script.entry.as_deref(), Some("phys_startup_64"));
        assert_eq!(script.phdrs.len(), 3, "expected 3 phdrs");
        
        // 验证 PHDRS
        assert_eq!(script.phdrs[0].name, "text");
        assert_eq!(script.phdrs[1].name, "data");
        assert_eq!(script.phdrs[2].name, "note");
        
        // 验证 SECTIONS 不为空
        assert!(!script.sections.is_empty(), "sections should not be empty");
        
        // 验证顶层符号赋值 (jiffies = jiffies_64)
        assert!(!script.top_assignments.is_empty(), "should have top-level assignments");
        
        println!("Successfully parsed kernel vmlinux.lds:");
        println!("  - {} sections", script.sections.len());
        println!("  - {} phdrs", script.phdrs.len());
        println!("  - {} top assignments", script.top_assignments.len());
        println!("  - {} trailing commands", script.trailing_commands.len());
    }
}

// ─── Linker Script Executor ─────────────────────────────────────────────────

use super::object::InputBundle;
use super::resolve::ResolvedSymbol;
use crate::codegen::elf::{SHT_NOBITS, SHT_PROGBITS};

/// 链接脚本执行结果
pub struct ScriptLayoutResult {
    /// 输出节列表（按脚本顺序）
    pub sections: Vec<ScriptOutputSection>,
    /// 符号赋值
    pub symbols: Vec<ScriptSymbol>,
    /// 入口地址
    pub entry: Option<u64>,
    /// 断言失败消息
    pub assert_failures: Vec<String>,
}

/// 脚本定义的输出节
pub struct ScriptOutputSection {
    pub name: String,
    pub vma: u64,
    pub lma: u64,
    pub align: u64,
    pub fill: u64,
    pub size: u64,
    pub data: Vec<u8>,
    pub flags: u32,
    pub ty: u32,
    pub phdrs: Vec<String>,
    pub input_sections: Vec<InputSectionMatch>,
    pub is_discard: bool,
    pub is_info: bool,
}

/// 输入节匹配结果
pub struct InputSectionMatch {
    pub input_index: u32,
    pub pattern: String,
    pub offset: u64,
}

/// 脚本定义的符号
pub struct ScriptSymbol {
    pub name: String,
    pub value: u64,
    pub is_provide: bool,
}

/// 执行链接脚本，生成布局结果
pub fn execute_script(
    script: &LinkerScript,
    bundle: &InputBundle,
    _resolved: &[ResolvedSymbol],
) -> Result<ScriptLayoutResult, String> {
    let mut ctx = EvalContext::new();
    let mut result = ScriptLayoutResult {
        sections: Vec::new(),
        symbols: Vec::new(),
        entry: None,
        assert_failures: Vec::new(),
    };

    // 1. 处理顶层符号赋值
    for assign in &script.top_assignments {
        let value = ctx.eval(&assign.expr).unwrap_or(0);
        ctx.symbols.insert(assign.name.clone(), value);
        result.symbols.push(ScriptSymbol {
            name: assign.name.clone(),
            value,
            is_provide: assign.is_provide,
        });
    }

    // 2. 处理 SECTIONS 块
    for cmd in &script.sections {
        match cmd {
            SectionCommand::LocationAssign(expr) => {
                ctx.location_counter = ctx.eval(expr).unwrap_or(ctx.location_counter);
            }
            SectionCommand::SymbolAssign(assign) => {
                let value = ctx.eval(&assign.expr).unwrap_or(0);
                ctx.symbols.insert(assign.name.clone(), value);
                result.symbols.push(ScriptSymbol {
                    name: assign.name.clone(),
                    value,
                    is_provide: assign.is_provide,
                });
            }
            SectionCommand::OutputSection(sec_def) => {
                let out_sec = execute_output_section(sec_def, bundle, &mut ctx)?;
                if !out_sec.is_discard {
                    ctx.section_addrs.insert(out_sec.name.clone(), out_sec.vma);
                    ctx.section_sizes.insert(out_sec.name.clone(), out_sec.size);
                    ctx.location_counter = out_sec.vma + out_sec.size;
                    result.sections.push(out_sec);
                }
            }
            SectionCommand::AssertCmd(expr, msg) => {
                let val = ctx.eval(expr).unwrap_or(0);
                if val == 0 {
                    result.assert_failures.push(msg.clone());
                }
            }
            SectionCommand::ProvideCmd(name, expr) => {
                if !ctx.symbols.contains_key(name) {
                    let value = ctx.eval(expr).unwrap_or(0);
                    ctx.symbols.insert(name.clone(), value);
                    result.symbols.push(ScriptSymbol {
                        name: name.clone(),
                        value,
                        is_provide: true,
                    });
                }
            }
        }
    }

    // 3. 处理顶层 ASSERT/PROVIDE
    for cmd in &script.trailing_commands {
        match cmd {
            TrailingCommand::Assert(expr, msg) => {
                let val = ctx.eval(expr).unwrap_or(0);
                if val == 0 {
                    result.assert_failures.push(msg.clone());
                }
            }
            TrailingCommand::Provide(name, expr) => {
                if !ctx.symbols.contains_key(name) {
                    let value = ctx.eval(expr).unwrap_or(0);
                    ctx.symbols.insert(name.clone(), value);
                    result.symbols.push(ScriptSymbol {
                        name: name.clone(),
                        value,
                        is_provide: true,
                    });
                }
            }
            TrailingCommand::LocationAssign(expr) => {
                ctx.location_counter = ctx.eval(expr).unwrap_or(ctx.location_counter);
            }
        }
    }

    // 4. 设置入口地址
    if let Some(ref entry_name) = script.entry {
        if let Some(&addr) = ctx.symbols.get(entry_name) {
            result.entry = Some(addr);
        }
    }

    Ok(result)
}

/// 执行单个输出节定义
fn execute_output_section(
    sec_def: &OutputSectionDef,
    bundle: &InputBundle,
    ctx: &mut EvalContext,
) -> Result<ScriptOutputSection, String> {
    let mut out_sec = ScriptOutputSection {
        name: sec_def.name.clone(),
        vma: ctx.location_counter,
        lma: ctx.location_counter,
        align: 1,
        fill: 0,
        size: 0,
        data: Vec::new(),
        flags: 0,
        ty: SHT_PROGBITS,
        phdrs: sec_def.phdrs.clone(),
        input_sections: Vec::new(),
        is_discard: sec_def.is_discard,
        is_info: sec_def.is_info,
    };

    // 计算地址
    if let Some(ref addr_expr) = sec_def.address {
        out_sec.vma = ctx.eval(addr_expr).unwrap_or(ctx.location_counter);
    }
    if let Some(ref lma_expr) = sec_def.lma {
        out_sec.lma = ctx.eval(lma_expr).unwrap_or(out_sec.vma);
    }
    if let Some(ref align_expr) = sec_def.align {
        out_sec.align = ctx.eval(align_expr).unwrap_or(1);
    }
    if let Some(fill) = sec_def.fill {
        out_sec.fill = fill;
    }

    // 对齐位置计数器
    if out_sec.align > 1 {
        ctx.location_counter = (ctx.location_counter + out_sec.align - 1) & !(out_sec.align - 1);
        out_sec.vma = ctx.location_counter;
    }

    // 处理节体
    let mut current_offset: u64 = 0;
    for item in &sec_def.body {
        match item {
            SectionBodyItem::LocationAssign(expr) => {
                let val = ctx.eval(expr).unwrap_or(0);
                ctx.location_counter = out_sec.vma + val;
                current_offset = val;
            }
            SectionBodyItem::SymbolAssign(assign) => {
                let val = ctx.eval(&assign.expr).unwrap_or(0);
                ctx.symbols.insert(assign.name.clone(), val);
            }
            SectionBodyItem::InputSection(spec) => {
                // 匹配输入节
                let matches = match_input_sections(spec, bundle, ctx);
                for m in matches {
                    let in_sec = &bundle.sections[m.input_index as usize];
                    // 更新节标志
                    out_sec.flags |= in_sec.flags;
                    if in_sec.ty == SHT_NOBITS {
                        out_sec.ty = SHT_NOBITS;
                    }
                    // 对齐
                    let align = in_sec.align.max(1);
                    let aligned = (current_offset + align - 1) & !(align - 1);
                    if aligned > current_offset && out_sec.ty != SHT_NOBITS {
                        let pad = aligned - current_offset;
                        out_sec.data.extend(std::iter::repeat_n(0, pad as usize));
                    }
                    current_offset = aligned;
                    // 添加数据
                    if in_sec.ty != SHT_NOBITS {
                        out_sec.data.extend_from_slice(&in_sec.data);
                        current_offset += in_sec.data.len() as u64;
                    }
                    // 记录匹配
                    out_sec.input_sections.push(InputSectionMatch {
                        input_index: m.input_index,
                        pattern: m.pattern,
                        offset: aligned,
                    });
                }
            }
            SectionBodyItem::Constructors => {
                // CONSTRUCTORS 用于 a.out 兼容，现代 ELF 不需要特殊处理
            }
            SectionBodyItem::DataFill(value, size) => {
                let bytes = match size {
                    1 => vec![*value as u8],
                    2 => (*value as u16).to_le_bytes().to_vec(),
                    4 => (*value as u32).to_le_bytes().to_vec(),
                    8 => value.to_le_bytes().to_vec(),
                    _ => vec![],
                };
                out_sec.data.extend_from_slice(&bytes);
                current_offset += *size as u64;
            }
        }
    }

    out_sec.size = current_offset;
    ctx.location_counter = out_sec.vma + out_sec.size;

    Ok(out_sec)
}

/// 匹配输入节到输出节规范
fn match_input_sections(
    spec: &InputSectionSpec,
    bundle: &InputBundle,
    _ctx: &EvalContext,
) -> Vec<InputSectionMatch> {
    let mut matches = Vec::new();

    for (i, in_sec) in bundle.sections.iter().enumerate() {
        // 跳过空节名和特殊节
        if in_sec.name.is_empty()
            || in_sec.name == ".symtab"
            || in_sec.name == ".strtab"
            || in_sec.name == ".shstrtab"
        {
            continue;
        }

        // 检查是否匹配任何模式
        for pat in &spec.section_patterns {
            if section_name_matches(&in_sec.name, &pat.pattern) {
                matches.push(InputSectionMatch {
                    input_index: i as u32,
                    pattern: pat.pattern.clone(),
                    offset: 0,
                });
                break;
            }
        }
    }

    matches
}

/// 检查节名是否匹配模式（支持通配符 * 和 ?）
fn section_name_matches(name: &str, pattern: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') && !pattern.contains('?') {
        return name == pattern;
    }

    // 简单的通配符匹配
    let mut ni = 0;
    let mut pi = 0;
    let name_chars: Vec<char> = name.chars().collect();
    let pattern_chars: Vec<char> = pattern.chars().collect();
    let mut star_idx = None;
    let mut match_idx = 0;

    while ni < name_chars.len() {
        if pi < pattern_chars.len() && (pattern_chars[pi] == '?' || pattern_chars[pi] == name_chars[ni]) {
            ni += 1;
            pi += 1;
        } else if pi < pattern_chars.len() && pattern_chars[pi] == '*' {
            star_idx = Some(pi);
            match_idx = ni;
            pi += 1;
        } else if let Some(si) = star_idx {
            pi = si + 1;
            match_idx += 1;
            ni = match_idx;
        } else {
            return false;
        }
    }

    while pi < pattern_chars.len() && pattern_chars[pi] == '*' {
        pi += 1;
    }

    pi == pattern_chars.len()
}
