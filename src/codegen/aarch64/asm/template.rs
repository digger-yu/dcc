// src/codegen/aarch64/asm/template.rs
// AArch64 Assembly Template String Expansion
// 
// This module handles expansion of inline assembly template strings:
// - Placeholder substitution ($0, $1, etc.)
// - Register name transformations (x0 → w0, X0 → W0)
// - Immediate value formatting (#value, =value)
// - Symbol name resolution for labels and global symbols
//
// Phase A Day 4-5 - ASM Module Extraction (9/22/2026)

use std::fmt;
use std::str::FromStr;

/// Error type for template operations
#[derive(Debug, Clone)]
pub enum TemplateError {
    /// Invalid placeholder format
    InvalidPlaceholder(String),
    /// Placeholder index out of range
    OutOfBounds { placeholder: String, max_index: usize },
    /// Unmatched quotes or delimiters
    UnmatchedDelimiter(char, usize),
    /// Unknown operand specifier
    UnknownSpecifier(char),
}

impl fmt::Display for TemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TemplateError::InvalidPlaceholder(s) => {
                write!(f, "invalid placeholder syntax: {}", s)
            }
            TemplateError::OutOfBounds { placeholder, max_index } => {
                write!(f, "{} references non-existent operand {} (max {})", 
                       placeholder, placeholder.trim_start_matches('$'), max_index)
            }
            TemplateError::UnmatchedDelimiter(delim, line) => {
                write!(f, "unmatched '{}' at line {}", delim, line)
            }
            TemplateError::UnknownSpecifier(specifier) => {
                write!(f, "unknown operand specifier: '{}'", specifier)
            }
        }
    }
}

impl std::error::Error for TemplateError {}

/// Represents a placeholder in the template string
#[derive(Debug, Clone)]
pub struct Placeholder {
    /// Operand index (0-based)
    pub index: usize,
    /// Optional modifier character (e.g., 'w' for word size, 'l' for label)
    pub modifier: Option<char>,
    /// Whether this is a memory reference instead of register
    pub memory: bool,
    /// Raw representation (e.g., "$0", "$1l")
    pub raw: String,
}

impl Placeholder {
    /// Create a new placeholder from parsed components
    pub fn new(index: usize, modifier: Option<char>, memory: bool, raw: String) -> Self {
        Self {
            index,
            modifier,
            memory,
            raw,
        }
    }

    /// Get formatted version based on constraint context
    pub fn format_with_reg(&self, reg_name: &str, constraint_is_output: bool) -> String {
        let prefix = if constraint_is_output && self.memory {
            "[]"
        } else {
            ""
        };
        
        // Apply modifier if present
        if let Some(modifier) = self.modifier {
            match modifier {
                // Lowercase letter means use 32-bit variant
                'w' | 'h' | 'b' if !reg_name.is_empty() => {
                    let lower_case = reg_name.chars().next().unwrap_or('x').to_ascii_lowercase();
                    format!("{}{}{}", prefix, lower_case, &reg_name[1..])
                }
                _ => format!("{}{}", prefix, reg_name)
            }
        } else {
            format!("{}{}", prefix, reg_name)
        }
    }

    /// Format as immediate value
    pub fn format_immediate(&self, value: i64) -> String {
        format!("#{}", value)
    }
}

impl fmt::Display for Placeholder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.raw)
    }
}

/// Expander for assembly template strings
pub struct TemplateExpander;

/// Common patterns for placeholder matching
static PLACEHOLDER_PATTERN_CACHE: std::sync::OnceLock<std::sync::Mutex<Vec<usize>>> = std::sync::OnceLock::new();

