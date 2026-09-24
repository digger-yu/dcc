// src/codegen/arm/special/mod.rs - inline asm and VFP float support.
// Mechanically split from the monolithic arm backend. Pure move:
// semantics preserved; only visibility and import paths were adjusted.

mod float;
mod inline_asm;
