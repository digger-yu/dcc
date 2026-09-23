//! Schema validation module
//! Validates instruction specifications against JSON schema

use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;

/// Validate a single instruction spec against the schema
pub fn validate_instruction_spec(spec: &crate::parser::InstructionSpec) -> Result<()> {
    let mut errors = Vec::new();
    
    // Check required fields
    if spec.name.is_empty() {
        errors.push("Instruction name is required".to_string());
    }
    
    if !regex_match(spec.name.as_str(), r"^[a-z][a-z0-9]*$") {
        errors.push(format!(
            "Instruction name must be lowercase with optional numbers: {}",
            spec.name
        ));
    }
    
    // Check architecture
    match spec.arch {
        crate::parser::Architecture::X8664
        | crate::parser::Architecture::AArch64
        | crate::parser::Architecture::Riscv64 => {}
    }
    
    // Check encoding spec exists and has at least one format
    if spec.encoding_spec.formats.is_empty() {
        errors.push("At least one encoding format is required".to_string());
    }
    
    // Validate each format
    for (idx, format) in spec.encoding_spec.formats.iter().enumerate() {
        if format.name.is_empty() {
            errors.push(format!("Encoding format {} has no name", idx));
        }
        
        if format.opcode.is_empty() {
            errors.push(format!(
                "Encoding format '{}' has no opcode bytes",
                format.name
            ));
        }
        
        // Check opcode byte values
        for (byte_idx, byte) in format.opcode.iter().enumerate() {
            if *byte > 255 {
                errors.push(format!(
                    "Opcode byte {} in format '{}' exceeds 255",
                    byte_idx, format.name
                ));
            }
        }
        
        if format.operand_pattern.is_empty() {
            errors.push(format!(
                "Encoding format '{}' has no operand pattern",
                format.name
            ));
        }
    }
    
    // Validate operands
    if spec.operands.is_empty() {
        errors.push("At least one operand is required".to_string());
    }
    
    // Check operand constraints if present
    if let Some(ref rules) = spec.validation_rules {
        if let Some(constraints) = &rules.operand_constraints {
            for constraint in constraints {
                if constraint.position >= spec.operands.len() {
                    errors.push(format!(
                        "Operand constraint position {} exceeds operand count {}",
                        constraint.position,
                        spec.operands.len()
                    ));
                }
            }
        }
    }
    
    if !errors.is_empty() {
        Err(anyhow::anyhow!(
            "Validation failed with {} error(s):\n{}",
            errors.len(),
            errors.join("\n")
        ))
    } else {
        Ok(())
    }
}

/// Validate all specs from directory
pub fn validate_all_specs(dir_path: &std::path::Path) -> Result<Vec<String>> {
    let specs = crate::parser::load_specs_from_dir(dir_path)?;
    
    let mut errors = Vec::new();
    
    for spec in &specs.all_specs {
        if let Err(e) = validate_instruction_spec(spec) {
            errors.push(format!("[{}] Error in '{}': {}", spec.arch, spec.name, e));
        }
    }
    
    if errors.is_empty() {
        println!("✅ All {} specifications validated successfully", specs.all_specs.len());
    } else {
        eprintln!("❌ Validation found {} issues:", errors.len());
        for error in &errors {
            eprintln!("{}", error);
        }
    }
    
    Ok(errors)
}

/// Load and validate schema file
pub fn load_schema(schema_path: &str) -> Result<Value> {
    let content = fs::read_to_string(schema_path)
        .context("Failed to read schema file")?;
    
    let schema: Value = serde_json::from_str(&content)
        .context("Failed to parse schema JSON")?;
    
    Ok(schema)
}

/// Check if regex pattern matches (simple implementation)
fn regex_match(text: &str, pattern: &str) -> bool {
    // This is a simplified implementation
    // In production, use the 'regex' crate
    if pattern == r"^[a-z][a-z0-9]*$" {
        text.chars().enumerate().all(|(i, c)| {
            if i == 0 {
                c.is_ascii_lowercase()
            } else {
                c.is_ascii_lowercase() || c.is_ascii_digit()
            }
        }) && !text.is_empty()
    } else {
        // Default to true for other patterns (basic implementation)
        !text.is_empty()
    }
}

/// Generate validation report
pub fn generate_validation_report(
    dir_path: &std::path::Path,
    output_path: Option<&std::path::Path>,
) -> Result<String> {
    let specs = crate::parser::load_specs_from_dir(dir_path)?;
    
    let mut report = String::new();
    report.push_str("# Instruction Specification Validation Report\n\n");
    report.push_str(&format!("Generated at: {}\n\n", chrono::Local::now()));
    report.push_str(&format!("Directory: {:?}\n\n", dir_path));
    
    // Summary by architecture
    report.push_str("## Summary by Architecture\n\n");
    
    let arch_counts: HashMap<crate::parser::Architecture, usize> = HashMap::new();
    let mut arch_counts: HashMap<crate::parser::Architecture, usize> = HashMap::new();
    
    for arch in &[
        crate::parser::Architecture::X8664,
        crate::parser::Architecture::AArch64,
        crate::parser::Architecture::Riscv64,
    ] {
        let count = specs
            .specs_by_arch
            .get(arch)
            .map(|s| s.len())
            .unwrap_or(0);
        
        report.push_str(&format!(
            "- {}: {} instructions\n",
            arch.to_string().to_uppercase(),
            count
        ));
    }
    
    report.push_str(&format!(
        "\n**Total**: {} instructions\n\n",
        specs.all_specs.len()
    ));
    
    // List all instructions
    report.push_str("## All Instructions\n\n");
    
    let mut sorted_specs = specs.all_specs.clone();
    sorted_specs.sort_by(|a, b| a.name.cmp(&b.name));
    
    for spec in &sorted_specs {
        let formats = spec.encoding_spec.formats.len();
        let desc = spec.description.trim_matches('"');
        
        report.push_str(&format!(
            "### {}. `{}` ({})\n{}\n   - Formats: {}\n   - Operands: {}\n\n",
            specs
                .all_specs
                .iter()
                .position(|s| s.name == spec.name)
                .unwrap()
                + 1,
            spec.name,
            spec.arch.to_string(),
            desc,
            formats,
            spec.operands.len()
        ));
    }
    
    // Validation results
    report.push_str("## Validation Results\n\n");
    
    let validation_errors = validate_all_specs(dir_path)?;
    
    if validation_errors.is_empty() {
        report.push_str("✅ No validation errors found.\n");
    } else {
        report.push_str("❌ Validation errors found:\n\n");
        for error in &validation_errors {
            report.push_str(&format!("- {}\n", error));
        }
    }
    
    let report_content = report;
    
    // Write to file if specified
    if let Some(path) = output_path {
        fs::write(path, &report_content)?;
    }
    
    Ok(report_content)
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_valid_add_spec() {
        let spec = crate::parser::load_instruction_spec(std::path::Path::new(
            "src/codegen/data/x86_instructions/add.json"
        )).unwrap();
        
        assert!(validate_instruction_spec(&spec).is_ok());
    }
}
