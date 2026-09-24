//! Data-driven instruction specification system
//!
//! This module provides TOML-based strong-typed instruction specifications with:
//! - Smart encoding format selection based on operands
//! - Automatic machine code byte emission
//! - Validation and test generation support
//!
//! ## Architecture
//!
//! ```
//! Instruction Loader → Encoding Selector → Byte Emitter → Machine Code
//!      (TOML)              (Pattern Match)    (Binary)       (Vec<u8>)
//! ```
//!
//! ## Usage Example
//!
//! ```rust,no_run
//! use crate::codegen::data::*;
//!
//! // Load all instructions from directory
//! let mut loader = InstructionLoader::new("src/codegen/data/x86_instructions");
//! let specs = loader.load_all_instructions()?;
//!
//! // Get specific instruction
//! let add_instr = specs.get("add").unwrap();
//!
//! // Select best encoding for given operands
//! let selector = SmartEncodingSelector::new();
//! let analysis = vec![
//!     OperandAnalysis { op_type: "reg".to_string(), size_bits: Some(64), ... },
//!     OperandAnalysis { op_type: "reg".to_string(), size_bits: Some(64), ... },
//! ];
//! let selected = selector.select_format(add_instr, &analysis)?;
//!
//! // Emit actual bytes
//! let emitter = ByteEmitter::new();
//! let bytes = emitter.emit(&selected)?;
//! println!("Encoded: {}", bytes_to_hex(&bytes));
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
