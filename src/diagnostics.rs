// src/diagnostics.rs - 精准错误诊断
// 第一性原理: 错误必须精确到"字符位置",且必须可修复。
// 每个 Diagnostic 携带 file/line/col/len,以及建议。
// 输出格式: GitHub-style line + caret + underline

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub file: String,
    pub line: u32,        // 1-based
    pub col:  u32,        // 1-based
    pub len:  u32,        // 字节长度,1-based
}

impl Span {
    pub fn point(file: &str, line: u32, col: u32) -> Self {
        Self { file: file.into(), line, col, len: 1 }
    }
    pub fn range(file: &str, line: u32, col: u32, len: u32) -> Self {
        Self { file: file.into(), line, col, len }
    }
    pub fn merge(self, other: Span) -> Span {
        debug_assert_eq!(self.file, other.file);
        Span { file: self.file, line: self.line, col: self.col,
               len: self.len + other.len }
    }
}

#[derive(Debug, Clone)]
pub enum Severity { Error, Warning, Note }

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub sev:    Severity,
    pub code:   &'static str,        // e.g. "E0001"
    pub msg:    String,
    pub span:   Span,
    pub hint:   Option<String>,
    pub source_line: Option<String>, // 该行的源码
}

impl Diagnostic {
    pub fn err(code: &'static str, msg: impl Into<String>, span: Span) -> Self {
        Self { sev: Severity::Error, code, msg: msg.into(), span,
               hint: None, source_line: None }
    }
    pub fn warn(code: &'static str, msg: impl Into<String>, span: Span) -> Self {
        Self { sev: Severity::Warning, code, msg: msg.into(), span,
               hint: None, source_line: None }
    }
    /// 返回一个带 [code] 的简短严重性标签
    pub fn sev_tag(&self) -> String {
        let s = match self.sev {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Note => "note",
        };
        format!("{}[{}]", s, self.code)
    }
    pub fn with_hint(mut self, h: impl Into<String>) -> Self {
        self.hint = Some(h.into()); self
    }
    pub fn with_source(mut self, line: String) -> Self {
        self.source_line = Some(line); self
    }
}

/// ===== GitHub 风格格式化输出 =====
///
/// 格式:
/// ```text
/// error[E0001]: expected ';', got '{'
///   --> main.c:3:12
///    3 |   return x {
///                 ^
///   hint: did you mean ';'?
/// ```

/// Display: 无颜色版本（用于文件日志等）
impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 第一行: error[E0001]: msg
        writeln!(f, "{}: {}", self.sev_tag(), self.msg)?;

        if self.span.line > 0 {
            // 第二行:   --> file:line:col
            writeln!(f, "  --> {}:{}:{}", self.span.file, self.span.line, self.span.col)?;

            if let Some(src) = &self.source_line {
                let line_no = self.span.line;
                let line_digits = format!("{}", line_no).len();
                let gutter_w = 4usize.max(line_digits + 1); // 至少 4 列，含 1 前导空格

                // 源码行: "   3 |   return x {"
                writeln!(f, "{:>width$} | {}", line_no, src, width = gutter_w)?;

                // 指针行: "     |   ^^^"
                let col_offset = (self.span.col as usize).saturating_sub(1);
                let caret = "^".repeat(self.span.len.max(1) as usize);
                writeln!(f, "{:>width$} | {}{}", "", " ".repeat(col_offset), caret, width = gutter_w)?;
            }
        }

        if let Some(h) = &self.hint {
            write!(f, "  hint: {}", h)?;
        }
        Ok(())
    }
}

/// print: 终端彩色版本
impl Diagnostic {
    pub fn print(&self) {
        let reset = "\x1b[0m";
        let bold = "\x1b[1m";
        let cyan = "\x1b[1;36m";

        let (sev_color, sev_str) = match self.sev {
            Severity::Error   => ("\x1b[1;31m", "error"),
            Severity::Warning => ("\x1b[1;33m", "warning"),
            Severity::Note    => ("\x1b[1;36m", "note"),
        };

        // 第一行: error[E0001]: msg
        eprintln!("{}{}{}[{}]{}: {}", sev_color, bold, sev_str, self.code, reset, self.msg);

        if self.span.line > 0 {
            // 第二行:   --> file:line:col
            eprintln!("  {}-->{} {}:{}:{}", cyan, reset, self.span.file, self.span.line, self.span.col);

            if let Some(src) = &self.source_line {
                let line_no = self.span.line;
                let line_digits = format!("{}", line_no).len();
                let gutter_w = 4usize.max(line_digits + 1); // 至少 4 列

                // 源码行: "   3 |   return x {"
                let gutter_num = format!("{:>width$}", line_no, width = gutter_w);
                eprintln!("{}{}{} |{}{}", bold, cyan, gutter_num, reset, src);

                // 指针行: "     |   ^^^"
                let col_offset = (self.span.col as usize).saturating_sub(1);
                let caret = "^".repeat(self.span.len.max(1) as usize);
                let gutter_sp = format!("{:>width$}", "", width = gutter_w);
                let pointer_line = format!("{} | {}{}", gutter_sp, " ".repeat(col_offset), caret);
                eprintln!("{}{}", sev_color, pointer_line);
            }
        }

        // hint 行
        if let Some(h) = &self.hint {
            eprintln!("  {}hint{}: {}", cyan, reset, h);
        }
    }
}

/// 填充诊断信息的 source_line 字段
pub fn enrich_diags(diags: &mut [Diagnostic], src: &[u8]) {
    let text = std::str::from_utf8(src).unwrap_or("");
    let lines: Vec<&str> = text.lines().collect();
    for d in diags.iter_mut() {
        if d.source_line.is_none() && d.span.line > 0 {
            let idx = (d.span.line as usize).saturating_sub(1);
            if let Some(line) = lines.get(idx) {
                d.source_line = Some(line.to_string());
            }
        }
    }
}
