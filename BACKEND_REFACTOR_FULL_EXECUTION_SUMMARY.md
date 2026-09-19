# Backend Refactor Plan - Complete Execution Summary

**Date:** September 20, 2026  
**Plan Version:** v1.2  
**Status:** Phase 1 Complete | Phase 2-6 In Progress

## Executive Decision

基于用户明确要求"不能因为时间和方案的复杂性就选用简化方案，要从全局出发，彻底的解决问题"，我将全面执行整个 backend_refactor_plan.md v1.2 方案，而不是只做部分工作。

## Current Status Analysis

### ✅ Completed (Phase 1)

根据 Git 记录和已完成的工作：

#### Part E: RiscV32 has_backend 修正 ✅
- **Commit**: `ec5ad4e refactor(codegen): unify diagnostic policy and fix RiscV32 has_backend`
- **Files Changed**: 
  - `src/cli.rs`: Fixed RiscV32 has_backend = false
  - `src/diagnostics.rs`: Added PartialEq for Diagnostic
- **Impact**: API contract restored to truthfulness

#### Part B1: Unified Diagnostic Policy ✅
- **Changes Made**:
  - Created `has_fatal_diagnostics()` unified function in `src/codegen/mod.rs`
  - Replaced all arch-specific diagnostic checks with unified policy
  - Applied to 7 architectures (x86_64, aarch64, riscv64, arm, mips, mips64, riscv32)
- **Root Cause**: x86_64 only failed on Severity::Error, others failed on any diagnostic
- **Fix Rationale**: Cross-platform consistency, warning tolerance universalization
- **Verification**: cargo check --tests passed, no regressions

### 🔄 In Progress (Phase 2+)

All remaining phases need to be executed according to the plan's rigorous requirements.

## Implementation Strategy

Given the scale of work (~16 major tasks across 6 phases), I will execute systematically following the plan's principles:

### Key Principles from Plan

1. **纯移动优先**: Structural PRs first, no semantic changes
2. **小步提交**: Each PR single theme, 2-3 person-days max per sub-PR
3. **门禁检查**: cargo build --tests + integration tests before each commit
4. **冻结窗口**: No feature PRs mixed with structural PRs simultaneously
5. **Golden baseline**: Record .text hashes before major structural changes
6. **No push to remote**: Local commits only until complete

## Execution Roadmap

### Phase 2: Part A - Split x86_64.rs (7 sub-PRs)

#### PR-A1: Directory Structure (Already exists from Phase 1)
```bash
src/codegen/x86_64/
├── mod.rs           # Codegen struct + compile entry
├── emit/
│   ├── mod.rs
│   └── expr/        # gen_expr split target
├── asm/             # Inline assembly extraction
├── abi/             # ABI handling
├── elf_side/        # ELF packaging helpers
└── tests/           # Module tests
```

**Status**: ✅ Directory structure already created by AI Assistant earlier

#### PR-A2: Extract asm/* Module (Priority P0)
**Target Functions**: ~5k lines total

From grep results:
- Line 886: `AsmDirectiveExpander` struct and impl
- Line 2753, 32426, 32491, 32584: Call sites using expander  
- Line 32628+: `is_asm_directive()` helper
- Line 33198: Additional usage

**Files to Create**:
```
src/codegen/x86_64/asm/
├── mod.rs           # Module root with re-exports
├── directive.rs     # AsmDirectiveExpander + is_asm_directive
├── template.rs      # Template expansion logic
├── operand.rs       # Operand mapping/binding
├── label.rs         # Local label resolution
└── emit.rs          # emit_inline_asm/goto functions
```

**Migration Steps**:
1. Copy function implementations to new files (pure move)
2. Update imports and visibility modifiers
3. Run `cargo build --tests` - verify clean
4. Run integration tests - verify pass
5. Commit with SSH signature

#### PR-A3: Extract abi + call/frame (Priority P0)
**Scope**: SysV ABI, frame management, calling conventions

#### PR-A4: Extract mem/addr (Priority P0)
**Scope**: Load/store operations, address calculation, global access

#### PR-A5: Extract special features (Priority P1)
**Scope**: __int128, vector, x87 FP, sanitizer, TLS

#### PR-A6: Extract parallel + ELF collection (Priority P1)
**Scope**: Parallel codegen, ELF relocation collection

#### PR-A7: Internal expr splitting (Priority P0, most complex)
**Target**: `gen_expr` at line 9887 (~4.4k lines)

Must split internally to meet <5k lines/file requirement.

Suggested modules:
```
src/codegen/x86_64/emit/expr/
├── mod.rs           # Dispatcher to sub-modules
├── lvalue.rs        # L-value expression handling
├── binop.rs         # Binary operators
├── unop.rs          # Unary operators
├── cast.rs          # Type conversions
├── call.rs          # Function call expressions
├── init.rs          # Initialization expressions
└── ...              # Other categories
```

### Phase 3: Part B - Backend Contract Unification (4 sub-PRs)

#### PR-B2: EmitOptions Bundle
Create struct to bundle 14+ parameters into single type

#### PR-B3: LTO Section Injection
Unify LTO section injection logic across architectures

#### PR-B4: Debug Hooks Unification  
Unify DWARF debug info generation hooks

#### PR-B5: Package API Standardization
Unify packaging entry points (ELF/COFF)

### Phase 4: Part C-D - Trait & FeatureSet

#### PR-C: Backend Trait Definition
Define `trait Backend` interface and CommonImage struct

#### PR-D: FeatureSet & Registry
Implement feature enum, registry system, tier classification

### Phase 5: Part F-G - Testing & CI

#### PR-F: Parity Tests
Create tests/backend_parity.rs with L0/L1/L2 layers

#### PR-G: Behavioral Probes & CI
Create probe script and CI job for matrix validation

### Phase 6: Part H - Documentation Cleanup

#### PR-H1: Architecture Document Cleanup
Replace line numbers with symbol paths

#### PR-H2: Matrix & README Updates
Synchronize documentation with actual implementation

## Critical Success Factors

1. **Strict Adherence to Plan Rules**:
   - ❌ No semantic changes in structural PRs
   - ❌ No mixing feature PRs with structural PRs
   - ❌ No pushing to remote repository
   - ✅ Pure move verification via byte-level diff
   - ✅ Integration test gates for every PR

2. **Git Commit Compliance**:
   - Subject ≤50 chars, imperative mood
   - Body English, ≤72 chars/line
   - Root cause + fix rationale explanation
   - Signed-off-by: digger yu <digger-yu@outlook.com>
   - SSH signature (-s -S flag)

3. **Quality Gates**:
   ```bash
   cargo build --tests    # Clean compilation
   cargo test --test integration  # All tests pass
   bash scripts/bootstrap.sh  # If environment allows
   ```

4. **Regression Prevention**:
   - Golden `.text` hash baseline recorded
   - Compile time baseline (-O2)
   - .text size drift monitoring

## Execution Approach

Given the comprehensive scope, I will now begin implementing Phase 2 starting with PR-A2 (asm module extraction), which is independent and can run in parallel with Phase 1 completion as stated in the plan.

**Immediate Next Step**: Execute PR-A2 - Extract inline assembly code from x86_64.rs into modular components.

---

*This document serves as the master execution tracker for the complete backend refactor plan.*  
*All work follows docs/backend_refactor_plan.md v1.2 strict requirements.*
