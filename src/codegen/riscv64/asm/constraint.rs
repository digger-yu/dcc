// src/codegen/riscv64/asm/constraint/mod.rs - RISC-V Inline Assembly Constraint Parser
// Phase B Day 3 - Extracted from riscv64.rs.bak (2026/9/22)
//
// Source location in riscv64.rs.bak:
// - asm_constraints.rs contains constraint parsing logic used by the backend

use std::collections::HashMap;

/// Operand constraint type for inline assembly
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AsmConstraint {
    pub r#type: ConstraintType,
    pub rw: ConstraintRW,
    pub modifier: char, // 'w', 'h', 'b' for width modifiers
}

/// Constraint types supported by RISC-V backend
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConstraintType {
    Register,      // "r" - General purpose register
    Memory,        // "m" - Memory reference
    Immediate,     // "i", "n" - Immediate value
    FloatRegister, // "f" - Floating point register
    AnyRegister,   // "x" - Any register (no specific class)
}

impl Default for ConstraintType {
    fn default() -> Self {
        ConstraintType::Register
    }
}

/// Read/write direction for constraints
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum ConstraintRW {
    #[default]
    Write,       // "=r" - write-only
    ReadWrite,   // "+r" or "r" - read/write
    Read,        // No explicit prefix - effectively read for output positions
}

/// High-level constraint parser
pub struct ConstraintParser {
    pub rules: Vec<AsmConstraint>,
}

impl ConstraintParser {
    pub fn new() -> Self {
        Self {
            rules: Vec::new(),
        }
    }

    /// Parse a single constraint string (e.g., "r", "=m", "+w")
    pub fn parse(constraint: &str) -> Option<AsmConstraint> {
        let s = constraint.trim();
        if s.is_empty() {
            return None;
        }

        let mut rw = ConstraintRW::ReadWrite;
        let mut modifier = '\0';
        let mut start_idx = 0;

        // Check prefixes
        if s.starts_with('=') {
            rw = ConstraintRW::Write;
            start_idx = 1;
        } else if s.starts_with('+') {
            rw = ConstraintRW::ReadWrite;
            start_idx = 1;
        }

        let remaining = &s[start_idx..];
        
        // Check for modifiers before main constraint character
        let chars: Vec<char> = remaining.chars().collect();
        if !chars.is_empty() && chars[0].is_alphabetic() || chars[0] == '&' {
            let c = chars[0];
            
            // Handle early-clobber modifier
            if c == '&' {
                modifier = '&';
                start_idx += 1;
                if chars.len() > 1 {
                    return Self::parse_simple(chars[start_idx..].iter().collect::<String>(), rw);
                }
            } else {
                return Self::parse_simple(c.to_string(), rw);
            }
        }

        None
    }

    /// Parse simple constraint without prefix
    fn parse_simple(s: String, rw: ConstraintRW) -> Option<AsmConstraint> {
        let chars: Vec<char> = s.chars().collect();
        if chars.is_empty() {
            return None;
        }

        let c = chars[0];
        let modifier = if chars.len() > 1 && chars[1].is_alphabetic() {
            chars[1]
        } else {
            '\0'
        };

        let constraint_type = match c {
            'r' => ConstraintType::Register,
            'm' => ConstraintType::Memory,
            'i' | 'n' => ConstraintType::Immediate,
            'f' => ConstraintType::FloatRegister,
            'x' => ConstraintType::AnyRegister,
            _ => return None,
        };

        Some(AsmConstraint {
            r#type: constraint_type,
            rw,
            modifier,
        })
    }

    /// Parse multiple comma-separated constraints
    pub fn parse_multiple(constraints: &str) -> Vec<AsmConstraint> {
        if constraints.trim().is_empty() {
            return Vec::new();
        }

        constraints.split(',')
            .map(|c| Self::parse(c).unwrap_or_default())
            .collect()
    }

    /// Validate that an operand matches its expected constraint
    pub fn validate(&self, operands: &[OperandInfo]) -> bool {
        if self.rules.len() != operands.len() {
            return false;
        }

        self.rules.iter().zip(operands).all(|(constraint, operand)| {
            self.match_constraint(constraint, operand)
        })
    }

    /// Check if a constraint matches an operand's type
    fn match_constraint(&self, constraint: &AsmConstraint, operand: &OperandInfo) -> bool {
        use ConstraintType::*;

        match constraint.r#type {
            Register => matches!(operand.kind, OperandKind::Register),
            Memory => matches!(operand.kind, OperandKind::Memory),
            Immediate => matches!(operand.kind, OperandKind::Immediate | OperandKind::Register),
            FloatRegister => matches!(operand.kind, OperandKind::FloatRegister),
            AnyRegister => true,
        }
    }
}

