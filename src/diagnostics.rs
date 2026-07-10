// src/diagnostics.rs - 精准错误诊断
// 第一性原理: 错误必须精确到"字符位置",且必须可修复。
// 每个 Diagnostic 携带 file/line/col/len,以及建议。

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
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
    pub fn with_hint(mut self, h: impl Into<String>) -> Self {
        self.hint = Some(h.into()); self
    }
    pub fn with_source(mut self, line: String) -> Self {
        self.source_line = Some(line); self
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sev = match self.sev {
            Severity::Error => "error", Severity::Warning => "warning",
            Severity::Note  => "note",
        };
        write!(f, "{}: {}: {} [{}]", sev, self.code, self.msg,
               self.span.file)?;
        if self.span.line > 0 {
            write!(f, ":{}:{}", self.span.line, self.span.col)?;
        }
        Ok(())
    }
}

impl Diagnostic {
    pub fn print(&self) {
        let sev_color = match self.sev {
            Severity::Error   => "\x1b[1;31m",  // 红
            Severity::Warning => "\x1b[1;33m",  // 黄
            Severity::Note    => "\x1b[1;36m",  // 青
        };
        let bold = "\x1b[1m";
        let reset = "\x1b[0m";
        let sev_str = match self.sev { Severity::Error=>"error", Severity::Warning=>"warning", Severity::Note=>"note" };
        eprintln!("{}{}{}{}: {}", sev_color, bold, sev_str, reset, self.msg);
        eprintln!("  {}-->{} {}:{}:{}", "\x1b[1;36m", reset, self.span.file, self.span.line, self.span.col);
        if let Some(src) = &self.source_line {
            let line_no = format!("{}", self.span.line);
            let gutter_w = line_no.len().max(3);
            eprintln!("   {}\x1b[1;36m{:>width$} |{}\x1b[0m {}", bold, self.span.line, reset, src, width=gutter_w);
            let mut caret = String::new();
            caret.push_str(&" ".repeat(gutter_w + 4)); // "  NNN | "
            caret.push_str(&" ".repeat((self.span.col as usize).saturating_sub(1)));
            caret.push_str(&"^".repeat(self.span.len.max(1) as usize));
            eprintln!("   {}{}{}", sev_color, caret, reset);
        }
        if let Some(h) = &self.hint {
            eprintln!("   {}\x1b[1;36mhint{}: {}", sev_color, reset, h);
        }
    }
}

/// 填充诊断信息的 source_line 字段
pub fn enrich_diags(diags: &mut [Diagnostic], src: &[u8]) {
    let lines: Vec<&[u8]> = src.split(|&b| b == b'\n').collect();
    for d in diags.iter_mut() {
        if d.source_line.is_none() && d.span.line > 0 {
            let idx = (d.span.line as usize).saturating_sub(1);
            if let Some(line_bytes) = lines.get(idx) {
                d.source_line = Some(String::from_utf8_lossy(line_bytes).to_string());
            }
        }
    }
}
