# Backend Refactor Plan Implementation Report

**Date:** September 20, 2026  
**Version:** v1.0 (Initial Phase Report)  
**Status:** Phase 0 Complete - Analysis & Preparation Done; Execution in Progress

---

## Executive Summary

This document reports on the execution of the complete backend refactor plan from `docs/backend_refactor_plan.md`. The plan is extremely comprehensive and involves:

1. **Part A**: Splitting the massive `x86_64.rs` (~41,659 lines, ~1.9MB) into modular components
2. **Part B**: Unifying Backend contracts across all architectures
3. **Part C-D**: Creating FeatureSet and registry system
4. **Part E**: Fixing RiscV32 has_backend inconsistency
5. **Part F-G**: Adding parity tests and behavioral probes
6. **Part H**: Documentation updates

### Current Status

✅ **Phase 0 (Complete)**: Code structure analysis and diagnosis completed  
🔄 **Part A1 (Complete)**: Directory structure created (`src/codegen/x86_64/{emit/expr,asm,abi,elf_side,tests}`)  
🔄 **Part A2 (In Progress)**: Extracting asm/* modules  
⏳ **Remaining Parts**: A3-H pending  

---

## Key Findings from Analysis

### 1. Code Structure Issues Confirmed

- **x86_64.rs**: 41,659 lines in single file (plan predicted ~39,562 lines, actual is even larger)
- **Module dependencies tightly coupled**: Code generation, assembly emission, ELF packaging, debug info all mixed together
- **No Backend trait**: Each architecture implements its own interface
- **Diagnostics strategy inconsistent**: x86_64 fails only on Error; others fail on any diagnostic

### 2. Critical Functions Identified for Extraction

| Function | Lines | Purpose | Priority |
|----------|-------|---------|----------|
| `gen_expr` | ~4,400 | Expression code generation (needs internal split) | P0 |
| `gen_stmt` | ~800 | Statement code generation | P0 |
| `emit_inline_asm` | ~500+ | Inline assembly emission | P0 |
| `compile` | ~1,000 | Overall compilation orchestration | P1 |
| `build_elf_*` | N/A | ELF packaging (in mod.rs) | P1 |

### 3. Assembly-Related Code Locations

Found inline asm functions at:
- Line 32455: `emit_inline_asm`
- Line 32549: `emit_inline_asm_goto`  
- Line 32627+: `is_asm_directive` and related helpers
- Line 12150, 8125, 9706: Call sites

Additional asm infrastructure:
- `AsmDirectiveExpander` - macro/directive expansion
- `AsmLocalLabels` - local label resolution
- `build_asm_operand_map` - operand binding
- `resolve_asm_template` - template substitution

---

## Planned Implementation Strategy

### Part A - x86_64 Module Splitting (Sequential PRs)

#### PR-A1 ✅: Create Directory Structure (COMPLETED)
```bash
mkdir -p src/codegen/x86_64/{emit/expr,asm,abi,elf_side,tests}
```

#### PR-A2 🔄: Extract asm/* Module (IN PROGRESS)
**Scope**: Inline assembly code + mini-asm integration

**Files to create**:
- `src/codegen/x86_64/asm/mod.rs` - Module root with re-exports
- `src/codegen/x86_64/asm/template.rs` - Template expansion logic
- `src/codegen/x86_64/asm/operand.rs` - Operand mapping/binding
- `src/codegen/x86_64/asm/directive.rs` - Directive recognition
- `src/codegen/x86_64/asm/label.rs` - Local label handling
- `src/codegen/x86_64/asm/emit.rs` - Main emission functions

**Functions to extract**:
- `emit_inline_asm()` → `asm/emit.rs`
- `emit_inline_asm_goto()` → `asm/emit.rs`
- `build_asm_operand_map()` → `asm/operand.rs`
- `resolve_asm_template()` → `asm/template.rs`
- `is_asm_directive()` → `asm/directive.rs`
- `emit_resolved_asm_lines()` → `asm/emit.rs`
- `merge_standalone_prefixes()` → `asm/directive.rs`

**Migration steps**:
1. Copy functions to new module files (no semantic changes)
2. Update imports and visibility
3. Verify compile: `cargo build --tests`
4. Run integration tests

#### PR-A3: Extract abi + call/frame
#### PR-A4: Extract mem/addr
#### PR-A5: Extract special (int128/vector/x87/sanitizer/tls)
#### PR-A6: Extract parallel + elf collection
#### PR-A7: Internal expr splitting

---

### Part B - Backend Contract Unification

#### PR-B1: Unified Diagnostic Policy
**Goal**: Make all architectures follow same failure semantics

**Changes**:
```rust
// New unified function
pub fn has_fatal_diagnostics(diagnostics: &[Diagnostic]) -> bool {
    diagnostics.iter().any(|d| d.severity == Severity::Error)
}
```

**Affected locations**:
- All match arms in `codegen/mod.rs::emit()`
- Replace: `if !cg.diagnostics.is_empty()` → `if has_fatal_diagnostics(&cg.diagnostics)`

#### PR-B2: EmitOptions Bundle
**Goal**: Reduce function signature complexity

**Before**:
```rust
pub fn emit(
    module: &Module, target: &Target, relocatable: bool, 
    emit_asm: bool, source_name: &str, debug_level: u8,
    dwarf_version: u8, sanitizers: SanitizerFlags,
    lto_enabled: bool, function_sections: bool,
    parallel_codegen: bool, bounds_check: bool,
    trapv: bool, offload: Option<&...>, line_map: &[LineSpan],
    verbose: bool
) -> Result<Vec<u8>, Vec<Diagnostic>>
```

**After**:
```rust
pub struct EmitOptions<'a> {
    pub relocatable: bool,
    pub emit_asm_text: bool,
    pub source_name: String,
    pub debug_level: u8,
    pub dwarf_version: u8,
    pub sanitizers: SanitizerFlags,
    pub lto_enabled: bool,
    pub function_sections: bool,
    pub parallel_codegen: bool,
    pub bounds_check: bool,
    pub trapv: bool,
    pub offload: Option<&'a crate::profile::offload::OffloadSection>,
    pub line_map: &'a [LineSpan],
    pub verbose: bool,
}

pub struct CompileInput<'a> {
    pub module: &'a Module,
    pub target: &'a Target,
    pub opts: EmitOptions<'a>,
}
```

#### PR-B3: LTO Section Injection
#### PR-B4: Debug Hooks Unification
#### PR-B5: Package API Standardization

---

### Part C - Backend Trait Definition

```rust
// src/codegen/backend.rs

pub trait Backend {
    fn arch(&self) -> Arch;
    fn features(&self) -> FeatureSet;
    fn compile(&mut self, input: &CompileInput<'_>) -> Result<CommonImage, Vec<Diagnostic>>;
    fn render_asm(&mut self, input: &CompileInput<'_>) -> Result<String, Vec<Diagnostic>>;
}

pub struct CommonImage {
    pub code: Vec<u8>,
    pub rodata: Vec<u8>,
    pub data: Vec<u8>,
    pub bss: u64,
    pub tls: TlsImage,
    pub symbols: ObjectSymbols,
    pub relocs: ObjectRelocs,
    pub extra_sections: Vec<ExtraSection>,
    pub diagnostics: Vec<Diagnostic>,
    pub func_sizes: BTreeMap<String, u64>,
}
```

---

### Part D - FeatureSet & Registry

```rust
// src/codegen/features.rs

#[non_exhaustive]
pub enum Feature {
    CoreArith, CoreCall, Float, LongDouble, Int128, Bitfield,
    VaList, Tls, InlineAsm(AsmLevel), Vector, Atomics,
    Sanitizer(SanKind), CfiUnwind, StackProtect,
    ParallelCodegen, Lto, BoundsCheck, Trapv,
    ObjectFormat(ObjectFmt), Dwarf,
}

pub struct FeatureSet {
    pub features: HashSet<Feature>,
    pub tier: Tier,
}

pub enum Tier {
    Tier1, // Full support (kernel/Lua/libiberty)
    Tier2, // Basic support (hello/cross smoke)
    Disabled,
}
```

```rust
// src/codegen/registry.rs

pub fn backend_for(arch: Arch) -> Option<Box<dyn Backend>> {
    match arch {
        Arch::X86_64 => Some(Box::new(x86_64::Codegen::new(target))),
        Arch::AArch64 => Some(Box::new(aarch64::Aarch64Codegen::new(target))),
        Arch::RiscV64 => Some(Box::new(riscv64::Riscv64Codegen::new(target))),
        Arch::Mips | Arch::Mips64 => Some(Box::new(mips::MipsCodegen::new(target))),
        Arch::Arm => Some(Box::new(arm::ArmCodegen::new(target))),
        Arch::RiscV32 => None,  // Fixed!
    }
}
```

---

### Part E - Fix RiscV32 has_backend

**Current Issue**: `has_backend()` returns `true` but `emit()` unconditionally errors with E0399

**Fix Options**:
1. **Option A (Recommended)**: Set `has_backend = false` and update documentation
2. **Option B**: Implement minimal RV32 stub (out of scope for this phase)

**Implementation**:
- Modify `target.rs` or `cli.rs` where `has_backend()` is defined
- Update `docs/backend_feature_matrix.md`
- Update `README.md` to reflect actual state

---

### Part F - Parity Tests

**File**: `tests/backend_parity.rs`

**Test Layers**:
- **L0**: Compilation success/failure + ELF validation (all archs)
- **L1**: QEMU/user-runner execution (CI Linux only)
- **L2**: Golden hash / disassembly comparison (mainly x86_64)

**Structure**:
```rust
#[test]
fn test_backend_parity() {
    let fixtures = read_fixture_list();
    for fixture in fixtures {
        for arch in ALL_ARCHS {
            let result = compile(fixture, arch);
            expect_test_result(&fixture, arch, result);
        }
    }
}
```

---

### Part G - Behavioral Probes & CI

**Script**: `scripts/probe_backend_features.sh`

**Output**: JSON/TSV matrix of `arch,feature,status`

**CI Job**: `check_feature_matrix`
- Runs probe script
- Compares against `docs/backend_feature_matrix.md`
- Fails if misaligned (prevents documentation drift)

---

### Part H - Documentation Updates

**Files to update**:
- `docs/architecture.md`: Replace line numbers with symbol paths
- `docs/backend_feature_matrix.md`: Add generated header
- `README.md`: Link to matrix instead of inline tables
- `docs/support_features.md`: Remove absolute references

**New docs to create**:
- `docs/backend_tier1.md`: Product-tier checklist
- `src/codegen/x86_64/README.md`: Module overview

---

## Risk Mitigation

| Risk | Mitigation Strategy |
|------|---------------------|
| Breaking changes during split | Pure move PRs first, no semantic changes |
| Missing tests | Integration tests must pass before merge |
| Regression in codegen quality | Golden `.text` hash baseline (phase 0) |
| Diagnostics policy breaking user code | Document as breaking change, provide migration guide |
| Documentation drift | CI check_job enforces alignment |

---

## Testing Strategy

### Pre-Merge Checks (All PRs):
```bash
cargo build --tests
cargo test --test integration
bash scripts/bootstrap.sh  # if environment allows
```

### Regression Baseline:
- Record SHA256 of 10-20 golden `.o` files (x86_64 only, Phase 0)
- Compile-time baseline (-O2 on representative fixtures)
- `.text` section size baseline

### Post-Merge Verification:
- Full integration test suite passes
- No new warnings in `cargo build`
- Golden hashes unchanged (unless intended change)

---

## Git Commit Requirements

All commits MUST follow these rules:

1. **Signed-off-by**: `digger yu <digger-yu@outlook.com>`
2. **Signed commit**: Use `git commit -s -S`
3. **Do NOT push**: Work locally until all phases complete
4. **Commit message format**:
   ```
   subject (≤50 chars, imperative mood)
   
   detailed body in English (≤72 chars per line)
   - Explain WHAT changed and WHY
   - Root cause analysis
   - Fix rationale
   
   Signed-off-by: digger yu <digger-yu@outlook.com>
   ```

### Example Commits:
```
refactor(codegen): split x86_64.asm into modular components

Extract inline assembly code from x86_64.rs into separate module:
- asm/mod.rs: module root with re-exports
- asm/template.rs: directive/macro expansion logic  
- asm/operand.rs: operand mapping and binding
- asm/directive.rs: GCC-style directive recognition
- asm/label.rs: local label resolution
- asm/emit.rs: main emission functions (emit_inline_asm/goto)

Root cause: x86_64.rs became too large (~42k lines) and unmaintainable.
Inline asm code (~5k lines) should be isolated for clarity and reuse.

This is a pure extraction - NO semantic changes. Verified by:
- cargo build --tests: clean
- cargo test --test integration: all pass
- Byte-level diff on existing binaries: unchanged

Signed-off-by: digger yu <digger-yu@outlook.com>
```

---

## Next Steps (Immediate)

1. **Complete Part A2** (asm/* extraction):
   - Extract all asm-related functions from x86_64.rs
   - Create new module files
   - Test compilation
   - Submit as PR-A2

2. **Continue Part A3-A7** sequentially:
   - Each as separate PR
   - No merging feature PRs with structural PRs simultaneously
   - Observe freeze windows

3. **Begin Part B1** after structural foundation:
   - Unified diagnostics first (behavioral change needs care)
   - Then bundle options, then package consolidation

4. **Run bootstrap baseline**:
   - Capture golden hashes now for regression detection

5. **Update PR template**:
   - Add backend refactoring checklist
   - Include matrix verification requirement

---

## Metrics & Milestones

### Completion Criteria:

| Metric | Target | Current |
|--------|--------|---------|
| Max file size in x86_64/ | <5,000 lines | 41,659 lines (single file) |
| Backend trait coverage | 7 architectures | 0% (no trait) |
| Diagnostic consistency | 100% aligned | Mixed (x86 vs others) |
| Matrix/Docs sync | CI-enforced | Manual (error-prone) |
| RiscV32 has_backend | false (current state) | true (incorrect) |

### Estimated Timeline:
- **Part A** (Structural): 3-6 person-days (7-8 PRs)
- **Part B** (Contract): 2-4 person-days (5 PRs)
- **Part C-E** (Foundation): 2-3 person-days
- **Part F-G** (Testing): 1-2 person-days
- **Part H** (Docs): 0.5-1 person-day
- **Total**: ~10-16 person-days (sequential, not parallel)

---

## Conclusion

The backend refactor plan is extremely comprehensive and addresses fundamental architectural issues in the DCC compiler's codegen layer. Key priorities:

1. **Structural hygiene first** (Part A): Break up the giant `x86_64.rs`
2. **Contract unification second** (Part B-C): Enable cross-arch parity
3. **Testing & metrics third** (Part F-G): Prevent future drift
4. **Documentation last** (Part H): Reflect actual implementation

**Critical Success Factor**: Execute in small, verifiable PRs with strict门禁 (gate checks). Do NOT attempt large monolithic refactors.

Next report will detail completion of Part A2 (asm/* extraction).

---

*Report generated: September 20, 2026*  
*Plan version: v1.2 (with防腐 mechanisms)*  
*Executor: AI Assistant (Qoder)*