/// Find all $N placeholders in template string without full regex support
fn find_placeholders_in_template(template: &str) -> Vec<(usize, Option<char>, String)> {
    let mut results = Vec::new();
    
    let mut chars = template.char_indices().peekable();
    while let Some((idx, c)) = chars.next() {
        if c == '$' {
            // Check next characters for digit
            if let Some(&(next_idx, ch)) = chars.peek() {
                if ch.is_ascii_digit() {
                    // Parse full number
                    let mut num_str = String::new();
                    num_str.push(ch);
                    chars.next();
                    
                    while let Some(&(_, nc)) = chars.peek() {
                        if nc.is_ascii_digit() {
                            num_str.push(nc);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    
                    // Check for optional modifier
                    let modifier = chars.peek()
                        .filter(|&(_, c)| !c.is_whitespace() && !c.is_ascii_digit())
                        .map(|&(_, c)| c);
                    
                    if let Ok(index) = num_str.parse::<usize>() {
                        let raw = format!("${}${}", num_str,
                            modifier.map(|m| m.to_string()).unwrap_or_default());
                        results.push((index, modifier, raw));
                    }
                }
            }
        }
    }
    
    results
}

impl TemplateExpander {
    /// Expand all placeholders in a template string
    /// 
    /// # Arguments
    /// * `template` - The template string with placeholders like "$0", "$1"
    /// * `operands` - List of operand values to substitute
    /// 
    /// # Returns
    /// Expanded template string with all placeholders replaced
    pub fn expand(template: &str, operands: &[OperandInfo]) -> Result<String, TemplateError> {
        let mut result = String::new();
        let last_idx = operands.len() - 1;
        
        // Simple parser without regex dependency
        let placeholders = find_placeholders_in_template(template);
        
        let mut last_end = 0;
        for (idx, modifier, _raw) in placeholders {
            if idx > last_idx {
                result.push_str(&template[last_end..]);
                return Err(TemplateError::OutOfBounds {
                    placeholder: format!("${}", idx),
                    max_index: last_idx,
                });
            }
            
            // Add text between last placeholder and this one
            let placeholder_start = template.find(format!("${}", idx)).ok_or_else(|| {
                TemplateError::InvalidPlaceholder(format!("${}", idx))
            })?;
            
            result.push_str(&template[last_end..placeholder_start]);
            
            // Expand placeholder
            let operand = &operands[idx];
            let expanded = operand.resolve_placeholder(idx, modifier, false);
            result.push_str(&expanded);
            
            last_end = placeholder_start + format!("${}", idx).len();
        }
        
        // Add remaining text after last placeholder
        result.push_str(&template[last_end..]);
        
        Ok(result)
    }

    /// Expand placeholders treating outputs specially
    pub fn expand_with_context(
        template: &str, 
        operands: &[OperandInfo],
        constraints: &[ConstraintContext]
    ) -> Result<String, TemplateError> {
        let mut result = String::new();
        let last_idx = operands.len() - 1;
        
        let placeholders = find_placeholders_in_template(template);
        
        let mut last_end = 0;
        for (idx, modifier, _raw) in placeholders {
            if idx > last_idx {
                result.push_str(&template[last_end..]);
                return Err(TemplateError::OutOfBounds {
                    placeholder: format!("${}", idx),
                    max_index: last_idx,
                });
            }
            
            let placeholder_start = template.find(format!("${}", idx)).ok_or_else(|| {
                TemplateError::InvalidPlaceholder(format!("${}", idx))
            })?;
            
            result.push_str(&template[last_end..placeholder_start]);
            
            let is_output = constraints.get(idx)
                .map(|c| c.is_output)
                .unwrap_or(false);
            
            let operand = &operands[idx];
            let expanded = operand.resolve_placeholder(idx, modifier, is_output);
            result.push_str(&expanded);
            
            last_end = placeholder_start + format!("${}", idx).len();
        }
        
        result.push_str(&template[last_end..]);
        
        Ok(result)
    }

    /// Find all placeholders in a template without expanding
    pub fn find_placeholders(template: &str) -> Vec<Placeholder> {
        let placeholders = find_placeholders_in_template(template);
        
        placeholders.iter()
            .map(|(idx, modifier, raw)| {
                Placeholder::new(*idx, *modifier, false, raw.clone())
            })
            .collect()
    }
}

/// Context information about each constraint
#[derive(Debug, Clone)]
pub struct ConstraintContext {
    /// Whether this is an output constraint
    pub is_output: bool,
    /// Whether it has early-clobber modifier
    pub early_clobber: bool,
    /// Primary constraint character
    pub primary: char,
}

/// Information about an operand for placeholder resolution
#[derive(Debug, Clone)]
pub struct OperandInfo {
    /// Name/description of the operand
    pub name: String,
    /// Register name if allocated (e.g., "x0", "v1")
    pub register: Option<String>,
    /// Memory address if applicable
    pub memory_address: Option<String>,
    /// Immediate value if applicable
    pub immediate_value: Option<i64>,
    /// Symbol name if applicable
    pub symbol_name: Option<String>,
    /// Original C expression/source text
    pub source_text: String,
}

impl OperandInfo {
    /// Resolve this operand as a placeholder replacement
    pub fn resolve_placeholder(
        &self, 
        _idx: usize, 
        modifier: Option<char>,
        is_output: bool
    ) -> String {
        // Priority order: register > memory > immediate > symbol
        if let Some(reg) = &self.register {
            // Apply modifier if present
            if let Some(mod_char) = modifier {
                return self.apply_modifier(reg, mod_char, is_output);
            }
            reg.clone()
        }
        else if let Some(addr) = &self.memory_address {
            if is_output || modifier == Some('m') {
                format!("[{}]", addr)
            } else {
                addr.clone()
            }
        }
        else if let Some(val) = self.immediate_value {
            format!("#{}", val)
        }
        else if let Some(sym) = &self.symbol_name {
            sym.clone()
        }
        else {
            self.source_text.clone()
        }
    }

    /// Apply a modifier to a register name
    fn apply_modifier(&self, reg: &str, mod_char: char, is_output: bool) -> String {
        // Handle register size modifiers
        match mod_char {
            // Convert uppercase to lowercase for 32-bit variant
            'w' if reg.starts_with('X') => {
                format!("w{}", &reg[1..])
            }
            'W' if reg.starts_with('x') => {
                format!("x{}", &reg[1..])
            }
            // Convert Xn to xn style
            'l' if reg.starts_with('X') || reg.starts_with('x') => {
                reg.to_lowercase()
            }
            // Convert x0 to x0.l-style notation
            'u' if reg.starts_with('x') => {
                format!("{}", reg)
            }
            _ => reg.to_string(),
        }
    }
}

/// Parse an operand info from various sources
impl FromStr for OperandInfo {
    type Err = TemplateError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self {
            name: String::new(),
            register: None,
            memory_address: None,
            immediate_value: None,
            symbol_name: None,
            source_text: s.to_string(),
        })
    }
}

