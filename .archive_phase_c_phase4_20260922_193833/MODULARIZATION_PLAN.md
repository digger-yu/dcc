# DCC x86_64 Backend Modularization - Execution Plan

## Current State Analysis (Turn 29)

### Monolithic mod.rs Statistics:
- **Total Lines**: 35,107 lines
- **Current Modules Declared** (lines 36-54):
  - `mod asm;` 
  - `mod backend;`
  - `mod codegen;`
  - `mod abi;` (partially integrated - shadow modules)
  - `mod emit;` (mostly integrated)
  - `mod mem;` (partially integrated - shadow modules)
  - `mod parallel;`
  - `mod special;`

### Already Extracted Modules (from Turns 21-28):

#### ABI Module Family (`src/codegen/x86_64/abi/`):
- ✅ `mod.rs` (463 bytes)
- ✅ `classes.rs` (10,420 bytes) - Argument classification
- ✅ `call.rs` (39,558 bytes) - SysV AMD64 calling convention  
- ⚠️ `frame.rs` (3,093 bytes) - Frame management (partially integrated)

#### MEM Module Family (`src/codegen/x86_64/mem/`):
- ✅ `mod.rs` (405 bytes)
- ✅ `addr.rs` (20,272 bytes) - Address calculations
- ⚠️ `load_store.rs` (1,939 bytes) - Load/store helpers (shadow module)
- ⚠️ `global.rs` (1,225 bytes) - Global variable handling (shadow module)

#### EMIT Expression Submodules (`src/codegen/x86_64/emit/expr/`):
All completed in Turn 28:
- ✅ `literal.rs` - Literal expressions
- ✅ `lvalue.rs` - L-value handling
- ✅ `unary.rs` - Unary operators
- ✅ `binary.rs` - Binary operators
- ✅ `init.rs` - Assignment and initialization
- ✅ `call.rs` - Function calls (Turn 28 completion)
- ✅ `cast.rs` - Cast expressions
- ✅ `compound.rs` - Compound literals, tuples, conditionals
- ✅ `va_arg.rs` - Variadic argument handling
- ✅ `mod.rs` (18 lines) - Re-exports

#### ASM Submodule (`src/codegen/x86_64/asm/`):
- ✅ `mod.rs` (23 lines)
- ✅ `directive.rs`
- ✅ `expansion.rs`
- ✅ `emit.rs`
- ✅ `label.rs`
- ✅ `operand.rs`
- ✅ `template.rs`
- ✅ `disasm.rs`
- ✅ `planner/mod.rs` - Register planning

#### SPECIAL Features (`src/codegen/x86_64/special/`):
- ✅ `int128.rs`
- ✅ `tls.rs`
- ✅ `sanitizer.rs`
- ✅ `x87_floating_point.rs`
- ✅ `mod.rs`

#### PARALLEL Infrastructure (`src/codegen/x86_64/parallel/`):
- ✅ Partial structure exists

### Code Distribution Analysis:

From reading mod.rs (~1000 lines sampled), key functional areas identified:

1. **Struct Definition** (lines 121-371): ~250 lines
   - Core fields for code generation state machine
   
2. **Helper Functions** (lines 59-115, etc.): ~500 lines scattered
   - `resolve_designator`, `extract_index_expr`, etc.
   
3. **Impl Blocks** (scattered throughout 35K lines):
   - `impl Codegen`: Main methods
   - `impl AsmRegisterPlanner`: Register allocation
   - Various impl blocks for specific features

4. **Public API Functions** (~50 functions):
   - `new()`, `emit()`, `pos()`, `label()`, `fresh_label()`
   - `resolve()`, `compile()`, etc.

---

## Target Architecture (User Specification):

```
src/codegen/
├── x86_64/
│   ├── mod.rs                    # Module declarations only (~300 lines MAX)
│   ├── codegen.rs                # Codegen struct definition
│   ├── public_api.rs             # Public exports
│   ├── abi/                      # Calling convention handling
│   │   ├── classes.rs            # Argument classification
│   │   └── call.rs              # Function call lowering
│   ├── asm/                      # Assembly encoding
│   │   ├── directive.rs
│   │   ├── expansion.rs
│   │   ├── emitter.rs
│   │   └── planner/
│   │       └── orchestration.rs
│   ├── emit/                     # IR → assembly emission
│   │   ├── expr/
│   │   │   ├── literal.rs        ✅ EXISTING
│   │   │   ├── lvalue.rs         ✅ EXISTING
│   │   │   ├── unary.rs          ✅ EXISTING
│   │   │   ├── binary.rs         ✅ EXISTING
│   │   │   ├── init.rs           ✅ EXISTING
│   │   │   ├── call.rs           ✅ EXISTING
│   │   │   ├── cast.rs           ✅ EXISTING
│   │   │   ├── compound.rs       ✅ EXISTING
│   │   │   ├── va_arg.rs         ✅ EXISTING
│   │   │   └── mod.rs            # Re-exports all expr modules
│   │   └── stmt.rs               # Statement emission
│   ├── mem/                      # Memory addressing
│   │   └── addr.rs
│   ├── special/                  # Special features
│   │   ├── int128.rs
│   │   └── tls.rs
│   ├── parallel/                 # Function-level parallelism
│   │   └── orchestration.rs
│   └── backend.rs                # Backend trait implementation
```

---

## Execution Strategy - PHASED APPROACH

### Phase 0: Analysis Complete ✅
**Status**: IN PROGRESS - Documenting current state and mapping code sections

