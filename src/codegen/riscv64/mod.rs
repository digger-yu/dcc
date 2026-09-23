// src/codegen/riscv64.rs - RISC-V 64 后端模块化
// Phase B Day 1-5 - Complete modularization from riscv64.rs.bak (2026/9/22)
//
/// # Module Structure
/// Modular extraction from original riscv64.rs.bak:
//
/// ## Completed ✅
/// - `registers/`      - Register constants and utilities
/// - `asm/`            - Inline assembly handling
/// - `codegen/`        - Core code generation logic (~1659 lines)
/// - `abi/`            - SysV ABI calling convention implementation
/// - `emit/stmt`       - Statement-level emission (~448 lines) NEW
/// - `emit/expr/*`     - Expression handling: binary/binary/unary/call/lvalue/literal
/// - `helpers/encoding`- Encoding utilities (~424 lines) NEW
/// - `special/tls`     - TLS support (~269 lines) NEW
//
// ├── registers/  - Register constants and utilities ✅
// ├── asm/        - Inline assembly handling ✅
// ├── abi/        - Calling convention ✅
// ├── codegen/    - Core code generation logic ✅
// ├── emit/
// │   ├── stmt.rs         - Statement emission (~448 lines) NEW
// │   └── expr/
// │       ├── binary.rs   - Binary operations (~400 lines) NEW
// │       ├── unary.rs    - Unary operators (~442 lines) NEW
// │       ├── call.rs     - Function calls (~544 lines) NEW
// │       ├── lvalue.rs   - L-value addresses (~259 lines) NEW
// │       └── literal.rs  - Literal values (~211 lines) NEW
// ├── helpers/
// │   └── encoding.rs   - Encoding utilities (~424 lines) NEW
// └── special/
//     └── tls.rs        - TLS support (~269 lines) NEW

// ============================================================================
// Module Declarations (incrementally extracted from monolith)
// ============================================================================
mod backend;
mod registers;   // Day 1-2 - Register utilities
mod asm;         // Day 2-3 - Inline assembly module
mod codegen;     // Week 2 - Core code generation module
mod abi;         // SysV ABI calling convention
mod emit;        // Statement/expression emission (NEW)
mod helpers;     // Utility functions (NEW)
mod special;     // Special features: RVV vector extension, TLS (NEW)
mod expression_impl;  // Phase C - Expression executor backend


pub use self::registers::*;  // Re-export register utilities
pub use self::abi::*;        // Re-export ABI utilities
pub use self::emit::*;       // Re-export statement/expression emission
pub use self::helpers::*;    // Re-export utility functions
pub use self::special::*;    // Re-export special features (RVV, TLS)
pub use self::codegen::{Riscv64Codegen, var_is_static};

// Module substructure exports for granular access
// Note: Only export publicly accessible items
pub(crate) use self::emit::stmt;
pub(crate) use self::helpers::encoding::{self, encode_r_type, decode_i_type};

// 辅助函数导出
pub use crate::codegen::riscv64::emit::expr::lvalue::{find_aggregate_fields, align_up_u64};
