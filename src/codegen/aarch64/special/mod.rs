// src/codegen/aarch64/special/mod.rs
// Special Features Module
//
// Handles architecture-specific features:
// - Thread-Local Storage (TLS) access modes (TTV, direct, GD, IE)
// - Memory Tagging Extension (MTE) support
// - HWASAN + MTE integration for hardware address sanitization
//
// Extracted functionality from original_monolith.rs into focused submodules.

mod tls;
mod mte;
pub mod sanitizer;

pub use tls::*;
pub use mte::*;
pub use sanitizer::*;
