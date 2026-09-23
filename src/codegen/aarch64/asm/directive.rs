// src/codegen/aarch64/asm/directive.rs
// AArch64 Assembly Directive Handling
// 
// This module provides functionality for parsing and emitting common GNU Assembler (GAS)
// directives used in inline assembly. Supported directives include:
// - Data definition: .word, .quad, .half, .byte
// - Alignment: .align
// - Frame control: .cfi_*, .size, .type
//
// Phase A Day 4-5 - ASM Module Extraction (9/22/2026)

use std::fmt;
use std::str::FromStr;

/// Represents different types of assembly directives
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectiveType {
    /// .word - Define 32-bit word(s)
    Word,
    /// .quad - Define 64-bit quadword(s)
    Quad,
    /// .half - Define 16-bit halfword(s)
    Half,
    /// .byte - Define 8-bit byte(s)
    Byte,
    /// .align - Align to power of 2 boundary
    Align,
    /// .cfi_* - Call Frame Information directives
    Cfi,
    /// .type - Symbol type attribute
    Type,
    /// .size - Symbol size attribute
    Size,
    /// .globl/.global - Make symbol globally visible
    Globl,
    /// .func - Function start marker
    Func,
    /// .p2align - Power-of-2 alignment
    P2Align,
    /// Unknown/unrecognized directive
    Unknown(String),
}

/// Represents a complete assembly directive with its type and arguments
#[derive(Debug, Clone)]
pub struct AsmDirective {
    /// The directive type (e.g., .word, .quad)
    pub directive_type: DirectiveType,
    /// Raw arguments as they appeared in the source
    pub args: String,
    /// Parsed comma-separated argument list
    pub parsed_args: Vec<String>,
}

/// Error type for directive operations
#[derive(Debug, Clone)]
pub enum DirectiveError {
    /// Unrecognized directive name
    Unrecognized(String),
    /// Invalid argument syntax
    InvalidArguments { directive: String, reason: String },
    /// Invalid numeric value
    InvalidNumber { value: String, reason: String },
    /// Missing required argument
    MissingArgument(String),
}

impl fmt::Display for DirectiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DirectiveError::Unrecognized(dir) => {
                write!(f, "unrecognized directive: {}", dir)
            }
            DirectiveError::InvalidArguments { directive, reason } => {
                write!(f, "invalid arguments for {}: {}", directive, reason)
            }
            DirectiveError::InvalidNumber { value, reason } => {
                write!(f, "invalid number '{}': {}", value, reason)
            }
            DirectiveError::MissingArgument(arg) => {
                write!(f, "missing required argument: {}", arg)
            }
        }
    }
}

impl std::error::Error for DirectiveError {}

/// Parse directive type from text (without the leading dot)
fn parse_directive_type(name: &str) -> Result<DirectiveType, DirectiveError> {
    let normalized = name.to_lowercase();
    
    Ok(match normalized.as_str() {
        "word" => DirectiveType::Word,
        "quad" => DirectiveType::Quad,
        "half" => DirectiveType::Half,
        "byte" => DirectiveType::Byte,
        "align" => DirectiveType::Align,
        "cfi" | "cfi_alias" | "cfi_def_cfa" | "cfi_def_cfa_register" | "cfi_def_cfa_offset" |
        "cfi_adjust_cfa_offset" | "cfi_label" | "cfi_undefined" | "cfi_window_start" |
        "cfi_negate_cfa_expression" | "cfi_remember_state" | "cfi_restore_state" |
        "cfi_push_object_stack" | "cfi_pop_object_stack" | "cfi_return_address" => {
            DirectiveType::Cfi
        }
        "type" => DirectiveType::Type,
        "size" => DirectiveType::Size,
        "globl" | "global" => DirectiveType::Globl,
        "func" => DirectiveType::Func,
        "p2align" => DirectiveType::P2Align,
        _ => DirectiveType::Unknown(name.to_string()),
    })
}

