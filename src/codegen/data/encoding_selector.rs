//! Smart encoding selector for automatic instruction format selection
//! 
//! This module analyzes IR operands and selects the most appropriate encoding format
//! from the available instruction specifications.

use crate::codegen::data::instruction_loader::{EncodingFormat, InstructionDefinition};
use anyhow::Result;
use std::collections::HashMap;

/// Represents the binding of an operand to a pattern position
#[derive(Debug, Clone)]
pub struct OperandBinding {
    /// Pattern slot index (0-based)
    pub slot: usize,
    
    /// Original operand type classification
    pub op_type: String,
    
    /// Operand size in bits (if applicable)
    pub size_bits: Option<u32>,
}

/// Result of format selection process
#[derive(Debug, Clone)]
pub struct SelectedFormat {
    /// The selected format definition
    pub format: EncodingFormat,
    
    /// Bindings for all operands to pattern slots
    pub bindings: Vec<OperandBinding>,
    
    /// Confidence score (0.0-1.0) - higher means more certain match
    pub confidence: f64,
}

/// Analysis results for operand matching
#[derive(Debug, Clone)]
pub struct OperandAnalysis {
    /// Type category (reg, mem, imm, etc.)
    pub op_type: String,
    
    /// Size in bits
    pub size_bits: Option<u32>,
    
    /// Whether operand is read-only
    pub is_readonly: bool,
    
    /// Whether operand is write-only
    pub is_writeonly: bool,
    
    /// Whether operand can be both read and written
    pub is_readwrite: bool,
}

/// Smart encoding selector engine
pub struct SmartEncodingSelector;

impl SmartEncodingSelector {
    /// Create new selector instance
    pub fn new() -> Self {
        Self
    }
    
    /// Select best encoding format based on operands
    pub fn select_format(
        &self,
        instruction: &InstructionDefinition,
        operands: &[OperandAnalysis],
    ) -> Result<SelectedFormat> {
        // Analyze each available format
        let mut candidates: Vec<(EncodingFormat, f64)> = Vec::new();
        
        for format in &instruction.encoding_spec.formats {
            let confidence = self.evaluate_format(format, operands);
            if confidence > 0.0 {
                candidates.push((format.clone(), confidence));
            }
        }
        
        if candidates.is_empty() {
            return Err(anyhow::anyhow!(
                "No suitable encoding format found for {:?}", 
                instruction.name
            ));
        }
        
        // Sort by confidence score
        candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        
        let (best_format, best_confidence) = candidates.remove(0);
        
        Ok(SelectedFormat {
            format: best_format,
            bindings: self.create_bindings(&best_format, operands),
            confidence: best_confidence,
        })
    }
    
    /// Evaluate how well a format matches given operands
    fn evaluate_format(
        &self,
        format: &EncodingFormat,
        operands: &[OperandAnalysis],
    ) -> f64 {
        let pattern_parts: Vec<&str> = format.operand_pattern.split_whitespace().collect();
        
        // Check if number of operands matches
        if pattern_parts.len() != operands.len() {
            return 0.0;
        }
        
        let mut total_score: f64 = 0.0;
        
        for (i, (pattern_part, analysis)) in pattern_parts.iter().zip(operands.iter()).enumerate() {
            let match_score = self.match_operand_type(pattern_part, analysis);
            total_score += match_score;
        }
        
        // Normalize to 0.0-1.0 range
        total_score / pattern_parts.len() as f64
    }
    
    /// Match individual operand against pattern part
    fn match_operand_type(&self, pattern: &str, analysis: &OperandAnalysis) -> f64 {
        match pattern {
            // Register patterns
            "r" | "r8" | "r16" | "r32" | "r64" => {
                if analysis.op_type == "reg" || analysis.op_type.starts_with("reg") {
                    return 1.0;
                }
            }
            
            // Memory patterns  
            "m" | "mem" => {
                if analysis.op_type == "mem" || analysis.op_type.starts_with("mem") {
                    return 1.0;
                }
            }
            
            // Flexible register/memory patterns
            "r/m" | "r/mem" => {
                if analysis.op_type == "reg" || analysis.op_type == "mem" 
                    || analysis.op_type == "regmem" {
                    return 0.9; // Slightly lower for combined types
                }
            }
            
            // Immediate patterns
            "imm" | "immediate" => {
                if analysis.op_type == "immediate" || analysis.op_type.ends_with("immediate") {
                    return 1.0;
                }
            }
            
            // Combined patterns
            "regmem" => {
                if analysis.op_type == "reg" || analysis.op_type == "mem" 
                    || analysis.op_type == "regmem" {
                    return 0.8;
                }
            }
            
            "regmemimmediate" | "reg_immed" => {
                if analysis.op_type.contains("reg") 
                    || analysis.op_type.contains("mem") 
                    || analysis.op_type.contains("immediate") {
                    return 0.75; // Very flexible pattern
                }
            }
            
            // Special cases
            "al" => {
                if analysis.size_bits == Some(8) && analysis.op_type == "reg" {
                    return 1.0;
                }
            }
            
            // Size-specific registers
            "reg" => {
                if analysis.op_type == "reg" {
                    return 0.8; // Generic reg match
                }
            }
            
            _ => {
                // Unknown pattern, try exact match
                if pattern == analysis.op_type {
                    return 0.5; // Fallback partial match
                }
            }
        }
        
        0.0 // No match
    }
    
    /// Create operand bindings for selected format
    fn create_bindings(
        &self,
        format: &EncodingFormat,
        operands: &[OperandAnalysis],
    ) -> Vec<OperandBinding> {
        let pattern_parts: Vec<&str> = format.operand_pattern.split_whitespace().collect();
        
        pattern_parts
            .iter()
            .enumerate()
            .map(|(slot_idx, pattern_part)| OperandBinding {
                slot: slot_idx,
                op_type: pattern_part.to_string(),
                size_bits: operands.get(slot_idx).and_then(|o| o.size_bits),
            })
            .collect()
    }
    
    /// Get default format for instruction
    pub fn get_default_format<'a>(
        &self,
        instruction: &'a InstructionDefinition,
    ) -> Option<&'a EncodingFormat> {
        if let Some(ref default_name) = instruction.encoding_spec.default_format {
            instruction.encoding_spec.formats
                .iter()
                .find(|f| f.name == *default_name)
        } else {
            instruction.encoding_spec.formats.first()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::data::instruction_loader::InstructionDefinition;
    
    #[test]
    fn test_operand_analysis_match() {
        let selector = SmartEncodingSelector::new();
        
        let analysis = OperandAnalysis {
            op_type: "reg".to_string(),
            size_bits: Some(32),
            is_readonly: true,
            is_writeonly: false,
            is_readwrite: false,
        };
        
        assert!(selector.match_operand_type("r", &analysis) > 0.9);
        assert!(selector.match_operand_type("r/m", &analysis) > 0.8);
    }
}
