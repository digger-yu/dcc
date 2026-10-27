// src/codegen/x86_64/asm/directive.rs
/// Advanced directive expansion for inline assembly
///
/// Extracted from src/codegen/x86_64/mod.rs (Turn 35 ASM module migration)
/// Implements GNU as-style advanced directives:
/// - `.rept N ... .endr`: Repeat block N times
/// - `.irp sym, v1, v2, ... ... .endr`: Iterate over value list
/// - `.irpc sym, chars ... .endr`: Iterate over characters
/// - `.macro name [args] ... .endm`: Define macros
/// - `.include "file"`: Include external files
/// - `.if/.elseif/.else/.endif`: Conditional assembly
/// - `.ifdef/.ifndef`: Symbol presence checks (always false/true respectively)
/// - `.intel_syntax/.att_syntax`: Syntax mode switching
/// - `.altmacro/.noaltmacro`: Alternate macro mode toggling
///
/// # Design Notes
/// - Performs text-level preprocessing before mini-assembler
/// - Recursive expansion handles nested directives
/// - Maintains syntax state (Intel/AT&T) throughout expansion
/// - Macro calls use backslash prefix: `\param`, `\(param)`
use std::collections::HashMap;

/// Advanced directive expander for inline assembly
///
/// Processes high-level assembly directives before the assembly is emitted.
/// Provides GCC-compatible macro language support with repetition, iteration,
/// conditional assembly, and macro definitions.
///
/// # Features
/// - **Repetition**: `.rept N` repeats a block N times
/// - **Iteration**: `.irp` (list), `.irpc` (character string) loop over values
/// - **Macros**: `.macro`/`.endm` define reusable code blocks with parameters
/// - **Conditionals**: `.if/.elseif/.else/.endif` with simple integer expressions
/// - **Includes**: `.include` reads external files into expansion stream
/// - **Syntax mode**: Track Intel/AT&T syntax preferences
pub struct AsmDirectiveExpander {
    /// Defined macros: name -> (parameter_names, body_lines)
    macros: HashMap<String, (Vec<MacroParam>, Vec<String>)>,

    /// Current syntax mode: true = Intel, false = AT&T
    intel_syntax: bool,

    /// Alternate macro mode enabled
    alt_macro: bool,

    /// `.set`/`.equ`/`sym = expr` 留下的整数符号表。
    ///
    /// 为什么非要有它：内核的 `extable_type_reg` 这类宏靠 `.set .Lregnr, .Lregnr+1`
    /// 在 `.irp` 里数寄存器编号，再把 `.long \type + (.Lregnr << 8)` 写进
    /// `__ex_table`。没有符号表时那条 `.long` 算不出来 ⇒ 第三个字（type）直接不发
    /// ⇒ `__ex_table` 条目变 8 字节，`ld -r vmlinux.o` 后 objtool 报
    /// 「__ex_table size not a multiple of 12」。
    symbols: HashMap<String, i64>,

    /// `\@`：本 asm 块里宏被展开的次数（GAS 同一语义）。
    macro_seq: usize,
}

/// 宏形参：GAS 写 `name:req`（必传）、`name:var` 与 `name=默认值`，且多个形参之间
/// 可以用空格而不是逗号分隔（`extable_type_reg type:req reg:req` 就是这种）。
#[derive(Clone, Debug)]
struct MacroParam {
    name: String,
    default: Option<String>,
}

impl MacroParam {
    ///  parse 一个形参书写：`reg` / `\reg` / `type:req` / `sp_reg=0` / `signal:var=0`。
    fn parse(s: &str) -> Self {
        let s = s.trim().trim_start_matches('\\');
        let (head, default) = match s.split_once('=') {
            Some((h, d)) => (h, Some(d.trim().to_string())),
            None => (s, None),
        };
        let name = match head.split_once(':') {
            Some((n, _kind)) => n,
            None => head,
        };
        Self {
            name: name.trim().to_string(),
            default,
        }
    }
}

impl Default for AsmDirectiveExpander {
    /// 默认状态与 [`AsmDirectiveExpander::new`] 一致: AT&T 语法 / 空宏表 / 非 altmacro.
    fn default() -> Self {
        Self::new()
    }
}

impl AsmDirectiveExpander {
    /// Create new directive expander with default settings
    ///
    /// Initial state:
    /// - Empty macro table
    /// - AT&T syntax mode (default for GCC)
    /// - Alternate macro mode disabled
    pub fn new() -> Self {
        Self {
            macros: HashMap::new(),
            intel_syntax: false,
            alt_macro: false,
            symbols: HashMap::new(),
            macro_seq: 0,
        }
    }

