// src/diagnostics.rs - 精准错误诊断
// 第一性原理: 错误必须精确到"字符位置",且必须可修复。
// 每个 Diagnostic 携带 file/line/col/len,以及建议。
//
// 公共 API:
//   * GitHub 风格四段式 (默认): 错误首行 +  --> file:line:col + 源码行 + 指针
//   * Clang 风格 (--diagnostics-format=clang): 兼容 clang 的 caret 形态
//   * MSVC 风格 (--diagnostics-format=msvc): file(line,col): error E0001: msg
//   * vi 风格   (--diagnostics-format=vi)  : 仅首行,无 caret,适合编辑器
//   * SARIF     (--diagnostics-format=sarif): JSON 2.1.0, 供 IDE/工具消费
//
// 颜色控制 (-fdiagnostics-color={always,never,auto}):
//   * Always: 始终输出 ANSI 转义序列
//   * Never:  始终不输出
//   * Auto:   仅当 stderr 是 TTY 时输出 (use std::io::IsTerminal)

use std::fmt;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub file: Arc<str>,
    pub line: u32, // 1-based
    pub col: u32,  // 1-based
    pub len: u32,  // 字节长度,1-based
}

impl Span {
    pub fn point(file: &str, line: u32, col: u32) -> Self {
        Self {
            file: file.into(),
            line,
            col,
            len: 1,
        }
    }
    pub fn range(file: &str, line: u32, col: u32, len: u32) -> Self {
        Self {
            file: file.into(),
            line,
            col,
            len,
        }
    }
    /// Convenience method for tests/mockups
    pub fn unknown() -> Self {
        Self::point("", 0, 0)
    }
    /// Deprecated alias for Span::unknown()
    #[deprecated(note = "Use Span::unknown() instead")]
    pub fn dummy() -> Self {
        Self::unknown()
    }
    pub fn merge(self, other: Span) -> Span {
        debug_assert_eq!(self.file, other.file);
        Span {
            file: self.file,
            line: self.line,
            col: self.col,
            len: self.len + other.len,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Note,
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub sev: Severity,
    pub code: &'static str, // e.g. "E0001"
    pub msg: String,
    pub span: Span,
    pub hint: Option<String>,
    pub source_line: Option<String>, // 该行的源码
}

impl Diagnostic {
    pub fn err(code: &'static str, msg: impl Into<String>, span: Span) -> Self {
        Self {
            sev: Severity::Error,
            code,
            msg: msg.into(),
            span,
            hint: None,
            source_line: None,
        }
    }
    pub fn warn(code: &'static str, msg: impl Into<String>, span: Span) -> Self {
        Self {
            sev: Severity::Warning,
            code,
            msg: msg.into(),
            span,
            hint: None,
            source_line: None,
        }
    }
    /// 通用构造函数：支持动态严重级别（模块化提取的 emit/abi 代码使用）。
    ///
    /// 与 `err`/`warn` 的区别在于严重级别由调用方给定，且 span 可选
    /// （无位置信息时退化为 `file:"" line:0`，各渲染器会跳过 caret 段）。
    pub fn new(sev: Severity, msg: &str, span: Option<Span>) -> Self {
        Self {
            sev,
            code: "E0500",
            msg: msg.to_string(),
            span: span.unwrap_or_else(|| Span::point("", 0, 0)),
            hint: None,
            source_line: None,
        }
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
        self.hint = Some(h.into());
        self
    }
    pub fn with_source(mut self, line: String) -> Self {
        self.source_line = Some(line);
        self
    }
}

// ============================================================================
// 颜色 / 格式 配置
// ============================================================================

/// 颜色策略 (--diagnostics-color / -fdiagnostics-color=)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorPolicy {
    /// 始终输出 ANSI 转义序列
    Always,
    /// 始终不输出
    Never,
    /// 仅当 stderr 是 TTY 时输出 (默认)
    Auto,
}

impl ColorPolicy {
    /// 按当前 stderr 的 TTY 状态求值 Auto 分支
    pub fn resolve(self) -> bool {
        match self {
            ColorPolicy::Always => true,
            ColorPolicy::Never => false,
            ColorPolicy::Auto => {
                // std::io::stderr().is_terminal() 在 unix 与 windows 都可用
                std::io::IsTerminal::is_terminal(&std::io::stderr())
            }
        }
    }
    pub fn from_arg(s: &str) -> Result<Self, String> {
        match s {
            "always" => Ok(ColorPolicy::Always),
            "never" => Ok(ColorPolicy::Never),
            "auto" => Ok(ColorPolicy::Auto),
            other => Err(format!(
                "invalid -fdiagnostics-color value: {} (expected always|never|auto)",
                other
            )),
        }
    }
}

/// 输出格式 (--diagnostics-format / -fdiagnostics-format=)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatStyle {
    /// 默认 GitHub 风格: error[E0001]: ...  + caret 指针
    Github,
    /// Clang 风格: error: ...  + caret
    Clang,
    /// MSVC 风格: file(line,col): error E0001: ...
    Msvc,
    /// vi 风格: 仅单行,无 caret
    Vi,
    /// SARIF 2.1.0 JSON (写到 stderr 末尾追加换行)
    Sarif,
}

