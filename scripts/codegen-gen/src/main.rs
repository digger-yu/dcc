//! Code Generator Tool for Data-Driven Instruction Encoding
//! 
//! This tool reads JSON instruction specifications and generates
//! Rust code for the DCC compiler's data-driven encoding layer.

mod parser;
mod generator;
mod validator;
mod cli;

use anyhow::{Context, Result};
use clap::Parser;
use std::fs;
use std::path::Path;

fn main() -> Result<()> {
    println!("🚀 DCC Codegen-Gen v0.1.0");
    println!("=========================\n");
    
    let cli = cli::Cli::parse();
    
    // Clone fields before using them (to avoid partial move issues)
    let input_dir = cli.input_dir.clone().unwrap_or_else(|| Path::new("src/codegen/data").to_path_buf());
    let output_dir = cli.output_dir.clone().unwrap_or_else(|| Path::new("src/codegen/generated").to_path_buf());
    let report_file_opt = cli.report_file.clone();
    
    println!("📂 Input directory: {:?}", input_dir);
    println!("📁 Output directory: {:?}", output_dir);
    
    // Create output directory if needed
    fs::create_dir_all(&output_dir).context("Failed to create output directory")?;
    
    // Validate first if requested
    if cli.validate_only || cli.generate_report {
        println!("\n🔍 Validating instruction specifications...\n");
        
        if cli.generate_report {
            let mut report_file = output_dir.clone();
            report_file.push("validation_report.md");
            
            let report_file = if let Some(f) = report_file_opt {
                f
            } else {
                report_file
            };
            
            let report = validator::generate_validation_report(&input_dir, Some(&report_file))?;
            println!("✅ Validation report written to: {:?}", report_file);
            println!("\n{}", report);
        } else {
            let errors = validator::validate_all_specs(&input_dir)?;
            if !errors.is_empty() {
                return Err(anyhow::anyhow!("Validation failed with {} errors", errors.len()));
            }
        }
    }
    
    // Parse all specs
    println!("\n📖 Loading instruction specifications...");
    let specs = parser::load_specs_from_dir(&input_dir)?;
    
    println!("✅ Loaded {} instruction specifications", specs.all_specs.len());
    
    // Get target architectures
    let target_archs = cli.get_architectures()?;
    
    // Generate code for each architecture
    println!("\n🔨 Generating Rust code...\n");
    
    for arch in &target_archs {
        let arch_specs = specs
            .specs_by_arch
            .get(arch)
            .cloned()
            .unwrap_or_default();
        
        if arch_specs.is_empty() {
            println!("⚠️  No specifications for {}: {}", arch.to_string().to_uppercase(), "(skipping)");
            continue;
        }
        
        println!("   Processing {}...", arch);
        
        let generated_code = generator::generate_arch_code(*arch, &arch_specs)?;
        
        let file_name = format!("{}_instructions.rs", arch.to_string());
        let out_path = output_dir.join(&file_name);
        
        fs::write(&out_path, &generated_code)
            .with_context(|| format!("Failed to write: {:?}", out_path))?;
        
        println!(
            "   ✅ Generated: {}",
            out_path.display()
        );
    }
    
    // Generate shared modules module
    println!("\n📝 Generating shared modules index...");
    let shared_modules = generate_shared_modules(&target_archs, &output_dir)?;
    fs::write(output_dir.join("shared.rs"), &shared_modules)?;
    println!("   ✅ Generated: src/codegen/generated/shared.rs");
    
    println!("\n✨ Code generation complete!");
    println!("=============================");
    
    Ok(())
}

/// Generate shared modules module that includes all architecture-specific modules
fn generate_shared_modules(archs: &[crate::parser::Architecture], output_dir: &Path) -> Result<String> {
    let mut output = String::new();
    
    output.push_str("// Shared modules index\n");
    output.push_str("// AUTO-GENERATED - DO NOT EDIT!\n\n");
    
    output.push_str("/// This module re-exports all architecture-specific instruction definitions\n\n");
    
    for arch in archs {
        let module_name = format!("{}_instructions", arch.to_string());
        output.push_str(&format!("pub mod {};\n", module_name));
    }
    
    output.push_str("\n/// Re-export all instructions for easy access\n");
    
    for arch in archs {
        let module_name = format!("{}_instructions", arch.to_string());
        output.push_str(&format!("pub use self::{}::*;\n", module_name));
    }
    
    Ok(output)
}
