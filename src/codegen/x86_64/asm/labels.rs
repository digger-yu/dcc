// src/codegen/x86_64/asm/labels.rs
/// GNU as local numeric label handling for inline assembly
///
/// Extracted from src/codegen/x86_64/mod.rs (Turn 34 ASM module migration)
/// Implements support for GNU assembler local numeric labels:
/// - Definition: `6:` (or any N:)
/// - Forward reference: `6f` (go to next definition of 6)
/// - Backward reference: `6b` (go to previous definition of 6)
///
/// # Design Notes
/// - Scans all lines first to build definition map
/// - Resolves f/b suffixes by finding appropriate definition
/// - Converts to unique internal names to avoid conflicts
/// - Handles both numeric labels (6:) and C-style named labels (found:)
use std::collections::HashMap;

/// GNU as local numeric label resolver
///
/// Maps numeric label references (`N:f`, `N:b`) to unique internal names.
/// Enables safe resolution of forward/backward jumps within inline assembly blocks.
///
/// # Example
/// ```text
/// Original assembly:
///   1:
///     jmp 1f    // Forward jump to next '1:'
///   2:
///     jmp 1b    // Backward jump to previous '1:'
///   
/// After substitution:
///   .Lasm_local_X_1_0:
///     jmp .Lasm_local_X_1_1
///   .Lasm_local_X_1_2:
///     jmp .Lasm_local_X_1_0
/// ```
pub struct AsmLocalLabels {
    /// Definition records: (line_number, original_label_num, internal_name)
    defs: Vec<(usize, u32, String)>,
}

impl AsmLocalLabels {
    /// Scan assembly lines and collect all numeric label definitions
    ///
    /// # Arguments
    /// * `lines` - Vector of assembly instruction lines
    /// * `scope` - Unique scope identifier (prevents name collisions across multiple asm blocks)
    ///
    /// # Algorithm
    /// 1. Iterate through all lines
    /// 2. Identify numeric labels matching pattern `N:` (e.g., `1:`, `42:`)
    /// 3. Assign unique internal name with incrementing serial number
    /// 4. Store (line_idx, original_num, internal_name) tuples
    ///
    /// # Returns
    /// New `AsmLocalLabels` instance ready for label resolution
    pub fn scan(lines: &[String], scope: u32) -> Self {
        let mut defs = Vec::new();
        let mut serial: HashMap<u32, u32> = HashMap::new();

        for (i, line) in lines.iter().enumerate() {
            let t = line.trim();
            if let Some(num) = Self::parse_numeric_label(t) {
                let n = serial.entry(num).or_insert(0);
                let name = format!(".Lasm_local_{scope}_{num}_{n}");
                *n += 1;
                defs.push((i, num, name));
            }
        }

        Self { defs }
    }

    /// Strip AT&T style comments from assembly line
    ///
    /// GNU as AT&T syntax: everything from `#` to end of line is comment
    /// Example: `mov %eax, %ebx # comment` → `mov %eax, %ebx`
    ///
    /// # Arguments
    /// * `s` - Assembly line that may contain comment
    ///
    /// # Returns
    /// Line without trailing comment
    pub fn strip_asm_comment(s: &str) -> &str {
        s.split('#').next().unwrap_or(s).trim_end()
    }

    /// Parse a numeric label from line start
    ///
    /// Recognizes GNU as numeric label syntax: `N:` where N is decimal digits
    /// Strips comments and validates format
    ///
    /// # Arguments
    /// * `s` - Line that may be a label definition
    ///
    /// # Returns
    /// Some(label_number) if valid numeric label, None otherwise
    ///
    /// # Examples
    /// ```rust
    /// use dcc::codegen::x86_64::AsmLocalLabels;
    /// assert_eq!(AsmLocalLabels::parse_numeric_label("6:"), Some(6));
    /// assert_eq!(AsmLocalLabels::parse_numeric_label("42:"), Some(42));
    /// assert_eq!(AsmLocalLabels::parse_numeric_label("mov $1, $2"), None);  // Not a label
    /// assert_eq!(AsmLocalLabels::parse_numeric_label(".Lfoo:"), None);      // Named label (starts with dot)
    /// ```
    pub fn parse_numeric_label(s: &str) -> Option<u32> {
        // Remove trailing colon
        let s = s.strip_suffix(':')?;

        // Strip comments and whitespace
        let s = Self::strip_asm_comment(s).trim();

        // Must be non-empty and all digits
        if s.is_empty() || !s.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }

