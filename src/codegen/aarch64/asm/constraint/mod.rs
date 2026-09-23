// src/codegen/aarch64/asm/constraint/mod.rs
// AArch64 Inline Assembly Constraint Parsing
// 
// This module handles parsing and validation of inline assembly constraints:
// - Input constraints: r, m, i, n, f, etc.
// - Output constraints: =r, =m, =w, etc.
// - Read-write constraints: +r, +m, etc.
// - Early-clobber constraints: &=r, etc.
//
// Phase A Day 4-5 - ASM Module Extraction (9/22/2026)

use std::fmt;
use std::str::FromStr;

/// Error type for constraint operations
#[derive(Debug, Clone)]
pub enum ConstraintError {
    /// Invalid constraint syntax
    InvalidSyntax(String),
    /// Unknown constraint type
    UnknownConstraint(char),
    /// Constraint incompatible with operand type
    Incompatible { constraint: String, operand_type: String },
    /// Multiple constraints specified where only one allowed
    MultipleConstraints(String),
}

impl fmt::Display for ConstraintError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConstraintError::InvalidSyntax(s) => {
                write!(f, "invalid constraint syntax: {}", s)
            }
            ConstraintError::UnknownConstraint(c) => {
                write!(f, "unknown constraint modifier: '{}'", c)
            }
            ConstraintError::Incompatible { constraint, operand_type } => {
                write!(f, "constraint '{}' incompatible with type '{}'", constraint, operand_type)
            }
            ConstraintError::MultipleConstraints(s) => {
                write!(f, "multiple constraints in single field: {}", s)
            }
        }
    }
}

impl std::error::Error for ConstraintError {}

/// Represents the direction/mode of an assembly constraint
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConstraintMode {
    /// Input-only constraint
    Input,
    /// Output-only constraint
    Output,
    /// Read-write (input-output) constraint
    InputOutput,
    /// Memory constraint (can be read or written)
    Memory,
}

/// Represents a parsed constraint with its components
#[derive(Debug, Clone)]
pub struct Constraint {
    /// Mode: input, output, or read-write
    pub mode: ConstraintMode,
    /// Primary constraint character (r, m, i, f, etc.)
    pub primary: char,
    /// Optional early-clobber modifier (&)
    pub early_clobber: bool,
    /// Additional modifiers (e.g., "x" for x86-style, "z" for zero-extending)
    /// For AArch64, typically empty or specifies register class
    pub modifiers: Vec<String>,
}

impl Constraint {
    /// Create a new input constraint
    pub fn input(primary: char, modifiers: Vec<String>) -> Self {
        Self {
            mode: ConstraintMode::Input,
            primary,
            early_clobber: false,
            modifiers,
        }
    }

    /// Create a new output constraint
    pub fn output(primary: char, modifiers: Vec<String>) -> Self {
        Self {
            mode: ConstraintMode::Output,
            primary,
            early_clobber: false,
            modifiers,
        }
    }

    /// Create a new read-write constraint
    pub fn input_output(primary: char, modifiers: Vec<String>) -> Self {
        Self {
            mode: ConstraintMode::InputOutput,
            primary,
            early_clobber: false,
            modifiers,
        }
    }

    /// Check if this is an output constraint
    pub fn is_output(&self) -> bool {
        matches!(self.mode, ConstraintMode::Output | ConstraintMode::InputOutput)
    }

    /// Check if this has early-clobber modifier
    pub fn is_early_clobber(&self) -> bool {
        self.early_clobber
    }

    /// Get primary constraint character
    pub fn primary_char(&self) -> char {
        self.primary
    }

    /// Check if constraint allows register allocation
    pub fn allows_register(&self) -> bool {
        matches!(self.primary, 'r' | 'd' | 's')
    }

    /// Check if constraint is memory-based
    pub fn is_memory(&self) -> bool {
        matches!(self.primary, 'm' | 'Z')
    }

