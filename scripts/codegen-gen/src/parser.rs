//! Instruction specification parser module
//! Parses JSON instruction specifications into Rust structs

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Represents a complete instruction specification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstructionSpec {
    /// Reference to schema (optional)
    pub schema: Option<String>,
    
    /// Instruction name (e.g., "add", "sub")
    pub name: String,
    
    /// Human-readable description
    pub description: String,
    
    /// Target architecture
    pub arch: Architecture,
    
    /// List of operands
    pub operands: Vec<OperandDef>,
    
    /// Encoding specification
    pub encoding_spec: EncodingSpec,
    
    /// Semantic information
    #[serde(default)]
    pub semantics: Option<InstructionSemantics>,
    
    /// Validation rules
    #[serde(default)]
    pub validation_rules: Option<ValidationRules>,
    
    /// Alternative names
    #[serde(default)]
    pub aliases: Vec<String>,
}

/// Target architecture enumeration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Architecture {
    X8664,
    AArch64,
    Riscv64,
}

impl std::fmt::Display for Architecture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Architecture::X8664 => write!(f, "x86_64"),
            Architecture::AArch64 => write!(f, "aarch64"),
            Architecture::Riscv64 => write!(f, "riscv64"),
        }
    }
}

/// Operand definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperandDef {
    /// Operand identifier used in templates
    pub name: String,
    
    /// Type of operand
    pub op_type: OperandType,
    
    /// Direction of data flow
    pub direction: OperandDirection,
    
    /// Size hint
    #[serde(default)]
    pub size: Option<String>,
    
    /// Specific registers if restricted
    #[serde(default)]
    pub registers: Option<Vec<String>>,
    
    /// Description
    #[serde(default)]
    pub description: Option<String>,
}

/// Operand type enumeration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OperandType {
    Reg,
    Mem,
    RegMem,
    Immediate,
    Memory,
    RegMemImmediate,
}

/// Operand direction enumeration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OperandDirection {
    Read,
    Write,
    ReadWrite,
    None,
}

/// Encoding specification with multiple formats
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncodingSpec {
    /// Multiple encoding formats available
    pub formats: Vec<EncodingFormat>,
    
    /// Default format to use
    #[serde(default)]
    pub default_format: Option<String>,
}

/// Individual encoding format definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncodingFormat {
    /// Format name (e.g., "RegMemReg")
    pub name: String,
    
    /// Description
    #[serde(default)]
    pub description: Option<String>,
    
    /// Expected operand pattern
    pub operand_pattern: String,
    
    /// Base opcode bytes
    pub opcode: Vec<u8>,
    
    /// Whether ModR/M byte is required
    #[serde(default)]
    pub modrm_required: bool,
    
    /// Whether REX prefix may be needed (x86 only)
    #[serde(default)]
    pub rex_prefix: bool,
    
    /// Escape/extension bytes
    #[serde(default)]
    pub escape_bytes: Option<Vec<u8>>,
    
    /// Addressing mode template
    #[serde(default)]
    pub addressing_mode: Option<String>,
    
    /// Whether this has extra bytes (like immediates)
    #[serde(default)]
    pub extra_bytes: bool,
    
    /// Special case marker
    #[serde(default)]
    pub special_case: bool,
}

/// Instruction semantics and side effects
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstructionSemantics {
    /// Formal effect description
    #[serde(default)]
    pub effect: Option<String>,
    
    /// Flags modified by instruction
    #[serde(default)]
    pub flags_modified: Option<Vec<String>>,
    
    /// Exceptions that may be raised
    #[serde(default)]
    pub exceptions: Option<Vec<String>>,
    
    /// CPU modes where valid
    #[serde(default)]
    pub cpu_modes: Option<Vec<String>>,
}

/// Validation rules for operands
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationRules {
    /// Maximum number of operands allowed
    #[serde(default)]
    pub max_operands: Option<usize>,
    
    /// Specific operand constraints
    #[serde(default)]
    pub operand_constraints: Option<Vec<OperandConstraint>>,
}

/// Single operand constraint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperandConstraint {
    /// Position in operand list
    pub position: usize,
    
    /// Requirement description
    pub requirement: String,
}

/// Collection of all parsed specs grouped by architecture
#[derive(Debug, Clone)]
pub struct ParsedSpecs {
    /// Specifications for each architecture
    pub specs_by_arch: HashMap<Architecture, Vec<InstructionSpec>>,
    
    /// All specs combined
    pub all_specs: Vec<InstructionSpec>,
}

impl ParsedSpecs {
    pub fn new() -> Self {
        let mut specs_by_arch = HashMap::new();
        specs_by_arch.insert(Architecture::X8664, Vec::new());
        specs_by_arch.insert(Architecture::AArch64, Vec::new());
        specs_by_arch.insert(Architecture::Riscv64, Vec::new());
        
        ParsedSpecs {
            specs_by_arch,
            all_specs: Vec::new(),
        }
    }
}

impl Default for ParsedSpecs {
    fn default() -> Self {
        Self::new()
    }
}

/// Load and parse a single instruction specification from file
pub fn load_instruction_spec(path: &Path) -> Result<InstructionSpec> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read spec file: {:?}", path))?;
    
    let spec: InstructionSpec = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse JSON from: {:?}", path))?;
    
    Ok(spec)
}

/// Load all instruction specifications from a directory
pub fn load_specs_from_dir(dir_path: &Path) -> Result<ParsedSpecs> {
    let mut specs = ParsedSpecs::new();
    
    if !dir_path.exists() || !dir_path.is_dir() {
        return Err(anyhow::anyhow!(
            "Directory does not exist or is not a directory: {:?}",
            dir_path
        ));
    }
    
    // Collect and sort files for consistent ordering
    let mut files: Vec<PathBuf> = Vec::new();
    
    for entry in fs::read_dir(dir_path)? {
        let entry = entry?;
        let path = entry.path();
        
        if path.is_file() && path.extension().map_or(false, |ext| ext == "json") {
            files.push(path);
        }
    }
    
    files.sort();
    
    // Parse each file
    for file_path in files {
        let spec = load_instruction_spec(&file_path)?;
        
        // Group by architecture
        specs
            .specs_by_arch
            .get_mut(&spec.arch)
            .map(|list| list.push(spec.clone()));
        
        // Add to all specs
        specs.all_specs.push(spec);
    }
    
    Ok(specs)
}

/// Load specs for a specific architecture
pub fn load_specs_for_arch(dir_path: &Path, arch: Architecture) -> Result<Vec<InstructionSpec>> {
    let specs = load_specs_from_dir(dir_path)?;
    
    specs
        .specs_by_arch
        .get(&arch)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("No specs found for architecture: {}", arch))
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_load_single_spec() {
        let spec = load_instruction_spec(Path::new(
            "src/codegen/data/x86_instructions/add.json"
        )).unwrap();
        
        assert_eq!(spec.name, "add");
        assert_eq!(spec.arch, Architecture::X8664);
        assert_eq!(spec.encoding_spec.formats.len(), 3);
    }
}
