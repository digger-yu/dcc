//! Instruction encoding traits for data-driven encoding layer

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Encoded format definition (generated from JSON specs)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncodingFormat {
    /// Format name identifier
    pub name: String,
    
    /// Human-readable description
    pub description: Option<String>,
    
    /// Operand pattern specification
    pub operand_pattern: String,
    
    /// Opcode bytes for this encoding
    pub opcode: Vec<u8>,
    
    /// Whether ModR/M byte is required
    pub modrm_required: bool,
    
    /// Whether REX prefix may be needed
    pub rex_prefix: bool,
    
    /// Escape or extension bytes
    pub escape_bytes: Option<Vec<u8>>,
    
    /// Addressing mode template
    pub addressing_mode: Option<String>,
    
    /// Whether extra bytes are emitted
    pub extra_bytes: bool,
    
    /// Special case marker
    pub special_case: bool,
}

/// Complete instruction definition combining metadata and encodings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstructionDefinition {
    /// Instruction name
    pub name: String,
    
    /// Description
    pub description: String,
    
    /// Available encoding formats
    pub encoding_formats: Vec<EncodingFormat>,
}

/// Common instruction encoder trait
pub trait InstructionEncoder {
    /// Encode instruction with given operands
    fn encode(
        &self,
        operands: &[Operand],
        mode: AssemblyMode,
    ) -> Result<Vec<u8>>;
    
    /// Get instruction name
    fn name(&self) -> &'static str {
        "unknown"
    }
    
    /// Check if instruction supports a particular encoding format
    fn has_format(&self, _format_name: &str) -> bool {
        false
    }
}

/// Assembly mode enumeration
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssemblyMode {
    /// 32-bit protected mode
    Mode32,
    
    /// 64-bit long mode
    Mode64,
    
    /// 16-bit real mode
    Mode16,
}

impl Default for AssemblyMode {
    fn default() -> Self {
        AssemblyMode::Mode64
    }
}

/// Operand types for instructions
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operand {
    Register(u8),
    Memory(Box<MemoryOperand>),
    Immediate(i64),
    Label(String),
}

/// Memory operand structure
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryOperand {
    pub base_reg: Option<u8>,
    pub index_reg: Option<u8>,
    pub scale: u8,
    pub displacement: i64,
}

impl Default for MemoryOperand {
    fn default() -> Self {
        MemoryOperand {
            base_reg: None,
            index_reg: None,
            scale: 0,
            displacement: 0,
        }
    }
}
