//! Data-driven instruction specification loader using TOML format
//!
//! This module provides strong-typed instruction specifications for data-driven code generation.
//! TOML is preferred over JSON due to its type safety and human-readable format.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Package metadata block used by the TOML spec files.
///
/// In the on-disk wire layout `[package]` is the first table header and the
/// identity fields (`arch`/`aliases`) are written before any subsequent
/// table header, so TOML nests them inside this table.
#[derive(Debug, Deserialize)]
struct PackageInfo {
    name: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    description: Option<String>,
    arch: String,
    #[serde(default)]
    aliases: Vec<String>,
}

/// Wire layout of a TOML instruction spec: identity lives under
/// `[package]` while semantics are named `class_semantics`.
#[derive(Debug, Deserialize)]
struct RawInstruction {
    package: PackageInfo,
    #[serde(default)]
    operands: Vec<OperandSpec>,
    encoding_spec: EncodingSpecification,
    #[serde(default)]
    class_semantics: Option<InstructionSemantics>,
    #[serde(default)]
    examples: Vec<UsageExample>,
}

impl RawInstruction {
    fn into_definition(self) -> InstructionDefinition {
        InstructionDefinition {
            name: self.package.name,
            version: self.package.version,
            description: self.package.description,
            arch: self.package.arch,
            aliases: self.package.aliases,
            operands: self.operands,
            encoding_spec: self.encoding_spec,
            semantics: self.class_semantics,
            examples: self.examples,
        }
    }
}

/// Complete instruction definition combining metadata and encodings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstructionDefinition {
    /// Instruction name (lowercase identifier)
    pub name: String,

    /// Version info (semantic versioning)
    #[serde(default)]
    pub version: Option<String>,

    /// Human-readable description
    pub description: Option<String>,

    /// Target architecture: x8664, aarch64, or riscv64
    pub arch: String,

    /// Alternative names for this instruction
    #[serde(default)]
    pub aliases: Vec<String>,

    /// Available operand specifications
    #[serde(default)]
    pub operands: Vec<OperandSpec>,

    /// Encoding specifications with multiple formats
    pub encoding_spec: EncodingSpecification,

    /// Instruction semantics and side effects
    #[serde(default)]
    pub semantics: Option<InstructionSemantics>,

    /// Usage examples with hex codes
    #[serde(default)]
    pub examples: Vec<UsageExample>,
}

/// Operand specification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperandSpec {
    /// Operand name identifier
    pub name: String,

    /// Type classification
    pub op_type: String,

    /// Data flow direction
    pub direction: Option<String>,

    /// Size constraint (optional)
    pub size: Option<String>,

    /// Human-readable description
    pub description: Option<String>,
}

/// Encoding specification container
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncodingSpecification {
    /// Default format name when multiple exist
    #[serde(default)]
    pub default_format: Option<String>,

    /// Available encoding formats
    pub formats: Vec<EncodingFormat>,
}

/// Single encoding format definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncodingFormat {
    /// Format name identifier
    pub name: String,

    /// Human-readable description
    pub description: Option<String>,

    /// Operand pattern specification (e.g., "r/m r")
    pub operand_pattern: String,

    /// Opcode byte sequence
    pub opcode: Vec<u8>,

    /// Whether ModR/M byte is required
    #[serde(default)]
    pub modrm_required: bool,

    /// Whether REX prefix may be needed
    #[serde(default)]
    pub rex_prefix: bool,

    /// Escape or extension bytes
    #[serde(default)]
    pub escape_bytes: Option<Vec<u8>>,

    /// Addressing mode template
    #[serde(default)]
    pub addressing_mode: Option<String>,

    /// Whether extra bytes are emitted
    #[serde(default)]
    pub extra_bytes: bool,

    /// Special case marker
    #[serde(default)]
    pub special_case: bool,
}

/// Instruction semantics and CPU behavior
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstructionSemantics {
    /// Semantic effect equation
    pub effect: Option<String>,

    /// Affected status flags
    #[serde(default)]
    pub flags_modified: Option<Vec<String>>,

    /// Possible exceptions
    #[serde(default)]
    pub exceptions: Option<Vec<String>>,

    /// Supported CPU operation modes
    #[serde(default)]
    pub cpu_modes: Option<Vec<String>>,
}

/// Usage example with expected output
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageExample {
    /// Assembly syntax representation
    pub assembly: String,

    /// Expected machine code in hex notation
    pub machine_code_hex: String,

    /// Optional explanation
    pub description: Option<String>,
}

/// Instruction loader for managing data-driven specs
pub struct InstructionLoader {
    /// Cache of loaded instructions
    cache: HashMap<String, InstructionDefinition>,

    /// Base directory for spec files
    base_path: PathBuf,
}

impl InstructionLoader {
    /// Create new loader with base directory
    pub fn new<P: AsRef<Path>>(base_path: P) -> Self {
        Self {
            cache: HashMap::new(),
            base_path: base_path.as_ref().to_path_buf(),
        }
    }

    /// Load single instruction by name
    pub fn load_instruction(&mut self, name: &str) -> Result<&InstructionDefinition> {
        if !self.cache.contains_key(name) {
            let file_path = self.base_path.join(format!("{}.toml", name));
            let content = fs::read_to_string(&file_path)?;
            let raw: RawInstruction = toml::from_str(&content)?;
            self.cache
                .insert(raw.package.name.clone(), raw.into_definition());
        }

        Ok(self.cache.get(name).unwrap())
    }

    /// Load all instructions from directory
    pub fn load_all_instructions(&mut self) -> Result<HashMap<String, InstructionDefinition>> {
        self.cache.clear();

        if !self.base_path.exists() {
            return Err(anyhow::anyhow!(
                "Instruction specs directory not found: {:?}",
                self.base_path
            ));
        }

        for entry in fs::read_dir(&self.base_path)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().and_then(|s| s.to_str()) == Some("toml") {
                let content = fs::read_to_string(&path)?;
                let raw: RawInstruction = toml::from_str(&content)?;
                let instruction = raw.into_definition();
                self.cache.insert(instruction.name.clone(), instruction);
            }
        }

        Ok(self.cache.clone())
    }

    /// Get instruction by name without loading first
    pub fn get_optional(&self, name: &str) -> Option<&InstructionDefinition> {
        self.cache.get(name)
    }

    /// Check if instruction exists
    pub fn has_instruction(&self, name: &str) -> bool {
        self.cache.contains_key(name)
    }

    /// Get all available instruction names
    pub fn list_instructions(&self) -> Vec<&String> {
        self.cache.keys().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_add_instruction() {
        let mut loader = InstructionLoader::new("src/codegen/data/x86_instructions");
        let add_instr = loader.load_instruction("add").expect("Failed to load add");

        assert_eq!(add_instr.name, "add");
        assert_eq!(add_instr.arch, "x8664");
        assert_eq!(add_instr.encoding_spec.formats.len(), 3);
    }

    #[test]
    fn test_list_all_instructions() {
        let mut loader = InstructionLoader::new("src/codegen/data/x86_instructions");
        let all = loader.load_all_instructions().expect("Failed to load all");

        assert!(all.contains_key("add"));
        assert!(all.contains_key("mov"));
    }
}