/// Parse a single directive line into AsmDirective structure
/// Returns None if the line doesn't start with a directive
pub fn parse_directive(line: &str) -> Option<Result<AsmDirective, DirectiveError>> {
    let trimmed = line.trim();
    
    // Check if this is a directive line
    if !trimmed.starts_with('.') || trimmed.len() < 2 {
        return None;
    }
    
    let directive_text = &trimmed[1..];
    
    // Find the first whitespace or end of line to get directive name
    let space_pos = directive_text.find(|c: char| c.is_whitespace())
        .unwrap_or(directive_text.len());
    
    let directive_name = &directive_text[..space_pos];
    let args = if space_pos < directive_text.len() {
        directive_text[space_pos + 1..].to_string()
    } else {
        String::new()
    };
    
    Some(Ok(AsmDirective {
        directive_type: parse_directive_type(directive_name).ok()?,
        args: args.clone(),
        parsed_args: parse_arguments(&args),
    }))
}

/// Parse comma-separated arguments, handling quoted strings
fn parse_arguments(args: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut escape_next = false;
    
    for c in args.chars() {
        if escape_next {
            current.push(c);
            escape_next = false;
            continue;
        }
        
        match c {
            '\\' => escape_next = true,
            '"' | '\'' => {
                in_quotes = !in_quotes;
                current.push(c);
            }
            ',' if !in_quotes => {
                result.push(current.trim().to_string());
                current = String::new();
            }
            _ => {
                current.push(c);
            }
        }
    }
    
    if !current.trim().is_empty() {
        result.push(current.trim().to_string());
    }
    
    result
}

/// Emit a directive as raw bytes at current position
pub fn emit_directive_bytes(directive: &AsmDirective, offset: &mut usize) -> Vec<u8> {
    match directive.directive_type {
        DirectiveType::Word => emit_word_directive(&directive.parsed_args),
        DirectiveType::Quad => emit_quad_directive(&directive.parsed_args),
        DirectiveType::Half => emit_half_directive(&directive.parsed_args),
        DirectiveType::Byte => emit_byte_directive(&directive.parsed_args),
        DirectiveType::Align => emit_align_directive(&directive.parsed_args),
        DirectiveType::P2Align => emit_p2align_directive(&directive.parsed_args),
        DirectiveType::Size => emit_size_directive(&directive.parsed_args),
        DirectiveType::Globl | DirectiveType::Type | DirectiveType::Func | 
        DirectiveType::Cfi => {
            // These don't emit raw bytes directly, they affect metadata
            vec![]
        }
        DirectiveType::Unknown(_) => vec![],
    }
}

/// Emit .word directive bytes (32-bit values)
fn emit_word_directive(args: &[String]) -> Vec<u8> {
    let mut bytes = Vec::new();
    
    for arg in args {
        if let Some(value) = parse_numeric_arg(arg) {
            bytes.extend_from_slice(&(value as u32).to_le_bytes());
        }
    }
    
    bytes
}

/// Emit .quad directive bytes (64-bit values)
fn emit_quad_directive(args: &[String]) -> Vec<u8> {
    let mut bytes = Vec::new();
    
    for arg in args {
        if let Some(value) = parse_numeric_arg(arg) {
            bytes.extend_from_slice(&(value as u64).to_le_bytes());
        }
    }
    
    bytes
}

/// Emit .half directive bytes (16-bit values)
fn emit_half_directive(args: &[String]) -> Vec<u8> {
    let mut bytes = Vec::new();
    
    for arg in args {
        if let Some(value) = parse_numeric_arg(arg) {
            bytes.extend_from_slice(&(value as u16).to_le_bytes());
        }
    }
    
    bytes
}

/// Emit .byte directive bytes (8-bit values)
fn emit_byte_directive(args: &[String]) -> Vec<u8> {
    let mut bytes = Vec::new();
    
    for arg in args {
        if let Some(value) = parse_numeric_arg(arg) {
            bytes.push((value & 0xff) as u8);
        }
    }
    
    bytes
}

/// Emit alignment padding bytes
fn emit_align_directive(args: &[String]) -> Vec<u8> {
    if args.is_empty() {
        return vec![];
    }
    
    // .align n: align to 2^n bytes
    // Default n=2 if not specified (4-byte alignment)
    let n: usize = parse_numeric_arg(&args[0]).map(|v| v as usize).unwrap_or(2);
    let alignment = 1 << n;
    
    // Simple alignment - pad with NOPs (0xd503201f)
    let mut bytes = Vec::new();
    let nop = 0xd503201fu32;
    
    while bytes.len() % alignment != 0 {
        bytes.extend_from_slice(&nop.to_le_bytes());
    }
    
    bytes
}

