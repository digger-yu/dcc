//! Byte emission engine for generating actual machine code from encoding specifications
//! 
//! This module takes selected encoding formats and generates the actual byte sequences
//! that represent instruction machine code.

use crate::codegen::data::{encoding_selector::SelectedFormat, instruction_loader::InstructionDefinition};
use anyhow::Result;

/// Register numbers for encoding
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Register {
    Rax = 0,
    Rcx = 1,
    Rdx = 2,
    Rbx = 3,
    Rsp = 4,
    Rbp = 5,
    Rsi = 6,
    Rdi = 7,
    R8 = 8,
    R9 = 9,
    R10 = 10,
    R11 = 11,
    R12 = 12,
    R13 = 13,
    R14 = 14,
    R15 = 15,
}

impl Register {
    /// Get register index (0-15)
    pub fn index(&self) -> u8 {
        *self as u8
    }
}

/// Memory addressing mode information
#[derive(Debug, Clone)]
pub struct MemoryAddress {
    /// Base register (optional)
    pub base: Option<Register>,
    /// Index register (optional)
    pub index: Option<Register>,
    /// Scale factor (1, 2, 4, or 8)
    pub scale: u8,
    /// Displacement value
    pub displacement: i32,
}

impl MemoryAddress {
    /// Create new memory address with optional base
    pub fn with_base(base: Register) -> Self {
        Self {
            base: Some(base),
            ..Default::default()
        }
    }
    
    /// Add index register with scale
    pub fn with_index(mut self, index: Register, scale: u8) -> Self {
        self.index = Some(index);
        self.scale = scale;
        self
    }
    
    /// Set displacement
    pub fn with_displacement(mut self, disp: i32) -> Self {
        self.displacement = disp;
        self
    }
}

impl Default for MemoryAddress {
    fn default() -> Self {
        Self {
            base: None,
            index: None,
            scale: 1,
            displacement: 0,
        }
    }
}

/// Byte emitter for data-driven instruction encoding
pub struct ByteEmitter;

impl ByteEmitter {
    /// Create new emitter instance
    pub fn new() -> Self {
        Self
    }
    
    /// Emit complete instruction bytes based on selected format
    pub fn emit(&self, selected: &SelectedFormat) -> Result<Vec<u8>> {
        self.emit_with_addressing(selected, &MemoryAddress::default())
    }
    
    /// Emit with explicit addressing mode information
    pub fn emit_with_addressing(
        &self, 
        selected: &SelectedFormat,
        address: &MemoryAddress,
    ) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        
        // Emit REX prefix if needed (x86-64 specific)
        if selected.format.rex_prefix {
            self.emit_rex_prefix(&mut bytes, selected);
        }
        
        // Emit opcode bytes
        bytes.extend_from_slice(&selected.format.opcode);
        
        // Emit ModR/M byte if required
        if selected.format.modrm_required {
            self.emit_modrm(&mut bytes, selected, address)?;
        }
        
        // Emit additional bytes (SIB, displacement, immediate)
        if selected.format.extra_bytes || selected.format.escape_bytes.is_some() {
            self.emit_extra_bytes(&mut bytes, selected, address)?;
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
    /// 
    /// ModR/M format:
    ///   Bits 7-6: mod (addressing mode)
    ///   Bits 5-3: reg (register or opcode extension)
    ///   Bits 2-0: r/m (register or memory addressing)
    fn emit_modrm(
        &self,
        bytes: &mut Vec<u8>,
        selected: &SelectedFormat,
        address: &MemoryAddress,
    ) -> Result<()> {
        let mut modrm = 0u8;
        
        // Determine mod field based on addressing mode
        let mod_field = if address.base.is_none() && address.index.is_none() {
            // Register-only addressing: mod=11b
            0b11
        } else if address.displacement == 0 && address.base != Some(Register::Rbp) {
            // Memory with no displacement or RBP base: mod=00b
            0b00
        } else if (-128..=127).contains(&address.displacement) {
            // Small displacement fits in 8-bit signed: mod=01b
            0b01
        } else {
            // Large displacement requires 32-bit: mod=10b
            0b10
        };
        
        modrm |= (mod_field as u8) << 6;
        
        // For now, use register 0 as default reg field
        // In full implementation, this would come from operand analysis
        modrm |= 0b000_00000; // reg = 0
        
        // Determine r/m field
        let rm_field = if let Some(base) = address.base {
            // Base register is present
            base.index()
        } else {
            // No base register - use register 0 as fallback
            0
        };
        
        modrm |= rm_field;
        
        bytes.push(modrm);
        Ok(())
    }
    
    /// Emit extra bytes including SIB, displacement, immediates
    fn emit_extra_bytes(
        &self,
        bytes: &mut Vec<u8>,
        selected: &SelectedFormat,
        address: &MemoryAddress,
    ) -> Result<()> {
        // Handle escape bytes first
        if let Some(ref escapes) = selected.format.escape_bytes {
            bytes.extend_from_slice(escapes);
        }
        
        // Emit displacement if present
        if address.displacement != 0 || (address.base.is_some() && address.base != Some(Register::Rbp)) {
            self.emit_displacement(bytes, address)?;
        }
        
        Ok(())
    }
    
    /// Emit displacement bytes
    fn emit_displacement(
        &self,
        bytes: &mut Vec<u8>,
        address: &MemoryAddress,
    ) -> Result<()> {
        let disp = address.displacement;
        
        // Determine displacement size based on ModR/M mod field
        // If mod=01, use 8-bit signed displacement
        // If mod=10, use 32-bit unsigned displacement
        
        if (-128..=127).contains(&disp) {
            // Small displacement fits in 8-bit signed
            bytes.push((disp as i8) as u8);
        } else {
            // Large displacement requires 32-bit
            bytes.extend_from_slice(&disp.to_le_bytes());
        }
        
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
