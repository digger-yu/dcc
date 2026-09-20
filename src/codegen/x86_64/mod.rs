// src/codegen/x86_64/mod.rs - x86_64 backend module root
// Part of Path C backend refactor (docs/backend_refactor_plan.md v1.2)

// This file acts as the glue between the original monolithic x86_64.rs and new modular structure.
// It declares all submodules and maintains backward compatibility for external APIs.

pub mod asm;
pub mod abi;
pub mod emit;
pub mod mem;
pub mod special;
pub mod parallel;
pub mod elf_side;

// Keep Codegen struct temporarily in place during initial extraction phase.
// The actual struct will be moved/refactored once expression generation is fully modularized.
// Per plan §3.1 rule #3: "可见性：`pub(crate)` / `pub(super)`；`struct Codegen` 字段留 `mod.rs`"

#[cfg(test)]
mod tests;