### Phase 1: Directory Structure Creation
**Goal**: Create entire directory tree BEFORE any file extraction
**Files to create**:
```bash
mkdir -p src/codegen/x86_64/{asm/planner,emit/expr,mem,special,parallel}
```

**Note**: Most directories already exist from Turns 21-28! Only need verification.

### Phase 2: Content Extraction Strategy
Extract these categories from monolithic mod.rs into respective modules:

#### Category A: ABI-Related Code
**Source**: Lines ~X-X (needs identification)
**Destination**: `src/codegen/x86_64/abi/`
- Argument classification functions → `classes.rs`
- SysV calling convention → `call.rs` (already done in Turn 28)
- Frame pointer management → `frame.rs`

#### Category B: ASM-Related Code
**Source**: Scattered throughout
**Destination**: `src/codegen/x86_64/asm/`
- Instruction encoding helpers → existing modules
- Directive expansion → `directive.rs`
- Template instantiation → `template.rs`
- Register planning → `planner/mod.rs`

#### Category C: Memory Addressing
**Source**: Lines ~X-X
**Destination**: `src/codegen/x86_64/mem/`
- RIP-relative addressing → `addr.rs`
- Load/store operations → `load_store.rs`
- Global variable references → `global.rs`

#### Category D: Special Features
**Source**: Throughout
**Destination**: `src/codegen/x86_64/special/`
- `__int128` support → `int128.rs`
- TLS handling → `tls.rs`
- Sanitizer instrumentation → `sanitizer.rs`
- x87 floating point → `x87_floating_point.rs`

#### Category E: Parallel Infrastructure
**Source**: Throughout
**Destination**: `src/codegen/x86_64/parallel/`
- Thread-local expression evaluation → `orchestration.rs`
- Function-level parallel dispatch

#### Category F: Statement Emission
**Source**: Mixed with expressions
**Destination**: `src/codegen/x86_64/emit/stmt.rs`
- Control flow statements
- Declaration statements
- Expression statements

### Phase 3: Cleanup & Integration
1. **Reduce mod.rs**: Delete moved code, keep only module declarations
2. **Create public_api.rs**: Re-export high-level APIs
3. **Refactor codegen.rs**: Move struct definition there
4. **Final verification**: Run unified test suite

---

## File Extraction Mapping (To Be Completed)

Need to grep mod.rs for specific patterns to map:

### Pattern 1: Function Definitions by Domain
```bash
grep -n "fn [a-z_]*(.*abi\|fn [a-z_]*(.*class\|fn [a-z_]*(.*sysv" src/codegen/x86_64/mod.rs
grep -n "fn [a-z_]*(.*asm\|fn [a-z_]*(.*directive\|fn [a-z_]*(.*instruction" src/codegen/x86_64/mod.rs
grep -n "fn [a-z_]*(.*addr\|fn [a-z_]*(.*memory\|fn [a-z_]*(.*load\|fn [a-z_]*(.*store" src/codegen/x86_64/mod.rs
```

### Pattern 2: Struct Fields by Responsibility
```bash
# ABI-related fields (calling convention, registers, frame)
# ASM-related fields (directives, templates, labels)
# Memory-related fields (addresses, relocations, globals)
```

### Pattern 3: Method Groupings
```bash
# Methods starting with gen_ (generation)
# Methods starting with emit_ (emission)
# Methods starting with resolve_ (resolution)
# Methods starting with lower_ (lowering)
```

---

## Known Issues & Risk Mitigation

### Risks Identified:
1. **Inter-module Dependencies**: Some functions may call each other across boundaries
   - *Mitigation*: Track dependency graph before extraction
   
2. **Test Failures**: Expected during migration (per user instructions)
   - *Mitigation*: Accept intermediate errors, validate only at end
   
3. **Dead Code**: Comment mentions "dead_code allows" for unconnected modules
   - *Mitigation*: Run cargo check with --force-warn after final integration

4. **Line Count Overflow**: User requires ≤ 72 chars per line in commit files
   - *Mitigation*: Strict line length checking script

### Dependencies Between Modules:
```
abi/call.rs depends on: abi/classes.rs
emit/expr/call.rs depends on: abi/call.rs, mem/addr.rs
asm/* depends on: no internal dependencies (leaf module)
special/* likely independent
parallel/orchestration depends on: most modules (top-level coordinator)
```

---

## Success Metrics

✅ **Phase 1 Complete**: All directories created  
✅ **Phase 2 Complete**: All code extracted from mod.rs  
✅ **Phase 3 Complete**: mod.rs ≤ 300 lines  
✅ **Zero Behavior Change**: Same compilation output  
✅ **Unified Test Suite Passes**: All golden hashes match  

---

## Next Steps (Immediate Actions)

1. **Verify existing directory structure** - Confirm what's already created
2. **Run pattern analysis** - Identify exact line ranges for each category
3. **Document extraction mapping** - Line-to-file reference table
4. **Begin Phase 1** - Create any missing directories
5. **Start extraction scripts** - Batch-migration approach

---

## Progress Tracking

| Phase | Status | Files Created | Lines Moved | Notes |
|-------|--------|---------------|-------------|-------|
| Phase 0 | 🔄 In Progress | N/A | N/A | Analyzing current state |
| Phase 1 | ⏳ Pending | TBD | TBD | Directory creation |
| Phase 2 | ⏳ Pending | TBD | TBD | Content extraction |
| Phase 3 | ⏳ Pending | TBD | TBD | Cleanup & integration |

**Current Timestamp**: Tuesday, September 22, 2026
**Next Turn Focus**: Complete analysis, identify exact line mappings, begin Phase 1
