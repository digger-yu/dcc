// src/codegen/aarch64/asm/expansion.rs
// AArch64 Assembly Expansion - Syntax Sugar Handling
// 
// This module handles expansion of assembly syntax sugar into primitive instructions:
// - Addressing mode expansion (ldr x0, [x1, #64]) → load with offset calculation
// - Register alias resolution (x0 vs w0, X0 vs W0)
// - Convenience pattern expansion
// - Memory access simplification
//
// Phase A Day 4-5 - ASM Module Extraction (9/22/2026)

use std::fmt;
use std::str::FromStr;

/// Error type for expansion operations
#[derive(Debug, Clone)]
pub enum ExpansionError {
    /// Unsupported addressing mode
    UnsupportedAddressingMode(String),
    /// Invalid operand format
    InvalidOperand(String),
    /// Unrecognized instruction pattern
    UnrecognizedPattern(String),
    /// Expansion would produce invalid code
    InvalidExpansion(String),
}

impl fmt::Display for ExpansionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExpansionError::UnsupportedAddressingMode(mode) => {
                write!(f, "unsupported addressing mode: {}", mode)
            }
            ExpansionError::InvalidOperand(op) => {
                write!(f, "invalid operand: {}", op)
            }
            ExpansionError::UnrecognizedPattern(pattern) => {
                write!(f, "unrecognized instruction pattern: {}", pattern)
            }
            ExpansionError::InvalidExpansion(exp) => {
                write!(f, "invalid expansion: {}", exp)
            }
        }
    }
}

impl std::error::Error for ExpansionError {}

/// Result type for expansion operations
pub type ExpandResult<T> = Result<T, ExpansionError>;

/// Represents an expanded instruction sequence
#[derive(Debug, Clone)]
pub struct ExpandedSequence {
    /// Original instruction text
    pub original: String,
    /// List of primitive instructions that replace it
    pub primitives: Vec<String>,
    /// Metadata about the expansion
    pub metadata: ExpansionMetadata,
}

/// Metadata about an expansion
#[derive(Debug, Clone, Default)]
pub struct ExpansionMetadata {
    /// Number of primitives generated
    pub primitive_count: usize,
    /// Was this a simple 1-to-1 expansion?
    pub was_simple: bool,
    /// Description of what was expanded
    pub description: String,
}

/// Expander for assembly syntax sugar
pub struct AsmExpander;

impl AsmExpander {
    /// Expand a single assembly instruction line
    pub fn expand_instruction(line: &str) -> ExpandResult<ExpandedSequence> {
        let trimmed = line.trim();
        
        // Match common patterns and expand them
        
        // LDR with pre-indexed mode: ldr x0, [x1, #64]!
        if let Some(m) = Self::match_ldr_pre_indexed(trimmed) {
            return Ok(Self::expand_ldr_pre_indexed(m));
        }
        
        // LDR with post-indexed mode: ldr x0, [x1], #64
        if let Some(m) = Self::match_ldr_post_indexed(trimmed) {
            return Ok(Self::expand_ldr_post_indexed(m));
        }
        
        // STR with pre-indexed mode: str x0, [x1, #64]!
        if let Some(m) = Self::match_str_pre_indexed(trimmed) {
            return Ok(Self::expand_str_pre_indexed(m));
        }
        
        // STR with post-indexed mode: str x0, [x1], #64
        if let Some(m) = Self::match_str_post_indexed(trimmed) {
            return Ok(Self::expand_str_post_indexed(m));
        }
        
        // MOVE with large immediate (needs multiple instructions for values > 0xfff)
        if let Some(m) = Self::match_large_move_imm(trimmed) {
            return Ok(Self::expand_large_mov(m));
        }
        
        // Simple instructions pass through unchanged
        Ok(ExpandedSequence {
            original: line.to_string(),
            primitives: vec![line.to_string()],
            metadata: ExpansionMetadata {
                primitive_count: 1,
                was_simple: true,
                description: "no expansion needed".to_string(),
            },
        })
    }