impl FormatStyle {
    pub fn from_arg(s: &str) -> Result<Self, String> {
        match s {
            "github" => Ok(FormatStyle::Github),
            "clang" => Ok(FormatStyle::Clang),
            "msvc" => Ok(FormatStyle::Msvc),
            "vi" => Ok(FormatStyle::Vi),
            "sarif" => Ok(FormatStyle::Sarif),
            other => Err(format!(
                "invalid -fdiagnostics-format value: {} \
                 (expected github|clang|msvc|vi|sarif)",
                other
            )),
        }
    }
}

/// 诊断配置: 颜色策略 + 输出格式 + 模板树开关
#[derive(Debug, Clone, Copy)]
pub struct DiagnosticsConfig {
    pub color: ColorPolicy,
    pub format: FormatStyle,
    pub show_template_tree: bool,
}

impl Default for DiagnosticsConfig {
    fn default() -> Self {
        Self {
            color: ColorPolicy::Auto,
            format: FormatStyle::Github,
            show_template_tree: false,
        }
    }
}

// ============================================================================
// GitHub 风格（默认）
// ============================================================================
//
// 格式:
// ```text
// error[E0001]: expected ';', got '{'
//   --> main.c:3:12
//    3 |   return x {
//                 ^
//   hint: did you mean ';'?
// ```

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 第一行: error[E0001]: msg
        writeln!(f, "{}: {}", self.sev_tag(), self.msg)?;

        if self.span.line > 0 {
            // 第二行:   --> file:line:col
            writeln!(
                f,
                "  --> {}:{}:{}",
                self.span.file, self.span.line, self.span.col
            )?;

            if let Some(src) = &self.source_line {
                let line_no = self.span.line;
                let line_digits = format!("{}", line_no).len();
                let gutter_w = 4usize.max(line_digits + 1); // 至少 4 列，含 1 前导空格

                // 源码行: "   3 |   return x {" (注意 | 后无空格,与历史 snapshot 一致)
                writeln!(f, "{:>width$} |{}", line_no, src, width = gutter_w)?;

                // 指针行: "     |   ^^^" (| 后有空格对齐)
                let col_offset = (self.span.col as usize).saturating_sub(1);
                let caret = "^".repeat(self.span.len.max(1) as usize);
                writeln!(
                    f,
                    "{:>width$} | {}{}",
                    "",
                    " ".repeat(col_offset),
                    caret,
                    width = gutter_w
                )?;
            }
        }

        if let Some(h) = &self.hint {
            write!(f, "  hint: {}", h)?;
        }
        Ok(())
    }
}

// ============================================================================
// 格式化器
// ============================================================================

/// GitHub 风格首行标签 (无 ANSI)
fn gh_first_line(d: &Diagnostic) -> String {
    format!("{}: {}", d.sev_tag(), d.msg)
}

