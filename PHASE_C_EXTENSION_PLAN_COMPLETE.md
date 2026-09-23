# DCC Phase C Extension: Critical Missing Components Implementation Plan

**Date**: September 22, 2026  
**Status**: Ready for Execution  
**Priority**: P0 (Critical)  

---

## 🎯 **Executive Summary**

Based on comprehensive review of `docs/data_driven_codegen_refactor.md`, two critical components remain incomplete:

### ❌ **Missing Components:**

1. **Data-Driven Instruction Encoding Layer** - Currently hardcoded in backends
2. **"Adding New ISA" Formal Guide** - No complete standalone document

This plan provides detailed implementation strategy to complete both components and achieve 100% Phase C compliance.

---

## 📊 **Impact Assessment**

### Current State vs Target State:

| Component | Current | Target | Gap |
|-----------|---------|--------|-----|
| **Instruction Encoding** | Hardcoded (~800L per backend) | JSON/YAML + Codegen Gen (Future) | ❌ Missing |
| **ISA Onboarding Guide** | Scattered examples | Single authoritative guide | ❌ Missing |
| **Achievement Rate** | ~82% | 100% | ⚠️ 18% gap |

**Why These Are Critical:**
- Without data-driven layer → Lost the "data-driven architecture" vision
- Without ISA guide → New ISAs take 4+ weeks instead of 2 weeks
- Both are high-value additions despite being "optional" in original scope

---

## 🔧 **Component 1: Data-Driven Instruction Encoding Layer**

### 🎯 **Objective:**
Replace hardcoded instruction encoding with configurable JSON/YAML specifications + automatic code generation.

### 🏗️ **Architecture Design:**

```
┌─────────────────────────────────────────────────────────┐
│            Instruction Specification Layer               │
│              (JSON/YAML files per ISA)                  │
└───────────────┬─────────────────────────────────────────┘
                │
                ▼
┌─────────────────────────────────────────────────────────┐
│           codegen-gen Tool (Rust Binary)               │
│         Reads specs → Generates Rust code             │
└───────────────┬─────────────────────────────────────────┘
                │
                ▼
┌─────────────────────────────────────────────────────────┐
│       Generated Instructions Module                     │
│  src/codegen/generated/{x86_64,aarch64,riscv64}.rs     │
└───────────────┬─────────────────────────────────────────┘
                │
                ▼
┌─────────────────────────────────────────────────────────┐
│    Backend implementations delegate to generated code  │
│    Uses Strategy Pattern for ISA-specific hooks        │
└─────────────────────────────────────────────────────────┘
```

### 📋 **Implementation Steps:**

#### **Step 1: Define JSON Schema & Specifications** (Week 1)

Create instruction specification format for x86_64 first (most mature):

**File**: `src/codegen/data/x86_instructions.json`

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "name": "add",
  "description": "Integer addition: dest = dest + src",
  "arch": "x86_64",
  
  "encoding_spec": {
    "formats": [
      {
        "pattern": "r/m r",
        "description": "Register or memory plus register",
        "opcode": [0x03],
        "modrm_required": true,
        "reg_field_mapping": "source_register",
        "rm_field_mapping": "dest_register_or_memory"
      },
      {
        "pattern": "imm r/m8",
        "description": "8-bit immediate to byte operand",
        "opcode": [0x83, 0xE0],
        "immediates": [
          {
            "size_bits": 8,
            "sign_extend": true
          }
        ]
      },
      {
        "pattern": "imm r/m32",
        "description": "32-bit immediate to dword operand",
        "opcode": [0x81, 0xC0],
        "immediates": [
          {
            "size_bits": 32,
            "sign_extend": false
          }
        ]
      }
    ]
  },
  
  "operands": [
    {
      "position": "dest",
      "constraints": ["writable", "GP_register", "memory"],
      "modes": ["register", "memory"]
    },
    {
      "position": "src", 
      "constraints": ["read-only", "GP_register", "immediate"],
      "modes": ["register", "memory", "immediate"]
    }
  ],
  
  "semantics": {
    "effect": "dest = dest + src",
    "flags_modified": [
      {"name": "CF", "description": "Carry flag", "set_when": "unsigned overflow"},
      {"name": "OF", "description": "Overflow flag", "set_when": "signed overflow"},
      {"name": "ZF", "description": "Zero flag"},
      {"name": "SF", "description": "Sign flag"},
      {"name": "PF", "description": "Parity flag"}
    ]
  },
  
  "examples": [
    {
      "assembly": "add eax, ebx",
      "machine_code_hex": "01 DB",
      "description": "Add EBX to EAX"
    }
  ]
}
```

**Total instructions to encode initially:**
- Add/Sub/Mul/Div (integer arithmetic): 20 variations each
- And/Or/Xor (bitwise): 15 variations each
- Shifts (Shl/Shr/Sar): 12 variations each
- Comparison ops (CMP): 8 variations
- Total: ~55 core instructions for phase 1

#### **Step 2: Build codegen-gen Tool** (Week 2-3)

Create Rust binary that reads JSON specs and generates Rust code:

**File**: `scripts/codegen-gen/Cargo.toml`

```toml
[package]
name = "codegen-gen"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
anyhow = "1.0"
clap = { version = "4.0", features = ["derive"] }

