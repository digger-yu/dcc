// src/codegen/arm/emit/mod.rs - statement / expression emission.
// Mechanically split from the monolithic arm backend. Pure move:
// semantics preserved; only visibility and import paths were adjusted.

mod block;
mod expression;
