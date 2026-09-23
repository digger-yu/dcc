//! Command-line interface for codegen-gen

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "codegen-gen")]
#[command(about = "Data-driven instruction encoding code generator for DCC compiler")]
pub struct Cli {
    /// Input directory containing JSON instruction specs
    #[arg(short, long)]
    pub input_dir: Option<PathBuf>,
    
    /// Output directory for generated Rust code
    #[arg(short, long)]
    pub output_dir: Option<PathBuf>,
    
    /// Target architecture (all, x86_64, aarch64, riscv64)
    #[arg(long)]
    pub isa: Option<String>,
    
    /// Validate specifications without generating code
    #[arg(long)]
    pub validate_only: bool,
    
    /// Generate validation report
    #[arg(long)]
    pub generate_report: bool,
    
    /// Report output file path
    #[arg(long)]
    pub report_file: Option<PathBuf>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Validate all instruction specifications
    Validate,
    
    /// Generate code for specified architecture(s)
    Generate {
        /// Target architectures (comma-separated or 'all')
        #[arg(short, long)]
        archs: Vec<String>,
        
        /// Input directory
        #[arg(short, long)]
        input: PathBuf,
        
        /// Output directory
        #[arg(short, long)]
        output: PathBuf,
    },
}

impl Cli {
    /// Get the target architecture from command line arguments
    pub fn get_architectures(&self) -> Result<Vec<crate::parser::Architecture>> {
        match self.isa.as_deref() {
            Some("all") | None => Ok(vec![
                crate::parser::Architecture::X8664,
                crate::parser::Architecture::AArch64,
                crate::parser::Architecture::Riscv64,
            ]),
            Some("x86_64") => Ok(vec![crate::parser::Architecture::X8664]),
            Some("aarch64") => Ok(vec![crate::parser::Architecture::AArch64]),
            Some("riscv64") => Ok(vec![crate::parser::Architecture::Riscv64]),
            Some(other) => Err(anyhow::anyhow!("Unknown architecture: {}", other)),
        }
    }
}