[bin]
name = "codegen-gen"
path = "src/main.rs"
```

**File**: `scripts/codegen-gen/src/main.rs` (~300 lines)

Key functionality:
1. Parse command-line arguments (spec path, output dir)
2. Load all JSON spec files for target ISA
3. Validate against schema
4. Generate Rust constants/instructions for each opcode
5. Write to `src/codegen/generated/` directory

Example generated output (`src/codegen/generated/x86_instructions.rs`):

```rust
// Auto-generated from src/codegen/data/x86_instructions.json
// DO NOT EDIT MANUALLY!

use crate::codegen::shared::expression_executor::Instruction;

/// ADD instruction - Integer addition
pub const ADD_INSTRUCTION: Instruction = Instruction {
    name: "add",
    encoding_formats: vec![
        InstructionFormat {
            pattern: InstructionPattern::RegMemReg,
            opcode_bytes: vec![0x03],
            modrm_required: true,
            reg_field: RegisterField::Source,
            rm_field: RegisterField::Dest,
        },
        InstructionFormat {
            pattern: InstructionPattern::Imm8RegMem,
            opcode_bytes: vec![0x83, 0xE0],
            immediate_info: Some(ImmediateInfo {
                size_bits: 8,
                sign_extend: true,
            }),
        },
        // ... more formats
    ],
    semantics: InstructionSemantics {
        description: "Integer addition: dest = dest + src",
        effects: vec!["dest = dest + src"],
        flags_modified: FlagMask::ALL_ARITHMETIC,
    },
};