/// Pattern matcher for common inline assembly sequences
pub struct AsmPatternMatcher;

impl AsmPatternMatcher {
    /// Match barrier/memory ordering patterns
    /// e.g., "nop" followed by specific instructions
    pub fn is_barrier_pattern(line: &str) -> bool {
        let trimmed = line.trim().to_lowercase();
        
        // ARM memory barriers typically use dmb, dsb, isb instructions
        matches!(trimmed.as_str(), "dmb" | "dsb" | "isb")
    }

    /// Check if this looks like a function prologue pattern
    pub fn is_prologue_pattern(line: &str) -> bool {
        let trimmed = line.trim().to_lowercase();
        
        trimmed.contains("stp") && 
        (trimmed.contains("sp,") || trimmed.contains("fp,"))
    }

    /// Check if this looks like a function epilogue pattern
    pub fn is_epilogue_pattern(line: &str) -> bool {
        let trimmed = line.trim().to_lowercase();
        
        trimmed.contains("ldp") && (trimmed.contains("br lr"))
    }

    /// Match simple move patterns: "mov x0, #val" or "mov x0, x1"
    pub fn is_move_pattern(line: &str) -> bool {
        let trimmed = line.trim().to_lowercase();
        trimmed.starts_with("mov ")
    }

    /// Match load/store patterns
    pub fn is_load_store_pattern(line: &str) -> bool {
        let trimmed = line.trim().to_lowercase();
        trimmed.starts_with("ldr ") || trimmed.starts_with("str ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_simple_placeholder() {
        let template = "add $0, $1, #0";
        let operands = vec![
            OperandInfo {
                name: "dest".to_string(),
                register: Some("x0".to_string()),
                memory_address: None,
                immediate_value: None,
                symbol_name: None,
                source_text: "dest".to_string(),
            },
            OperandInfo {
                name: "src1".to_string(),
                register: Some("x1".to_string()),
                memory_address: None,
                immediate_value: None,
                symbol_name: None,
                source_text: "src1".to_string(),
            },
        ];
        
        let expanded = TemplateExpander::expand(template, &operands).unwrap();
        assert_eq!(expanded, "add x0, x1, #0");
    }

    #[test]
    fn test_expand_memory_reference() {
        let template = "ldr $0, [$1]";
        let operands = vec![
            OperandInfo {
                name: "result".to_string(),
                register: Some("x0".to_string()),
                memory_address: None,
                immediate_value: None,
                symbol_name: None,
                source_text: "result".to_string(),
            },
            OperandInfo {
                name: "addr".to_string(),
                register: None,
                memory_address: Some("x1".to_string()),
                immediate_value: None,
                symbol_name: None,
                source_text: "addr".to_string(),
            },
        ];
        
        let expanded = TemplateExpander::expand(template, &operands).unwrap();
        assert_eq!(expanded, "ldr x0, [x1]");
    }

    #[test]
    fn test_find_placeholders() {
        let template = "mov $0, #42\nstr $1, [$2]";
        let placeholders = TemplateExpander::find_placeholders(template);
        
        assert_eq!(placeholders.len(), 3);
        assert_eq!(placeholders[0].index, 0);
        assert_eq!(placeholders[1].index, 1);
        assert_eq!(placeholders[2].index, 2);
    }

    #[test]
    fn test_invalid_placeholder_index() {
        let template = "mov $5, x0";
        let operands = vec![
            OperandInfo {
                name: "test".to_string(),
                register: Some("x0".to_string()),
                memory_address: None,
                immediate_value: None,
                symbol_name: None,
                source_text: "test".to_string(),
            },
        ];
        
        let result = TemplateExpander::expand(template, &operands);
        assert!(result.is_err());
    }

    #[test]
    fn test_is_move_pattern() {
        assert!(AsmPatternMatcher::is_move_pattern("mov x0, x1"));
        assert!(AsmPatternMatcher::is_move_pattern("MOV X0, #42"));
        assert!(!AsmPatternMatcher::is_move_pattern("add x0, x1, x2"));
    }

    #[test]
    fn test_is_load_store_pattern() {
        assert!(AsmPatternMatcher::is_load_store_pattern("ldr x0, [x1]"));
        assert!(AsmPatternMatcher::is_load_store_pattern("str x0, [x1, #8]"));
        assert!(!AsmPatternMatcher::is_load_store_pattern("bl my_func"));
    }
}
