// src/codegen/arm/mod.rs - ARM Backend Implementation
// Phase C Extension - Modular ARM backend (stub implementation)

use crate::cli::Arch;
use crate::codegen::shared::backend::{
    Backend, CommonImage, CompileInput, ExtraSection, ObjectRelocs, ObjectSymbols, TlsImage,
};
use crate::codegen::shared::features::FeatureSet;
use crate::diagnostics::Diagnostic;

pub struct ArmCodegen {
    // TODO: Implement full ARM backend
    // For now, placeholder to allow compilation
}

impl ArmCodegen {
    pub fn new(_target: crate::target::Target, _relocatable: bool) -> Self {
        ArmCodegen {}
    }
}

impl Backend for ArmCodegen {
    fn arch(&self) -> Arch {
        Arch::Arm
    }

    fn features(&self) -> FeatureSet {
        // ARM backend has limited feature support
        FeatureSet { tier1_offload: false, ..FeatureSet::default() }
    }

    fn compile(&mut self, _input: &CompileInput<'_>) -> Result<CommonImage, Vec<Diagnostic>> {
        Err(vec![Diagnostic {
            level: "ERROR".to_string(),
            location: None,
            message: "ARM backend not yet implemented".to_string(),
            detail: None,
            hint: None,
        }])
    }

    fn render_asm(&mut self, _input: &CompileInput<'_>) -> Result<Vec<u8>, Vec<Diagnostic>> {
        Err(vec![Diagnostic {
            level: "ERROR".to_string(),
            location: None,
            message: "ARM backend not yet implemented".to_string(),
            detail: None,
            hint: None,
        }])
    }

    fn build_object(&mut self) -> Result<Vec<u8>, Vec<Diagnostic>> {
        Err(vec![Diagnostic {
            level: "ERROR".to_string(),
            location: None,
            message: "ARM backend not yet implemented".to_string(),
            detail: None,
            hint: None,
        }])
    }
}
