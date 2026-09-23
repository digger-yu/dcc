// src/codegen/aarch64/asm/mod.rs
// AArch64 Inline Assembly Module
// 
// This module handles inline assembly code generation for the AArch64 backend.
// It provides functionality for:
// - Parsing and emitting assembly directives (.word, .quad, .byte, etc.)
// - Managing label resolution and symbol mapping
// - Parsing constraint strings (=r, +m, etc.)
// - Expanding assembly template strings with placeholders ($0, $1, ...)
// - Unified instruction emission with register encoding helpers
// - Expanding syntax sugar (e.g., ldr x0, [x1, #64])
// - Future instruction planning and scheduling
//
// Phase A Day 4-5 - ASM Module Extraction (9/22/2026)
// Reference: ARM Architecture Reference Manual, AAPCS64

pub mod directive;
pub mod label;
pub mod constraint;
pub mod template;
pub mod emitter;
pub mod expansion;
pub mod planner;

// Re-export commonly used types for convenience
pub use self::directive::*;
pub use self::label::*;
pub use self::constraint::*;
pub use self::template::*;
pub use self::emitter::*;
pub use self::expansion::*;
pub use self::planner::*;

/// Version information for the ASM module
pub const ASM_MODULE_VERSION: &str = "0.1.0";

/// Get the version string of this module
pub fn version() -> &'static str {
    ASM_MODULE_VERSION
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_version() {
        assert_eq!(version(), "0.1.0");
    }

    #[test]
    fn test_all_submodules_imported() {
        // Verify that all submodules are properly declared
        use super::{
            directive,
            label,
            constraint,
            template,
            emitter,
            expansion,
            planner,
        };
        
        // Just checking they compile is enough - no actual usage needed here
        let _ = std::mem::size_of::<directive::DirectiveType>();
    }
}