// Generate encoder function
impl InstructionEncoder for AddInstruction {
    fn encode(&self, operands: &[Operand], mode: AssemblyMode) -> Result<Vec<u8>> {
        match self.select_encoding(operands)? {
            EncodingFormat::RegMemReg => {
                // Encode: opcode[0x03] + MODRM(dest, src)
                Ok(vec![0x03, encode_modrm(dest.as_regmem(), src.as_reg())])
            }
            EncodingFormat::Imm8RegMem => {
                let imm8 = operands[1].immediate_value()? as u8;
                Ok(vec![0x83, 0xE0, imm8])
            }
            // ... more variants
        }
    }
}
```

#### **Step 3: Integrate into Build System** (Week 4)

**File**: `src/codegen/build.rs`

```rust
fn main() {
    println!("cargo:rerun-if-changed=data/x86_instructions.json");
    println!("cargo:rerun-if-changed=data/aarch64_instructions.json");
    println!("cargo:rerun-if-changed=data/riscv64_instructions.json");
    
    // Run codegen-gen during build
    let status = std::process::Command::new("cargo")
        .args(&[
            "run", "--manifest-path", "scripts/codegen-gen/Cargo.toml", 
            "--", 
            "--output-dir", "src/codegen/generated/",
            "--isa", "all"
        ])
        .status()
        .expect("Failed to run codegen-gen");
    
    if !status.success() {
        panic!("Code generation failed!");
    }
}
```

Update `src/codegen/mod.rs`:

```rust
// Include generated instruction modules
include!(concat!(env!("OUT_DIR"), "/generated_instructions.rs"));
```

#### **Step 4: Update Backends to Use Generated Code** (Week 5)

Modify existing backend implementations:

Before (hardcoded):
```rust
fn emit_add(&mut self, dest: u8, src: u8) {
    // Hand-written assembly encoding
    self.emit(&[0x03, encode_modrm(dest, src)]);
}
```

After (using generated):
```rust
fn emit_add(&mut self, dest: u8, src: u8) {
    // Delegate to generated encoder
    let instr = &ADD_INSTRUCTION;
    let operands = vec![Operand::from_reg(dest), Operand::from_reg(src)];
    let encoded = instr.encode(&operands, self.mode)?;
    self.emit(&encoded);
}
```

---

### 📈 **Expected Impact:**

| Metric | Before | After | Improvement |
|--------|--------|-------|-------------|
| **Instructions Modified** | 800L hardcoding | 50L JSON spec | ↓93% |
| **New Instruction Time** | 4 hours | 30 min | ↑8x faster |
| **Error Detection** | Post-compile | Pre-compile validation | ✅ Earlier |
| **Documentation** | Comments only | JSON schema + examples | ✅ Richer |
| **Maintainability** | High risk | Low risk | ✅ Safer |

---

### ⏱️ **Timeline:**

- **Week 1**: JSON specs for x86_64 core instructions (20-30 instructions)
- **Week 2-3**: Build & test codegen-gen tool
- **Week 4**: Build system integration
- **Week 5**: Backend refactoring to use generated code
- **Week 6**: Validation & golden baseline comparison

**Total Estimated Effort**: 6 weeks (~30 working days)

---

## 📞 **Component 2: "Adding New ISA" Formal Guide**

### 🎯 **Objective:**
Create comprehensive, step-by-step guide that reduces new ISA onboarding time from 2 months to 2 weeks.

### 📝 **Document Structure:**

**File**: `docs/adding_new_isa.md`

```markdown
# How to Add a New ISA to DCC Compiler

**Version**: 1.0  
**Last Updated**: September 22, 2026  
**Estimated Time**: 2 weeks for complete implementation

---

## 🎯 Prerequisites

Before starting, ensure you have:

1. **Technical Knowledge**
   - ✅ Understanding of your target architecture's ABI specification
   - ✅ Complete instruction set manual available
   - ✅ QEMU or similar emulator for testing
   
2. **Development Environment**
   - ✅ Rust compiler installed (latest stable)
   - ✅ Cross-compiler for target architecture
   - ✅ QEMU-user emulation for execution testing

3. **Time Commitment**
   - Week 1: Infrastructure & registers
   - Week 2: Codegen & validation

---

## 📋 Step-by-Step Implementation

### Step 1: Create Directory Structure (Day 1)

```bash
# From project root
mkdir -p src/codegen/{your_arch}/{registers,abi,asm,codegen,emit,special}

# Backup original monolith (if extracting)
cp src/codegen/original_your_arch.rs \
   src/codegen/your_arch/backup/original_monolith.rs.bak
```

**Directory Layout:**
```
src/codegen/your_arch/
├── mod.rs                    # Module entry point + re-exports
├── registers/
│   ├── mod.rs                # Register definitions
│   ├── gprs.rs               # General-purpose registers
│   └── fprs.rs               # Floating-point registers
├── abi/
│   ├── mod.rs
│   ├── calling_convention.rs # ABI specification
│   └── argument_passing.rs   # Parameter rules
├── asm/
│   ├── mod.rs
│   ├── emitter.rs            # Assembly emission logic
│   └── constraints.rs        # Operand constraints
├── codegen/
│   ├── mod.rs
│   ├── statements.rs         # Statement-level code gen
│   ├── expressions.rs        # Expression evaluation
│   └── control_flow.rs       # Control flow handling
├── emit/
│   ├── mod.rs
│   ├── prologue_epilogue.rs  # Frame setup/teardown
│   └── relocation.rs         # PIC support
└── special/
    ├── mod.rs
    ├── simd.rs               # SIMD extensions
    └── extensions.rs         # Architecture-specific features
```

---

### Step 2: Define Register Constants (Day 2)

Create `registers/mod.rs`:

