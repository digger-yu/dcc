// src/link/linker_script.rs - 链接器脚本解析与执行
//
// 支持 GNU ld 链接器脚本的核心子集，面向 Linux 内核 vmlinux.lds.S 预处理输出：
//   - OUTPUT_FORMAT / OUTPUT_ARCH / ENTRY
//   - PHDRS { ... }
//   - MEMORY { ... } 含 ORIGIN/LENGTH/ORG
//   - SECTIONS { ... } 含 AT() / ALIGN() / fill / :phdr
//   - 符号赋值 (sym = expr) / PROVIDE(sym = expr)
//   - 输入节通配 *(.text ...) / KEEP / SORT / SORT_BY_ALIGNMENT / SORT_BY_NAME
//   - EXCLUDE_FILE 排除特定文件的输入节
//   - ASSERT / SIZEOF / ADDR / ABSOLUTE / ALIGN / ADDR
//   - NEXT / LOG2CEIL / ORIGIN / ORG / LENGTH 函数
//   - /DISCARD/ 与 (INFO) 虚拟节
//   - VERSION { ... } 符号版本控制块 (global/local/parent)
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
    Memory,
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
    Origin,
    Next,
    Log2Ceil,
    Constructors,
    Discard,
    Info,
    ExcludeFile,
    Input,
    Group,
    RegionAlias,
    NoCrossRefs,
    Insert,
    Version,
    Common,
    Global,
    Local,
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
    Float(f64),
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

    fn read_number(&mut self, first: char) -> Token {
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
                    return Token::Int(u64::from_str_radix(&hex, 16).unwrap_or(0));
                }
                _ => {}
            }
        }
        // 读取整数部分
        let mut num = String::from(first);
        while let Some(c) = self.peek_char() {
            if c.is_ascii_digit() {
                num.push(c);
                self.next_char();
            } else {
                break;
            }
        }
        // 检测浮点: 小数点或指数
        let mut is_float = false;
        if self.peek_char() == Some('.') {
            // 确保后面不是标识符字符（避免将 0x1234.abcd 误判为浮点）
            // 链接器脚本中浮点数的小数点后应是数字
            let next_pos = self.pos + 1;
            if next_pos < self.chars.len() && self.chars[next_pos].is_ascii_digit() {
                is_float = true;
                num.push('.');
                self.next_char(); // consume '.'
                while let Some(c) = self.peek_char() {
                    if c.is_ascii_digit() {
                        num.push(c);
                        self.next_char();
                    } else {
                        break;
                    }
                }
            }
        }
        // 检测指数部分 (e/E +/- digits)
        if let Some(c) = self.peek_char() {
            if c == 'e' || c == 'E' {
                is_float = true;
                num.push(c);
                self.next_char();
                if let Some(sign) = self.peek_char() {
                    if sign == '+' || sign == '-' {
                        num.push(sign);
                        self.next_char();
                    }
                }
                while let Some(c) = self.peek_char() {
                    if c.is_ascii_digit() {
                        num.push(c);
                        self.next_char();
                    } else {
                        break;
                    }
                }
            }
        }
        if is_float {
            Token::Float(num.parse::<f64>().unwrap_or(0.0))
        } else {
            // handle K/M suffixes
            match self.peek_char() {
                Some('K') | Some('k') => {
                    self.next_char();
                    Token::Int(num.parse::<u64>().unwrap_or(0) * 1024)
                }
                Some('M') | Some('m') => {
                    self.next_char();
                    Token::Int(num.parse::<u64>().unwrap_or(0) * 1024 * 1024)
                }
                _ => Token::Int(num.parse().unwrap_or(0)),
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
            '/' => {
                // Check for /DISCARD/
                let saved = self.pos;
                let discard = "DISCARD/";
                let mut matches = true;
                for expected in discard.chars() {
                    if self.peek_char() != Some(expected) {
                        matches = false;
                        break;
                    }
                    self.next_char();
                }
                if matches {
                    Token::Discard
                } else {
                    self.pos = saved;
                    Token::Slash
                }
            }
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
                    // Handle .. in section names like .text..__x86
                    // The first . is consumed, then read_ident picks up .__x86
                    if c2 == '.' {
                        return Token::Ident(self.read_ident('.'));
                    }
                }
                Token::Dot
            }
            c if c.is_ascii_digit() => self.read_number(c),
            c if c.is_ascii_alphabetic() || c == '_' => {
                let ident = self.read_ident(c);
                match ident.as_str() {
                    "OUTPUT_FORMAT" => Token::OutputFormat,
                    "OUTPUT_ARCH" => Token::OutputArch,
                    "ENTRY" => Token::Entry,
                    "PHDRS" => Token::Phdrs,
                    "SECTIONS" => Token::Sections,
                    "MEMORY" => Token::Memory,
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
                    "ORIGIN" | "ORG" => Token::Origin,
                    "NEXT" => Token::Next,
                    "LOG2CEIL" => Token::Log2Ceil,
                    "CONSTRUCTORS" => Token::Constructors,
                    "EXCLUDE_FILE" => Token::ExcludeFile,
                    "INPUT" => Token::Input,
                    "GROUP" => Token::Group,
                    "REGION_ALIAS" => Token::RegionAlias,
                    "NOCROSSREFS" => Token::NoCrossRefs,
                    "INSERT" => Token::Insert,
                    "VERSION" => Token::Version,
                    "COMMON" => Token::Common,
                    "global" => Token::Global,
                    "local" => Token::Local,
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
    pub memory_regions: Vec<MemoryRegion>,
    pub sections: Vec<SectionCommand>,
    /// 顶层符号赋值 (SECTIONS 之外)
    pub top_assignments: Vec<SymbolAssignment>,
    /// SECTIONS 之后的 ASSERT/PROVIDE
    pub trailing_commands: Vec<TrailingCommand>,
    /// INPUT/GROUP 指定的输入文件
    pub input_files: Vec<String>,
    /// NOCROSSREFS 检查的节列表
    pub no_cross_refs: Vec<Vec<String>>,
    /// INSERT 命令
    pub insert_commands: Vec<InsertCommand>,
    /// VERSION 块中的版本定义节点
    pub version_nodes: Vec<VersionNode>,
}

/// MEMORY 块中的内存区域定义
#[derive(Debug, Clone)]
pub struct MemoryRegion {
    pub name: String,
    pub origin: Expr,
    pub length: Expr,
    pub attributes: String,
}

/// INSERT 命令
#[derive(Debug, Clone)]
pub struct InsertCommand {
    pub after: Option<String>,
    pub before: Option<String>,
    pub section_name: String,
}

/// VERSION 块中的版本定义节点
///
/// GNU ld 语法:
/// ```text
/// VERSION {
///     version_id [(@parent_id)] {
///         global:
///             symbol1;
///             symbol2;
///         local:
///             symbol3;
///             *;
///     };
/// }
/// ```
#[derive(Debug, Clone)]
pub struct VersionNode {
    /// 版本标识符 (e.g. "GCC_3.0", "GLIBC_2.14")
    pub version_id: String,
    /// 父版本 (e.g. Some("GCC_2.0") 表示 "@GCC_2.0")
    pub parent: Option<String>,
    /// global: 列表中的符号 (支持通配符 "*")
    pub global_symbols: Vec<String>,
    /// local: 列表中的符号 (支持通配符 "*")
    pub local_symbols: Vec<String>,
}

impl LinkerScript {
    /// 提取链接脚本中定义的所有符号名（用于符号解析前预注册）
    pub fn extract_defined_symbols(&self) -> Vec<String> {
        let mut syms = Vec::new();
        // 顶层赋值
        for a in &self.top_assignments {
            syms.push(a.name.clone());
        }
        // SECTIONS 块内的符号
        for cmd in &self.sections {
            match cmd {
                SectionCommand::SymbolAssign(a) => {
                    syms.push(a.name.clone());
                }
                SectionCommand::ProvideCmd(name, _) => {
                    syms.push(name.clone());
                }
                SectionCommand::OutputSection(def) => {
                    for item in &def.body {
                        if let SectionBodyItem::SymbolAssign(a) = item {
                            syms.push(a.name.clone());
                        }
                    }
                }
                _ => {}
            }
        }
        // 尾部 PROVIDE
        for cmd in &self.trailing_commands {
            if let TrailingCommand::Provide(name, _) = cmd {
                syms.push(name.clone());
            }
        }
        // VERSION 块中 global: 列的符号 (这些符号需要被定义)
        for node in &self.version_nodes {
            for sym in &node.global_symbols {
                if sym != "*" {
                    syms.push(sym.clone());
                }
            }
        }
        syms
    }

    /// 提取 VERSION 块中的版本定义信息
    ///
    /// 返回 (version_id, global_symbols, local_symbols) 元组的列表，
    /// 用于调用方将版本信息导入 VersionTable。
    pub fn extract_version_info(&self) -> Vec<(&str, &[String], &[String])> {
        self.version_nodes
            .iter()
            .map(|n| {
                (
                    n.version_id.as_str(),
                    n.global_symbols.as_slice(),
                    n.local_symbols.as_slice(),
                )
            })
            .collect()
    }
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
    /// EXCLUDE_FILE 排除的文件模式列表
    pub exclude_files: Vec<String>,
    /// 是否为 COMMON 符号 (*(COMMON))
    pub is_common: bool,
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
    Float(f64),
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
            memory_regions: Vec::new(),
            sections: Vec::new(),
            top_assignments: Vec::new(),
            trailing_commands: Vec::new(),
            input_files: Vec::new(),
            no_cross_refs: Vec::new(),
            insert_commands: Vec::new(),
            version_nodes: Vec::new(),
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
                Token::Memory => {
                    script.memory_regions = self.parse_memory()?;
                }
                Token::Sections => {
                    script.sections = self.parse_sections()?;
                }
                Token::Input | Token::Group => {
                    // INPUT(file1, file2, ...) or GROUP(file1, file2, ...)
                    self.next();
                    self.expect(&Token::LParen)?;
                    while !self.eat(&Token::RParen) {
                        match self.peek().clone() {
                            Token::Str(s) => {
                                self.next();
                                script.input_files.push(s);
                            }
                            Token::Ident(s) => {
                                self.next();
                                script.input_files.push(s);
                            }
                            Token::Comma => {
                                self.next();
                            }
                            Token::Eof => break,
                            _ => {
                                self.next(); // skip unknown
                            }
                        }
                    }
                }
                Token::NoCrossRefs => {
                    // NOCROSSREFS(section1, section2, ...)
                    self.next();
                    self.expect(&Token::LParen)?;
                    let mut sections = Vec::new();
                    while !self.eat(&Token::RParen) {
                        match self.peek().clone() {
                            Token::Ident(s) => {
                                self.next();
                                sections.push(s);
                            }
                            Token::Comma => {
                                self.next();
                            }
                            Token::Eof => break,
                            _ => {
                                self.next(); // skip unknown
                            }
                        }
                    }
                    script.no_cross_refs.push(sections);
                }
                Token::Insert => {
                    // INSERT AFTER section_name or INSERT BEFORE section_name
                    self.next();
                    let mut after = None;
                    let mut before = None;
                    if let Token::Ident(ref s) = self.peek().clone() {
                        if s == "AFTER" {
                            self.next();
                            if let Token::Ident(name) = self.next() {
                                after = Some(name);
                            }
                        } else if s == "BEFORE" {
                            self.next();
                            if let Token::Ident(name) = self.next() {
                                before = Some(name);
                            }
                        }
                    }
                    // The section name to insert
                    let section_name = if let Token::Ident(s) = self.next() {
                        s
                    } else {
                        String::new()
                    };
                    script.insert_commands.push(InsertCommand {
                        after,
                        before,
                        section_name,
                    });
                    self.eat(&Token::Semicolon);
                }
                Token::Version => {
                    script.version_nodes = self.parse_version()?;
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

    fn parse_memory(&mut self) -> Result<Vec<MemoryRegion>, String> {
        self.next(); // consume MEMORY
        self.expect(&Token::LBrace)?;
        let mut regions = Vec::new();
        while !self.eat(&Token::RBrace) {
            // Parse: name [(attr)] : ORIGIN = expr, LENGTH = expr
            let name = if let Token::Ident(s) = self.next() {
                s
            } else {
                return Err("expected memory region name".into());
            };
            
            // Optional attributes in parentheses
            let mut attributes = String::new();
            if self.eat(&Token::LParen) {
                while !self.eat(&Token::RParen) {
                    match self.peek().clone() {
                        Token::Ident(s) => {
                            self.next();
                            attributes.push_str(&s);
                        }
                        Token::Comma => {
                            self.next();
                        }
                        Token::Eof => break,
                        _ => {
                            self.next(); // skip unknown
                        }
                    }
                }
            }
            
            self.expect(&Token::Colon)?;
            
            // Parse ORIGIN = expr, LENGTH = expr
            let mut origin = Expr::Int(0);
            let mut length = Expr::Int(0);
            
            // ORIGIN = expr
            if let Token::Origin = self.peek().clone() {
                self.next();
                self.expect(&Token::Assign)?;
                origin = self.parse_expr()?;
            }
            self.eat(&Token::Comma);
            
            // LENGTH = expr
            if let Token::Length = self.peek().clone() {
                self.next();
                self.expect(&Token::Assign)?;
                length = self.parse_expr()?;
            }
            
            self.eat(&Token::Semicolon);
            regions.push(MemoryRegion { name, origin, length, attributes });
        }
        Ok(regions)
    }

    /// 解析 VERSION { ... } 块
    ///
    /// 语法:
    /// ```text
    /// VERSION {
    ///     version_id [(@parent_id)] {
    ///         global:
    ///             symbol;
    ///         local:
    ///             symbol;
    ///     };
    /// }
    /// ```
    fn parse_version(&mut self) -> Result<Vec<VersionNode>, String> {
        self.next(); // consume VERSION
        self.expect(&Token::LBrace)?;
        let mut nodes = Vec::new();

        while !self.eat(&Token::RBrace) {
            if self.peek() == &Token::Eof {
                return Err("unexpected EOF in VERSION block".into());
            }

            // 解析 version_id (可以是标识符，也可能包含 '.' 如 "GCC_3.0")
            let version_id = match self.peek().clone() {
                Token::Ident(s) => {
                    self.next();
                    s
                }
                _ => {
                    return Err(format!(
                        "expected version identifier, found {:?}",
                        self.peek()
                    ));
                }
            };

            // 可选的父版本: (@parent_id) 或 (parent_id)
            // GNU ld 语法用 @ 前缀表示父版本，但 lexer 会跳过 @ 字符
            let parent = if self.eat(&Token::LParen) {
                let parent_id = if let Token::Ident(s) = self.next() {
                    s
                } else {
                    return Err("expected parent version identifier".into());
                };
                self.expect(&Token::RParen)?;
                Some(parent_id)
            } else {
                None
            };

            // 版本体 { global: ...; local: ...; }
            self.expect(&Token::LBrace)?;
            let mut global_symbols = Vec::new();
            let mut local_symbols = Vec::new();

            while !self.eat(&Token::RBrace) {
                if self.peek() == &Token::Eof {
                    return Err("unexpected EOF in version node".into());
                }

                if self.peek() == &Token::Global {
                    self.next(); // consume 'global'
                    self.expect(&Token::Colon)?;
                    // 读取符号直到 '}' 或 'local:'
                    loop {
                        match self.peek().clone() {
                            Token::RBrace | Token::Eof => break,
                            Token::Local => break,
                            Token::Star => {
                                self.next();
                                global_symbols.push("*".to_string());
                                self.eat(&Token::Semicolon);
                            }
                            Token::Ident(s) => {
                                self.next();
                                global_symbols.push(s);
                                self.eat(&Token::Semicolon);
                            }
                            _ => {
                                self.next(); // skip unknown
                            }
                        }
                    }
                } else if self.peek() == &Token::Local {
                    self.next(); // consume 'local'
                    self.expect(&Token::Colon)?;
                    // 读取符号直到 '}'
                    loop {
                        match self.peek().clone() {
                            Token::RBrace | Token::Eof => break,
                            Token::Star => {
                                self.next();
                                local_symbols.push("*".to_string());
                                self.eat(&Token::Semicolon);
                            }
                            Token::Ident(s) => {
                                self.next();
                                local_symbols.push(s);
                                self.eat(&Token::Semicolon);
                            }
                            _ => {
                                self.next(); // skip unknown
                            }
                        }
                    }
                } else {
                    // 没有 global:/local: 前缀的符号，默认归入 global
                    if let Token::Ident(s) = self.peek().clone() {
                        self.next();
                        global_symbols.push(s);
                        self.eat(&Token::Semicolon);
                    } else if self.peek() == &Token::Star {
                        self.next();
                        global_symbols.push("*".to_string());
                        self.eat(&Token::Semicolon);
                    } else {
                        self.next(); // skip unknown
                    }
                }
            }

            self.eat(&Token::Semicolon);

            nodes.push(VersionNode {
                version_id,
                parent,
                global_symbols,
                local_symbols,
            });
        }

        Ok(nodes)
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

        // 节名（可能由多个 token 组成，如 .data..percpu = Ident(".data") + Ident("..percpu")）
        let (name, is_discard) = match self.next() {
            Token::Ident(s) if s == "/DISCARD/" => (s.clone(), true),
            Token::Ident(s) => {
                let mut full_name = s;
                // 合并以 .. 开头的续接 token
                while let Token::Ident(ref cont) = self.peek().clone() {
                    if cont.starts_with("..") {
                        full_name.push_str(cont);
                        self.next();
                    } else {
                        break;
                    }
                }
                (full_name, false)
            }
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
                exclude_files: Vec::new(),
                is_common: false,
            });
        }

        let mut patterns = Vec::new();
        let mut exclude_files = Vec::new();
        let mut is_common = false;
        
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
                Token::ExcludeFile => {
                    // EXCLUDE_FILE(file1, file2, ...)
                    self.next();
                    self.expect(&Token::LParen)?;
                    while !self.eat(&Token::RParen) {
                        match self.peek().clone() {
                            Token::Ident(s) => {
                                self.next();
                                exclude_files.push(s);
                            }
                            Token::Str(s) => {
                                self.next();
                                exclude_files.push(s);
                            }
                            Token::Comma => {
                                self.next();
                            }
                            Token::Eof => break,
                            _ => {
                                self.next(); // skip unknown
                            }
                        }
                    }
                }
                Token::Common => {
                    // *(COMMON) - special handling for COMMON symbols
                    self.next();
                    is_common = true;
                    // Add COMMON as a section pattern
                    patterns.push(SectionPattern {
                        pattern: "COMMON".to_string(),
                        sort: SortMode::None,
                    });
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
            exclude_files,
            is_common,
        })
    }

    fn parse_section_pattern(&mut self) -> Result<Option<SectionPattern>, String> {
        // 节名模式由多个 token 组成（如 .text.asan.* 或 .text..__x86.rethunk_untrain）
        // 当遇到一个新的以 . 开头的 Ident 且前面已经有内容时，表示新模式的开始
        let mut pattern = String::new();
        
        loop {
            match self.peek().clone() {
                Token::RParen | Token::Comma | Token::Eof => break,
                Token::Ident(s) => {
                    if !pattern.is_empty() && s.starts_with('.') {
                        let last_char = pattern.chars().last().unwrap_or(' ');
                        // Ident 以 .. 开头一定是节名续接（如 ..__x86）
                        if !s.starts_with("..") {
                            // Case 1: pattern 以字母/数字/]/_ 等结尾
                            // 新的 .xxx 一定是新模式
                            if last_char != '.' && last_char != '*' {
                                break;
                            }
                            // Case 2: pattern 以 * 结尾
                            // 如果新 Ident 以 .字母/数字/_ 开头，则是新模式
                            // 例如 .text.asan.* 后接 .text.tsan → 两个独立模式
                            if last_char == '*' {
                                let second = s.chars().nth(1);
                                if second.map_or(false, |c| c.is_ascii_alphanumeric() || c == '_') {
                                    break;
                                }
                            }
                            // Case 3: pattern 以 . 结尾，新 Ident 以 .字母 开头
                            // 比如 .text. + .data → 新模式
                            if last_char == '.' {
                                let second = s.chars().nth(1);
                                if second.map_or(false, |c| c.is_ascii_alphanumeric() || c == '_') {
                                    break;
                                }
                            }
                        }
                    }
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
            Token::Float(f) => {
                self.next();
                Ok(Expr::Float(f))
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
                // 合并以 .. 开头的续接 token（如 .data..percpu）
                let mut full_name = name;
                while let Token::Ident(ref cont) = self.peek().clone() {
                    if cont.starts_with("..") {
                        full_name.push_str(cont);
                        self.next();
                    } else {
                        break;
                    }
                }
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
                    Ok(Expr::Call(full_name, args))
                } else {
                    Ok(Expr::Symbol(full_name))
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

/// 表达式求值内部值类型，支持整数和浮点混合运算
#[derive(Debug, Clone, Copy)]
enum Value {
    I(u64),
    F(f64),
}

impl Value {
    fn as_u64(self) -> u64 {
        match self {
            Value::I(n) => n,
            Value::F(f) => f as u64,
        }
    }
    fn as_f64(self) -> f64 {
        match self {
            Value::I(n) => n as f64,
            Value::F(f) => f,
        }
    }
    fn is_float(self) -> bool {
        matches!(self, Value::F(_))
    }
}

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
    /// 内存区域表 (region name -> (origin, length))
    pub memory_regions: HashMap<String, (u64, u64)>,
}

impl EvalContext {
    pub fn new() -> Self {
        Self {
            location_counter: 0,
            symbols: HashMap::new(),
            section_addrs: HashMap::new(),
            section_sizes: HashMap::new(),
            memory_regions: HashMap::new(),
        }
    }

    pub fn eval(&self, expr: &Expr) -> Result<u64, String> {
        let val = self.eval_value(expr)?;
        Ok(val.as_u64())
    }

    /// 求值并返回 Value（支持 int/float 混合运算）
    fn eval_value(&self, expr: &Expr) -> Result<Value, String> {
        match expr {
            Expr::Int(n) => Ok(Value::I(*n)),
            Expr::Float(f) => Ok(Value::F(*f)),
            Expr::Symbol(name) => {
                self.symbols.get(name).map(|&v| Value::I(v)).ok_or_else(|| {
                    format!("undefined symbol '{}' in linker script expression", name)
                })
            }
            Expr::Dot => Ok(Value::I(self.location_counter)),
            Expr::BinaryOp(left, op, right) => {
                let l = self.eval_value(left)?;
                let r = self.eval_value(right)?;
                // 如果任一操作数为浮点，则提升为浮点运算
                if l.is_float() || r.is_float() {
                    let lf = l.as_f64();
                    let rf = r.as_f64();
                    Ok(Value::F(match op {
                        BinOp::Add => lf + rf,
                        BinOp::Sub => lf - rf,
                        BinOp::Mul => lf * rf,
                        BinOp::Div => {
                            if rf == 0.0 { 0.0 } else { lf / rf }
                        }
                        BinOp::Mod => {
                            if rf == 0.0 { 0.0 } else { lf % rf }
                        }
                        BinOp::Eq => if lf == rf { 1.0 } else { 0.0 },
                        BinOp::Ne => if lf != rf { 1.0 } else { 0.0 },
                        BinOp::Lt => if lf < rf { 1.0 } else { 0.0 },
                        BinOp::Gt => if lf > rf { 1.0 } else { 0.0 },
                        BinOp::Le => if lf <= rf { 1.0 } else { 0.0 },
                        BinOp::Ge => if lf >= rf { 1.0 } else { 0.0 },
                        // 位运算不适用于浮点，回退为整数
                        BinOp::BitAnd => Value::I(l.as_u64() & r.as_u64()).as_f64(),
                        BinOp::BitOr => Value::I(l.as_u64() | r.as_u64()).as_f64(),
                        BinOp::BitXor => Value::I(l.as_u64() ^ r.as_u64()).as_f64(),
                        BinOp::LShift => Value::I(l.as_u64() << r.as_u64()).as_f64(),
                        BinOp::RShift => Value::I(l.as_u64() >> r.as_u64()).as_f64(),
                        BinOp::LogAnd => {
                            if lf != 0.0 && rf != 0.0 { 1.0 } else { 0.0 }
                        }
                        BinOp::LogOr => {
                            if lf != 0.0 || rf != 0.0 { 1.0 } else { 0.0 }
                        }
                    }))
                } else {
                    let li = l.as_u64();
                    let ri = r.as_u64();
                    Ok(Value::I(match op {
                        BinOp::Add => li.wrapping_add(ri),
                        BinOp::Sub => li.wrapping_sub(ri),
                        BinOp::Mul => li.wrapping_mul(ri),
                        BinOp::Div => {
                            if ri == 0 { 0 } else { li / ri }
                        }
                        BinOp::Mod => {
                            if ri == 0 { 0 } else { li % ri }
                        }
                        BinOp::BitAnd => li & ri,
                        BinOp::BitOr => li | ri,
                        BinOp::BitXor => li ^ ri,
                        BinOp::LShift => li << ri,
                        BinOp::RShift => li >> ri,
                        BinOp::Eq => if li == ri { 1 } else { 0 },
                        BinOp::Ne => if li != ri { 1 } else { 0 },
                        BinOp::Lt => if li < ri { 1 } else { 0 },
                        BinOp::Gt => if li > ri { 1 } else { 0 },
                        BinOp::Le => if li <= ri { 1 } else { 0 },
                        BinOp::Ge => if li >= ri { 1 } else { 0 },
                        BinOp::LogAnd => if li != 0 && ri != 0 { 1 } else { 0 },
                        BinOp::LogOr => if li != 0 || ri != 0 { 1 } else { 0 },
                    }))
                }
            }
            Expr::UnaryOp(op, expr) => {
                let v = self.eval_value(expr)?;
                if v.is_float() {
                    let f = v.as_f64();
                    Ok(Value::F(match op {
                        UnaryOp::Neg => -f,
                        UnaryOp::Not => if f == 0.0 { 1.0 } else { 0.0 },
                        UnaryOp::BitNot => (!(f as u64)) as f64,
                    }))
                } else {
                    let i = v.as_u64();
                    Ok(Value::I(match op {
                        UnaryOp::Neg => i.wrapping_neg(),
                        UnaryOp::Not => if i == 0 { 1 } else { 0 },
                        UnaryOp::BitNot => !i,
                    }))
                }
            }
            Expr::Ternary(cond, then_e, else_e) => {
                let c = self.eval_value(cond)?;
                if c.as_u64() != 0 {
                    self.eval_value(then_e)
                } else {
                    self.eval_value(else_e)
                }
            }
            Expr::Call(name, args) => {
                match name.as_str() {
                    "ABSOLUTE" => {
                        if args.is_empty() {
                            return Err("ABSOLUTE() requires 1 argument".into());
                        }
                        self.eval_value(&args[0])
                    }
                    "ADDR" => {
                        if args.is_empty() {
                            return Err("ADDR() requires 1 argument".into());
                        }
                        if let Expr::Symbol(sec_name) = &args[0] {
                            self.section_addrs.get(sec_name).map(|&v| Value::I(v)).ok_or_else(|| {
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
                            Ok(Value::I(
                                self.section_sizes.get(sec_name).copied().unwrap_or(0)
                            ))
                        } else {
                            Err("SIZEOF() requires section name".into())
                        }
                    }
                    "ALIGN" => {
                        if args.is_empty() {
                            return Err("ALIGN() requires 1 argument".into());
                        }
                        let align = self.eval_value(&args[0])?.as_u64();
                        if align == 0 {
                            return Ok(Value::I(self.location_counter));
                        }
                        Ok(Value::I(
                            self.location_counter.wrapping_add(align - 1) & !(align - 1)
                        ))
                    }
                    "MAX" => {
                        if args.len() < 2 {
                            return Err("MAX() requires 2 arguments".into());
                        }
                        let a = self.eval_value(&args[0])?;
                        let b = self.eval_value(&args[1])?;
                        if a.is_float() || b.is_float() {
                            Ok(Value::F(a.as_f64().max(b.as_f64())))
                        } else {
                            Ok(Value::I(a.as_u64().max(b.as_u64())))
                        }
                    }
                    "MIN" => {
                        if args.len() < 2 {
                            return Err("MIN() requires 2 arguments".into());
                        }
                        let a = self.eval_value(&args[0])?;
                        let b = self.eval_value(&args[1])?;
                        if a.is_float() || b.is_float() {
                            Ok(Value::F(a.as_f64().min(b.as_f64())))
                        } else {
                            Ok(Value::I(a.as_u64().min(b.as_u64())))
                        }
                    }
                    "DEFINED" => {
                        if args.is_empty() {
                            return Err("DEFINED() requires 1 argument".into());
                        }
                        if let Expr::Symbol(name) = &args[0] {
                            Ok(Value::I(
                                if self.symbols.contains_key(name) { 1 } else { 0 }
                            ))
                        } else {
                            Ok(Value::I(0))
                        }
                    }
                    "ORIGIN" | "ORG" => {
                        if args.is_empty() {
                            return Err("ORIGIN() requires 1 argument".into());
                        }
                        if let Expr::Symbol(region_name) = &args[0] {
                            self.memory_regions.get(region_name)
                                .map(|(o, _)| Value::I(*o))
                                .ok_or_else(|| {
                                    format!(
                                        "ORIGIN: memory region '{}' not found",
                                        region_name
                                    )
                                })
                        } else {
                            Err("ORIGIN() requires memory region name".into())
                        }
                    }
                    "LENGTH" => {
                        if args.is_empty() {
                            return Err("LENGTH() requires 1 argument".into());
                        }
                        if let Expr::Symbol(region_name) = &args[0] {
                            self.memory_regions.get(region_name)
                                .map(|(_, l)| Value::I(*l))
                                .ok_or_else(|| {
                                    format!(
                                        "LENGTH: memory region '{}' not found",
                                        region_name
                                    )
                                })
                        } else {
                            Err("LENGTH() requires memory region name".into())
                        }
                    }
                    "NEXT" => {
                        if args.is_empty() {
                            return Err("NEXT() requires 1 argument".into());
                        }
                        let val = self.eval_value(&args[0])?.as_u64();
                        Ok(Value::I((val.wrapping_add(7)) & !7))
                    }
                    "LOG2CEIL" => {
                        if args.is_empty() {
                            return Err("LOG2CEIL() requires 1 argument".into());
                        }
                        let val = self.eval_value(&args[0])?.as_u64();
                        if val == 0 {
                            return Ok(Value::I(0));
                        }
                        let mut result = 0u64;
                        let mut v = val - 1;
                        while v > 0 {
                            result += 1;
                            v >>= 1;
                        }
                        Ok(Value::I(result))
                    }
                    "REGION_ALIAS" => {
                        if args.len() < 2 {
                            return Err("REGION_ALIAS() requires 2 arguments".into());
                        }
                        if let Expr::Symbol(region_name) = &args[1] {
                            self.memory_regions.get(region_name)
                                .map(|(o, _)| Value::I(*o))
                                .ok_or_else(|| {
                                    format!(
                                        "REGION_ALIAS: memory region '{}' not found",
                                        region_name
                                    )
                                })
                        } else {
                            Err(
                                "REGION_ALIAS() requires memory region name \
                                 as second argument".into()
                            )
                        }
                    }
                    other => Err(format!(
                        "unknown function '{}' in linker script", other
                    )),
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
    fn parse_section_pattern_splitting() {
        // 验证空格分隔的多个模式被正确拆分
        let text = r#"
            SECTIONS {
                .text : {
                    *(.text.asan.* .text.tsan.*)
                    *(.text..__x86.rethunk_untrain)
                    *(.text .text.[_0-9A-Za-df-rt-z]* .text.s[_0-9A-Za-su-z]*)
                    *(.text.hot .text.hot.*)
                    *(.text.unlikely .text.unlikely.*)
                }
            }
        "#;
        let script = LinkerScript::parse(text).unwrap();
        // 找到 .text 输出节
        let text_sec = script.sections.iter().find_map(|cmd| {
            if let SectionCommand::OutputSection(def) = cmd {
                if def.name == ".text" { Some(def) } else { None }
            } else { None }
        }).expect("should have .text output section");
        // 收集所有 InputSectionSpec
        let specs: Vec<&InputSectionSpec> = text_sec.body.iter().filter_map(|item| {
            if let SectionBodyItem::InputSection(spec) = item { Some(spec) } else { None }
        }).collect();
        assert_eq!(specs.len(), 5, "expected 5 input section specs");
        // .text.asan.* .text.tsan.* → 2 patterns
        assert_eq!(specs[0].section_patterns.len(), 2,
            "asan/tsan should be 2 patterns: {:?}",
            specs[0].section_patterns.iter().map(|p| &p.pattern).collect::<Vec<_>>());
        assert_eq!(specs[0].section_patterns[0].pattern, ".text.asan.*");
        assert_eq!(specs[0].section_patterns[1].pattern, ".text.tsan.*");
        // .text..__x86.rethunk_untrain → 1 pattern
        assert_eq!(specs[1].section_patterns.len(), 1);
        assert_eq!(specs[1].section_patterns[0].pattern, ".text..__x86.rethunk_untrain");
        // .text .text.[...]  .text.s[...] → 3 patterns
        assert_eq!(specs[2].section_patterns.len(), 3,
            "complex pattern should be 3: {:?}",
            specs[2].section_patterns.iter().map(|p| &p.pattern).collect::<Vec<_>>());
        assert_eq!(specs[2].section_patterns[0].pattern, ".text");
        // .text.hot .text.hot.* → 2 patterns
        assert_eq!(specs[3].section_patterns.len(), 2);
        assert_eq!(specs[3].section_patterns[0].pattern, ".text.hot");
        assert_eq!(specs[3].section_patterns[1].pattern, ".text.hot.*");
        // .text.unlikely .text.unlikely.* → 2 patterns
        assert_eq!(specs[4].section_patterns.len(), 2);
    }

    #[test]
    fn parse_data_percpu_pattern() {
        let text = r#"
            SECTIONS {
                .data..percpu : {
                    *(.data..percpu..page_aligned)
                    *(.data..percpu..hot.*)
                    *(.data..percpu)
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

    #[test]
    fn parse_discard_section() {
        // 测试 /DISCARD/ 节的解析
        let text = r#"
            SECTIONS {
                /DISCARD/ : {
                    *(.note.GNU-stack)
                    *(.note.gnu.property)
                }
                .text : { *(.text) }
            }
        "#;
        let script = LinkerScript::parse(text).unwrap();
        let discard_sec = script.sections.iter().find_map(|cmd| {
            if let SectionCommand::OutputSection(def) = cmd {
                if def.name == "/DISCARD/" { Some(def) } else { None }
            } else { None }
        }).expect("should have /DISCARD/ output section");
        assert!(discard_sec.is_discard);
    }

    #[test]
    fn parse_sections_block_symbol_assign() {
        // 测试 SECTIONS 块内的符号赋值（如 phys_startup_64）
        let text = r#"
            SECTIONS {
                . = 0xffffffff81000000;
                phys_startup_64 = ABSOLUTE(startup_64 - 0xffffffff80000000);
                .text : { *(.text) }
            }
        "#;
        let script = LinkerScript::parse(text).unwrap();
        // 找到 SECTIONS 块内的 SymbolAssign
        let sym_assign = script.sections.iter().find_map(|cmd| {
            if let SectionCommand::SymbolAssign(assign) = cmd {
                if assign.name == "phys_startup_64" { Some(assign) } else { None }
            } else { None }
        }).expect("should have phys_startup_64 assignment in SECTIONS");
        assert_eq!(sym_assign.name, "phys_startup_64");
    }

    #[test]
    fn extract_defined_symbols_from_script() {
        // 测试从链接脚本提取定义的符号名
        let text = r#"
            jiffies = jiffies_64;
            SECTIONS {
                phys_startup_64 = ABSOLUTE(startup_64 - 0xffffffff80000000);
                .text : {
                    _text = .;
                    *(.text)
                    _etext = .;
                }
            }
            PROVIDE(__hidden = 0);
        "#;
        let script = LinkerScript::parse(text).unwrap();
        let syms = script.extract_defined_symbols();
        assert!(syms.contains(&"jiffies".to_string()));
        assert!(syms.contains(&"phys_startup_64".to_string()));
        assert!(syms.contains(&"_text".to_string()));
        assert!(syms.contains(&"_etext".to_string()));
        assert!(syms.contains(&"__hidden".to_string()));
    }

    #[test]
    fn parse_memory_block() {
        let text = r#"
            MEMORY {
                rom (rx) : ORIGIN = 0x00000000, LENGTH = 256K
                ram (rwx) : ORIGIN = 0x20000000, LENGTH = 64K
            }
            SECTIONS {
                .text : { *(.text) }
            }
        "#;
        let script = LinkerScript::parse(text).unwrap();
        assert_eq!(script.memory_regions.len(), 2);
        assert_eq!(script.memory_regions[0].name, "rom");
        assert_eq!(script.memory_regions[1].name, "ram");
    }

    #[test]
    fn eval_origin_length() {
        let mut ctx = EvalContext::new();
        ctx.memory_regions.insert("rom".to_string(), (0x1000, 0x2000));
        
        let origin_expr = Expr::Call("ORIGIN".into(), vec![Expr::Symbol("rom".into())]);
        assert_eq!(ctx.eval(&origin_expr).unwrap(), 0x1000);
        
        let length_expr = Expr::Call("LENGTH".into(), vec![Expr::Symbol("rom".into())]);
        assert_eq!(ctx.eval(&length_expr).unwrap(), 0x2000);
    }

    #[test]
    fn eval_log2ceil() {
        let ctx = EvalContext::new();
        
        let expr1 = Expr::Call("LOG2CEIL".into(), vec![Expr::Int(1)]);
        assert_eq!(ctx.eval(&expr1).unwrap(), 0);
        
        let expr2 = Expr::Call("LOG2CEIL".into(), vec![Expr::Int(2)]);
        assert_eq!(ctx.eval(&expr2).unwrap(), 1);
        
        let expr3 = Expr::Call("LOG2CEIL".into(), vec![Expr::Int(5)]);
        assert_eq!(ctx.eval(&expr3).unwrap(), 3);
        
        let expr4 = Expr::Call("LOG2CEIL".into(), vec![Expr::Int(1024)]);
        assert_eq!(ctx.eval(&expr4).unwrap(), 10);
    }

    #[test]
    fn parse_exclude_file() {
        let text = r#"
            SECTIONS {
                .text : {
                    *(EXCLUDE_FILE("*special.o") .text .text.*)
                }
            }
        "#;
        let script = LinkerScript::parse(text).unwrap();
        let text_sec = script.sections.iter().find_map(|cmd| {
            if let SectionCommand::OutputSection(def) = cmd {
                if def.name == ".text" { Some(def) } else { None }
            } else { None }
        }).expect("should have .text output section");
        
        let spec = text_sec.body.iter().find_map(|item| {
            if let SectionBodyItem::InputSection(spec) = item { Some(spec) } else { None }
        }).expect("should have input section spec");
        
        assert_eq!(spec.exclude_files.len(), 1);
        assert_eq!(spec.exclude_files[0], "*special.o");
    }

    #[test]
    fn parse_common_section() {
        let text = r#"
            SECTIONS {
                .bss : {
                    *(COMMON)
                    *(.bss)
                }
            }
        "#;
        let script = LinkerScript::parse(text).unwrap();
        let bss_sec = script.sections.iter().find_map(|cmd| {
            if let SectionCommand::OutputSection(def) = cmd {
                if def.name == ".bss" { Some(def) } else { None }
            } else { None }
        }).expect("should have .bss output section");
        
        let spec = bss_sec.body.iter().find_map(|item| {
            if let SectionBodyItem::InputSection(spec) = item { Some(spec) } else { None }
        }).expect("should have input section spec");
        
        assert!(spec.is_common);
        assert!(spec.section_patterns.iter().any(|p| p.pattern == "COMMON"));
    }

    #[test]
    fn parse_input_group() {
        let text = r#"
            INPUT(file1.o, file2.o)
            GROUP(lib1.a, lib2.a)
            SECTIONS {
                .text : { *(.text) }
            }
        "#;
        let script = LinkerScript::parse(text).unwrap();
        assert_eq!(script.input_files.len(), 4);
        assert!(script.input_files.contains(&"file1.o".to_string()));
        assert!(script.input_files.contains(&"lib2.a".to_string()));
    }

    #[test]
    fn parse_nocrossrefs() {
        let text = r#"
            NOCROSSREFS(.text, .data)
            SECTIONS {
                .text : { *(.text) }
                .data : { *(.data) }
            }
        "#;
        let script = LinkerScript::parse(text).unwrap();
        assert_eq!(script.no_cross_refs.len(), 1);
        assert_eq!(script.no_cross_refs[0].len(), 2);
    }

    #[test]
    fn parse_version_block() {
        let text = r#"
            VERSION {
                GCC_3.0 {
                    global:
                        some_symbol;
                };
            }
            SECTIONS {
                .text : { *(.text) }
            }
        "#;
        let script = LinkerScript::parse(text).unwrap();
        assert!(!script.sections.is_empty());
        assert_eq!(script.version_nodes.len(), 1);
        assert_eq!(script.version_nodes[0].version_id, "GCC_3.0");
        assert!(script.version_nodes[0].parent.is_none());
        assert_eq!(
            script.version_nodes[0].global_symbols,
            vec!["some_symbol"]
        );
        assert!(script.version_nodes[0].local_symbols.is_empty());
    }

    #[test]
    fn parse_version_multiple_nodes() {
        let text = r#"
            VERSION {
                GLIBC_2.0 {
                    global:
                        printf;
                        malloc;
                };
                GLIBC_2.1 {
                    global:
                        memcpy;
                    local:
                        internal_func;
                };
            }
            SECTIONS { .text : { *(.text) } }
        "#;
        let script = LinkerScript::parse(text).unwrap();
        assert_eq!(script.version_nodes.len(), 2);
        assert_eq!(script.version_nodes[0].version_id, "GLIBC_2.0");
        assert_eq!(
            script.version_nodes[0].global_symbols,
            vec!["printf", "malloc"]
        );
        assert_eq!(script.version_nodes[1].version_id, "GLIBC_2.1");
        assert_eq!(
            script.version_nodes[1].global_symbols,
            vec!["memcpy"]
        );
        assert_eq!(
            script.version_nodes[1].local_symbols,
            vec!["internal_func"]
        );
    }

    #[test]
    fn parse_version_with_parent() {
        let text = r#"
            VERSION {
                BASE {
                    global: foo;
                };
                EXTENDED (@BASE) {
                    global: bar;
                };
            }
            SECTIONS { .text : { *(.text) } }
        "#;
        let script = LinkerScript::parse(text).unwrap();
        assert_eq!(script.version_nodes.len(), 2);
        assert!(script.version_nodes[0].parent.is_none());
        assert_eq!(
            script.version_nodes[1].parent.as_deref(),
            Some("BASE")
        );
    }

    #[test]
    fn parse_version_wildcard() {
        let text = r#"
            VERSION {
                MYLIB_1.0 {
                    global:
                        public_api;
                    local:
                        *;
                };
            }
            SECTIONS { .text : { *(.text) } }
        "#;
        let script = LinkerScript::parse(text).unwrap();
        assert_eq!(script.version_nodes.len(), 1);
        assert_eq!(
            script.version_nodes[0].global_symbols,
            vec!["public_api"]
        );
        assert_eq!(
            script.version_nodes[0].local_symbols,
            vec!["*"]
        );
    }

    #[test]
    fn parse_version_empty_block() {
        let text = r#"
            VERSION {
            }
            SECTIONS { .text : { *(.text) } }
        "#;
        let script = LinkerScript::parse(text).unwrap();
        assert!(script.version_nodes.is_empty());
    }

    #[test]
    fn parse_version_no_labels() {
        // Symbols without global:/local: prefix default to global
        let text = r#"
            VERSION {
                MYLIB_1.0 {
                    sym_a;
                    sym_b;
                };
            }
            SECTIONS { .text : { *(.text) } }
        "#;
        let script = LinkerScript::parse(text).unwrap();
        assert_eq!(script.version_nodes.len(), 1);
        assert_eq!(
            script.version_nodes[0].global_symbols,
            vec!["sym_a", "sym_b"]
        );
        assert!(script.version_nodes[0].local_symbols.is_empty());
    }

    #[test]
    fn version_extract_version_info() {
        let text = r#"
            VERSION {
                V1 {
                    global: foo; bar;
                    local: *;
                };
                V2 {
                    global: baz;
                };
            }
            SECTIONS { .text : { *(.text) } }
        "#;
        let script = LinkerScript::parse(text).unwrap();
        let info = script.extract_version_info();
        assert_eq!(info.len(), 2);
        assert_eq!(info[0].0, "V1");
        assert_eq!(info[0].1, &["foo", "bar"]);
        assert_eq!(info[0].2, &["*"]);
        assert_eq!(info[1].0, "V2");
        assert_eq!(info[1].1, &["baz"]);
        assert!(info[1].2.is_empty());
    }

    #[test]
    fn version_symbols_in_extract_defined() {
        let text = r#"
            VERSION {
                V1 {
                    global: exported_sym;
                    local: hidden_sym;
                };
            }
            SECTIONS { .text : { *(.text) } }
        "#;
        let script = LinkerScript::parse(text).unwrap();
        let syms = script.extract_defined_symbols();
        // global symbols should be in the defined set
        assert!(syms.contains(&"exported_sym".to_string()));
        // local symbols should NOT be in the defined set
        assert!(!syms.contains(&"hidden_sym".to_string()));
    }

    #[test]
    fn test_sort_by_name_execution() {
        // 测试 SORT_BY_NAME 执行时按节名排序
        use super::super::object::{FlatSection, InputObject};
        
        // 创建测试数据：多个 .text.* 节，名称乱序
        let mut bundle = InputBundle {
            objects: vec![InputObject {
                name: "test.o".to_string(),
                machine: 62, // EM_X86_64
                sections: vec![],
                symbols: vec![],
                relocations: vec![],
            }],
            symbols: vec![],
            relocations: vec![],
            sections: vec![],
            machine: 62, // EM_X86_64
        };
        
        // 添加乱序的 .text.* 节
        for name in &[".text.zzz", ".text.aaa", ".text.mmm", ".text.bbb"] {
            bundle.sections.push(FlatSection {
                name: name.to_string(),
                ty: SHT_PROGBITS,
                flags: 0x6, // SHF_ALLOC | SHF_EXECINSTR
                align: 1,
                data: vec![0x90; 16], // NOP sled
                size: 16,
                input_obj: 0,
                orig_index: 0,
            });
        }
        
        // 创建带 SORT 的 spec
        let spec = InputSectionSpec {
            file_pattern: "*".to_string(),
            section_patterns: vec![
                SectionPattern {
                    pattern: ".text.*".to_string(),
                    sort: SortMode::ByName,
                },
            ],
            keep: false,
            exclude_files: vec![],
            is_common: false,
        };
        
        let used = std::collections::HashSet::new();
        let ctx = EvalContext::new();
        let matches = match_input_sections(&spec, &bundle, &ctx, &used);
        
        // 验证按名称排序
        assert_eq!(matches.len(), 4);
        let names: Vec<&str> = matches.iter()
            .map(|m| bundle.sections[m.input_index as usize].name.as_str())
            .collect();
        assert_eq!(names, vec![".text.aaa", ".text.bbb", ".text.mmm", ".text.zzz"]);
    }

    #[test]
    fn test_sort_by_alignment_execution() {
        // 测试 SORT_BY_ALIGNMENT 执行时按对齐值降序排序
        use super::super::object::{FlatSection, InputObject};
        
        let mut bundle = InputBundle {
            objects: vec![InputObject {
                name: "test.o".to_string(),
                machine: 62,
                sections: vec![],
                symbols: vec![],
                relocations: vec![],
            }],
            symbols: vec![],
            relocations: vec![],
            sections: vec![],
            machine: 62,
        };
        
        // 添加不同对齐值的节
        let test_data = vec![
            (".data.1", 1u64),
            (".data.2", 16u64),
            (".data.3", 4u64),
            (".data.4", 8u64),
        ];
        
        for (name, align) in test_data {
            bundle.sections.push(FlatSection {
                name: name.to_string(),
                ty: SHT_PROGBITS,
                flags: 0x3, // SHF_ALLOC | SHF_WRITE
                align,
                data: vec![0; 32],
                size: 32,
                input_obj: 0,
                orig_index: 0,
            });
        }
        
        let spec = InputSectionSpec {
            file_pattern: "*".to_string(),
            section_patterns: vec![
                SectionPattern {
                    pattern: ".data.*".to_string(),
                    sort: SortMode::ByAlignment,
                },
            ],
            keep: false,
            exclude_files: vec![],
            is_common: false,
        };
        
        let used = std::collections::HashSet::new();
        let ctx = EvalContext::new();
        let matches = match_input_sections(&spec, &bundle, &ctx, &used);
        
        // 验证按对齐值降序排序
        assert_eq!(matches.len(), 4);
        let aligns: Vec<u64> = matches.iter()
            .map(|m| bundle.sections[m.input_index as usize].align)
            .collect();
        assert_eq!(aligns, vec![16, 8, 4, 1]);
    }

    #[test]
    fn test_mixed_sort_and_nonsort_patterns() {
        // 测试混合模式：SORT 包裹的模式排序，未包裹的保持文件顺序
        use super::super::object::{FlatSection, InputObject};
        
        let mut bundle = InputBundle {
            objects: vec![InputObject {
                name: "test.o".to_string(),
                machine: 62,
                sections: vec![],
                symbols: vec![],
                relocations: vec![],
            }],
            symbols: vec![],
            relocations: vec![],
            sections: vec![],
            machine: 62,
        };
        
        // 添加节：.text.* 应该排序，.data.* 保持文件顺序
        let test_sections = vec![
            (".text.zzz", 1u64),
            (".data.first", 1u64),
            (".text.aaa", 1u64),
            (".data.second", 1u64),
            (".text.mmm", 1u64),
        ];
        
        for (name, align) in test_sections {
            bundle.sections.push(FlatSection {
                name: name.to_string(),
                ty: SHT_PROGBITS,
                flags: 0x6,
                align,
                data: vec![0; 16],
                size: 16,
                input_obj: 0,
                orig_index: 0,
            });
        }
        
        // 第一个模式 SORT(.text.*)，第二个模式 .data.*（无 SORT）
        let spec = InputSectionSpec {
            file_pattern: "*".to_string(),
            section_patterns: vec![
                SectionPattern {
                    pattern: ".text.*".to_string(),
                    sort: SortMode::ByName,
                },
                SectionPattern {
                    pattern: ".data.*".to_string(),
                    sort: SortMode::None,
                },
            ],
            keep: false,
            exclude_files: vec![],
            is_common: false,
        };
        
        let used = std::collections::HashSet::new();
        let ctx = EvalContext::new();
        let matches = match_input_sections(&spec, &bundle, &ctx, &used);
        
        // 验证：.text.* 按名称排序，.data.* 保持文件顺序
        assert_eq!(matches.len(), 5);
        let names: Vec<&str> = matches.iter()
            .map(|m| bundle.sections[m.input_index as usize].name.as_str())
            .collect();
        
        // .text.* 应该排序在前
        assert_eq!(&names[0..3], &[".text.aaa", ".text.mmm", ".text.zzz"]);
        // .data.* 保持文件顺序
        assert_eq!(&names[3..5], &[".data.first", ".data.second"]);
    }

    #[test]
    fn eval_float_literal() {
        // 测试浮点字面量解析和求值
        let ctx = EvalContext::new();
        // 简单浮点字面量
        let expr = Expr::Float(3.14);
        assert_eq!(ctx.eval(&expr).unwrap(), 3);

        let expr = Expr::Float(0.5);
        assert_eq!(ctx.eval(&expr).unwrap(), 0);

        let expr = Expr::Float(100.0);
        assert_eq!(ctx.eval(&expr).unwrap(), 100);
    }

    #[test]
    fn eval_float_arithmetic() {
        // 测试浮点算术运算
        let ctx = EvalContext::new();

        // 浮点加法: 1.5 + 2.5 = 4.0 -> 4
        let expr = Expr::BinaryOp(
            Box::new(Expr::Float(1.5)),
            BinOp::Add,
            Box::new(Expr::Float(2.5)),
        );
        assert_eq!(ctx.eval(&expr).unwrap(), 4);

        // 浮点乘法: 3.0 * 2.0 = 6.0 -> 6
        let expr = Expr::BinaryOp(
            Box::new(Expr::Float(3.0)),
            BinOp::Mul,
            Box::new(Expr::Float(2.0)),
        );
        assert_eq!(ctx.eval(&expr).unwrap(), 6);

        // 浮点除法: 7.0 / 2.0 = 3.5 -> 3
        let expr = Expr::BinaryOp(
            Box::new(Expr::Float(7.0)),
            BinOp::Div,
            Box::new(Expr::Float(2.0)),
        );
        assert_eq!(ctx.eval(&expr).unwrap(), 3);
    }

    #[test]
    fn eval_mixed_int_float() {
        // 测试整数和浮点混合运算
        let ctx = EvalContext::new();

        // int + float: 10 + 0.5 = 10.5 -> 10
        let expr = Expr::BinaryOp(
            Box::new(Expr::Int(10)),
            BinOp::Add,
            Box::new(Expr::Float(0.5)),
        );
        assert_eq!(ctx.eval(&expr).unwrap(), 10);

        // float * int: 2.5 * 4 = 10.0 -> 10
        let expr = Expr::BinaryOp(
            Box::new(Expr::Float(2.5)),
            BinOp::Mul,
            Box::new(Expr::Int(4)),
        );
        assert_eq!(ctx.eval(&expr).unwrap(), 10);

        // int / float: 10 / 3.0 = 3.333 -> 3
        let expr = Expr::BinaryOp(
            Box::new(Expr::Int(10)),
            BinOp::Div,
            Box::new(Expr::Float(3.0)),
        );
        assert_eq!(ctx.eval(&expr).unwrap(), 3);
    }

    #[test]
    fn parse_float_in_script() {
        // 测试链接器脚本中浮点表达式的解析和求值
        let text = r#"
            SECTIONS {
                . = 0x1000 + 1.5 * 2;
                .text : { *(.text) }
            }
        "#;
        let script = LinkerScript::parse(text).unwrap();
        assert!(!script.sections.is_empty());
    }

    #[test]
    fn lex_float_with_exponent() {
        // 测试指数形式的浮点字面量
        let text = "1e3";
        let mut lexer = Lexer::new(text);
        let tok = lexer.next_token();
        match tok {
            Token::Float(f) => assert!((f - 1000.0).abs() < 1e-10),
            _ => panic!("expected Float token, got {:?}", tok),
        }

        let text = "2.5e2";
        let mut lexer = Lexer::new(text);
        let tok = lexer.next_token();
        match tok {
            Token::Float(f) => assert!((f - 250.0).abs() < 1e-10),
            _ => panic!("expected Float token, got {:?}", tok),
        }

        let text = "1.0E-3";
        let mut lexer = Lexer::new(text);
        let tok = lexer.next_token();
        match tok {
            Token::Float(f) => assert!((f - 0.001).abs() < 1e-10),
            _ => panic!("expected Float token, got {:?}", tok),
        }
    }

    #[test]
    fn lex_float_decimal() {
        // 测试小数形式的浮点字面量
        let text = "3.14";
        let mut lexer = Lexer::new(text);
        let tok = lexer.next_token();
        match tok {
            Token::Float(f) => assert!((f - 3.14).abs() < 1e-10),
            _ => panic!("expected Float token, got {:?}", tok),
        }

        let text = "0.5";
        let mut lexer = Lexer::new(text);
        let tok = lexer.next_token();
        match tok {
            Token::Float(f) => assert!((f - 0.5).abs() < 1e-10),
            _ => panic!("expected Float token, got {:?}", tok),
        }
    }

    #[test]
    fn lex_int_not_float() {
        // 确保整数不被误解析为浮点
        let text = "42";
        let mut lexer = Lexer::new(text);
        let tok = lexer.next_token();
        assert_eq!(tok, Token::Int(42));

        // 十六进制不被误解析为浮点
        let text = "0x1A";
        let mut lexer = Lexer::new(text);
        let tok = lexer.next_token();
        assert_eq!(tok, Token::Int(0x1A));
    }

    #[test]
    fn execute_common_symbol_placement() {
        // 测试 *(COMMON) 执行时正确放置 COMMON 符号
        use super::super::object::{FlatSection, InputObject};
        use super::super::resolve::ResolvedSymbol;
        use crate::codegen::elf::{STB_GLOBAL, STT_OBJECT};

        let bundle = InputBundle {
            objects: vec![InputObject {
                name: "test.o".to_string(),
                machine: 62,
                sections: vec![],
                symbols: vec![],
                relocations: vec![],
            }],
            symbols: vec![],
            relocations: vec![],
            sections: vec![
                FlatSection {
                    name: ".text".to_string(),
                    ty: SHT_PROGBITS,
                    flags: 0x6, // SHF_ALLOC | SHF_EXECINSTR
                    align: 1,
                    data: vec![0x90; 16],
                    size: 16,
                    input_obj: 0,
                    orig_index: 0,
                },
            ],
            machine: 62,
        };

        // 创建已解析符号：一个 COMMON 符号和一个普通符号
        let resolved = vec![
            ResolvedSymbol {
                name: "common_var".to_string(),
                value: 0,
                size: 32,
                bind: STB_GLOBAL,
                ty: STT_OBJECT,
                visibility: 0,
                section: 0,
                vaddr: 0,
                defined: true,
                is_common: true,
                needs_plt: false,
                needs_got: false,
                source_lib: None,
            },
            ResolvedSymbol {
                name: "normal_var".to_string(),
                value: 0,
                size: 8,
                bind: STB_GLOBAL,
                ty: STT_OBJECT,
                visibility: 0,
                section: 1, // .text
                vaddr: 0,
                defined: true,
                is_common: false,
                needs_plt: false,
                needs_got: false,
                source_lib: None,
            },
        ];

        // 解析包含 *(COMMON) 的链接脚本
        let text = r#"
            SECTIONS {
                . = 0x1000;
                .text : { *(.text) }
                .bss : {
                    *(COMMON)
                    *(.bss)
                }
            }
        "#;
        let script = LinkerScript::parse(text).unwrap();
        let result = execute_script(&script, &bundle, &resolved).unwrap();

        // 验证 COMMON 符号被放置
        assert_eq!(result.common_placements.len(), 1);
        let cp = &result.common_placements[0];
        assert_eq!(cp.resolved_idx, 0); // common_var 是 resolved[0]
        assert_eq!(cp.offset, 0); // 第一个 COMMON 符号偏移为 0

        // 验证 .bss 输出节存在且类型为 NOBITS
        let bss_sec = result.sections.iter()
            .find(|s| s.name == ".bss")
            .expect("should have .bss section");
        assert_eq!(bss_sec.ty, SHT_NOBITS);
        assert!(bss_sec.flags & SHF_WRITE != 0);
        assert!(bss_sec.flags & SHF_ALLOC != 0);
        // .bss 大小应包含 COMMON 符号的 32 字节
        assert!(bss_sec.size >= 32);
    }

    #[test]
    fn parse_none_phdr() {
        // 测试 :NONE 解析为特殊 PHDR 标记
        let text = r#"
            PHDRS {
                text PT_LOAD FLAGS(5);
            }
            SECTIONS {
                .text : { *(.text) } :text
                .debug_info : { *(.debug_info) } :NONE
            }
        "#;
        let script = LinkerScript::parse(text).unwrap();
        // 找到 .debug_info 输出节
        let debug_sec = script.sections.iter().find_map(|cmd| {
            if let SectionCommand::OutputSection(def) = cmd {
                if def.name == ".debug_info" { Some(def) } else { None }
            } else { None }
        }).expect("should have .debug_info section");
        // :NONE 应被解析为 phdrs 列表中的 "NONE"
        assert!(debug_sec.phdrs.contains(&"NONE".to_string()),
            "phdrs should contain NONE: {:?}", debug_sec.phdrs);
    }

    #[test]
    fn execute_none_phdr_excludes_from_load() {
        // 测试 :NONE 节在执行后标记为 is_none_phdr
        use super::super::object::{FlatSection, InputObject};
        use super::super::resolve::ResolvedSymbol;

        let bundle = InputBundle {
            objects: vec![InputObject {
                name: "test.o".to_string(),
                machine: 62,
                sections: vec![],
                symbols: vec![],
                relocations: vec![],
            }],
            symbols: vec![],
            relocations: vec![],
            sections: vec![
                FlatSection {
                    name: ".text".to_string(),
                    ty: SHT_PROGBITS,
                    flags: 0x6,
                    align: 1,
                    data: vec![0xc3; 8],
                    size: 8,
                    input_obj: 0,
                    orig_index: 0,
                },
                FlatSection {
                    name: ".debug_info".to_string(),
                    ty: SHT_PROGBITS,
                    flags: 0,
                    align: 1,
                    data: vec![0; 32],
                    size: 32,
                    input_obj: 0,
                    orig_index: 1,
                },
            ],
            machine: 62,
        };

        let resolved: Vec<ResolvedSymbol> = vec![];

        let text = r#"
            PHDRS {
                text PT_LOAD FLAGS(5);
            }
            SECTIONS {
                . = 0x1000;
                .text : { *(.text) } :text
                .debug_info : { *(.debug_info) } :NONE
            }
        "#;
        let script = LinkerScript::parse(text).unwrap();
        let result = execute_script(&script, &bundle, &resolved).unwrap();

        // .text 节不应是 :NONE
        let text_sec = result.sections.iter()
            .find(|s| s.name == ".text")
            .expect("should have .text");
        assert!(!text_sec.is_none_phdr, ".text should not be :NONE");

        // .debug_info 节应标记为 :NONE
        let debug_sec = result.sections.iter()
            .find(|s| s.name == ".debug_info")
            .expect("should have .debug_info");
        assert!(debug_sec.is_none_phdr,
            ".debug_info should be :NONE");
    }
}

// ─── Linker Script Executor ─────────────────────────────────────────────────

use super::object::InputBundle;
use super::resolve::ResolvedSymbol;
use crate::codegen::elf::{SHT_NOBITS, SHT_PROGBITS, SHF_ALLOC, SHF_WRITE};

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
    /// COMMON 符号放置信息（*(COMMON) 模式处理结果）
    pub common_placements: Vec<CommonPlacement>,
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
    /// 是否为 :NONE（不属于任何程序头/LOAD 段）
    pub is_none_phdr: bool,
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

/// COMMON 符号放置信息
///
/// COMMON 符号没有对应的输入节，它们在 *(COMMON) 模式处理时
/// 被直接分配到输出节中（类似 .bss 的 NOBITS 空间）。
pub struct CommonPlacement {
    /// 符号在 resolved 数组中的索引
    pub resolved_idx: usize,
    /// 输出节的 VMA
    pub output_vma: u64,
    /// 符号在输出节内的偏移
    pub offset: u64,
}

/// 执行链接脚本，生成布局结果
pub fn execute_script(
    script: &LinkerScript,
    bundle: &InputBundle,
    resolved: &[ResolvedSymbol],
) -> Result<ScriptLayoutResult, String> {
    let mut ctx = EvalContext::new();
    let mut result = ScriptLayoutResult {
        sections: Vec::new(),
        symbols: Vec::new(),
        entry: None,
        assert_failures: Vec::new(),
        common_placements: Vec::new(),
    };
    // 跟踪已匹配的输入节，避免重复匹配
    let mut used_sections: std::collections::HashSet<u32> = std::collections::HashSet::new();

    // 0. 初始化内存区域
    for region in &script.memory_regions {
        let origin = ctx.eval(&region.origin).unwrap_or(0);
        let length = ctx.eval(&region.length).unwrap_or(0);
        ctx.memory_regions.insert(region.name.clone(), (origin, length));
    }

    // 0. 将已解析的输入符号加入 ctx.symbols（值为节内偏移，稍后会更新）
    for sym in resolved {
        if !sym.name.is_empty() && sym.section > 0 {
            ctx.symbols.insert(sym.name.clone(), sym.value);
        }
    }

    // 1. 处理顶层符号赋值（第一遍：用节相对值）
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
                let out_sec = execute_output_section(
                    sec_def, bundle, &mut ctx, &mut used_sections, resolved,
                    &mut result.common_placements,
                )?;
                if !out_sec.is_discard {
                    ctx.section_addrs.insert(out_sec.name.clone(), out_sec.vma);
                    ctx.section_sizes.insert(out_sec.name.clone(), out_sec.size);
                    ctx.location_counter = out_sec.vma.wrapping_add(out_sec.size);
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

    // 2.5 将 COMMON 符号加入 ctx.symbols（*(COMMON) 处理后已有放置信息）
    for cp in &result.common_placements {
        let sym = &resolved[cp.resolved_idx];
        let abs_addr = cp.output_vma.wrapping_add(cp.offset);
        ctx.symbols.insert(sym.name.clone(), abs_addr);
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

    // 3.5 更新输入符号为绝对地址
    // 构建 flat section index → (output VMA + offset within output) 的映射
    let mut sec_vma_map: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
    for out_sec in &result.sections {
        for m in &out_sec.input_sections {
            sec_vma_map.insert(m.input_index, out_sec.vma.wrapping_add(m.offset));
        }
    }
    // 用绝对地址更新输入符号
    for sym in resolved {
        if sym.section > 0 && !sym.name.is_empty() {
            if let Some(&sec_vma) = sec_vma_map.get(&sym.section) {
                let abs_addr = sec_vma.wrapping_add(sym.value);
                ctx.symbols.insert(sym.name.clone(), abs_addr);
            }
        }
    }

    // 3.6 重新求值顶层符号赋值（用绝对地址）
    for (i, assign) in script.top_assignments.iter().enumerate() {
        let value = ctx.eval(&assign.expr).unwrap_or(0);
        ctx.symbols.insert(assign.name.clone(), value);
        if i < result.symbols.len() {
            result.symbols[i].value = value;
        }
    }

    // 3.7 重新求值 SECTIONS 块内的符号赋值（用绝对地址）
    let mut sym_idx = script.top_assignments.len();
    for cmd in &script.sections {
        if let SectionCommand::SymbolAssign(assign) = cmd {
            let value = ctx.eval(&assign.expr).unwrap_or(0);
            ctx.symbols.insert(assign.name.clone(), value);
            if sym_idx < result.symbols.len() {
                result.symbols[sym_idx].value = value;
            }
            sym_idx += 1;
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
    used_sections: &mut std::collections::HashSet<u32>,
    resolved: &[ResolvedSymbol],
    common_placements: &mut Vec<CommonPlacement>,
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
        is_none_phdr: sec_def.phdrs.iter().any(|p| p == "NONE"),
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
        ctx.location_counter = ctx.location_counter.wrapping_add(out_sec.align - 1) & !(out_sec.align - 1);
        out_sec.vma = ctx.location_counter;
    }

    // 处理节体
    let mut current_offset: u64 = 0;
    for item in &sec_def.body {
        match item {
            SectionBodyItem::LocationAssign(expr) => {
                let val = ctx.eval(expr).unwrap_or(0);
                // val 可能是绝对地址（如 ALIGN 返回的）或相对偏移
                // 如果 val >= out_sec.vma，视为绝对地址
                let new_offset = if val >= out_sec.vma {
                    ctx.location_counter = val;
                    val.wrapping_sub(out_sec.vma)
                } else {
                    // 相对偏移
                    ctx.location_counter = out_sec.vma.wrapping_add(val);
                    val
                };
                // 如果 new_offset 超出当前数据长度，添加填充字节
                // 这样 ALIGN 等指令产生的空隙会被填零
                if new_offset > out_sec.data.len() as u64 {
                    let pad = new_offset - out_sec.data.len() as u64;
                    if pad < 0x10000000 {
                        // 限制最大填充为 256MB，防止错误
                        out_sec.data.extend(std::iter::repeat_n(0, pad as usize));
                    }
                }
                // current_offset 不能回退到已有数据之前
                current_offset = new_offset.max(out_sec.data.len() as u64);
            }
            SectionBodyItem::SymbolAssign(assign) => {
                let val = ctx.eval(&assign.expr).unwrap_or(0);
                ctx.symbols.insert(assign.name.clone(), val);
            }
            SectionBodyItem::InputSection(spec) => {
                // 匹配输入节
                let matches = match_input_sections(spec, bundle, ctx, used_sections);
                for m in matches {
                    // 标记为已使用
                    used_sections.insert(m.input_index);
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
                        if pad < 0x10000000 {
                            out_sec.data.extend(std::iter::repeat_n(0, pad as usize));
                        }
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

                // *(COMMON) 特殊处理: 收集 COMMON 符号并分配 NOBITS 空间
                // COMMON 符号没有对应的输入节，它们直接来自符号表
                if spec.is_common {
                    out_sec.ty = SHT_NOBITS;
                    out_sec.flags |= SHF_WRITE | SHF_ALLOC;
                    for (idx, sym) in resolved.iter().enumerate() {
                        if !sym.is_common || sym.name.is_empty() {
                            continue;
                        }
                        let align = sym.size.max(1);
                        let aligned = (current_offset + align - 1) & !(align - 1);
                        current_offset = aligned + sym.size;
                        common_placements.push(CommonPlacement {
                            resolved_idx: idx,
                            output_vma: out_sec.vma,
                            offset: aligned,
                        });
                    }
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

    // size 应该是 current_offset 和实际数据长度的最大值
    // 因为 . = ALIGN(...) 等位置赋值可能重置 current_offset，
    // 但之前累积的数据仍然在 out_sec.data 中
    out_sec.size = current_offset.max(out_sec.data.len() as u64);
    ctx.location_counter = out_sec.vma.wrapping_add(out_sec.size);
    Ok(out_sec)
}

/// 匹配输入节到输出节规范
///
/// 按模式顺序匹配输入节，每个模式独立排序后追加到结果中。
/// 这确保 SORT() 包裹的模式其匹配节按指定方式排序，
/// 而未包裹的节保持文件出现顺序。
fn match_input_sections(
    spec: &InputSectionSpec,
    bundle: &InputBundle,
    _ctx: &EvalContext,
    used_sections: &std::collections::HashSet<u32>,
) -> Vec<InputSectionMatch> {
    let mut matches = Vec::new();
    // 跟踪本次调用中已匹配的节（跨模式去重）
    let mut locally_used: std::collections::HashSet<u32> = std::collections::HashSet::new();

    // 按模式顺序处理：每个模式独立匹配、排序、追加
    for pat in &spec.section_patterns {
        let mut pat_matches = Vec::new();

        for (i, in_sec) in bundle.sections.iter().enumerate() {
            // 跳过空节名和特殊节
            if in_sec.name.is_empty()
                || in_sec.name == ".symtab"
                || in_sec.name == ".strtab"
                || in_sec.name == ".shstrtab"
                || in_sec.name.starts_with(".rela")
                || in_sec.name.starts_with(".rel")
            {
                continue;
            }

            // 跳过已被之前模式或之前 spec 匹配的节
            if used_sections.contains(&(i as u32)) || locally_used.contains(&(i as u32)) {
                continue;
            }

            // 检查是否被 EXCLUDE_FILE 排除
            let file_name = &bundle.objects[in_sec.input_obj].name;
            let excluded = spec.exclude_files.iter().any(|ex| {
                section_name_matches(file_name, ex)
            });
            if excluded {
                continue;
            }

            // 检查是否匹配当前模式
            if section_name_matches(&in_sec.name, &pat.pattern) {
                pat_matches.push(InputSectionMatch {
                    input_index: i as u32,
                    pattern: pat.pattern.clone(),
                    offset: 0,
                });
            }
        }

        // 根据当前模式的 sort mode 排序
        match pat.sort {
            SortMode::ByName => {
                pat_matches.sort_by(|a, b| {
                    let name_a = &bundle.sections[a.input_index as usize].name;
                    let name_b = &bundle.sections[b.input_index as usize].name;
                    name_a.cmp(name_b)
                });
            }
            SortMode::ByAlignment => {
                pat_matches.sort_by(|a, b| {
                    let align_a = bundle.sections[a.input_index as usize].align;
                    let align_b = bundle.sections[b.input_index as usize].align;
                    align_b.cmp(&align_a) // 降序，对齐值大的在前
                });
            }
            SortMode::None => {
                // 保持文件出现顺序（已是遍历顺序）
            }
        }

        // 标记为已匹配并追加到结果
        for m in &pat_matches {
            locally_used.insert(m.input_index);
        }
        matches.extend(pat_matches);
    }

    matches
}

/// 检查节名是否匹配模式（支持通配符 * ? 和字符类 [...]）
fn section_name_matches(name: &str, pattern: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') && !pattern.contains('?') && !pattern.contains('[') {
        return name == pattern;
    }

    let name_chars: Vec<char> = name.chars().collect();
    let pattern_chars: Vec<char> = pattern.chars().collect();
    glob_match(&name_chars, &pattern_chars)
}

/// 递归通配符匹配，支持 * ? [...]
fn glob_match(name: &[char], pattern: &[char]) -> bool {
    let mut ni = 0;
    let mut pi = 0;
    let mut star_pi: Option<usize> = None;
    let mut star_ni: usize = 0;

    while ni < name.len() {
        if pi < pattern.len() && pattern[pi] == '?' {
            ni += 1;
            pi += 1;
        } else if pi < pattern.len() && pattern[pi] == '[' {
            // 字符类匹配
            if let Some((matched, end_pi)) = match_char_class(&name[ni], &pattern[pi..]) {
                if matched {
                    ni += 1;
                    pi = end_pi;
                } else if let Some(si) = star_pi {
                    pi = si + 1;
                    star_ni += 1;
                    ni = star_ni;
                } else {
                    return false;
                }
            } else if let Some(si) = star_pi {
                pi = si + 1;
                star_ni += 1;
                ni = star_ni;
            } else {
                return false;
            }
        } else if pi < pattern.len() && pattern[pi] == '*' {
            star_pi = Some(pi);
            star_ni = ni;
            pi += 1;
        } else if pi < pattern.len() && pattern[pi] == name[ni] {
            ni += 1;
            pi += 1;
        } else if let Some(si) = star_pi {
            pi = si + 1;
            star_ni += 1;
            ni = star_ni;
        } else {
            return false;
        }
    }

    while pi < pattern.len() && pattern[pi] == '*' {
        pi += 1;
    }

    pi == pattern.len()
}

/// 匹配字符类 [...]，返回 (是否匹配, 结束位置)
fn match_char_class(c: &char, pattern: &[char]) -> Option<(bool, usize)> {
    // pattern[0] == '['
    let mut i = 1;
    let negate = if i < pattern.len() && (pattern[i] == '!' || pattern[i] == '^') {
        i += 1;
        true
    } else {
        false
    };

    let mut matched = false;
    while i < pattern.len() && pattern[i] != ']' {
        let start = pattern[i];
        // 检查范围 a-z
        if i + 2 < pattern.len() && pattern[i + 1] == '-' && pattern[i + 2] != ']' {
            let end = pattern[i + 2];
            if *c >= start && *c <= end {
                matched = true;
            }
            i += 3;
        } else {
            if *c == start {
                matched = true;
            }
            i += 1;
        }
    }

    if i < pattern.len() && pattern[i] == ']' {
        let result = if negate { !matched } else { matched };
        Some((result, i + 1))
    } else {
        None // 未找到 ']'
    }
}