/// Clang 风格首行:  `error: msg`
fn clang_first_line(d: &Diagnostic) -> String {
    let s = match d.sev {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Note => "note",
    };
    format!("{}: {}", s, d.msg)
}

/// MSVC 风格首行:  `file(line,col): error E0001: msg`
fn msvc_first_line(d: &Diagnostic) -> String {
    let s = match d.sev {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Note => "note",
    };
    if d.span.line > 0 {
        format!(
            "{}({},{}): {} {}: {}",
            d.span.file, d.span.line, d.span.col, s, d.code, d.msg
        )
    } else {
        format!("{} {}: {}", s, d.code, d.msg)
    }
}

/// vi 风格首行:  `file:line:col: error[E0001]: msg`
fn vi_first_line(d: &Diagnostic) -> String {
    if d.span.line > 0 {
        format!(
            "{}:{}:{}: {}",
            d.span.file,
            d.span.line,
            d.span.col,
            gh_first_line(d)
        )
    } else {
        format!("{}: {}", d.span.file, gh_first_line(d))
    }
}

/// 共享源码 + caret 区段（GitHub/Clang 共用）
///
/// 注: 为与历史 snapshot 保持一致,源码行使用 `gutter_num |src` 形式
/// (| 后无空格);指针行使用 `gutter_sp |      ^^` 形式 (| 后有空格对齐)。
///
/// 当 source_line 缺失时(例如 Driver 直接构造的 E0300 错误,未走
/// enrich_diags 补全),不输出任何附加行;位置行 `  -->` 由 render()
/// 单独负责,避免出现两条 `-->` 行。
fn source_caret_block(d: &Diagnostic) -> String {
    if d.span.line == 0 {
        return String::new();
    }
    let src = match &d.source_line {
        Some(s) => s,
        None => return String::new(),
    };
    let mut out = String::new();
    let line_no = d.span.line;
    let line_digits = format!("{}", line_no).len();
    let gutter_w = 4usize.max(line_digits + 1);
    out.push_str(&format!("{:>width$} |{}\n", line_no, src, width = gutter_w));
    // 列号钳制: 防止宏展开产生的列号超出源码行长度
    let src_len = src.len();
    let col_offset = (d.span.col as usize).saturating_sub(1).min(src_len);
    let caret_len = (d.span.len.max(1) as usize)
        .min(src_len.saturating_sub(col_offset))
        .max(1);
    let caret = "^".repeat(caret_len);
    out.push_str(&format!(
        "{:>width$} | {}\n",
        "",
        " ".repeat(col_offset) + &caret,
        width = gutter_w
    ));
    out
}

/// 共享 hint 尾巴
fn hint_tail(d: &Diagnostic) -> String {
    match &d.hint {
        Some(h) => format!("  hint: {}\n", h),
        None => String::new(),
    }
}

/// 按 FormatStyle 渲染一个 diagnostic 为单条字符串
pub fn render(d: &Diagnostic, fmt: FormatStyle) -> String {
    match fmt {
        FormatStyle::Github => {
            // 与原 fmt::Display 一致
            let mut s = format!("{}: {}\n", d.sev_tag(), d.msg);
            if d.span.line > 0 {
                s.push_str(&format!(
                    "  --> {}:{}:{}\n",
                    d.span.file, d.span.line, d.span.col
                ));
                s.push_str(&source_caret_block(d));
            }
            s.push_str(&hint_tail(d));
            s
        }
        FormatStyle::Clang => {
            let mut s = format!("{}\n", clang_first_line(d));
            if d.span.line > 0 {
                s.push_str(&source_caret_block(d));
            }
            s.push_str(&hint_tail(d));
            s
        }
        FormatStyle::Msvc => {
            format!("{}\n", msvc_first_line(d))
        }
        FormatStyle::Vi => {
            format!("{}\n", vi_first_line(d))
        }
        FormatStyle::Sarif => {
            // 单条 diagnostic 在 SARIF 模式由 render_sarif 统一处理
            String::new()
        }
    }
}