```rust
//! YourArch Register Definitions

use crate::codegen::{Register, RegisterClass};

/// Total number of GPRs
pub const NUM_GPRS: usize = {/* e.g., 16 */};

/// All general-purpose registers
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchGpr {
    R0, R1, /* ... all registers */
    Zero,  // Implicit zero register
}

impl ArchGpr {
    pub fn as_register(&self) -> Register {
        match self {
            Self::R0 => Register::new(RegisterClass::GeneralPurpose, 0),
            Self::R1 => Register::new(RegisterClass::GeneralPurpose, 1),
            // ... etc
            Self::Zero => Register::invalid(),
        }
    }
    
    /// Is callee-saved per ABI?
    pub fn is_callee_saved(&self) -> bool {
        matches!(self, Self::R4 | Self::R5 | Self::R6 | /* ... */)
    }
}

/// Total number of FPRs
pub const NUM_FPRS: usize = {/* e.g., 32 */};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchFpr {
    F0, F1, /* ... all float regs */
}

// Implement conversion methods...
```

**Checklist:**
- [ ] All GPRs defined with indices
- [ ] All FPRs defined
- [ ] Special registers documented (zero, pc, sp, lr)
- [ ] Callee-saved/caller-saved classifications correct
- [ ] Unit tests pass

---

### Step 3: Implement Calling Convention (Day 3-4)

Create `abi/calling_convention.rs`:

```rust
//! YourArch Calling Convention Implementation

use crate::codegen::{Type, Register};
use super::registers::{ArchGpr, ArchFpr};

/// Maximum integer args in registers
pub const MAX_INREG_ARG_COUNT: usize = 4;

/// Maximum float args in registers
pub const MAX_FPINREG_ARG_COUNT: usize = 8;

pub struct ArchAbi;

impl ArchAbi {
    /// Determine register for nth argument
    pub fn arg_register(ty: &Type, n: usize) -> Option<Register> {
        if n >= MAX_INREG_ARG_COUNT {
            return None;  // Spilled to stack
        }
        
        match ty.kind() {
            TypeKind::Int | TypeKind::Ptr => {
                let reg_num = n as u8;
                Some(Register::from_gpr(ArchGpr::R0 as u8 + reg_num))
            },
            TypeKind::Float | TypeKind::Double => {
                let fpr_idx = n as u8;
                Some(Register::from_fpr(ArchFpr::S0 as u8 + fpr_idx))
            },
            _ => None,
        }
    }
    
    /// Return value register assignment
    pub fn return_register(ty: &Type) -> Option<Register> {
        let size = ty.size_bytes();
        
        match size {
            s if s <= 4 => {
                Some(Register::from_gpr(ArchGpr::R0))
            },
            s if s <= 8 => {
                Some(Register::from_gpr(ArchGpr::R0))
            },
            _ => {
                // Large structs via hidden pointer
                Some(Register::from_gpr(ArchGpr::R0))
            },
        }
    }
}
```

**Checklist:**
- [ ] Argument passing rules documented
- [ ] Return value conventions correct
- [ ] Stack alignment requirements met
- [ ] Variadic function handling implemented
- [ ] Unit tests validate edge cases

---

### Step 4: Implement Instruction Encoders (Day 5-10)

Create `codegen/statements.rs` and `codegen/expressions.rs`:

```rust
//! Statement-level Code Generation
//! Leverages shared library for common logic

use super::super::registers::*;
use super::super::abi::*;

/// YourArch statement emitter
pub struct ArchStatementEmitter<'a> {
    target: &'a crate::target::Target,
    reg_allocator: Box<dyn RegisterAllocator>,
    label_gen: LabelGenerator,
}

impl<'a> ArchStatementEmitter<'a> {
    /// Emit assignment with proper spill/load
    pub fn emit_assign_stmt(
        &mut self,
        dest: &LValue,
        rhs: &TypedExpr,
    ) -> Result<()> {
        let rhs_val = self.eval_expression(rhs)?;
        
        match dest {
            LValue::Register(reg) => {
                self.emit_mov(*reg, rhs_val.reg)?;
            },
            LValue::StackSlot(slot) => {
                self.store_to_slot(*slot, rhs_val.reg)?;
            },
            // ... other variants
        }
        
        Ok(())
    }
    
    /// Function call following ABI
    pub fn emit_call_stmt(
        &mut self,
        func: &FuncRef,
        args: &[TypedExpr],
        ret_ty: Type,
    ) -> Result<TypedExpr> {
        // Allocate result register per ABI rules
        let ret_reg = ArchAbi::return_register(&ret_ty)
            .expect("Return type must be simple");
        
        // Assign argument registers
        for (i, arg) in args.iter().enumerate().take(MAX_INREG_ARG_COUNT) {
            if let Some(arg_reg) = ArchAbi::arg_register(&arg.ty(), i) {
                let arg_val = self.eval_expression(arg)?;
                self.emit_mov(&arg_reg, arg_val.reg)?;
            } else {
                self.push_to_stack(arg)?;
            }
        }
        
        // Emit branch-link instruction
        self.emit_bl(func.address(), ret_reg)?;
        
        // Restore caller-saved if needed
        self.restore_caller_saved()?;
        
        Ok(TypedExpr::new(ret_ty.clone(), Some(ret_reg)))
    }
}
```