/// Emit power-of-2 alignment
fn emit_p2align_directive(args: &[String]) -> Vec<u8> {
    if args.is_empty() {
        return vec![];
    }
    
    // .p2align n[,max][,val]
    let n: usize = parse_numeric_arg(&args[0]).map(|v| v as usize).unwrap_or(0);
    let alignment = 1 << n;
    
    let mut bytes = Vec::new();
    let nop = 0xd503201fu32;
    
    while bytes.len() % alignment != 0 {
        bytes.extend_from_slice(&nop.to_le_bytes());
    }
    
    bytes
}

/// Emit .size directive (no bytes emitted, just metadata)
fn emit_size_directive(_args: &[String]) -> Vec<u8> {
    // .size affects symbol table, no actual code bytes
    vec![]
}

/// Parse a numeric argument (decimal, hex, or symbolic)
fn parse_numeric_arg(arg: &str) -> Option<i64> {
    let trimmed = arg.trim();
    
    // Try hex format
    if trimmed.starts_with("0x") || trimmed.starts_with("0X") {
        return i64::from_str_radix(&trimmed[2..], 16).ok();
    }
    
    // Try decimal format
    if let Ok(val) = trimmed.parse::<i64>() {
        return Some(val);
    }
    
    // Return None for symbolic references we can't resolve
    None
}

/// Convert directive to textual representation
impl fmt::Display for AsmDirective {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let prefix = match &self.directive_type {
            DirectiveType::Word => ".word",
            DirectiveType::Quad => ".quad",
            DirectiveType::Half => ".half",
            DirectiveType::Byte => ".byte",
            DirectiveType::Align => ".align",
            DirectiveType::P2Align => ".p2align",
            DirectiveType::Size => ".size",
            DirectiveType::Globl => ".globl",
            DirectiveType::Type => ".type",
            DirectiveType::Func => ".func",
            DirectiveType::Cfi => ".cfi",
            DirectiveType::Unknown(name) => return write!(f, ".{}", name),
        };
        
        write!(f, "{} {}", prefix, self.args.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_word_directive() {
        let line = ".word 42, 0x10";
        let result = parse_directive(line).unwrap().unwrap();
        
        assert_eq!(result.directive_type, DirectiveType::Word);
        assert_eq!(result.parsed_args.len(), 2);
        assert_eq!(result.parsed_args[0], "42");
        assert_eq!(result.parsed_args[1], "0x10");
    }

    #[test]
    fn test_parse_quadruple_directive() {
        let line = ".quad 0x123456789abcdef0";
        let result = parse_directive(line).unwrap().unwrap();
        
        assert_eq!(result.directive_type, DirectiveType::Quad);
        assert_eq!(result.parsed_args[0], "0x123456789abcdef0");
    }

    #[test]
    fn test_parse_align_directive() {
        let line = ".align 3";
        let result = parse_directive(line).unwrap().unwrap();
        
        assert_eq!(result.directive_type, DirectiveType::Align);
        assert_eq!(result.parsed_args[0], "3");
    }

    #[test]
    fn test_emit_word_bytes() {
        let args = vec!["42".to_string(), "255".to_string()];
        let bytes = emit_word_directive(&args);
        
        assert_eq!(bytes.len(), 8); // 2 words * 4 bytes each
        assert_eq!(&bytes[0..4], &42u32.to_le_bytes());
        assert_eq!(&bytes[4..8], &255u32.to_le_bytes());
    }

    #[test]
    fn test_emit_byte_bytes() {
        let args = vec!["1".to_string(), "2".to_string(), "3".to_string()];
        let bytes = emit_byte_directive(&args);
        
        assert_eq!(bytes.len(), 3);
        assert_eq!(bytes, vec![1, 2, 3]);
    }

    #[test]
    fn test_parse_arguments_with_commas() {
        let args = "x0, x1, #42";
        let result = parse_arguments(args);
        
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], "x0");
        assert_eq!(result[1], "x1");
        assert_eq!(result[2], "#42");
    }
}