// ============================================================================
// SARIF 2.1.0 输出
// ============================================================================

/// 把一组 diagnostic 渲染为 SARIF 2.1.0 JSON 字符串
pub fn render_sarif(diags: &[Diagnostic]) -> String {
    let mut results = String::new();
    for (i, d) in diags.iter().enumerate() {
        if i > 0 {
            results.push_str(",\n");
        }
        results.push_str(&sarif_result(d));
    }
    format!(
        r#"{{
  "$schema": "https://schemastore.azurewebsites.net/schemas/json/sarif-2.1.0.json",
  "version": "2.1.0",
  "runs": [
    {{
      "tool": {{
        "driver": {{
          "name": "dcc",
          "version": "{ver}",
          "informationUri": "https://github.com/dcc/dcc"
        }}
      }},
      "results": [
        {results}
      ]
    }}
  ]
}}
"#,
        ver = env!("CARGO_PKG_VERSION"),
        results = if results.is_empty() { " ".to_string() } else { results },
    )
}

fn sarif_level(d: &Diagnostic) -> &'static str {
    match d.sev {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Note => "note",
    }
}

fn sarif_escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

fn sarif_result(d: &Diagnostic) -> String {
    let level = sarif_level(d);
    let code = sarif_escape_json(d.code);
    let msg = sarif_escape_json(&d.msg);
    let file = sarif_escape_json(&d.span.file);
    let hint_part = match &d.hint {
        Some(h) => format!(
            ",\n      \"properties\": {{\"hint\": \"{}\"}}",
            sarif_escape_json(h)
        ),
        None => String::new(),
    };
    if d.span.line > 0 {
        format!(
            r#"    {{
      "ruleId": "{code}",
      "level": "{level}",
      "message": {{ "text": "{msg}" }},
      "locations": [
        {{
          "physicalLocation": {{
            "artifactLocation": {{ "uri": "{file}" }},
            "region": {{
              "startLine": {line},
              "startColumn": {col},
              "endColumn": {end_col}
            }}
          }}
        }}
      ]{hint}
    }}"#,
            code = code,
            level = level,
            msg = msg,
            file = file,
            line = d.span.line,
            col = d.span.col.max(1),
            end_col = d
                .span
                .col
                .saturating_sub(1)
                .saturating_add(d.span.len.max(1))
                .max(1),
            hint = hint_part,
        )
    } else {
        format!(
            r#"    {{
      "ruleId": "{code}",
      "level": "{level}",
      "message": {{ "text": "{msg}" }}{hint}
    }}"#,
            code = code,
            level = level,
            msg = msg,
            hint = hint_part,
        )
    }
}

// ============================================================================
// 模板树 (-fdiagnostics-show-template-tree)
// ============================================================================
//
// 简单 C 前端没有真实模板;语义上为占位实现: 当开关开启且 msg 含
// 尖括号嵌套（形如 `Foo<int, Bar<float>>`）时,在 hint 后面追加一个
// 折叠视图,展示每个层级的类型参数。

/// 返回附加的模板树展示,空串表示无需展示
pub fn template_tree_tail(d: &Diagnostic) -> String {
    if !looks_like_template(&d.msg) {
        return String::new();
    }
    let tree = build_template_tree(&d.msg);
    format!(
        "  template-tree:\n{}\n",
        tree.lines()
            .map(|l| format!("    {}", l))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

fn looks_like_template(s: &str) -> bool {
    // 简易启发: 含 `Name<` 且 `<` 配对闭合
    if !s.contains('<') {
        return false;
    }
    let mut depth = 0i32;
    for c in s.chars() {
        match c {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    depth == 0
}

fn build_template_tree(s: &str) -> String {
    // 找到第一个 `<`,把前面的名字当作根,后面按层级缩进展开
    let Some(open) = s.find('<') else {
        return s.to_string();
    };
    let root = s[..open].trim();
    let rest = &s[open + 1..];
    // 找匹配 `>`: 简单深度计数
    let mut depth = 1i32;
    let mut close = rest.len();
    for (i, c) in rest.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    close = i;
                    break;
                }
            }
            _ => {}
        }
    }
    let inside = &rest[..close];
    let args = split_top_level_commas(inside);
    let mut out = String::new();
    out.push_str(&format!("{}<", root));
    for (i, arg) in args.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        if looks_like_template(arg) {
            out.push('\n');
            for l in build_template_tree(arg).lines() {
                out.push_str(&format!("  {}", l));
                out.push('\n');
            }
            // 去掉末尾多出的换行
            while out.ends_with('\n') && out.ends_with("  \n") {
                out.pop();
            }
        } else {
            out.push_str(arg.trim());
        }
    }
    out.push('>');
    out
}