**For Expressions**, follow similar pattern but include:
- Load-store unit optimizations
- Barrel shifter usage (if applicable)
- Immediate operand validity checks
- SIMD vector operations (if supported)

---

### Step 5: Integration Testing (Day 11-12)

Run comprehensive validation:

```bash
#!/bin/bash
# scripts/test_new_isa.sh

set -e

echo "=== Testing YourArch Backend ==="

# Test 1: Compilation
dcc tests/fixtures/basic/add.c -m=your_arch -o /tmp/out.o || exit 1
echo "✅ Basic compilation works"

# Test 2: Golden baseline comparison
objdump -d /tmp/out.o > golden_baseline.S
gcc test.c -m=your_arch --features=your_arch-modular -o /tmp/mod.o
objdump -d /tmp/mod.o > modular_output.S
diff golden_baseline.S modular_output.S && echo "✅ Output identical"

# Test 3: Functional execution
gcc test.c -m=your_arch -o /tmp/test_bin
/tmp/test_bin | diff <(expected_output) - && echo "✅ Functional correctness"

# Test 4: Performance check
time gcc big_test.c -m=your_arch -o /tmp/big.o
```

---

### Step 6: Final Documentation & Release (Day 13-14)

Update project documentation:

1. **Add to src/codegen/mod.rs**:
```rust
pub mod your_arch;
```

2. **Update Cargo.toml feature flags**:
```toml
[features]
default = []
your_arch-modular = []
your_arch-legacy = ["your_arch-modular"]  # fallback option
full-modular = ["arm-modular", "mips-modular", "your_arch-modular"]
```

3. **Add to registry.rs**:
```rust
match arch {
    Arch::YourArch => Some(Box::new(
        AnyBackend::YourArch(your_arch::create_backend(true))
    )),
    // ... other architectures
}
```

4. **Create README in new backend directory**:
```markdown
# YourArch Backend

## Overview
YourArch backend implements code generation for the YourArch ISA.

## Features
- Supports GCC ABI compatibility mode
- Full SIMD extension coverage
- Inline assembly support

## Usage
Compile with `-m=your_arch --features=your_arch-modular`

## Testing
Run `./scripts/run_your_arch_tests.sh` to validate

## Maintainers
Current maintainer: @username
See CODEOWNERS for update process
```

---

## 🎯 Success Criteria Checklist

- [ ] ✅ All register definitions accurate
- [ ] ✅ Calling convention fully compliant
- [ ] ✅ Basic instructions work (add, sub, mul, div, loads, stores)
- [ ] ✅ Advanced features functional (simd, exceptions if applicable)
- [ ] ✅ Golden baselines match legacy monolith output
- [ ] ✅ No regression in existing tests
- [ ] ✅ Performance within 5% of monolith version
- [ ] ✅ Documentation complete
- [ ] ✅ Feature flags integrated
- [ ] ✅ Clean git history with descriptive commits

---

## 🚨 Common Pitfalls & Solutions

### Problem: Delay Slots (MIPS-style architectures)
**Solution**: Implement delay slot filler algorithm before branch emission
**Reference**: See MIPS backend `codegen/delay_slots/mod.rs`

### Problem: Unaligned Accesses
**Solution**: Add runtime checks or compile-time guarantees based on ABI
**Reference**: See ARM backend `emit/prologue_epilogue.rs`

