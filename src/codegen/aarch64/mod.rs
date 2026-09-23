// src/codegen/aarch64.rs - AArch64 后端模块化
// Phase A Day 4-5 - Created asm/ module (9/22/2026)
// Phase A Week 2-3 - Created codegen/ core module (9/22/2026)
// Phase A Turn 48+ - Created emit/, helpers/, special/ modules (2026)
//
/// # Module Structure
/// Modular extraction from original_monolith.rs:
///
/// ## Completed ✅
/// - `registers/` (Day 3)     - Register constants and utilities  
/// - `asm/` (Day 4-5)         - Inline assembly handling
/// - `codegen/` (Week 2-3)    - Core code generation logic (~1659 lines)
/// - `abi/`                  - Calling convention implementation
/// - `emit/` (Turn 48+)      - Statement/expression emission (~2980 lines)
/// - `helpers/` (Turn 48+)   - Utility functions & encodings (~278 lines)
/// - `special/` (Turn 48+)   - TLS, MTE, Sanitizer support (~140 lines)
//
// ├── registers/  - Register constants and utilities ✅
// ├── asm/        - Inline assembly handling ✅
// ├── abi/        - Calling convention ✅
// ├── codegen/    - Core code generation logic ✅ NEW
// ├── emit/       - Statement/expression emission ✅ NEW
// ├── helpers/    - Utility functions ✅ NEW
// └── special/    - Special features (TLS, MTE, Sanitizer) ✅ NEW

// ============================================================================
// Module Declarations (incrementally extracted from monolith)
// ============================================================================
mod backend;
mod registers;  // Day 3 - Register utilities
mod asm;        // Day 4-5 - Inline assembly module
mod codegen;    // Week 2-3 - Core code generation module
mod abi;        // AAPCS64 calling convention
mod emit;       // Statement/expression emission (NEW)
mod helpers;    // Utility functions (NEW)
mod special;    // Special features: TLS, MTE, Sanitizer (NEW)
mod expression_impl;  // Phase C - Expression executor backend
mod aarch64;  // AArch64 implementation core

// NOTE: Old architecture is deprecated and kept only for reference
// DO NOT USE: This file will be removed once all functionality is migrated
// TODO: Uncomment this line later if needed for debugging/reference:
// #[path = "original_monolith.rs.bak"]
// mod aarch64_impl_bak;  // OLD ARCHITECTURE - DEPRECATED

// Public exports flow through new architecture only
pub use self::registers::*;  // Re-export register utilities
// Codegen module exists as submodule
pub use self::emit::*;           // Re-export statement/expression emission
pub use self::helpers::*;        // Re-export utility functions
pub use self::special::*;        // Re-export special features

// Re-export main codegen type from implementation
pub use self::aarch64::Aarch64Codegen;