/// Operand information for validation
#[derive(Debug, Clone)]
pub struct OperandInfo {
    pub kind: OperandKind,
    pub name: String,
    pub is_early_clobber: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum OperandKind {
    Register,
    Memory,
    Immediate,
    FloatRegister,
}

impl Default for OperandInfo {
    fn default() -> Self {
        Self {
            kind: OperandKind::Register,
            name: String::new(),
            is_early_clobber: false,
        }
    }
}

impl Default for ConstraintParser {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolve C variable names to RISC-V register names
pub fn resolve_asm_operands(
    template: &str,
    output_constraints: &[String],
    input_constraints: &[String],
    output_names: &[String],
    input_names: &[String],
) -> Vec<String> {
    let mut results = Vec::new();
    
    // Process output operands
    for (i, constraint_str) in output_constraints.iter().enumerate() {
        if let Some(constraint) = ConstraintParser::parse(constraint_str) {
            let reg_name = get_register_for_constraint(constraint, i);
            results.push(reg_name);
        }
    }

    // Process input operands
    for (i, constraint_str) in input_constraints.iter().enumerate() {
        if let Some(constraint) = ConstraintParser::parse(constraint_str) {
            let reg_name = get_register_for_constraint(constraint, i);
            results.push(reg_name);
        }
    }
    
    results
}

/// Template expansion with constraint handling
pub fn resolve_asm_template_enhanced(
    template: &str,
    output_constraints: &[String],
    input_constraints: &[String],
    output_names: &[String],
    input_names: &[String],
    registers: &[String],
) -> String {
    let resolved_regs = resolve_asm_operands(template, output_constraints, input_constraints, output_names, input_names);
    
    let mut result = String::new();
    let mut temp_idx = 0;
    
    for char in template.chars() {
        if char == '%' {
            // Find the full %N pattern
            if let Some(num_str) = extract_operand_number(template, &mut temp_idx) {
                if let Some(reg_name) = resolved_regs.get(num_str.parse().unwrap_or(0)) {
                    result.push_str(reg_name);
                } else {
                    result.push('%');
                }
            }
        } else {
            result.push(char);
        }
    }
    
    result
}

fn extract_operand_number(template: &str, index: &mut usize) -> Option<String> {
    // Skip to find the digit after %
    while *index < template.len() {
        let c = template.chars().nth(*index)?;
        if c.is_digit(10) {
            let num = c.to_string();
            *index += 1;
            return Some(num);
        }
        *index += 1;
    }
    None
}

fn get_register_for_constraint(_constraint: AsmConstraint, idx: usize) -> String {
    // Simple mapping for demonstration - real implementation would allocate registers
    format!("a{}", idx.min(7))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_single_constraint() {
        assert_eq!(ConstraintParser::parse("r").unwrap().r#type, ConstraintType::Register);
        assert_eq!(ConstraintParser::parse("m").unwrap().r#type, ConstraintType::Memory);
        assert_eq!(ConstraintParser::parse("i").unwrap().r#type, ConstraintType::Immediate);
        assert_eq!(ConstraintParser::parse("f").unwrap().r#type, ConstraintType::FloatRegister);
    }

    #[test]
    fn test_parse_write_only_constraint() {
        let c = ConstraintParser::parse("=r").unwrap();
        assert_eq!(c.rw, ConstraintRW::Write);
    }

    #[test]
    fn test_parse_multiple_constraints() {
        let constraints = ConstraintParser::parse_multiple("r,m,i");
        assert_eq!(constraints.len(), 3);
        assert_eq!(constraints[0].r#type, ConstraintType::Register);
        assert_eq!(constraints[1].r#type, ConstraintType::Memory);
        assert_eq!(constraints[2].r#type, ConstraintType::Immediate);
    }

    #[test]
    fn test_operand_validation() {
        let parser = ConstraintParser::new();
        let operand = OperandInfo {
            kind: OperandKind::Register,
            name: "a0".to_string(),
            ..Default::default()
        };
        
        let constraint = AsmConstraint {
            r#type: ConstraintType::Register,
            rw: ConstraintRW::ReadWrite,
            modifier: '\0',
        };
        
        assert!(parser.match_constraint(&constraint, &operand));
    }
}
