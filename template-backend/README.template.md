# New ISA Backend Template

## Overview

This template provides a standardized scaffold for implementing new architecture backends in DCC compiler.

Based on the proven AArch64 modularization pattern from Phase A.

**Estimated completion time: 2 weeks** (vs. original 2 months estimate)

---

## Directory Structure Template

```
src/codegen/{isa_name}/
├── mod.rs                           # Main entry point
├── backend.rs                       # Backend trait implementation
│
├── registers/                       # Register definitions
│   ├── mod.rs
│   ├── gpr.rs                       # General purpose registers
│   ├── fpr.rs                       # Float/SIMD registers  
│   └── call_preserved.rs            # Callee-saved/broken classification
│
├── abi/                             # Calling convention (ISA-specific ABI)
│   ├── mod.rs
│   ├── call.rs                      # Parameter passing rules
│   ├── classes.rs                   # Type classification algorithm
│   └── frame.rs                     # Stack frame layout
│
├── asm/                             # Assembly layer
│   ├── mod.rs
│   ├── directive.rs                 # .word/.quad/.align directives
│   ├── label.rs                     # Label management
│   ├── constraint.rs                # Constraint parsing (=r, +m, etc.)
│   ├── template.rs                  # Inline assembly expansion
│   ├── emitter.rs                   # Unified instruction emission
│   ├── expansion.rs                 # Syntax sugar expansion
│   └── planner/
│       └── orchestration.rs         # Instruction scheduling (optional)
│
├── codegen/                         # IR lowering pipeline
│   ├── mod.rs
│   ├── compile.rs                   # Main compilation loop
│   ├── functions.rs                 # Function body traversal
│   ├── globals.rs                   # Global variable initialization
│   └── blocks.rs                    # Control flow graph processing
│
├── emit/                            # Binary section generation
│   ├── mod.rs
│   ├── stmt.rs                      # Statement-level emission
│   └── expr/                        # Expression evaluation
│       ├── mod.rs
│       ├── binary.rs                # Binary operations
│       ├── unary.rs                 # Unary operations
│       ├── literal.rs               # Literal handling
│       ├── cast.rs                  # Type conversion
│       ├── lvalue.rs                # LValue address computation
│       ├── call.rs                  # Function call sequences
│       ├── compound.rs              # Compound literals
│       ├── va_arg.rs                # Variadic arguments
│       └── init.rs                  # Initializers
│
├── helpers/                         # Utility functions
│   └── encoding.rs                  # ISA instruction bit encodings
│
└── special/                         # Special features
    ├── mod.rs
    ├── tls.rs                       # Thread-local storage access modes
    ├── mte.rs                       # Memory tagging (if applicable)
    └── sanitizer/
        └── hwasan.rs                # Hardware sanitizer integration
```

---

## Implementation Checklist

### Phase 1: Foundation (Days 1-3)
- [ ] Create directory structure matching template
- [ ] Implement `backend.rs` with `Backend` trait
- [ ] Define register constants in `registers/mod.rs`
- [ ] Create basic `mod.rs` declarations

### Phase 2: Core Modules (Days 4-7)
- [ ] Implement `abi/call.rs` - parameter passing rules
- [ ] Implement `abi/classes.rs` - type classification
- [ ] Implement `asm/constraint.rs` - inline asm parsing
- [ ] Implement `asm/emitter.rs` - instruction building

### Phase 3: CodeGen Pipeline (Days 8-10)
- [ ] Implement `codegen/compile.rs` - main compilation loop
- [ ] Implement `codegen/globals.rs` - global variable handling
- [ ] Implement `codegen/blocks.rs` - control flow processing
- [ ] Wire up function compilation

### Phase 4: Emission & Integration (Days 11-14)
- [ ] Implement `emit/stmt.rs` - statement emission
- [ ] Implement core expression handlers (binary, unary, call)
- [ ] Update main registry to include new backend
- [ ] Write unit tests for each module
- [ ] Run full test suite and verify correctness

---

## Key API Patterns

### Backend Trait Implementation

```rust
// src/codegen/{isa_name}/backend.rs
use crate::codegen::backend::{Backend, CompileInput, CommonImage};
use crate::target::Target;

pub struct {IsaName}Codegen {
    // ISA-specific state fields
    code: Vec<u8>,
    rodata: Vec<u8>,
    symbols: Vec<ElfSym>,
    // ... other fields
}

impl Backend for {IsaName}Codegen {
    fn arch(&self) -> Arch => Arch::{UPPER_ISA_NAME},
    
    fn features(&self) -> FeatureSet {
        FeatureSet {
            has_lto: true,
            has_sanitizers: true,
            // ... other flags
        }
    }
    
    fn compile(&mut self, input: &CompileInput<'_>) -> Result<CommonImage, Vec<Diagnostic>> {
        todo!("Implement compilation logic")
    }
    
    fn render_asm(&mut self, input: &CompileInput<'_>) -> Result<Vec<u8>, Vec<Diagnostic>> {
        todo!("Implement assembly rendering")
    }
    
    fn build_object(&mut self) -> Result<Vec<u8>, Vec<Diagnostic>> {
        todo!("Build ELF/COFF object file")
    }
}
```

### Module Organization Pattern

```rust
// src/codegen/{isa_name}/mod.rs
mod backend;          // Backend trait implementation
mod registers;        // Register definitions
mod abi;              // Calling convention
mod asm;              // Assembly layer
mod codegen;          // IR lowering
mod emit;             // Binary emission
mod helpers;          // Utilities
mod special;          // TLS, sanitizers

#[path = "{isa_name}.rs"]  // Original monolith backup
mod {isa_name}_impl;

// Re-export public APIs
pub use registers::*;
pub use abi::*;
pub use self::emit::*;
pub use self::helpers::*;
pub use self::special::*;
```

---

## Testing Requirements

Each module must have unit tests:

```rust
#[cfg(test)]
mod {isa_name}::registers {
    #[test]
    fn test_register_constants() {
        assert_eq!(GPR_COUNT, 32);
    }
    
    #[test]
    fn test_calling_convention() {
        let arg_regs = ARG_REGS_GP;
        assert_eq!(arg_regs.len(), 8);
    }
}
```

---

## Verification Steps

After implementation, run these commands:

```bash
# 1. Compilation check
cargo check --lib {isa_name}

# 2. Unit tests
cargo test --lib {isa_name}::[module_name]

# 3. Integration test with sample program
dcc test.c -m={isa_name} -o test.{isa_name}.o
readelf -h test.{isa_name}.o | grep Machine

# 4. Compare with golden baseline (if available)
diff old_output.S new_output.S
```

---

## Resources

- ARM v8-A Reference Manual (for AArch64 patterns)
- RISC-V ISA Manual
- GCC Backend Writing Guide
- LLVM Target Independent Code Generation
- DCC ARCHITECTURE.md

---

## Contact

For questions about this template:
- Technical: See inline comments in generated files
- Architecture: Refer to ARCHITECTURE.md
- Issues: Open GitHub issue with "new-isa-backend" tag

---

*Template based on successful AArch64 modularization experience*
*Last updated: September 22, 2026*
