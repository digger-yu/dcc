// src/codegen/aarch64/asm/label.rs
// AArch64 Assembly Label Management
// 
// This module provides functionality for managing assembly labels in inline code:
// - Unique label generation for local labels (.L*)
// - Symbol-to-label mapping for function names
// - Label resolution during template expansion
//
// Phase A Day 4-5 - ASM Module Extraction (9/22/2026)

use std::collections::HashMap;
use std::fmt;

/// Error type for label operations
#[derive(Debug, Clone)]
pub enum LabelError {
    /// Label not found in mapping
    NotFound(String),
    /// Invalid label format
    InvalidFormat(String),
    /// Duplicate label definition
    Duplicate(String),
}

impl fmt::Display for LabelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LabelError::NotFound(name) => {
                write!(f, "label not found: {}", name)
            }
            LabelError::InvalidFormat(name) => {
                write!(f, "invalid label format: {}", name)
            }
            LabelError::Duplicate(name) => {
                write!(f, "duplicate label definition: {}", name)
            }
        }
    }
}

impl std::error::Error for LabelError {}

/// Manager for assembly labels in a function
/// Handles both user-defined labels and auto-generated local labels
#[derive(Debug, Clone)]
pub struct AsmLabelManager {
    /// Mapping from source-level names to assembly labels
    /// Example: "my_func" -> ".Lfunc_my_func"
    symbol_to_label: HashMap<String, String>,
    
    /// Counter for generating unique local labels
    local_label_counter: u32,
    
    /// Cache of generated local labels
    generated_labels: HashMap<u32, String>,
}

impl Default for AsmLabelManager {
    fn default() -> Self {
        Self::new()
    }
}

impl AsmLabelManager {
    /// Create a new label manager
    pub fn new() -> Self {
        Self {
            symbol_to_label: HashMap::new(),
            local_label_counter: 0,
            generated_labels: HashMap::new(),
        }
    }

    /// Initialize with existing mappings (e.g., from collect_asm_labels)
    pub fn with_mappings(mappings: HashMap<String, String>) -> Self {
        Self {
            symbol_to_label: mappings,
            local_label_counter: 0,
            generated_labels: HashMap::new(),
        }
    }

    /// Insert a symbol-to-label mapping
    pub fn insert_mapping(&mut self, symbol: String, label: String) {
        self.symbol_to_label.insert(symbol, label);
    }

    /// Resolve a symbol name to its assembly label
    /// Returns the mapped label if present, otherwise the original name
    pub fn resolve(&self, name: &str) -> &str {
        self.symbol_to_label.get(name)
            .map(|s| s.as_str())
            .unwrap_or(name)
    }

    /// Check if a symbol has a custom label mapping
    pub fn has_mapping(&self, name: &str) -> bool {
        self.symbol_to_label.contains_key(name)
    }

    /// Get the current local label counter
    pub fn counter(&self) -> u32 {
        self.local_label_counter
    }

    /// Generate a unique local label name
    /// Format: .L<num> (e.g., .L0, .L1, .L2)
    pub fn generate_local_label(&mut self) -> String {
        let id = self.local_label_counter;
        self.local_label_counter += 1;
        
        // Use cached version if available
        if let Some(cached) = self.generated_labels.get(&id) {
            return cached.clone();
        }
        
        let label = format!(".L{}", id);
        self.generated_labels.insert(id, label.clone());
        label
    }

    /// Generate a local label with a prefix (for better debugging)
    /// Format: .Lprefix_<num> (e.g., .Lmov_0, .Lcmp_1)
    pub fn generate_prefixed_label(&mut self, prefix: &str) -> String {
        let id = self.local_label_counter;
        self.local_label_counter += 1;
        
        // Limit prefix length to avoid very long labels
        let safe_prefix = if prefix.len() > 8 {
            &prefix[..8]
        } else {
            prefix
        };
        
        format!(".L{}_{})", safe_prefix, id)
    }

    /// Reset the local label counter (for fresh compilation)
    pub fn reset_counters(&mut self) {
        self.local_label_counter = 0;
        self.generated_labels.clear();
    }

    /// Get all symbol-to-label mappings
    pub fn mappings(&self) -> &HashMap<String, String> {
        &self.symbol_to_label
    }

    /// Clear all mappings and counters
    pub fn clear(&mut self) {
        self.symbol_to_label.clear();
        self.reset_counters();
    }
}

/// Generator for creating unique labels
#[derive(Debug, Clone)]
pub struct LabelGenerator {
    /// Base name for generated labels
    base_name: String,
    /// Counter for this generator instance
    counter: u32,
}

