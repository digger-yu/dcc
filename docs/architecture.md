# DCC Compiler Architecture Design

**版本**: 3.0 - Advanced Modular Architecture (Design > Implementation)  
**日期**: 2024-09-23  
**项目**: DCC Multi-ISA Backend Extension - Phase C  

---

## 🎯 Core Philosophy

### Design Leads, Implementation Follows

**Key Principle:**  
The architecture design should be **ahead of implementation**, providing a complete blueprint for all backends regardless of their current completion status.

> "设计是设计，实现是实现，不完整的可以后期补齐，但设计要先进"

---

## 📊 Strategic Vision

### Universal Backend Pattern

```
┌─────────────────────────────────────────────────────────────────┐
│                     Unified Architecture Pattern                │
│                    (Applies to ALL Backends Equally)            │
└───────────────────────────┬─────────────────────────────────────┘
                            │
         ┌──────────────────┴──────────────────┐
         │                                     │
┌────────▼────────┐                   ┌───────▼────────┐
│ x86_64 Backend  │                   │ AArch64 Backend│
│ [Tier-1]        │                   │ [Tier-1]       │
│ Production      │                   │ Production     │
│ Ready           │                   │ Ready          │
└────────┬────────┘                   └───────┬────────┘
         │                                     │
         └──────────────┬──────────────────────┘
                        │
         ┌──────────────▼──────────────────────┐
         │           SHARED LAYER               │
         │   (codegen::shared/* modules)       │
         │   ────────────────────────────      │
         │   • Backend Trait Definition        │
         │   • Registry System                 │
         │   • Feature Set Management          │
         │   • ELF Object Generation           │
         │   • Cross-ISA Utilities             │
         └──────────────┬──────────────────────┘
                        │
         ┌──────────────┼──────────────────────┐
         │              │                      │
┌────────▼────┐ ┌──────▼──────┐    ┌─────────▼─────────┐
│RISC-V 64    │ │ MIPS/MIPS64 │    │     ARM           │
│[Tier-1]     │ │[Tier-1]     │    │   [Tier-2]        │
│Production   │ │Production   │    │ Development       │
│Ready        │ │Ready        │    │ Track             │
└─────────────┘ └─────────────┘    └───────────────────┘
```

### Important Note:
**Even though ARM is currently in development**, its **design is fully specified** and follows the same pattern as other backends. The difference is purely in implementation maturity, not architectural completeness.

---

## 🏗️ Complete Backend Template (Advanced Design)

### Every Backend MUST Have This Structure

```
src/codegen/{backend}/
├── mod.rs                              # Module declaration & exports
│   ├── Backend trait implementation
│   ├── Public API exports
│   └── Integration with registry system
│
├── registers/                          # ⭐ Register Abstraction Layer
│   ├── mod.rs                          # Register definitions for THIS ISA
│   ├── constants.rs                    # Named register constants
│   ├── encoding.rs                     # Register encoding rules
│   └── allocator.rs                    # Register allocation helpers
│
├── abi/                                # ⭐ ABI/Calling Convention Layer
│   ├── mod.rs                          # Public ABI interface
│   ├── calling_convention.rs           # Parameter passing rules
│   ├── stack_layout.rs                 # Stack frame organization
│   ├── prologue_epilogue.rs            # Function entry/exit code
│   └── varargs.rs                      # Variadic argument handling
│
├── codegen/                            # ⭐ Core Code Generation
│   ├── mod.rs                          # Main codegen dispatcher
│   ├── compile.rs                      # Top-level compilation flow
│   ├── functions.rs                    # Function-level codegen
│   ├── globals.rs                      # Global variable emission
│   ├── control_flow.rs                 # Branches, loops, switches
│   └── blocks.rs                       # Basic block processing
│
├── emit/                               # ⭐ Statement/Expression Emission
│   ├── mod.rs                          # Emitter coordination
│   ├── stmt.rs                         # Statement-level emission
│   ├── expr/                           # Expression submodules
│   │   ├── binary.rs                   # Binary operations
│   │   ├── unary.rs                    # Unary operations
│   │   ├── call.rs                     # Function calls
│   │   ├── lvalue.rs                   # L-value address computation
│   │   ├── literal.rs                  # Literal value emission
│   │   ├── compound.rs                 # Compound assignments
│   │   ├── cast.rs                     # Type casting
│   │   ├── init.rs                     # Initialization expressions
│   │   ├── va_arg.rs                   # va_list access
│   │   └── inline_asm.rs               # Inline assembly handling
│   └── asm/                            # Assembly-specific code
│       ├── emitter.rs                  # Machine code emitter
│       ├── planner.rs                  # Instruction scheduling
│       └── expansion.rs                # Macro/template expansion
│
├── helpers/                            # ⭐ Utility Functions
│   ├── mod.rs                          # Utility exports
│   ├── encoding.rs                     # Instruction encoding utilities
│   ├── addressing_modes.rs             # Addressing mode helpers
│   └── label_generator.rs              # Unique label generation
│
└── special/                            # ⭐ Special Features Layer
    ├── mod.rs                          # Feature exports
    ├── tls.rs                          # Thread-local storage support
    ├── sanitizer/                      # Sanitizer runtime support
    │   ├── asan.rs                     # AddressSanitizer
    │   ├── hwasan.rs                   # Hardware ASAN
    │   ├── msan.rs                     # MemorySanitizer
    │   └── ubsan.rs                    # UndefinedBehaviorSanitizer
    ├── mte.rs                          # Memory Tagging Extension
    └── vector_extensions.rs            # SIMD/vector extensions
```