    // ============================================================================
    // Load/Store expansions
    // ============================================================================

    /// Match: ldr x0, [x1, #64]!  (pre-indexed with update)
    fn match_ldr_pre_indexed(line: &str) -> Option<(u8, u8, i32)> {
        // Pattern: ldr <rt>, [<rn>, #imm]!
        if let Some((reg_rt, reg_rn, imm)) = Self::parse_load_store_addr(line, true, true) {
            return Some((reg_rt, reg_rn, imm));
        }
        None
    }

    /// Match: ldr x0, [x1], #64  (post-indexed)
    fn match_ldr_post_indexed(line: &str) -> Option<(u8, u8, i32)> {
        // Pattern: ldr <rt>, [<rn>], #imm
        if let Some((reg_rt, reg_rn, imm)) = Self::parse_load_store_addr(line, false, true) {
            return Some((reg_rt, reg_rn, imm));
        }
        None
    }

    /// Match: str x0, [x1, #64]!
    fn match_str_pre_indexed(line: &str) -> Option<(u8, u8, i32)> {
        if let Some((reg_rt, reg_rn, imm)) = Self::parse_store_addr(line, true, true) {
            return Some((reg_rt, reg_rn, imm));
        }
        None
    }

    /// Match: str x0, [x1], #64
    fn match_str_post_indexed(line: &str) -> Option<(u8, u8, i32)> {
        if let Some((reg_rt, reg_rn, imm)) = Self::parse_store_addr(line, false, true) {
            return Some((reg_rt, reg_rn, imm));
        }
        None
    }

    /// Parse register operands from load/store instruction
    fn parse_load_store_addr(
        line: &str,
        is_pre_indexed: bool,
        has_immediate: bool
    ) -> Option<(u8, u8, i32)> {
        // Simple parser for: ldr|str <rt>, [<rn>, #imm] or [<rn>] , #imm
        let parts: Vec<&str> = line.split(|c| c == ',' || c == ' ').collect();
        
        if parts.len() < 4 {
            return None;
        }
        
        // Extract destination/source register
        let rt_part = parts.get(1)?;
        let rt = Self::parse_register(rt_part)?;
        
        // Extract base register
        let rn_part = parts.get(4)?;
        let rn = Self::parse_register(rn_part)?;
        
        // Extract immediate value
        let imm_part = parts.iter().find(|p| p.starts_with('#'))?;
        let imm_str = imm_part.trim_start_matches('#');
        let imm = imm_part.parse::<i32>().ok()?;
        
        Some((rt, rn, imm))
    }

    fn parse_store_addr(line: &str, _is_pre: bool, _has_imm: bool) -> Option<(u8, u8, i32)> {
        Self::parse_load_store_addr(line, _is_pre, _has_imm)
    }

    /// Helper: parse register name to number
    fn parse_register(s: &str) -> Option<u8> {
        let s = s.trim();
        
        // Handle both x0-x30 and X0-X30 formats
        let cleaned: String = s.chars()
            .filter(|c| !c.is_ascii_whitespace())
            .collect();
        
        if cleaned.len() < 2 {
            return None;
        }
        
        let mut chars = cleaned.chars();
        let first = chars.next().unwrap();
        let rest: String = chars.collect();
        
        // Skip lowercase prefix
        let num_part = if first.is_lowercase() && first.is_ascii_alphabetic() {
            rest.clone()
        } else if first.is_uppercase() && first.is_ascii_alphabetic() {
            rest
        } else {
            return None;
        };
        
        num_part.parse::<u8>().ok()
    }

    // ============================================================================
    // Expansion implementations
    // ============================================================================

