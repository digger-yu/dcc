//! Data-driven instruction specification system
//!
//! This module provides TOML-based strong-typed instruction specifications with:
//! - Smart encoding format selection based on operands
//! - Automatic machine code byte emission
//! - Validation and test generation support
//!
//! ## Architecture
//!
//! ```text
//! Instruction Loader → Encoding Selector → Byte Emitter → Machine Code
//!      (TOML)              (Pattern Match)    (Binary)       (Vec<u8>)
//! ```
//!
//! ## Usage Example
//!
//! ```rust
//! use dcc::codegen::data::{
//!     bytes_to_hex, ByteEmitter, InstructionLoader, OperandAnalysis, SmartEncodingSelector,
//! };
//!
//! // Load all instructions from the TOML spec directory
//! let mut loader = InstructionLoader::new("src/codegen/data/x86_instructions");
//! let specs = loader.load_all_instructions().expect("load TOML specs");
//!
//! // Get specific instruction
//! let add_instr = specs.get("add").expect("add.toml exists");
//!
//! // Select best encoding for given operands: `add r64, r64`
//! let selector = SmartEncodingSelector::new();
//! let reg_rw = OperandAnalysis {
//!     op_type: "reg".to_string(),
//!     size_bits: Some(64),
//!     is_readonly: false,
//!     is_writeonly: true,
//!     is_readwrite: false,
//! };
//! let reg_ro = OperandAnalysis {
//!     op_type: "reg".to_string(),
//!     size_bits: Some(64),
//!     is_readonly: true,
//!     is_writeonly: false,
//!     is_readwrite: false,
//! };
//! let selected = selector
//!     .select_format(add_instr, &[reg_rw, reg_ro])
//!     .expect("reg/reg must match a format");
//! assert!(selected.confidence > 0.5, "高置信度匹配");
//!
//! // Emit actual bytes and format them as hex text
//! let emitter = ByteEmitter::new();
//! let bytes = emitter.emit(&selected).expect("emit machine code");
//! assert!(!bytes.is_empty(), "至少包含 opcode");
//! assert!(bytes_to_hex(&bytes).starts_with(&format!("{:02X}", bytes[0])));
//! ```
//!
//! ## TOML vs JSON Migration
//!
//! This module uses TOML instead of JSON for:
//! - **Strong typing**: Rust serde derives ensure compile-time type safety
//! - **Human readability**: TOML is more intuitive for configuration
//! - **Comments**: Support for explanatory comments in spec files
//! - **Type constraints**: Enumerated values, required fields enforced by schema

pub mod byte_emitter;
pub mod encoding_selector;
pub mod instruction_loader;
#[cfg(test)]
pub mod test;

// Re-export commonly used items
pub use byte_emitter::{bytes_to_hex, ByteEmitter};
pub use encoding_selector::{OperandAnalysis, SelectedFormat, SmartEncodingSelector};
pub use instruction_loader::{
    EncodingFormat, EncodingSpecification, InstructionDefinition, InstructionLoader,
    InstructionSemantics, OperandSpec, UsageExample,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_data_module_structure() {
        let mut loader = InstructionLoader::new("src/codegen/data/x86_instructions");

        // Test loading
        let _specs = loader
            .load_all_instructions()
            .expect("Failed to load specs");

        assert!(loader.has_instruction("add"));
        assert!(loader.has_instruction("mov"));
    }
}
