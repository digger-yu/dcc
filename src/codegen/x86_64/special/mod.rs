// src/codegen/x86_64/special/mod.rs
// Special features handling for x86_64 backend
// __int128, vector, x87 floating point, sanitizer, TLS support

pub mod int128;
pub mod x87_floating_point;
pub mod sanitizer;
pub mod tls;

pub use int128::{emit_int128_load, emit_int128_store};
pub use x87_floating_point::{emit_x87_fld, emit_x87_fst};
pub use sanitizer::emit_sanitize_bounds_check;
pub use tls::{emit_tls_address_calculation, TlsModel};