    /// Expand: ldr x0, [x1, #64]! → mov_k x1, #addr + ldr x0, [x1]
    fn expand_ldr_pre_indexed((rt, rn, imm): (u8, u8, i32)) -> ExpandedSequence {
        let prims = vec![
            format!("add x{}, x{}, #{}", rn, rn, imm),
            format!("ldr x{}, [x{}]", rt, rn),
        ];
        
        ExpandedSequence {
            original: format!("ldr x{}, [x{}, #{}]!", rt, rn, imm),
            primitives: prims,
            metadata: ExpansionMetadata {
                primitive_count: 2,
                was_simple: false,
                description: format!("expand ldr pre-indexed to add+ldr"),
            },
        }
    }

    /// Expand: ldr x0, [x1], #64 → ldr x0, [x1] + add x1, x1, #64
    fn expand_ldr_post_indexed((rt, rn, imm): (u8, u8, i32)) -> ExpandedSequence {
        let prims = vec![
            format!("ldr x{}, [x{}]", rt, rn),
            format!("add x{}, x{}, #{}", rn, rn, imm),
        ];
        
        ExpandedSequence {
            original: format!("ldr x{}, [x{}], #{}", rt, rn, imm),
            primitives: prims,
            metadata: ExpansionMetadata {
                primitive_count: 2,
                was_simple: false,
                description: format!("expand ldr post-indexed to ldr+add"),
            },
        }
    }

    /// Expand: str x0, [x1, #64]!
    fn expand_str_pre_indexed((rt, rn, imm): (u8, u8, i32)) -> ExpandedSequence {
        let prims = vec![
            format!("add x{}, x{}, #{}", rn, rn, imm),
            format!("str x{}, [x{}]", rt, rn),
        ];
        
        ExpandedSequence {
            original: format!("str x{}, [x{}, #{}]!", rt, rn, imm),
            primitives: prims,
            metadata: ExpansionMetadata {
                primitive_count: 2,
                was_simple: false,
                description: format!("expand str pre-indexed to add+str"),
            },
        }
    }

    /// Expand: str x0, [x1], #64
    fn expand_str_post_indexed((rt, rn, imm): (u8, u8, i32)) -> ExpandedSequence {
        let prims = vec![
            format!("str x{}, [x{}]", rt, rn),
            format!("add x{}, x{}, #{}", rn, rn, imm),
        ];
        
        ExpandedSequence {
            original: format!("str x{}, [x{}], #{}", rt, rn, imm),
            primitives: prims,
            metadata: ExpansionMetadata {
                primitive_count: 2,
                was_simple: false,
                description: format!("expand str post-indexed to str+add"),
            },
        }
    }

    // ============================================================================
    // Move immediate expansion
    // ============================================================================

    /// Match: mov x0, #42 where |42| > 0xfff
    fn match_large_move_imm(line: &str) -> Option<(u8, i64)> {
        // Pattern: mov <rt>, #imm
        if let Some((reg, imm)) = Self::parse_move_imm(line) {
            if imm.abs() > 0xfff as i64 {
                return Some((reg, imm));
            }
        }
        None
    }

    fn parse_move_imm(line: &str) -> Option<(u8, i64)> {
        let parts: Vec<&str> = line.split(|c| c == ',' || c == ' ').collect();
        
        if parts.len() < 3 {
            return None;
        }
        
        let rt_part = parts.get(1)?;
        let rt = Self::parse_register(rt_part)?;
        
        let imm_part = parts.iter().find(|p| p.starts_with('#'))?;
        let imm_str = imm_part.trim_start_matches('#');
        let imm = imm_part.parse::<i64>().ok()?;
        
        Some((rt, imm))
    }