### Problem: Floating Point Precision Loss
**Solution**: Implement proper rounding modes per IEEE 754
**Reference**: See AArch64 backend `special/simd.rs`

---

## 💡 Tips for Success

1. **Start Small**: Begin with integer arithmetic only, then add floats later
2. **Test Frequently**: Run unit tests after each module completion
3. **Use Shared Library**: Don't duplicate logic - leverage `shared/control_flow.rs`
4. **Profile Early**: Check performance in Week 1, not at the end
5. **Document Decisions**: Keep design rationale notes for future maintainers

---

## 📞 Resources & References

- **AMD64 SysV ABI**: https://refspecs.linuxbase.org/elf/
- **AArch64 AAPCS**: https://github.com/ARM-software/abi-aa
- **RISC-V Privileged Spec**: https://riscv.org/specifications/
- **LLVM Code Generator Model**: https://llvm.org/docs/CodeGenerator.html

---

## 🎉 Congratulations!

You've successfully added a new ISA backend to DCC Compiler! The backend now:
- ✅ Follows best practices
- ✅ Integrates seamlessly with shared library
- ✅ Passes all validation tests
- ✅ Is ready for production use

**Maintenance Schedule**: Review quarterly for bug fixes and improvements.

---

*Document Version: 1.0*  
*Last Updated: September 22, 2026*  
*Maintained by: DCC Core Team*
```

---

## 📋 **Implementation Timeline**

| Week | Component 1 Tasks | Component 2 Tasks | Deliverables |
|------|------------------|-------------------|--------------|
| **Week 1** | JSON specs for x86_64 | Directory structure creation | `x86_instructions.json` template |
| **Week 2** | codegen-gen parser development | Registers module | codegen-gen alpha |
| **Week 3** | codegen-gen generator development | ABI implementation | codegen-gen beta |
| **Week 4** | Build system integration | Statements codegen | Working pipeline |
| **Week 5** | Backend refactoring (x86_64) | Expressions codegen | First complete backend |
| **Week 6** | Validation & tuning | Testing framework | Production-ready components |
| **Week 7** | Extend to AArch64 | "Adding New ISA" final polish | Multi-ISA support |
| **Week 8** | Extend to RISC-V | Documentation publication | 100% Phase C complete |

---

## 🎯 **Success Metrics**

### Component 1: Data-Driven Encoding

| Metric | Target | Measurement Method |
|--------|--------|-------------------|
| Coverage | 100% core instructions | Count vs total defined |
| Generation Speed | <5 min full build | Wall clock timing |
| Error Detection | Pre-compilation | Schema validation |
| Maintainability | ↓90% code changes | Lines modified per update |

### Component 2: New ISA Guide

| Metric | Target | Measurement Method |
|--------|--------|-------------------|
| Onboarding Time | ≤2 weeks | Days from start to production |
| Completion Rate | 100% success | Test suite pass rate |
| Documentation Quality | ≥90% score | Peer review rating |
| Reusability | 3+ ISAs using same guide | Future adoption count |

---

## 🏆 **Final Outcome**

Upon completion:

✅ **Phase C achievement**: 100% (up from current 82%)  
✅ **Instruction encoding flexibility**: JSON-specified instead of hardcoded  
✅ **New ISA onboarding**: Reduced from 2 months to 2 weeks  
✅ **Documentation quality**: Professional standards met  
✅ **Project maturity**: Enterprise-grade compiler infrastructure  

---

## 🚀 **Recommendation**

**Execute both components in parallel:**

- **Team A** (2 engineers): Focus on data-driven instruction encoding
- **Team B** (1 engineer): Focus on comprehensive ISA guide
  
**Parallel execution**: 8 weeks total  
**Sequential execution**: 14 weeks total (6+8)

Given your constraint of single developer, recommend sequential approach:
1. Start with Component 1 (Weeks 1-6)
2. Then execute Component 2 (Weeks 7-8)

**Total commitment**: 8 additional weeks to reach 100% Phase C completion

---

*"The journey from monolith to modular perfection completes here!"* — Final Goal Achievement

---

**Document Version**: 1.0  
**Last Updated**: September 22, 2026  
**Approval Required**: Yes (before beginning implementation)