impl LabelGenerator {
    /// Create a new label generator with a specific base name
    pub fn new(base_name: &str) -> Self {
        Self {
            base_name: base_name.to_string(),
            counter: 0,
        }
    }

    /// Generate next unique label
    pub fn next(&mut self) -> String {
        let label = format!("{}.{:04x}", self.base_name, self.counter);
        self.counter += 1;
        label
    }

    /// Peek at next label without advancing counter
    pub fn peek(&self) -> String {
        format!("{}.{:04x}", self.base_name, self.counter)
    }

    /// Reset counter to zero
    pub fn reset(&mut self) {
        self.counter = 0;
    }
}

/// Parse an assembly label from text
/// Validates that it follows GAS naming conventions
pub fn parse_assembly_label(text: &str) -> Result<String, LabelError> {
    let trimmed = text.trim();
    
    if trimmed.is_empty() {
        return Err(LabelError::InvalidFormat("empty label".to_string()));
    }
    
    // Valid label characters: alphanumeric, underscore, dot, @, $
    // Must start with letter, dot, or special character (@, $, :)
    let mut chars = trimmed.chars();
    let first = chars.next().ok_or(LabelError::InvalidFormat(
        "empty label".to_string()
    ))?;
    
    // Allow these as starting characters
    if !first.is_alphabetic() && first != '.' && first != '@' && first != '$' && first != ':' {
        return Err(LabelError::InvalidFormat(trimmed.to_string()));
    }
    
    // Validate remaining characters
    for c in chars {
        if !c.is_alphanumeric() && c != '_' && c != '.' && c != '@' && c != '$' {
            return Err(LabelError::InvalidFormat(trimmed.to_string()));
        }
    }
    
    Ok(trimmed.to_string())
}

/// Extract label from an assembly line (if present)
/// Labels appear before colons: "label_name:" or ".Lname:"
pub fn extract_label_from_line(line: &str) -> Option<(String, &str)> {
    let trimmed = line.trim();
    
    // Look for colon
    if let Some(pos) = trimmed.find(':') {
        let label_part = trimmed[..pos].trim();
        
        if !label_part.is_empty() {
            // Validate label format
            if let Ok(valid_label) = parse_assembly_label(label_part) {
                return Some((valid_label, trimmed[pos + 1..].trim()));
            }
        }
    }
    
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_label_manager_creation() {
        let manager = AsmLabelManager::new();
        assert_eq!(manager.counter(), 0);
        assert!(manager.mappings().is_empty());
    }

    #[test]
    fn test_generate_local_label() {
        let mut manager = AsmLabelManager::new();
        
        let label1 = manager.generate_local_label();
        let label2 = manager.generate_local_label();
        
        assert_eq!(label1, ".L0");
        assert_eq!(label2, ".L1");
        assert_eq!(manager.counter(), 2);
    }

    #[test]
    fn test_generate_prefixed_label() {
        let mut manager = AsmLabelManager::new();
        
        let label1 = manager.generate_prefixed_label("mov");
        let label2 = manager.generate_prefixed_label("cmp");
        
        assert_eq!(label1, ".Lmov_0");
        assert_eq!(label2, ".Lcmp_1");
    }

    #[test]
    fn test_symbol_resolution() {
        let mut manager = AsmLabelManager::new();
        manager.insert_mapping("my_func".to_string(), ".Lfunc_my_func".to_string());
        
        assert_eq!(manager.resolve("my_func"), ".Lfunc_my_func");
        assert_eq!(manager.resolve("unknown"), "unknown");
    }

    #[test]
    fn test_label_generator() {
        let mut gen = LabelGenerator::new("test");
        
        assert_eq!(gen.next(), "test.0000");
        assert_eq!(gen.next(), "test.0001");
        assert_eq!(gen.peek(), "test.0002");
        assert_eq!(gen.next(), "test.0002");
    }

    #[test]
    fn test_extract_label_from_line() {
        let (label, rest) = extract_label_from_line("my_label: mov x0, #0").unwrap();
        assert_eq!(label, "my_label");
        assert_eq!(rest, "mov x0, #0");
        
        let (label, rest) = extract_label_from_line(".Llocal: cmp x0, x1").unwrap();
        assert_eq!(label, ".Llocal");
        assert_eq!(rest, "cmp x0, x1");
    }

    #[test]
    fn test_no_label_in_line() {
        let result = extract_label_from_line("mov x0, #0");
        assert!(result.is_none());
    }
}
