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
    macros: HashMap<String, (Vec<String>, Vec<String>)>,

    /// Current syntax mode: true = Intel, false = AT&T
    intel_syntax: bool,

    /// Alternate macro mode enabled
    alt_macro: bool,
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
                let expanded_body = self.expand(&body);
                for val in &vals {
                    for l in &expanded_body {
                        out.push(
                            l.replace(&format!("\\{sym}", sym = sym), val)
                                .replace(&format!("\\({sym})", sym = sym), val),
                        );
                    }
                }
                i = end_idx + 1;
                continue;
            }

            // .irpc sym, chars ... .endr
            if let Some((sym, chars)) = Self::parse_irpc(&line) {
                let (body, end_idx) = Self::collect_block(lines, i + 1, ".endr");
                let expanded_body = self.expand(&body);
                for c in chars.chars() {
                    for l in &expanded_body {
                        out.push(
                            l.replace(&format!("\\{sym}", sym = sym), &c.to_string())
                                .replace(&format!("\\({sym})", sym = sym), &c.to_string()),
                        );
                    }
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

            // .if/.ifdef/.ifndef ... .elseif/.else ... .endif
            if let Some(cond_result) = Self::parse_conditional(&line) {
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

            // Regular line: pass through
            out.push(line);
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
    /// ```rust,ignore
    /// assert_eq!(AsmDirectiveExpander::parse_rept(".rept 5"), Some(5));
    /// assert_eq!(AsmDirectiveExpander::parse_rept("mov eax, 1"), None);
    /// ```
    fn parse_rept(line: &str) -> Option<usize> {
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
    /// ```rust,ignore
    /// let result = AsmDirectiveExpander::parse_irp(".irp x, 1, 2, 3").unwrap();
    /// assert_eq!(result.0, "x");
    /// assert_eq!(result.1, vec!["1", "2", "3"]);
    /// ```
    fn parse_irp(line: &str) -> Option<(String, Vec<String>)> {
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
    /// ```rust,ignore
    /// let result = AsmDirectiveExpander::parse_irpc(".irpc c, ABC").unwrap();
    /// assert_eq!(result.0, "c");
    /// assert_eq!(result.1, "ABC");
    /// ```
    fn parse_irpc(line: &str) -> Option<(String, String)> {
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
    /// # Arguments
    /// * `line` - Line containing `.macro` definition
    ///
    /// # Returns
    /// Some((macro_name, parameter_names))
    ///
    /// # Example
    /// ```rust,ignore
    /// let result = AsmDirectiveExpander::parse_macro_def(".macro add a, b").unwrap();
    /// assert_eq!(result.0, "ADD");
    /// assert_eq!(result.1, vec!["a", "b"]);
    /// ```
    fn parse_macro_def(line: &str) -> Option<(String, Vec<String>)> {
        let t = line.trim();
        let rest = t.strip_prefix(".macro")?;
        let rest = rest.trim();

        // Split macro name from arguments
        let mut parts = rest.splitn(2, |c: char| c.is_whitespace());
        let name = parts.next()?.trim().to_string();
        let args_rest = parts.next().unwrap_or("").trim();

        let args: Vec<String> = if args_rest.is_empty() {
            Vec::new()
        } else {
            args_rest
                .split(',')
                .map(|s| s.trim().trim_start_matches('\\').to_string())
                .collect()
        };

        Some((name, args))
    }

    /// Attempt to expand a macro call on a line
    ///
    /// Checks if line starts with a known macro name and expands it.
    /// Replaces parameters with actual arguments using `\param` or `\param)` syntax.
    ///
    /// # Arguments
    /// * `line` - Assembly line that may be a macro call
    ///
    /// # Returns
    /// Some(expanded_lines) if it was a macro call, None otherwise
    ///
    /// # Macro Call Format
    /// `MACRO_NAME arg1, arg2, ...`
    /// Parameters inside macro body referenced as `\arg1`, `\arg2`, etc.
    fn try_expand_macro_call(&self, line: &str) -> Option<Vec<String>> {
        let t = line.trim();

        // Lines starting with '.' are directives, not macro calls
        if t.starts_with('.') {
            return None;
        }

        let mut parts = t.splitn(2, |c: char| c.is_whitespace());
        let name = parts.next()?.trim();

        if let Some((args_def, body)) = self.macros.get(name) {
            let rest = parts.next().unwrap_or("").trim();

            // Parse arguments
            let call_args: Vec<String> = if rest.is_empty() {
                Vec::new()
            } else {
                rest.split(',').map(|s| s.trim().to_string()).collect()
            };

            // Expand body with argument substitution
            let mut result = Vec::new();
            for l in body {
                let mut expanded = l.clone();
                for (i, arg_name) in args_def.iter().enumerate() {
                    let val = call_args.get(i).cloned().unwrap_or_default();
                    expanded =
                        expanded.replace(&format!("\\{arg_name}", arg_name = arg_name), &val);
                    expanded =
                        expanded.replace(&format!("\\({arg_name})", arg_name = arg_name), &val);
                }
                result.push(expanded);
            }

            Some(result)
        } else {
            None
        }
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

    /// Parse conditional directives (.if/.ifdef/.ifndef)
    ///
    /// Returns condition result (true = include block, false = skip block).
    /// For `.ifdef`, always returns false (no symbol table in asm context).
    /// For `.ifndef`, always returns true.
    /// For `.if`, evaluates simple integer expression.
    ///
    /// # Arguments
    /// * `line` - Line containing conditional directive
    ///
    /// # Returns
    /// Some(condition_result) or None if not a conditional directive
    fn parse_conditional(line: &str) -> Option<bool> {
        let t = line.trim();

        if let Some(rest) = t.strip_prefix(".ifdef") {
            // .ifdef: always false (no symbol table in asm)
            let _ = rest.trim();
            return Some(false);
        }

        if let Some(rest) = t.strip_prefix(".ifndef") {
            // .ifndef: always true
            let _ = rest.trim();
            return Some(true);
        }

        if let Some(rest) = t.strip_prefix(".if") {
            let rest = rest.trim();
            // Evaluate simple integer expression
            return Some(Self::eval_simple_expr(rest));
        }

        None
    }

    /// Evaluate simple integer expression for .if directive
    ///
    /// Supports: decimal/hex/binary numbers, operators (+, -, *, /, |, &, ^),
    /// comparisons (==, !=, <, >, <=, >=), negation (!).
    ///
    /// # Returns
    /// True if expression evaluates to non-zero
    fn eval_simple_expr(s: &str) -> bool {
        let s = s.trim();
        // Try to parse as integer
        if let Ok(v) = Self::eval_expr_value(s) {
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
    fn eval_expr_value(s: &str) -> Result<i64, ()> {
        let s = s.trim().trim_end_matches(',');

        // Handle parentheses
        if s.starts_with('(') && s.ends_with(')') {
            return Self::eval_expr_value(&s[1..s.len() - 1]);
        }

        // Handle unary !
        if let Some(rest) = s.strip_prefix('!') {
            let v = Self::eval_expr_value(rest)?;
            return Ok(if v == 0 { 1 } else { 0 });
        }

        // Handle unary -
        if let Some(rest) = s.strip_prefix('-') {
            if !rest.is_empty() && !rest.contains(['+', '-', '*', '/']) {
                let v = Self::eval_expr_value(rest)?;
                return Ok(-v);
            }
        }

        // Handle comparison operators (lowest priority)
        for op in &["==", "!=", "<=", ">=", "<", ">"] {
            if let Some(pos) = s.find(op) {
                let left = Self::eval_expr_value(&s[..pos])?;
                let right = Self::eval_expr_value(&s[pos + op.len()..])?;
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
            if let Some(pos) = s.rfind(*op) {
                let left = Self::eval_expr_value(&s[..pos])?;
                let right = Self::eval_expr_value(&s[pos + 1..])?;
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
            if let Some(pos) = s.rfind(*op) {
                if pos > 0 {
                    let left = Self::eval_expr_value(&s[..pos])?;
                    let right = Self::eval_expr_value(&s[pos + 1..])?;
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
            if let Some(pos) = s.rfind(*op) {
                if pos == 0 {
                    continue;
                }
                let left = Self::eval_expr_value(&s[..pos])?;
                let right = Self::eval_expr_value(&s[pos + 1..])?;
                return Ok(match op {
                    '*' => left * right,
                    '/' if right != 0 => left / right,
                    _ => return Err(()),
                });
            }
        }

        // Basic number parsing
        Self::parse_number(s)
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
    /// Counts depth for nested directives (.rept inside .rept, etc.)
    fn collect_block(lines: &[String], start: usize, end_directive: &str) -> (Vec<String>, usize) {
        let mut body = Vec::new();
        let mut depth = 1;
        let mut i = start;

        while i < lines.len() {
            let t = lines[i].trim();

            // Check for nested directive start
            if t.starts_with(".rept")
                || t.starts_with(".irp")
                || t.starts_with(".irpc")
                || t.starts_with(".macro")
                || t.starts_with(".if")
                || t.starts_with(".ifdef")
                || t.starts_with(".ifndef")
            {
                depth += 1;
                body.push(lines[i].clone());
            } else if t == end_directive || t.starts_with(&format!("{end_directive} ")) {
                depth -= 1;
                if depth == 0 {
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