    /// Expand all advanced directives in assembly lines
    ///
    /// This is the main entry point that processes a line-by-line assembly stream.
    /// Handles all directive types recursively and returns expanded instruction lines.
    ///
    /// # Algorithm
    /// 1. Iterate through lines
    /// 2. Check for directive prefixes (.rept, .irp, etc.)
    /// 3. Expand matched directive recursively
    /// 4. Try macro call expansion
    /// 5. Pass-through non-directive lines
    ///
    /// # Arguments
    /// * `lines` - Input assembly lines
    ///
    /// # Returns
    /// Expanded lines with all directives resolved
    ///
    /// # Examples
    /// ```text
    /// Input:  [".rept 2", "  mov eax, 1", ".endr"]
    /// Output: ["  mov eax, 1", "  mov eax, 1"]
    /// ```
    pub fn expand(&mut self, lines: &[String]) -> Vec<String> {
        let mut out = Vec::new();
        let mut i = 0;

        while i < lines.len() {
            let line = lines[i].trim().to_string();

            // Skip empty lines
            if line.is_empty() {
                i += 1;
                continue;
            }

            // .rept N ... .endr
            if let Some(count) = Self::parse_rept(&line) {
                let (body, end_idx) = Self::collect_block(lines, i + 1, ".endr");
                let expanded_body = self.expand(&body);
                for _ in 0..count {
                    out.extend(expanded_body.clone());
                }
                i = end_idx + 1;
                continue;
            }

            // .irp sym, v1, v2, ... ... .endr
            if let Some((sym, vals)) = Self::parse_irp(&line) {
                let (body, end_idx) = Self::collect_block(lines, i + 1, ".endr");
                // 先代入循环变量、再展开：GAS 是「每次迭代把 body 当代文本展开」，
                // `.irp` 体内嵌套的 `.ifc \reg, %\rs` / `.set` 必须看到已代入的值。
                // 先 `expand` 再代入会把那些条件行里的 `\rs` 留着，判据当场失效。
                for val in &vals {
                    let subst: Vec<String> = body
                        .iter()
                        .map(|l| {
                            l.replace(&format!("\\{sym}"), val)
                                .replace(&format!("\\({sym})"), val)
                        })
                        .collect();
                    let expanded_body = self.expand(&subst);
                    out.extend(expanded_body);
                }
                i = end_idx + 1;
                continue;
            }

            // .irpc sym, chars ... .endr
            if let Some((sym, chars)) = Self::parse_irpc(&line) {
                let (body, end_idx) = Self::collect_block(lines, i + 1, ".endr");
                for c in chars.chars() {
                    let cv = c.to_string();
                    let subst: Vec<String> = body
                        .iter()
                        .map(|l| {
                            l.replace(&format!("\\{sym}"), &cv)
                                .replace(&format!("\\({sym})"), &cv)
                        })
                        .collect();
                    let expanded_body = self.expand(&subst);
                    out.extend(expanded_body);
                }
                i = end_idx + 1;
                continue;
            }

            // .macro name [args] ... .endm
            if let Some((name, args)) = Self::parse_macro_def(&line) {
                let (body, end_idx) = Self::collect_block(lines, i + 1, ".endm");
                self.macros.insert(name.clone(), (args.clone(), body));
                i = end_idx + 1;
                continue;
            }

            // .purgem name: 删掉已定义的宏（内核定义后调用再 purgem 重用同一个名字）
            if let Some(name) = Self::parse_purgem(&line) {
                self.macros.remove(&name);
                i += 1;
                continue;
            }

            // .set sym, expr / .equ sym, expr / sym = expr: 记进符号表，本身不发
            if let Some((sym, expr)) = Self::parse_set_directive(&line) {
                let v = self.eval_expr_value(&expr).unwrap_or(0);
                self.symbols.insert(sym, v);
                i += 1;
                continue;
            }

            // Macro invocation: name [args]
            if let Some(expanded) = self.try_expand_macro_call(&line) {
                out.extend(expanded);
                i += 1;
                continue;
            }

            // .include "file"
            if let Some(path) = Self::parse_include(&line) {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    let inc_lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
                    let expanded = self.expand(&inc_lines);
                    out.extend(expanded);
                }
                // File not found: silently skip (GCC compatible behavior)
                i += 1;
                continue;
            }

            // .if/.ifdef/.ifndef/.ifc/.ifb ... .else/.elseif ... .endif
            if let Some(cond_result) = self.parse_conditional(&line) {
                let (body, end_idx) = Self::collect_conditional_block(lines, i + 1, cond_result);
                let expanded = self.expand(&body);
                out.extend(expanded);
                i = end_idx + 1;
                continue;
            }

            // .intel_syntax noprefix / .intel_syntax
            if &line == ".intel_syntax noprefix" || &line == ".intel_syntax" {
                self.intel_syntax = true;
                out.push(line);
                i += 1;
                continue;
            }

            // .att_syntax / .att_syntax prefix
            if &line == ".att_syntax" || &line == ".att_syntax prefix" {
                self.intel_syntax = false;
                out.push(line);
                i += 1;
                continue;
            }

            // .altmacro
            if &line == ".altmacro" {
                self.alt_macro = true;
                out.push(line);
                i += 1;
                continue;
            }

            // .noaltmacro
            if &line == ".noaltmacro" {
                self.alt_macro = false;
                out.push(line);
                i += 1;
                continue;
            }

            // Regular line: pass through（把 `.set` 定义的符号先折成数）
            let folded = self.fold_symbols(&line);
            out.push(folded);
            i += 1;
        }