    /// Expand large immediate moves using orr + zero register
    fn expand_large_mov((rt, imm): (u8, i64)) -> ExpandedSequence {
        // For large immediates, we need to break it down
        // Strategy: move high bits, then OR in low bits
        
        let mut prims = Vec::new();
        
        if imm >= 0 {
            // Positive: use orr with zero
            let upper = (imm >> 16) as u32;
            let lower = (imm & 0xffff) as u32;
            
            if upper != 0 || lower != 0 {
                // ORR X0, XZR, #imm16 (for each chunk)
                if lower != 0 {
                    prims.push(format!("orr x{}, xzr, #{}", rt, lower));
                }
                if upper != 0 {
                    prims.push(format!("orr x{}, x{}, #{}", rt, rt, upper));
                }
            } else {
                prims.push(format!("mov x{}, #0", rt));
            }
        } else {
            // Negative: sign extension handling
            let abs_val = imm.unsigned_abs();
            prims.push(format!("orr x{}, xzr, #{}", rt, abs_val));
            prims.push(format!("neg x{}, x{}, #0", rt, rt));
        }
        
        ExpandedSequence {
            original: format!("mov x{}, #{}", rt, imm),
            primitives: prims,
            metadata: ExpansionMetadata {
                primitive_count: prims.len(),
                was_simple: false,
                description: format!("expand large imm mov using orr chain"),
            },
        }
    }

    /// Expand compound register operations
    pub fn expand_compound_op(line: &str) -> ExpandResult<ExpandedSequence> {
        // Placeholder for future expansion rules
        // e.g., extending loads: ldrsw x0, [x1]
        Ok(ExpandedSequence {
            original: line.to_string(),
            primitives: vec![line.to_string()],
            metadata: ExpansionMetadata::default(),
        })
    }
}

/// Utility function to batch-expand multiple lines
pub fn expand_block(input: &str) -> Result<Vec<ExpandedSequence>, ExpansionError> {
    input.lines()
        .map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                Ok(ExpandedSequence {
                    original: line.to_string(),
                    primitives: vec![line.to_string()],
                    metadata: ExpansionMetadata {
                        primitive_count: 1,
                        was_simple: true,
                        description: "comment/empty".to_string(),
                    },
                })
            } else {
                AsmExpander::expand_instruction(line)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_register() {
        assert_eq!(AsmExpander::parse_register("x0"), Some(0));
        assert_eq!(AsmExpander::parse_register("X1"), Some(1));
        assert_eq!(AsmExpander::parse_register("lr"), Some(16));
        assert_eq!(AsmExpander::parse_register("W30"), Some(30));
    }

    #[test]
    fn test_expand_ldr_post_indexed() {
        let result = AsmExpander::expand_instruction("ldr x0, [x1], #8").unwrap();
        
        assert_eq!(result.primitives.len(), 2);
        assert!(result.primitives[0].contains("ldr"));
        assert!(result.primitives[1].contains("add"));
        assert!(!result.metadata.was_simple);
    }

    #[test]
    fn test_expand_str_pre_indexed() {
        let result = AsmExpander::expand_instruction("str x0, [x1, #16]!").unwrap();
        
        assert_eq!(result.primitives.len(), 2);
        assert!(result.primitives[0].contains("add"));
        assert!(result.primitives[1].contains("str"));
    }

    #[test]
    fn test_expand_large_move() {
        let result = AsmExpander::expand_instruction("mov x0, #4294967295").unwrap();
        
        assert!(!result.metadata.was_simple);
        assert!(result.primitives.len() > 1);
    }

    #[test]
    fn test_no_expansion_needed() {
        let result = AsmExpander::expand_instruction("add x0, x1, x2").unwrap();
        
        assert!(result.metadata.was_simple);
        assert_eq!(result.primitives.len(), 1);
        assert_eq!(result.original, result.primitives[0]);
    }

    #[test]
    fn test_batch_expand() {
        let input = "ldr x0, [x1], #8\nstr x0, [x2]\nmov x3, #42";
        let results = expand_block(input).unwrap();
        
        assert_eq!(results.len(), 3);
        assert!(!results[0].metadata.was_simple);
        assert!(results[1].metadata.was_simple);
        assert!(!results[2].metadata.was_simple);
    }
}