    /// Check if constraint accepts immediate values
    pub fn allows_immediate(&self) -> bool {
        matches!(self.primary, 'i' | 'n' | 'I' | 'J' | 'K' | 'L' | 'M')
    }

    /// Check if constraint accepts floating-point operands
    pub fn allows_float(&self) -> bool {
        matches!(self.primary, 'f')
    }
}

impl fmt::Display for Constraint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut str = String::new();
        
        // Add early-clobber prefix if present
        if self.early_clobber {
            str.push('&');
        }
        
        // Add output prefix if present
        match self.mode {
            ConstraintMode::Output => str.push('='),
            ConstraintMode::InputOutput => str.push('+'),
            _ => {}
        }
        
        // Add primary constraint
        str.push(self.primary);
        
        // Add modifiers separated by commas
        if !self.modifiers.is_empty() {
            str.push(':');
            str.push_str(&self.modifiers.join(","));
        }
        
        write!(f, "{}", str)
    }
}

/// Parser for constraint strings
pub struct ConstraintParser;

impl ConstraintParser {
    /// Parse a single constraint from text
    pub fn parse(input: &str) -> Result<Constraint, ConstraintError> {
        let trimmed = input.trim();
        
        if trimmed.is_empty() {
            return Err(ConstraintError::InvalidSyntax(
                "empty constraint".to_string()
            ));
        }
        
        let mut chars = trimmed.chars().peekable();
        let mut early_clobber = false;
        let mut mode = ConstraintMode::Input;
        let mut primary: Option<char> = None;
        let mut modifiers: Vec<String> = Vec::new();
        
        // Check for early-clobber
        if chars.peek() == Some(&'&') {
            chars.next();
            early_clobber = true;
        }
        
        // Check for output/input-output prefixes
        match chars.peek() {
            Some(&'=') => {
                chars.next();
                mode = ConstraintMode::Output;
            }
            Some(&'+') => {
                chars.next();
                mode = ConstraintMode::InputOutput;
            }
            _ => {}
        }
        
        // Skip whitespace after prefixes
        while let Some(&' ') = chars.peek() {
            chars.next();
        }
        
        // Get primary constraint character
        primary = Some(chars.next().ok_or_else(|| {
            ConstraintError::InvalidSyntax(trimmed.to_string())
        })?);
        
        // Check for colon-separated modifiers
        if chars.peek() == Some(&':') {
            chars.next();
            
            // Parse comma-separated modifiers
            loop {
                let mut modifier = String::new();
                
                while let Some(&c) = chars.peek() {
                    if c == ',' {
                        break;
                    } else if c.is_whitespace() {
                        break;
                    } else {
                        modifier.push(chars.next().unwrap());
                    }
                }
                
                if !modifier.is_empty() {
                    modifiers.push(modifier);
                }
                
                if chars.peek() == Some(&',') {
                    chars.next();
                    
                    // Skip whitespace after comma
                    while let Some(&' ') = chars.peek() {
                        chars.next();
                    }
                } else {
                    break;
                }
            }
        }
        
        let p = primary.ok_or_else(|| {
            ConstraintError::InvalidSyntax(trimmed.to_string())
        })?;
        
        Ok(Constraint {
            mode,
            primary: p,
            early_clobber,
            modifiers,
        })
    }

    /// Parse multiple comma-separated constraints
    /// Used when multiple operands share constraints
    pub fn parse_multiple(input: &str) -> Result<Vec<Constraint>, ConstraintError> {
        let trimmed = input.trim();
        
        if trimmed.is_empty() {
            return Err(ConstraintError::InvalidSyntax(
                "empty constraint list".to_string()
            ));
        }
        
        let mut constraints = Vec::new();
        let mut depth = 0;
        let mut current = String::new();
        
        for c in trimmed.chars() {
            match c {
                '(' => {
                    depth += 1;
                    current.push(c);
                }
                ')' => {
                    depth -= 1;
                    current.push(c);
                }
                ',' if depth == 0 => {
                    if let Ok(constraint) = Self::parse(&current) {
                        constraints.push(constraint);
                    }
                    current.clear();
                }
                _ => {
                    current.push(c);
                }
            }
        }
        
        if !current.is_empty() {
            if let Ok(constraint) = Self::parse(&current) {
                constraints.push(constraint);
            }
        }
        
        if constraints.is_empty() {
            Err(ConstraintError::InvalidSyntax(trimmed.to_string()))
        } else {
            Ok(constraints)
        }
    }