        out
    }

    /// Parse `.rept N` directive, return repeat count
    ///
    /// # Arguments
    /// * `line` - Line that may contain `.rept N`
    ///
    /// # Returns
    /// Some(count) if valid, None otherwise
    ///
    /// # Example
    /// ```rust
    /// use dcc::codegen::x86_64::AsmDirectiveExpander;
    /// assert_eq!(AsmDirectiveExpander::parse_rept(".rept 5"), Some(5));
    /// assert_eq!(AsmDirectiveExpander::parse_rept("mov eax, 1"), None);
    /// ```
    pub fn parse_rept(line: &str) -> Option<usize> {
        let t = line.trim();
        let rest = t.strip_prefix(".rept")?;
        let rest = rest.trim();
        rest.split_whitespace().next()?.parse().ok()
    }

    /// Parse `.irp sym, v1, v2, ...` directive
    ///
    /// # Arguments
    /// * `line` - Line containing `.irp` directive
    ///
    /// # Returns
    /// Some((symbol_name, value_list))
    ///
    /// # Example
    /// ```rust
    /// use dcc::codegen::x86_64::AsmDirectiveExpander;
    /// let result = AsmDirectiveExpander::parse_irp(".irp x, 1, 2, 3").unwrap();
    /// assert_eq!(result.0, "x");
    /// assert_eq!(result.1, vec!["1", "2", "3"]);
    /// ```
    pub fn parse_irp(line: &str) -> Option<(String, Vec<String>)> {
        let t = line.trim();
        let rest = t.strip_prefix(".irp")?;
        let rest = rest.trim();
        let mut parts = rest.splitn(2, ',');
        let sym = parts.next()?.trim().to_string();
        let vals_str = parts.next()?;
        let vals: Vec<String> = vals_str.split(',').map(|s| s.trim().to_string()).collect();
        Some((sym, vals))
    }

    /// Parse `.irpc sym, chars` directive
    ///
    /// # Arguments
    /// * `line` - Line containing `.irpc` directive
    ///
    /// # Returns
    /// Some((symbol_name, character_string))
    ///
    /// # Example
    /// ```rust
    /// use dcc::codegen::x86_64::AsmDirectiveExpander;
    /// let result = AsmDirectiveExpander::parse_irpc(".irpc c, ABC").unwrap();
    /// assert_eq!(result.0, "c");
    /// assert_eq!(result.1, "ABC");
    /// ```
    pub fn parse_irpc(line: &str) -> Option<(String, String)> {
        let t = line.trim();
        let rest = t.strip_prefix(".irpc")?;
        let rest = rest.trim();
        let mut parts = rest.splitn(2, ',');
        let sym = parts.next()?.trim().to_string();
        let chars = parts.next()?.trim().to_string();
        Some((sym, chars))
    }

    /// Parse `.macro name [arg1, arg2, ...]` directive
    ///
    /// 宏名区分大小写，原样返回（不做任何规范化）。形参列表可以同时混用逗号与空格
    /// （内核：`.macro extable_type_reg type:req reg:req`），并允许 `:req` / `:var`
    /// 后缀与 `=默认值`；例如 `.macro add a, b` 得到形参 `a`、`b`。
    fn parse_macro_def(line: &str) -> Option<(String, Vec<MacroParam>)> {
        let t = line.trim();
        let rest = t.strip_prefix(".macro")?;
        let rest = rest.trim();

        // Split macro name from arguments
        let mut parts = rest.splitn(2, |c: char| c.is_whitespace());
        let name = parts.next()?.trim().to_string();
        let args_rest = parts.next().unwrap_or("").trim();

        let args: Vec<MacroParam> = if args_rest.is_empty() {
            Vec::new()
        } else {
            args_rest
                .split([',', ' ', '\t'])
                .filter(|s| !s.trim().is_empty())
                .map(|s| MacroParam::parse(s.trim()))
                .collect()
        };

        Some((name, args))
    }

    /// `.purgem name` → Some(name)
    fn parse_purgem(line: &str) -> Option<String> {
        let t = line.trim();
        let rest = t.strip_prefix(".purgem")?;
        let name = rest.trim().split_whitespace().next()?.to_string();
        Some(name)
    }

    /// `.set sym, expr` / `.equ sym, expr` / `sym = expr` → Some((sym, expr))
    fn parse_set_directive(line: &str) -> Option<(String, String)> {
        let t = line.trim();
        for d in [".set", ".equ", ".equiv"] {
            if let Some(rest) = t.strip_prefix(d) {
                let rest = rest.trim();
                let (sym, expr) = rest.split_once([',', ' ', '\t'])?;
                let sym = sym.trim().to_string();
                let expr = expr.trim().to_string();
                if !sym.is_empty() && !expr.is_empty() {
                    return Some((sym, expr));
                }
                return None;
            }
        }
        // `sym = expr` 形式（inst.h 的宏体里就是这种写法）
        if t.starts_with('.') || t.contains([' ', '\t']) && !t.contains('=') {
            return None;
        }
        let (sym, expr) = t.split_once('=')?;
        let sym = sym.trim();
        let expr = expr.trim();
        // 左边必须是个名字，且不能是 `==`/`!=`/`<=`/`>=` 这类比较式的一部分。
        if sym.is_empty()
            || expr.is_empty()
            || !sym
                .chars()
                .all(|c| c.is_alphanumeric() || c == '.' || c == '_')
            || sym.ends_with('!')
            || sym.ends_with('<')
            || sym.ends_with('>')
        {
            return None;
        }
        Some((sym.to_string(), expr.to_string()))
    }

    /// 把已 `.set` 的符号名在行内整词替换成它的数。
    ///
    /// 后面的数据发射器只会算纯数表达式，认不得 `.Lregnr` 这种只存在于宏里的局部符号；
    /// GAS 则是在汇编时按「当时值」展开的。只在「本行里出现的整词恰好是 `.set` 过的名字」
    /// 时替换，不影响标签定义与重定位（那些名字不在符号表里）。
    fn fold_symbols(&self, line: &str) -> String {
        if self.symbols.is_empty() {
            return line.to_string();
        }
        let mut out = String::with_capacity(line.len());
        let bytes: Vec<char> = line.chars().collect();
        let is_ident = |c: char| c.is_alphanumeric() || c == '.' || c == '_';
        let mut i = 0usize;
        while i < bytes.len() {
            if is_ident(bytes[i]) {
                let start = i;
                while i < bytes.len() && is_ident(bytes[i]) {
                    i += 1;
                }
                let word: String = bytes[start..i].iter().collect();
                match self.symbols.get(&word) {
                    // 数字开头会被后面的解析当数；负值加括号，不改变优先级语义
                    Some(v) => {
                        if *v < 0 {
                            out.push_str(&format!("({v})"));
                        } else {
                            out.push_str(&v.to_string());
                        }
                    },
                    None => out.push_str(&word),
                }
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        out
    }

    /// Attempt to expand a macro call on a line
    ///
    /// Checks if line starts with a known macro name and expands it.
    /// Replaces parameters with actual arguments using `\param` or `\(param)` syntax.
    ///
    /// # Arguments
    /// * `line` - Assembly line that may be a macro call
    ///
    /// # Returns
    /// Some(expanded_lines) if it was a macro call, None otherwise
    ///
    /// # Macro Call Format
    /// `MACRO_NAME arg1, arg2, ...` 或 `MACRO_NAME key=value, key2=value2`
    /// （内核 `extable_type_reg reg="%k0", type="(17 | ((0) << 16))"` 是后者）
    /// 参数体内引用为 `\arg1` / `\(arg1)`，`\@` 是宏展开序号。
    fn try_expand_macro_call(&mut self, line: &str) -> Option<Vec<String>> {
        let t = line.trim();

        // Lines starting with '.' are directives, not macro calls
        if t.starts_with('.') {
            return None;
        }

        let mut parts = t.splitn(2, |c: char| c.is_whitespace());
        let name = parts.next()?;

        let (params, body) = self.macros.get(name)?.clone();
        let rest = parts.next().unwrap_or("").trim().to_string();
        let call_args = Self::split_macro_args(&rest);

        // 按 `key=value` 与位置两种形式塑入形参；没给且无默认值的 `:req` 参数按空处理
        // （GAS 会在此处报错，但本后端对未知宏参缺失不阻断编译）。
        let mut bound: Vec<(String, String)> = Vec::new();
        let mut pos = 0usize;
        for p in &params {
            let mut val: Option<String> = None;
            for a in &call_args {
                if let Some((k, v)) = a.split_once('=') {
                    if k.trim() == p.name {
                        val = Some(v.trim().trim_matches('"').to_string());
                    }
                }
            }
            if val.is_none() {
                if let Some(v) = call_args.get(pos) {
                    if !v.contains('=') {
                        val = Some(v.trim().trim_matches('"').to_string());
                    }
                }
                pos += 1;
            }
            let val = val.or_else(|| p.default.clone()).unwrap_or_default();
            bound.push((p.name.clone(), val));
        }
        // `\@` 是本宏被展开的次数（GAS：从 1 起算）
        self.macro_seq += 1;
        let seq = self.macro_seq.to_string();

        let mut substituted = Vec::with_capacity(body.len());
        for l in &body {
            let mut expanded = l.clone();
            // 长名字先替换，否则 `\r` 会把 `\reg` 的前缀先吃掉头
            bound.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
            for (k, v) in &bound {
                expanded = expanded.replace(&format!("\\({k})"), v);
                expanded = expanded.replace(&format!("\\{k}"), v);
            }
            expanded = expanded.replace("\\@", &seq);
            substituted.push(expanded);
        }
        // 宏体本身还可能含 `.irp`/`.ifc`/嵌套宏调用：代入后递归展开。
        Some(self.expand(&substituted))
    }

    /// 拆宏实参：逗号分隔，但引号内的逗号不拆（`type="(1 | ((2) << 8))"`）。
    fn split_macro_args(rest: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = String::new();
        let mut in_quote = false;
        for c in rest.chars() {
            match c {
                '"' => {
                    in_quote = !in_quote;
                    cur.push(c);
                },
                ',' if !in_quote => {
                    if !cur.trim().is_empty() {
                        out.push(cur.trim().to_string());
                    }
                    cur = String::new();
                },
                _ => cur.push(c),
            }
        }
        if !cur.trim().is_empty() {
            out.push(cur.trim().to_string());
        }
        out
    }

    /// Parse `.include "file"` directive
    ///
    /// # Arguments
    /// * `line` - Line containing `.include` directive
    ///
    /// # Returns
    /// Some(file_path) if valid include, None otherwise
    fn parse_include(line: &str) -> Option<String> {
        let t = line.trim();
        let rest = t.strip_prefix(".include")?;
        let rest = rest.trim();
        let path = rest.trim_matches('"').to_string();
        Some(path)
    }

    /// Parse conditional directives
    ///
    /// Returns condition result (true = include block, false = skip block).
    ///
    /// 支持 GAS 的那几条：`.if/.ifdef/.ifndef/.ifc/.ifb` 与它们的否定形式。
    /// `.ifc a,b` 是**字面量字符串**比较（内核用它比较寄存器名），必须先完成宏参/循环变量
    /// 代入再判——那已由 `expand` 的代入顺序保证。
    fn parse_conditional(&self, line: &str) -> Option<bool> {
        let t = line.trim();

        if let Some(rest) = t.strip_prefix(".ifdef") {
            let sym = rest.trim().to_string();
            return Some(self.symbols.contains_key(&sym));
        }

        if let Some(rest) = t.strip_prefix(".ifndef") {
            let sym = rest.trim().to_string();
            return Some(!self.symbols.contains_key(&sym));
        }

        // `.ifc`/`.ifnc`/`.ifb`/`.ifnb` 要在 `.if` 之前判：否则会被 `.if` 前缀先吃掉
        if let Some(args) = t.strip_prefix(".ifc") {
            return Some(Self::compare_cond(args, true));
        }
        if let Some(args) = t.strip_prefix(".ifnc") {
            return Some(!Self::compare_cond(args, true));
        }
        if let Some(args) = t.strip_prefix(".ifb") {
            return Some(Self::compare_cond(args, false));
        }
        if let Some(args) = t.strip_prefix(".ifnb") {
            return Some(!Self::compare_cond(args, false));
        }

        if t.starts_with(".if") {
            let rest = t.strip_prefix(".if")?.trim();
            // Evaluate simple integer expression
            return Some(self.eval_simple_expr(rest));
        }

        None
    }

    /// `.ifc A, B` / `.ifb A` 的参数比较：去引号、去空白、去 `%` 前缀后比字符串。
    /// `want_eq = true` 时比两个字面量相等；为 false 时判第二个参数是否为空。
    fn compare_cond(args: &str, want_eq: bool) -> bool {
        let mut it = args.splitn(2, ',').map(|s| {
            s.trim()
                .trim_matches('"')
                .chars()
                .filter(|c| !c.is_whitespace() && *c != '%')
                .collect::<String>()
        });
        let a = it.next().unwrap_or_default();
        let b = it.next().unwrap_or_default();
        if want_eq {
            a == b
        } else {
            b.is_empty()
        }
    }

    /// Evaluate simple integer expression for .if directive
    ///
    /// Supports: decimal/hex/binary numbers, operators (+, -, *, /, |, &, ^),
    /// comparisons (==, !=, <, >, <=, >=), negation (!).
    ///
    /// # Returns
    /// True if expression evaluates to non-zero
    fn eval_simple_expr(&self, s: &str) -> bool {
        let s = s.trim();
        // Try to parse as integer
        if let Ok(v) = self.eval_expr_value(s) {
            return v != 0;
        }
        // If can't parse, default to false
        false
    }

    /// Evaluate integer expression (simplified parser)
    ///
    /// Supports basic arithmetic and comparison operations.
    /// Operator precedence: () > !,- > *,/ > +,- > | > & > ^ > comparators
    ///
    /// # Returns
    /// Result<i64, ()> where Err means parse failure
    fn eval_expr_value(&self, s: &str) -> Result<i64, ()> {
        let s = s.trim().trim_end_matches(',');

        // Handle parentheses
        if s.starts_with('(') && s.ends_with(')') {
            return self.eval_expr_value(&s[1..s.len() - 1]);
        }

        // Handle unary !
        if let Some(rest) = s.strip_prefix('!') {
            let v = self.eval_expr_value(rest)?;
            return Ok(if v == 0 { 1 } else { 0 });
        }

        // Handle unary -
        if let Some(rest) = s.strip_prefix('-') {
            if !rest.is_empty() && !rest.contains(['+', '-', '*', '/']) {
                let v = self.eval_expr_value(rest)?;
                return Ok(-v);
            }
        }

        // 移位：内核的 type 字段就是 `(.Lregnr << 8)` 这种形状。
        // `>>` 先于 `<<` 判（两者都是双字符，不会自偷）。
        for op in &[">>", "<<"] {
            if let Some(pos) = self.find_op(s, op) {
                let left = self.eval_expr_value(&s[..pos])?;
                let right = self.eval_expr_value(&s[pos + op.len()..])?;
                let n: u32 = u32::try_from(right).unwrap_or(64).min(64);
                return Ok(if *op == ">>" { left >> n } else { left << n });
            }
        }

        // Handle comparison operators (lowest priority)
        for op in &["==", "!=", "<=", ">=", "<", ">"] {
            if let Some(pos) = self.find_op(s, op) {
                let left = self.eval_expr_value(&s[..pos])?;
                let right = self.eval_expr_value(&s[pos + op.len()..])?;
                return Ok(match *op {
                    "==" => {
                        if left == right {
                            1
                        } else {
                            0
                        }
                    },
                    "!=" => {
                        if left != right {
                            1
                        } else {
                            0
                        }
                    },
                    "<=" => {
                        if left <= right {
                            1
                        } else {
                            0
                        }
                    },
                    ">=" => {
                        if left >= right {
                            1
                        } else {
                            0
                        }
                    },
                    "<" => {
                        if left < right {
                            1
                        } else {
                            0
                        }
                    },
                    ">" => {
                        if left > right {
                            1
                        } else {
                            0
                        }
                    },
                    _ => return Err(()),
                });
            }
        }

        // Handle | & ^ (medium-low priority)
        for op in &['|', '&', '^'] {
            if let Some(pos) = self.rfind_op(s, *op) {
                let left = self.eval_expr_value(&s[..pos])?;
                let right = self.eval_expr_value(&s[pos + 1..])?;
                return Ok(match op {
                    '|' => left | right,
                    '&' => left & right,
                    '^' => left ^ right,
                    _ => return Err(()),
                });
            }
        }

        // Handle + - (medium priority)
        for op in &['+', '-'] {
            if let Some(pos) = self.rfind_op(s, *op) {
                if pos > 0 {
                    let left = self.eval_expr_value(&s[..pos])?;
                    let right = self.eval_expr_value(&s[pos + 1..])?;
                    return Ok(match op {
                        '+' => left + right,
                        '-' => left - right,
                        _ => return Err(()),
                    });
                }
            }
        }

        // Handle * / (high priority)
        for op in &['*', '/'] {
            if let Some(pos) = self.rfind_op(s, *op) {
                if pos == 0 {
                    continue;
                }
                let left = self.eval_expr_value(&s[..pos])?;
                let right = self.eval_expr_value(&s[pos + 1..])?;
                return Ok(match op {
                    '*' => left * right,
                    '/' if right != 0 => left / right,
                    _ => return Err(()),
                });
            }
        }

        // Basic number parsing
        if let Ok(v) = Self::parse_number(s) {
            return Ok(v);
        }
        // 最后才试符号表：`.set .Lregnr, .Lregnr+1` 这类自引用表达式里的名字
        self.symbols.get(s).copied().ok_or(())
    }

    /// 在括号外找一个双字符运算符的位置（比较/移位用）。
    fn find_op(&self, s: &str, op: &str) -> Option<usize> {
        let b = s.as_bytes();
        let mut depth = 0i32;
        let mut i = 0usize;
        while i + op.len() <= b.len() {
            match b[i] {
                b'(' => depth += 1,
                b')' => depth -= 1,
                _ => {},
            }
            if depth == 0 && &s[i..i + op.len()] == op {
                // `a < -1` 这种「比较符后紧跟一元负号」不应当成 `<=`
                if (op == "<" || op == ">") && i + 1 < b.len() && b[i + 1] == b'=' {
                    // 让 `<=`/`>=` 先配；此处已在更长的循环项里试过
                }
                return Some(i);
            }
            i += 1;
        }
        None
    }

    /// 从右往左找一个单字符运算符（保持旧的「低优先级在右边拆」语义），跳过括号内。
    fn rfind_op(&self, s: &str, op: char) -> Option<usize> {
        let b = s.as_bytes();
        let mut depth = 0i32;
        let mut i = b.len();
        while i > 0 {
            i -= 1;
            match b[i] {
                b')' => depth += 1,
                b'(' => depth -= 1,
                _ => {},
            }
            if depth == 0 && b[i] == op as u8 {
                // 移位与比较的双字符运算符里包含同一个字符（`<<` 里的 `<`、`>>` 里的 `>`），
                // 拆单字符时必须跳过，否则 `a << 8` 会被当成 `a <` 加一个奇怪的右边。
                if op == '<' && (i > 0 && b[i - 1] == b'<' || i + 1 < b.len() && b[i + 1] == b'<') {
                    continue;
                }
                if op == '>' && (i > 0 && b[i - 1] == b'>' || i + 1 < b.len() && b[i + 1] == b'>') {
                    continue;
                }
                if op == '='
                    && (i > 0
                        && (b[i - 1] == b'='
                            || b[i - 1] == b'!'
                            || b[i - 1] == b'<'
                            || b[i - 1] == b'>'))
                {
                    continue;
                }
                if op == '&' && i + 1 < b.len() && b[i + 1] == b'&' {
                    continue;
                }
                return Some(i);
            }
        }
        None
    }

    /// Parse number literal (decimal, hex 0x*, binary 0b*)
    fn parse_number(s: &str) -> Result<i64, ()> {
        let s = s.trim();
        if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            i64::from_str_radix(hex, 16).map_err(|_| ())
        } else if let Some(bin) = s.strip_prefix("0b").or_else(|| s.strip_prefix("0B")) {
            i64::from_str_radix(bin, 2).map_err(|_| ())
        } else {
            s.parse().map_err(|_| ())
        }
    }

    /// Collect block until matching end directive (handles nesting)
    ///
    /// # Arguments
    /// * `lines` - All assembly lines
    /// * `start` - Starting line index (after directive)
    /// * `end_directive` - Ending directive name (e.g., ".endr")
    ///
    /// # Returns
    /// (body_lines, end_line_index)
    ///
    /// # Nesting Handling
    /// 开块算 `.rept/.irp/.irpc/.macro/.if*`，收块算 `.endr/.endm/.endif`（任何一种），
    /// 到零层就收尾——`end_directive` 只是调用方的可读入参（历史上它只比一种
    /// 收尾，宏体一嵌套就把宏吃掉）。
    fn collect_block(lines: &[String], start: usize, _end_directive: &str) -> (Vec<String>, usize) {
        let mut body = Vec::new();
        let mut depth = 1;
        let mut i = start;

        while i < lines.len() {
            let t = lines[i].trim();

            // Check for nested directive start
            if Self::is_block_opener(t) {
                depth += 1;
                body.push(lines[i].clone());
            } else if Self::is_block_closer(t) {
                // 闭合的是「任何一种」块尾都要减一层：内核宏体里 `.macro` 套 `.irp` 套
                // `.ifc`，而旧规则只认 `end_directive` 自己那一种 ⇒ `.endm` 永远算不到
                // 零层，宏体一路吃到行尾，宏根本没注册（表现：`__ex_table` 少第三个字，
                // objtool 报「size not a multiple of 12」）。
                depth -= 1;
                if depth <= 0 {
                    return (body, i);
                }
                body.push(lines[i].clone());
            } else {
                body.push(lines[i].clone());
            }
            i += 1;
        }

        (body, lines.len())
    }

    /// 开块指令：`.rept`/`.irp`/`.irpc`/`.macro` 与各种条件（`.if*`，含 `.ifdef`/`.ifc`）。
    fn is_block_opener(t: &str) -> bool {
        t.starts_with(".rept")
            || t.starts_with(".irp")
            || t.starts_with(".macro")
            || t.starts_with(".if")
    }

    /// 收块指令：`.endr`（`.rept`/`.irp`/`.irpc`）、`.endm`（`.macro`）、`.endif`（`.if*`）。
    fn is_block_closer(t: &str) -> bool {
        t.starts_with(".endr") || t.starts_with(".endm") || t.starts_with(".endif")
    }

    /// Collect conditional block (.if/.elseif/.else/.endif)
    ///
    /// # Arguments
    /// * `lines` - All assembly lines
    /// * `start` - Starting line index
    /// * `first_cond` - Whether first block should be included
    ///
    /// # Returns
    /// (selected_branch_lines, endif_line_index)
    ///
    /// # Branch Selection Logic
    /// - First branch: included if first_cond=true
    /// - .else branches: included if previous branches were false
    /// - .elseif branches: simplified (always treated as false)
    fn collect_conditional_block(
        lines: &[String],
        start: usize,
        first_cond: bool,
    ) -> (Vec<String>, usize) {
        let mut current_branch = Vec::new();
        let mut selected_branch: Option<Vec<String>> = None;
        let mut current_is_true = first_cond;
        let mut depth = 1;
        let mut i = start;

        while i < lines.len() {
            let t = lines[i].trim();

            if t.starts_with(".if") || t.starts_with(".ifdef") || t.starts_with(".ifndef") {
                depth += 1;
                if depth == 2 {
                    current_branch.push(lines[i].clone());
                }
            } else if t == ".endif" || t.starts_with(".endif ") {
                depth -= 1;
                if depth == 0 {
                    // End: select current branch if still not selected and true
                    if selected_branch.is_none() && current_is_true {
                        selected_branch = Some(current_branch);
                    }
                    return (selected_branch.unwrap_or_default(), i);
                }
                if depth == 1 {
                    current_branch.push(lines[i].clone());
                }
            } else if depth == 1 && (t == ".else" || t.starts_with(".else ")) {
                // If current branch is true and not yet selected, select it
                if current_is_true && selected_branch.is_none() {
                    selected_branch = Some(current_branch);
                }
                current_branch = Vec::new();
                // .else branch: true only if no branch selected yet
                current_is_true = selected_branch.is_none();
            } else if depth == 1 && t.starts_with(".elseif") {
                if current_is_true && selected_branch.is_none() {
                    selected_branch = Some(current_branch);
                }
                current_branch = Vec::new();
                // .elseif: simplified to false
                current_is_true = false;
            } else {
                if depth == 1 {
                    current_branch.push(lines[i].clone());
                }
            }

            i += 1;
        }

        // No .endif found, return collected content
        if selected_branch.is_none() && current_is_true {
            (current_branch, i)
        } else {
            (selected_branch.unwrap_or_default(), i)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 内核 `extable_type_reg` 的完整形状。**注意入参文本是「已代入操作数」的**：
    /// `%k0` 已由操作数解析代成 `%eax`，`%%\rs` 已落回 `%\rs`——这正是 `emit.rs` 里
    /// 「先解析占位符、再展开宏」的顺序所保证的。形参用空格分隔并带 `:req`，实参是
    /// `key= value` 形式且值带引号；`.set` 计数器在 `.irp` 里累加；命中那一次的
    /// `.long \type + (.Lregnr << 8)` 就是 `__ex_table` 的第三个字。
    fn kernel_ex_table_lines() -> Vec<String> {
        [
            ".macro extable_type_reg type:req reg:req",
            ".set .Lfound, 0",
            ".set .Lregnr, 0",
            ".irp rs,rax,rcx",
            ".ifc \\reg, %\\rs",
            ".set .Lfound, .Lfound+1",
            ".long \\type + (.Lregnr << 8)",
            ".endif",
            ".set .Lregnr, .Lregnr+1",
            ".endr",
            ".set .Lregnr, 0",
            ".irp rs,eax,ecx",
            ".ifc \\reg, %\\rs",
            ".set .Lfound, .Lfound+1",
            ".long \\type + (.Lregnr << 8)",
            ".endif",
            ".set .Lregnr, .Lregnr+1",
            ".endr",
            ".if (.Lfound != 1)",
            ".error \"extable_type_reg: bad register argument\"",
            ".endif",
            ".endm",
            "extable_type_reg reg= \"%eax\" , type= \"(17 | ((0) << 16))\"",
            ".purgem extable_type_reg",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    #[test]
    fn expands_kernel_ex_table_type_reg_macro() {
        let lines = kernel_ex_table_lines();
        let mut ex = AsmDirectiveExpander::new();
        let out = ex.expand(&lines);
        // 宏定义本身不发；`.purgem` 不发；只剩命中那一次的 `.long`。
        let longs: Vec<&String> = out.iter().filter(|l| l.starts_with(".long")).collect();
        assert_eq!(
            longs.len(),
            1,
            "只应留下 `%eax` 命中的那一条 `.long`（第二个 `.irp` 列表的第 0 项）: {out:?}"
        );
        assert!(longs[0].contains("17"), "type 常量必须代入: {}", longs[0]);
        assert!(!longs[0].contains("\\"), "不应残留未代入的宏参: {}", longs[0]);
        assert!(
            !out.iter().any(|l| l.starts_with(".error")),
            ".Lfound == 1 时 .error 分支不该被拉进来: {out:?}"
        );
        assert!(
            !out.iter()
                .any(|l| l.starts_with(".macro") || l.starts_with(".irp")),
            "宏定义与循环结构不得落到发射端: {out:?}"
        );
    }
}