fn split_top_level_commas(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

// ============================================================================
// 终端输出
// ============================================================================

/// 旧 print() 入口保持不变：默认 GitHub + Always 颜色
impl Diagnostic {
    pub fn print(&self) {
        let cfg = DiagnosticsConfig {
            color: ColorPolicy::Always,
            format: FormatStyle::Github,
            show_template_tree: false,
        };
        self.print_with(&cfg);
    }

    /// 按 cfg 渲染并写到 stderr
    pub fn print_with(&self, cfg: &DiagnosticsConfig) {
        let use_color = cfg.color.resolve();
        if cfg.format == FormatStyle::Sarif {
            // SARIF 模式: 调用方统一收集所有 diagnostic 然后 render_sarif
            // 这里对单条调用走 eprintln 一条 SARIF result 包络 (兼容单条)
            let s = render_sarif(std::slice::from_ref(self));
            eprintln!("{}", s.trim_end());
            return;
        }
        let mut text = render(self, cfg.format);
        if cfg.show_template_tree {
            text.push_str(&template_tree_tail(self));
        }
        if !use_color {
            eprint!("{}", text);
            return;
        }
        // 上色: 第一行 sev_tag 染色,caret 行染色
        let sev_color = match self.sev {
            Severity::Error => "\x1b[1;31m",
            Severity::Warning => "\x1b[1;33m",
            Severity::Note => "\x1b[1;36m",
        };
        let bold = "\x1b[1m";
        let cyan = "\x1b[1;36m";
        let reset = "\x1b[0m";
        let mut out = String::new();
        // 首行
        if let Some(idx) = text.find('\n') {
            out.push_str(sev_color);
            out.push_str(bold);
            out.push_str(&text[..idx]);
            out.push_str(reset);
            out.push('\n');
            let rest = &text[idx + 1..];
            // caret 行 (含 `^`) 染色
            for line in rest.split_inclusive('\n') {
                if line.contains('^') && !line.contains("hint:") {
                    out.push_str(sev_color);
                    out.push_str(line);
                    out.push_str(reset);
                } else if line.contains("hint:") || line.contains(" --> ") {
                    out.push_str(cyan);
                    out.push_str(line);
                    out.push_str(reset);
                } else if line.contains(" | ") {
                    out.push_str(bold);
                    out.push_str(line);
                    out.push_str(reset);
                } else {
                    out.push_str(line);
                }
            }
        } else {
            out.push_str(sev_color);
            out.push_str(bold);
            out.push_str(&text);
            out.push_str(reset);
        }
        eprint!("{}", out);
    }
}

/// 对诊断去重: (相同 code + file + line + col + len + msg) 只保留第一条
fn dedup_diags(diags: &[Diagnostic]) -> Vec<Diagnostic> {
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(diags.len());
    for d in diags {
        let key = (
            d.code,
            d.span.file.as_ref(),
            d.span.line,
            d.span.col,
            d.span.len,
            d.msg.as_str(),
        );
        if seen.insert(key) {
            out.push(d.clone());
        }
    }
    out
}

/// 错误风暴上限: 单次编译最多输出 50 条警告, 超出时抑制并提示;
/// 错误 (Error) 永远不被抑制, 确保用户始终能看到全部编译错误。
const MAX_DIAGS: usize = 50;

/// 把一组 diagnostic 按 cfg 输出到 stderr
pub fn print_all(diags: &[Diagnostic], cfg: &DiagnosticsConfig) {
    let filtered = dedup_diags(diags);
    if cfg.format == FormatStyle::Sarif {
        eprintln!("{}", render_sarif(&filtered).trim_end());
        return;
    }
    let mut warning_count = 0;
    let mut suppressed = 0;
    for d in &filtered {
        if matches!(d.sev, Severity::Warning) {
            warning_count += 1;
            if warning_count > MAX_DIAGS {
                suppressed += 1;
                continue;
            }
        }
        d.print_with(cfg);
    }
    if suppressed > 0 {
        eprintln!("error: too many warnings, suppressing {} more", suppressed);
    }
}

/// 填充诊断信息的 source_line 字段
///
/// 当 span.file 与主文件不同时, 尝试从磁盘读取对应源文件
/// 提取该行的源码文本。`base_dir` 用于解析相对路径。
pub fn enrich_diags(
    diags: &mut [Diagnostic],
    src: &[u8],
    _file_name: &str,
    base_dir: Option<&std::path::Path>,
) {
    let main_text = std::str::from_utf8(src).unwrap_or("");
    let main_lines: Vec<&str> = main_text.lines().collect();
    // 缓存: 源文件路径 -> 行文本 (避免重复读磁盘).
    // remap 后 span.line 已是源文件行号 (主文件同样), 因此主文件也
    // 从磁盘读取源文本; 磁盘读取失败时 (如 stdin 输入) 回退到
    // main_lines (预处理输出, 与 remap 未命中场景保持同源).
    let mut file_cache: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for d in diags.iter_mut() {
        if d.span.line == 0 {
            continue;
        }
        let file_str: &str = &d.span.file;
        if !file_cache.contains_key(file_str) {
            let tried_paths = [
                base_dir.map(|b| b.join(file_str)),
                Some(std::path::PathBuf::from(file_str)),
            ];
            let mut found: Option<Vec<String>> = None;
            for p in tried_paths.into_iter().flatten() {
                if let Ok(content) = std::fs::read_to_string(&p) {
                    found = Some(content.lines().map(|s| s.to_string()).collect());
                    break;
                }
            }
            file_cache.insert(file_str.to_string(), found.unwrap_or_default());
        }
        let lines_vec = file_cache.get(file_str).unwrap();
        let total = lines_vec.len();
        if total == 0 {
            // 磁盘源文件不可得 (stdin / 预处理输出): 回退到 main_lines,
            // 此时 span.line 为预处理输出行号, 行文本与其同源, 不 clamp.
            let idx = (d.span.line as usize).saturating_sub(1);
            if d.source_line.is_none() {
                if let Some(line) = main_lines.get(idx) {
                    d.source_line = Some(line.to_string());
                }
            }
            continue;
        }
        // Clamp 行号: span 行号超过文件实际行数时 (常见于宏展开 /
        // 缓存回放造成的行号虚高), 回落到文件末尾行. 这样用户至少
        // 看到真实存在的文件和行号, 而不是 "sched.h:169110".
        let idx = (d.span.line as usize).saturating_sub(1);
        if idx >= total {
            d.span.line = total as u32;
        }
        if d.source_line.is_some() {
            continue;
        }
        let idx2 = (d.span.line as usize).saturating_sub(1);
        if let Some(line) = lines_vec.get(idx2) {
            d.source_line = Some(line.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Diagnostic {
        Diagnostic::err("E0001", "expected ';'", Span::point("main.c", 3, 12))
            .with_hint("add ';' here")
            .with_source("    return x {".to_string())
    }

    #[test]
    fn color_policy_from_arg_ok() {
        assert_eq!(
            ColorPolicy::from_arg("always").unwrap(),
            ColorPolicy::Always
        );
        assert_eq!(ColorPolicy::from_arg("never").unwrap(), ColorPolicy::Never);
        assert_eq!(ColorPolicy::from_arg("auto").unwrap(), ColorPolicy::Auto);
        assert!(ColorPolicy::from_arg("maybe").is_err());
    }

    #[test]
    fn format_style_from_arg_ok() {
        assert_eq!(
            FormatStyle::from_arg("github").unwrap(),
            FormatStyle::Github
        );
        assert_eq!(FormatStyle::from_arg("clang").unwrap(), FormatStyle::Clang);
        assert_eq!(FormatStyle::from_arg("msvc").unwrap(), FormatStyle::Msvc);
        assert_eq!(FormatStyle::from_arg("vi").unwrap(), FormatStyle::Vi);
        assert_eq!(FormatStyle::from_arg("sarif").unwrap(), FormatStyle::Sarif);
        assert!(FormatStyle::from_arg("rust").is_err());
    }

    #[test]
    fn render_github_has_caret_and_arrow() {
        let s = render(&sample(), FormatStyle::Github);
        assert!(s.contains("error[E0001]:"));
        assert!(s.contains("-->"));
        assert!(s.contains("^"));
        assert!(s.contains("hint:"));
    }

    #[test]
    fn render_clang_strips_brackets() {
        let s = render(&sample(), FormatStyle::Clang);
        assert!(s.starts_with("error: "));
        assert!(!s.contains("error[E"));
        assert!(s.contains("^"));
    }

    #[test]
    fn render_msvc_uses_file_line_col() {
        let s = render(&sample(), FormatStyle::Msvc);
        assert!(s.contains("main.c(3,12): error E0001:"));
    }

    #[test]
    fn render_vi_is_single_line() {
        let s = render(&sample(), FormatStyle::Vi);
        let lines: Vec<&str> = s.lines().collect();
        assert_eq!(lines.len(), 1);
        assert!(s.contains("main.c:3:12"));
    }

    #[test]
    fn sarif_emits_valid_json_envelope() {
        let d = sample();
        let s = render_sarif(std::slice::from_ref(&d));
        assert!(s.contains("\"$schema\""));
        assert!(s.contains("\"version\": \"2.1.0\""));
        assert!(s.contains("\"name\": \"dcc\""));
        assert!(s.contains("\"ruleId\": \"E0001\""));
        assert!(s.contains("\"startLine\": 3"));
        assert!(s.contains("\"uri\": \"main.c\""));
    }

    #[test]
    fn sarif_handles_multiple_diags() {
        let mut d1 = sample();
        d1.code = "E0001";
        let mut d2 = sample();
        d2.code = "E0002";
        d2.msg = "another".to_string();
        let s = render_sarif(&[d1, d2]);
        assert!(s.contains("\"E0001\""));
        assert!(s.contains("\"E0002\""));
    }

    #[test]
    fn sarif_escapes_specials() {
        let mut d = sample();
        d.msg = "needs \"quote\" and \\ and \nnewline".to_string();
        let s = render_sarif(std::slice::from_ref(&d));
        assert!(s.contains("\\\"quote\\\""));
        assert!(s.contains("\\\\"));
        assert!(s.contains("\\n"));
    }

    #[test]
    fn template_tree_expands_nested() {
        let mut d = sample();
        d.msg = "cannot match Foo<int, Bar<float, char>>".to_string();
        let tree = build_template_tree(&d.msg);
        assert!(tree.contains("Foo<"));
        assert!(tree.contains("Bar<"));
    }

    #[test]
    fn template_tree_ignores_non_template() {
        let mut d = sample();
        d.msg = "simple error".to_string();
        assert_eq!(template_tree_tail(&d), "");
    }

    #[test]
    fn render_github_matches_display_impl() {
        let d = sample();
        let a = render(&d, FormatStyle::Github);
        let b = format!("{}", d);
        // 仅末尾换行差异允许
        assert_eq!(a.trim_end(), b.trim_end());
    }
}
