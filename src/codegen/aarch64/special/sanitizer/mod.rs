// src/codegen/aarch64/special/sanitizer/mod.rs
// Sanitizer Module
//
// Handles compiler sanitizer instrumentation:
// HWASAN (Hardware Address Sanitizer) integration with MTE
// MSAN (Memory Sanitizer) stubs
// TSAN (Thread Sanitizer) stubs
//
// Extracted from original_monolith.rs sanitizer-related code

pub mod hwasan;

pub use hwasan::*;