        s.parse().ok()
    }

    /// Split label+instruction from same line into separate lines
    ///
    /// GNU as allows combining label and instruction: `loop_start: jmp loop_start`
    /// This function separates them for cleaner processing.
    ///
    /// # Algorithm
    /// Looks for `LABEL:` followed by instruction on same line.
    /// Distinguishes from:
    /// - AT&T segment prefixes: `%gs:movl ...`
    /// - Memory operands: `label(%rax)`
    /// - Non-label colons
    ///
    /// # Arguments
    /// * `lines` - Iterator over assembly lines
    ///
    /// # Returns
    /// Expanded lines with labels separated from instructions
    ///
    /// # Examples
    /// Input: `["6:jmp 6f", "ret"]`
    /// Output: `["6:", "jmp 6f", "ret"]`
    pub fn expand_label_insn_lines(lines: impl IntoIterator<Item = String>) -> Vec<String> {
        let mut out = Vec::new();

        for line in lines {
            let trimmed = Self::strip_asm_comment(line.trim()).trim().to_string();

            // Skip empty lines
            if trimmed.is_empty() {
                continue;
            }

            // Look for colon separating label from instruction
            // Must satisfy ALL conditions:
            // 1. No spaces/tabs in label portion (must be single word)
            // 2. No '%' (avoids AT&T segment prefixes like %gs:, %fs:)
            // 3. No '(' (avoids memory addressing like base(idx):)
            // 4. Has content after colon (not just "label:")
            let label_end = if let Some(colon) = trimmed.find(':') {
                let label = &trimmed[..colon];
                if !label.is_empty()
                    && !label.contains(' ')
                    && !label.contains('\t')
                    && !label.contains('%')
                    && !label.contains('(')
                {
                    Some(colon)
                } else {
                    None
                }
            } else {
                None
            };

            if let Some(colon) = label_end {
                let label = Self::strip_asm_comment(&trimmed[..colon]).trim();
                let rest = Self::strip_asm_comment(&trimmed[colon + 1..]).trim();

                // Valid label if:
                // - Numeric (all digits): e.g., "6"
                // - Named (letters, not starting with dot): e.g., "found"
                let is_numeric = label.chars().all(|c| c.is_ascii_digit());
                let is_named = !is_numeric && !label.starts_with('.');

                if is_numeric || is_named {
                    out.push(format!("{}:", label));
                    out.push(rest.to_string());
                    continue;
                }
            }

            // Not a split-able label line, keep as-is
            out.push(trimmed);
        }

        out
    }

    /// Get internal name for label defined at specific line
    ///
    /// # Arguments
    /// * `line_idx` - Zero-based line index in original assembly
    ///
    /// # Returns
    /// Internal name string or None if no label defined at this line
    pub fn name_at(&self, line_idx: usize) -> Option<&str> {
        self.defs
            .iter()
            .find(|(i, _, _)| *i == line_idx)
            .map(|(_, _, n)| n.as_str())
    }

    /// Resolve forward ('f') or backward ('b') label reference
    ///
    /// # Arguments
    /// * `num` - Numeric label number (e.g., 6 in "6f"/"6b")
    /// * `dir` - Direction: 'f' for forward, 'b' for backward
    /// * `at_line` - Current line index (context for search direction)
    ///
    /// # Algorithm
    /// - **Forward ('f')**: Find first definition AFTER current line
    /// - **Backward ('b')**: Find last definition BEFORE current line
    ///
    /// # Returns
    /// Internal label name or None if no matching definition found
    pub fn resolve(&self, num: u32, dir: char, at_line: usize) -> Option<&str> {
        match dir {
            // Forward: find first def with line > current
            'f' => self
                .defs
                .iter()
                .find(|(i, n, _)| *i > at_line && *n == num)
                .map(|(_, _, name)| name.as_str()),

            // Backward: find last def with line < current
            'b' => self
                .defs
                .iter()
                .rfind(|(i, n, _)| *i < at_line && *n == num)
                .map(|(_, _, name)| name.as_str()),

            // Invalid direction
            _ => None,
        }
    }

    /// Substitute numeric label references (N:f, N:b) with internal names
    ///
    /// Converts human-readable label references to unique internal names:
    /// - `6f` → `.Lasm_local_X_6_0` (next def of 6)
    /// - `6b` → `.Lasm_local_X_6_1` (prev def of 6)
    ///
    /// # Arguments
    /// * `line` - Assembly line potentially containing label references
    /// * `at_line` - Current line index (needed for f/b resolution)
    ///
    /// # Pattern Matching
    /// Only matches `N[f|b]` when:
    /// - Preceded by: start-of-line, or non-alphanumeric/_ character
    /// - Followed by: end-of-line, or non-alphanumeric/_ character
    /// - Prevents false matches: `x6f`, `label6f`, `foo6bg`
    ///
    /// # Returns
    /// Line with all numeric label references substituted
    pub fn substitute(&self, line: &str, at_line: usize) -> String {
        let chars: Vec<char> = line.chars().collect();
        let len = chars.len();
        let mut out = String::with_capacity(line.len());
        let mut i = 0;

        while i < len {
            if chars[i].is_ascii_digit() {
                // Start of potential numeric label reference
                let start = i;

                // Consume all consecutive digits
                while i < len && chars[i].is_ascii_digit() {
                    i += 1;
                }

                // Check for 'f' or 'b' suffix
                if i < len && (chars[i] == 'f' || chars[i] == 'b') {
                    // Boundary check: ensure no alphanumeric attachment
                    let before_ok = start == 0
                        || !(chars[start - 1].is_ascii_alphanumeric() || chars[start - 1] == '_');
                    let after_ok = i + 1 >= len
                        || !(chars[i + 1].is_ascii_alphanumeric() || chars[i + 1] == '_');

                    if before_ok && after_ok {
                        // Parse numeric label
                        let num: u32 = chars[start..i]
                            .iter()
                            .collect::<String>()
                            .parse()
                            .unwrap_or(0);

                        // Resolve to internal name
                        if let Some(name) = self.resolve(num, chars[i], at_line) {
                            out.push_str(name);
                            i += 1; // Skip 'f'/'b'
                            continue;
                        }
                    }
                }

                // No match, output digits as-is
                for c in &chars[start..i] {
                    out.push(*c);
                }
            } else {
                // Non-digit character, pass through
                out.push(chars[i]);
                i += 1;
            }
        }

        out
    }
}