    /// Validate that a constraint is compatible with an operand type
    pub fn validate_compatibility(constraint: &Constraint, operand_type: &str) -> Result<(), ConstraintError> {
        // Type-specific validation rules
        
        // Register constraints must have integer or float types
        if constraint.allows_register() {
            match operand_type {
                "int" | "long" | "short" | "char" | "pointer" | "reg" => Ok(()),
                "float" | "double" if constraint.allows_float() => Ok(()),
                _ => Err(ConstraintError::Incompatible {
                    constraint: constraint.to_string(),
                    operand_type: operand_type.to_string(),
                }),
            }
        }
        // Memory constraints must have memory-accessible types
        else if constraint.is_memory() {
            match operand_type {
                "memory" | "ptr" | "array" => Ok(()),
                _ => Err(ConstraintError::Incompatible {
                    constraint: constraint.to_string(),
                    operand_type: operand_type.to_string(),
                }),
            }
        }
        // Immediate constraints must have numeric types
        else if constraint.allows_immediate() {
            match operand_type {
                "int" | "long" | "const" => Ok(()),
                _ => Err(ConstraintError::Incompatible {
                    constraint: constraint.to_string(),
                    operand_type: operand_type.to_string(),
                }),
            }
        }
        else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_register_constraint() {
        let constraint = ConstraintParser::parse("r").unwrap();
        
        assert_eq!(constraint.primary_char(), 'r');
        assert_eq!(constraint.mode, ConstraintMode::Input);
        assert!(!constraint.early_clobber);
        assert!(constraint.allows_register());
    }

    #[test]
    fn test_parse_output_constraint() {
        let constraint = ConstraintParser::parse("=").unwrap_err();
        // Single '=' without constraint character should fail
        
        let constraint = ConstraintParser::parse("=r").unwrap();
        
        assert_eq!(constraint.primary_char(), 'r');
        assert_eq!(constraint.mode, ConstraintMode::Output);
    }

    #[test]
    fn test_parse_readwrite_constraint() {
        let constraint = ConstraintParser::parse("+r").unwrap();
        
        assert_eq!(constraint.mode, ConstraintMode::InputOutput);
    }

    #[test]
    fn test_parse_early_clobber_constraint() {
        let constraint = ConstraintParser::parse("&=r").unwrap();
        
        assert!(constraint.early_clobber);
        assert_eq!(constraint.mode, ConstraintMode::Output);
    }

    #[test]
    fn test_parse_memory_constraint() {
        let constraint = ConstraintParser::parse("m").unwrap();
        
        assert!(constraint.is_memory());
        assert_eq!(constraint.mode, ConstraintMode::Input);
    }

    #[test]
    fn test_parse_immediate_constraint() {
        let constraint = ConstraintParser::parse("i").unwrap();
        
        assert!(constraint.allows_immediate());
    }

    #[test]
    fn test_display_constraint() {
        let constraint = ConstraintParser::parse("=&r").unwrap();
        assert_eq!(constraint.to_string(), "=&r");
        
        let constraint = ConstraintParser::parse("+m").unwrap();
        assert_eq!(constraint.to_string(), "+m");
    }

    #[test]
    fn test_parse_constraint_with_modifiers() {
        let constraint = ConstraintParser::parse("=w:q0").unwrap();
        assert_eq!(constraint.primary_char(), 'w');
        assert_eq!(constraint.modifiers.len(), 1);
        assert_eq!(constraint.modifiers[0], "q0");
    }

    #[test]
    fn test_invalid_constraint() {
        let result = ConstraintParser::parse("");
        assert!(result.is_err());
    }
}