### Key Design Principles:

1. **Uniform Structure**  
   Every backend follows EXACTLY the same directory structure, regardless of completion status.

2. **Interface Completeness**  
   All interfaces must be defined upfront, even if some are temporary stubs.

3. **Progressive Implementation**  
   Start with minimal viable implementation, then progressively enhance each module.

4. **Layered Abstraction**  
   Clear separation between:
   - Register management
   - ABI/calling convention
   - Core codegen logic
   - Concrete expression/statement emission
   - Utility functions
   - Special features (TLS, sanitizers, etc.)

---

## 🔧 Shared Infrastructure (Universal Dependencies)

### All Backends Depend on These Common Modules:

```
src/codegen/shared/
├── mod.rs                          # Shared module exports
├── backend.rs                      # Unified Backend trait definition
├── registry.rs                     # Backend registration/discovery
├── features.rs                     # FeatureSet capability system
├── types.rs                        # Shared type definitions
├── elf.rs                          # ELF object file generator
├── coff.rs                         # COFF/PE format support (Windows)
├── asm_constraints.rs              # Cross-ISA inline asm constraint parsing
├── mini_asm.rs                     # Non-x86 inline assembly encoder
├── control_flow.rs                 # Generic control flow handlers
├── expressions.rs                  # Shared expression evaluation framework
├── instruction_encoding.rs         # Data-driven instruction encodings
├── register_allocator.rs           # Generic register allocation algorithms
└── optimization_passes.rs          # Common optimization passes
```

---

## 📋 Implementation Phases with Design-First Approach

### Phase 1: Foundation (Weeks 1-2) - ✅ Done

**Focus: Shared infrastructure only**
- ✅ Create `shared/` module structure
- ✅ Define unified `Backend` trait
- ✅ Implement registry system
- ✅ Add feature capability system

**Deliverable:** Working shared layer that ANY backend can implement

---

### Phase 2: Complete Backend Skeletons (Weeks 3-4) - PENDING

**Focus: Empty skeletons for ALL backends following design**
- ✅ Define complete directory structure for ARM
- ✅ Add empty stub implementations for all modules
- ✅ Ensure compile-time integration with registry
- ✅ Verify all interfaces match design specs

**Important:** Even incomplete backends have FULL design!

**Example: ARM Backend Skeleton**
```rust
// src/codegen/arm/mod.rs
mod backend;           // Implemented later
mod registers;         // Currently stub, implements R0-R15 + FP+LR+SP
mod abi;               // Currently stub, implements AAPCS
mod codegen;           // Currently stub, implements core functions
mod emit;              // Currently stub, implements statement/expression emission
mod helpers;           // Currently stub, utility functions
mod special;           // Currently stub, TLS/sanitizer stubs

// All these directories exist and follow the exact same pattern as x86_64, AArch64, etc.
// Only IMPLEMENTATION differs, NOT DESIGN
```

---

### Phase 3: Progressive Enhancement (Weeks 5-8) - PENDING

**Focus: Fill in implementations gradually**

#### Subphase 3a: Priority Backends
- x86_64 → already complete ✅
- AArch64 → already complete ✅
- RISC-V 64 → enhancement needed
- MIPS/MIPS64 → enhancement needed

#### Subphase 3b: ARM Backend
- Week 5: Implement `registers/` layer
- Week 6: Implement `abi/` layer  
- Week 7: Implement basic `codegen/` layer
- Week 8: Implement `emit/` layer

**Strategy:** Each layer starts as functional stub, gets progressively enhanced

---

### Phase 4: Refactoring & Duplication Elimination (Weeks 9-10)

**Focus: Apply data-driven refactoring across ALL backends**
- Identify common patterns
- Extract shared logic to `shared/` modules
- Replace hard-coded logic with data-driven configurations
- Achieve 60%+ code reduction target

