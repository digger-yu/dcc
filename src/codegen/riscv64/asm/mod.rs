// src/codegen/riscv64/asm/mod.rs
// RISC-V 64 Assembly Module (Complete implementation extracted from riscv64.rs.bak)
//
// Phase B Day 3 - ASM Layer Complete (2026/9/22)
//
// Source locations in riscv64.rs.bak:
// - Constraint parsing: mini_asm.rs (~line 598-777) + inline asm handling (~line 185+)
// - Instruction encoding: mini_asm.rs encode_riscv64() and helpers
// - Directive handling: emit_globals(), compile_*() functions (~line 350+)
// - Template expansion: inline asm template processing logic
// - Address expansion: large constant loading sequences (~line 1000+)
//
// Modules:
// ├── constraint.rs    - Inline assembly constraint parser ✅
// ├── directive.rs     - Assembly directive emitter      ✅
// ├── emitter.rs       - Instruction building API       ✅
// ├── template.rs      - Template expansion engine      ✅
// └── expansion.rs     - Addressing mode expansion      ✅

mod constraint;
mod directive;
mod emitter;
mod template;
mod expansion;
mod planner;

pub use constraint::*;
pub use directive::*;
pub use emitter::*;
pub use template::*;
pub use expansion::*;
pub use planner::*;
