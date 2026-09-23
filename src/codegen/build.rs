//! Build script for DCC compiler data-driven instruction encoding

use std::process::Command;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=../scripts/codegen-gen/Cargo.toml");
    
    // Watch for changes in instruction specs
    let isa_dirs = ["x86_64", "aarch64", "riscv64"];
    
    for isa_dir in &isa_dirs {
        let spec_path = format!(
            "../src/codegen/data/{}/*.json",
            isa_dir
        );
        println!("cargo:rerun-if-changed={}", spec_path);
    }
    
    // Run codegen-gen during build
    let status = Command::new("cargo")
        .args(&[
            "run",
            "--manifest-path", "scripts/codegen-gen/Cargo.toml",
            "--",
            "--input-dir", "src/codegen/data/",
            "--output-dir", "src/codegen/generated/",
            "--isa", "all"
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .status();
    
    if let Err(e) = status {
        eprintln!("Warning: Failed to run codegen-gen: {}", e);
        eprintln!("Note: Generated files should exist from previous generation");
        // Don't fail the build - generated files may exist
    }
}