---

### Phase 5: Optimization & Polish (Weeks 11-12)

**Focus: Performance tuning and final polish**
- Profile all backends
- Optimize hot paths
- Enhance error messages
- Document best practices

---

## 💡 Design Philosophy Highlights

### 1. Complete Blueprint First

**Before any implementation:**
- All module boundaries defined
- All interfaces specified
- All public APIs documented
- Directory structure created for EVERY backend

**After specification:**
- Some backends may have minimal implementations
- Some may have partial implementations
- Some may be fully implemented
- But **ALL have complete design**

### 2. Consistent Interface Contract

Every backend implements the SAME `Backend` trait:
```rust
pub trait Backend {
    fn arch(&self) -> Arch;
    fn features(&self) -> FeatureSet;
    fn compile(&mut self, input: &CompileInput<'_>) -> Result<CommonImage, Vec<Diagnostic>>;
    fn render_asm(&mut self, input: &CompileInput<'_>) -> Result<Vec<u8>, Vec<Diagnostic>>;
    fn build_object(&mut self) -> Result<Vec<u8>, Vec<Diagnostic>>;
}
```

This contract exists **regardless of implementation quality**.

### 3. Progression Path

```
┌─────────────────────────────────────────┐
│ Stage 1: Design Complete                │
│ All modules exist as empty/fill-in shells │
└─────────────────────────────────────────┘
                    ↓
┌─────────────────────────────────────────┐
│ Stage 2: Minimal Viable Implementation  │
│ Functional but basic per-module         │
└─────────────────────────────────────────┘
                    ↓
┌─────────────────────────────────────────┐
│ Stage 3: Enhanced Implementation        │
│ Full feature set, optimized             │
└─────────────────────────────────────────┘
                    ↓
┌─────────────────────────────────────────┐
│ Stage 4: Refactored & Optimized         │
│ Data-driven, deduplicated, polished     │
└─────────────────────────────────────────┘
```

---

## 🎯 Success Metrics

| Metric | Target | Measurement Method |
|--------|--------|-------------------|
| **Design Completeness** | 100% | All backends have full spec |
| **Implementation Coverage** | >95% | By end of Phase 3 |
| **Code Reduction** | 60%+ | By end of Phase 4 |
| **New ISA Integration Time** | < 2 weeks | Measured by template reuse |
| **Consistency Score** | 1.0 (Perfect) | Automated consistency checks |

---

## 📝 Documentation Strategy

### For Each Backend:

**Design Documents (Must Exist):**
- `ARCHITECTURE.md` - High-level design overview
- `MODULES.md` - Detailed module specifications
- `ABI.md` - Calling convention documentation
- `REGISTER_SET.md` - Register usage rules
- `INSTRUCTION_TABLES.md` - Data-driven instruction mappings

**Implementation Files:**
- Can start as stubs
- Can be incomplete
- Must follow the design exactly

---

## 🚀 Migration Strategy

### Step-by-Step Process:

1. **Define Design** ← Start here!
   - Specify all modules
   - Define all interfaces
   - Document all behaviors
   - Create templates

2. **Create Skeletons**
   - Empty directories
   - Stub implementations
   - Compile-only code

3. **Implement Layer by Layer**
   - Registers first
   - Then ABI
   - Then core codegen
   - Then emitters
   - Finally helpers/special

4. **Enhance Gradually**
   - From stubs to functional
   - From functional to complete
   - From complete to optimized

---

## 🎓 Lessons from Design-First Approach

### What Works Well:
- ✅ Clear expectations from day one
- ✅ Parallel development possible
- ✅ No surprises during implementation
- ✅ Easy to identify gaps
- ✅ Consistent quality across ISAs

### Challenges:
- ⚠️ Requires upfront thinking
- ⚠️ More initial documentation effort
- ⚠️ Need discipline to follow design
- ⚠️ May require rethinking designs

### Best Practices:
- Use RFC process for major design changes
- Keep design documents versioned
- Automate consistency validation
- Regular design reviews

---

## 📌 Key Takeaway

> **"The architecture design exists independently of implementation progress."**

Whether a backend is:
- Production-ready (x86_64, AArch64)
- Nearly complete (RISC-V, MIPS)
- Still developing (ARM)

**They all share the same complete design blueprint.** The difference is purely in implementation maturity, not architectural vision.

This ensures:
- Future backends can be added easily
- Current backends evolve consistently
- No technical debt from compromises
- Long-term maintainability guaranteed

---

*Document Version: 3.0 (Design-First Approach)*
*Last Updated: 2024-09-23*
*Status: Active Development - Following Design-First Philosophy*
