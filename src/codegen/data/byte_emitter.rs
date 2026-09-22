//! Byte emission engine for generating actual machine code from encoding specifications
//! 
//! This module takes selected encoding formats and generates the actual byte sequences
// that represent instruction machine code.

use crate::codegen::data::{encoding_selector::SelectedFormat, instruction_loader::InstructionDefinition};
use anyhow::Result;

/// Byte emitter for data-driven instruction encoding
pub struct ByteEmitter;

impl ByteEmitter {
    /// Create new emitter instance
    pub fn new() -> Self {
        Self
    }
    
    /// Emit complete instruction bytes based on selected format
    pub fn emit(&self, selected: &SelectedFormat) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        
        // Emit REX prefix if needed (x86-64 specific)
        if selected.format.rex_prefix {
            self.emit_rex_prefix(&mut bytes, selected);
        }
        
        // Emit opcode bytes
        bytes.extend_from_slice(&selected.format.opcode);
        
        // Emit ModR/M byte if required
        if selected.format.modrm_required {
            self.emit_modrm(&mut bytes, selected);
        }
        
        // Emit additional bytes (SIB, displacement, immediate)
        if selected.format.extra_bytes || selected.format.escape_bytes.is_some() {
            self.emit_extra_bytes(&mut bytes, selected)?;
        }
        
        Ok(bytes)
    }
    
    /// Emit REX prefix bytes for x86-64 extended features
    fn emit_rex_prefix(
        &self,
        bytes: &mut Vec<u8>,
        selected: &SelectedFormat,
    ) {
        // In real implementation, this would analyze operands to set correct REX bits
        // For now, emit a basic REX.W=0, R=0, X=0, B=0 prefix when needed
        if !bytes.is_empty() || selected.format.opcode.len() > 1 {
            // Check if we need REX.W for 64-bit operations
            // This is simplified - actual implementation needs operand size analysis
            
            // Basic REX prefix: 0x40 + flags
            // For simplicity, assume we might need 64-bit mode
            let rex_byte = 0x48u8;
            
            // Only emit if next byte is valid instruction opcode
            if !bytes.is_empty() && bytes[0] <= 255 {
                bytes.insert(0, rex_byte);
            } else if bytes.is_empty() && selected.format.opcode.first().map_or(false, |&b| b <= 255) {
                // Insert before opcodes
                bytes.insert(0, rex_byte);
            }
        }
    }
    
    /// Emit ModR/M byte for memory addressing
    fn emit_modrm(&self, bytes: &mut Vec<u8>, selected: &SelectedFormat) {
        // Placeholder - in real implementation this would:
        // 1. Analyze address mode from format description
        // 2. Encode mod, reg, r/m fields
        // 3. Add displacement/SIB as needed
        
        // For now, emit a simple register-to-register ModR/M
        // Mod=11 (register), reg=0, r/m=0 => 0xC0
        bytes.push(0xC0u8);
    }
    
    /// Emit extra bytes including SIB, displacement, immediates
    fn emit_extra_bytes(
        &self,
        bytes: &mut Vec<u8>,
        selected: &SelectedFormat,
    ) -> Result<()> {
        // Handle escape bytes first
        if let Some(ref escapes) = selected.format.escape_bytes {
            bytes.extend_from_slice(escapes);
        }
        
        // Additional handling for displacement/immediate would go here
        // This requires operand type/size information
        
        Ok(())
    }
    
    /// Generate multiple variant encodings for testing/validation
    pub fn generate_variants(
        &self,
        instruction: &InstructionDefinition,
    ) -> Result<Vec<(String, Vec<u8>)>> {
        let mut variants = Vec::new();
        
        for format in &instruction.encoding_spec.formats {
            let format_name = format.name.clone();
            
            // Create dummy selection for testing
            let dummy_selection = SelectedFormat {
                format: format.clone(),
                bindings: vec![],
                confidence: 1.0,
            };
            
            let encoded = self.emit(&dummy_selection)?;
            variants.push((format_name, encoded));
        }
        
        Ok(variants)
    }
}

/// Convert byte sequence to hex string for display
pub fn bytes_to_hex(bytes: &[u8]) -> String {
    bytes.iter()
         .map(|b| format!("{:02X}", b))
         .collect::<Vec<_>>()
         .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_emit_simple_instruction() {
        let emitter = ByteEmitter::new();
        
        let format = crate::codegen::data::instruction_loader::EncodingFormat {
            name: "test".to_string(),
            description: None,
            operand_pattern: "r m".to_string(),
            opcode: vec![0xC7, 0xC0],
            modrm_required: true,
            rex_prefix: false,
            escape_bytes: None,
            addressing_mode: None,
            extra_bytes: false,
            special_case: false,
        };
        
        let selection = SelectedFormat {
            format,
            bindings: vec![],
            confidence: 1.0,
        };
        
        let bytes = emitter.emit(&selection).unwrap();
        
        // Should have at least 2 bytes plus ModR/M
        assert_eq!(bytes.len(), 3);
        assert_eq!(bytes[0], 0xC7);
        assert_eq!(bytes[1], 0xC0);
    }
    
    #[test]
    fn test_bytes_to_hex() {
        let data = vec![0xAB, 0xCD, 0xEF];
        assert_eq!(bytes_to_hex(&data), "AB CD EF");
    }
}
