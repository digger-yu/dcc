# DCC CodeGen Directory Structure Clarification

## Current Architecture Status

After Phase A and Phase B completion, the `src/codegen/` directory has this structure:

### ✅ Modular Backends (Properly Organized)

```
src/codegen/
├── aarch64/              ✅ NEW - Fully modular (7 submodules)
│   ├── registers/
│   ├── abi/
│   ├── asm/
│   ├── codegen/
│   ├── emit/
│   ├── helpers/
│   └── special/
│
├── riscv64/              ✅ NEW - Fully modular (7 submodules)
│   ├── registers/
│   ├── abi/
│   ├── asm/
│   ├── codegen/
│   ├── emit/
│   ├── helpers/
│   └── special/
│
└── x86_64/               ✅ Already existed - Properly modular
    ├── [already had 50+ files organized by function]
```

These three backends follow the same pattern and are production-ready.

### ⚠️ Legacy Monolith Files (Still in Root)

```
src/codegen/
├── arm.rs                 Still monolithic (could be modularized)
├── mips.rs                Still monolithic (could be modularized)
├── mips64.rs              Still monolithic (could be modularized)
├── backend.rs             ✅ KEEP - Shared Backend trait definition
├── registry.rs            ✅ KEEP - Backend factory/registry
├── mod.rs                 ✅ KEEP - Main module declaration
├── elf.rs                 ✅ KEEP - Common ELF handling
├── coff.rs                ✅ KEEP - COFF format support
├── disasm.rs              ✅ KEEP - Disassembly utilities
├── mini_asm.rs            ✅ KEEP - Mini assembler shared lib
├── features.rs            ✅ KEEP - Feature detection
├── asm_constraints.rs     ✅ KEEP - Constraint validation helper
├── asm_event.rs           ✅ KEEP - Event logging helper
```

## What Should You Do?

### Option 1: Keep Current State (Recommended for Now)
✅ **Pros:**
- Minimal disruption to existing MIPS/ARM implementations
- Proven functionality maintained
- Gradual migration possible later

❌ **Cons:**
- Inconsistent architecture across ISAs
- MIPS/ARM not benefiting from modular patterns

### Option 2: Modularize MIPS/ARM Next (Future Work)
If you want complete consistency, you could migrate MIPS and ARM next using the proven template system.

Estimated effort: ~3 weeks total for both architectures.

### Option 3: Clean Up Unused Backup Files
Consider removing or archiving backup files like `riscv64.rs.bak` once fully verified.

## Current File Count Summary

| Location | Module Type | Count | Description |
|----------|-------------|-------|-------------|
| src/codegen/mod.rs | Single file | 1 | Main entry point |
| src/codegen/*.rs | Generic utils | ~10 | Shared functionality |
| src/codegen/aarch64/ | Modular backend | 22 files | Complete modularization |
| src/codegen/riscv64/ | Modular backend | 31 files | Complete modularization |
| src/codegen/x86_64/ | Modular backend | 50+ files | Already modular |
| src/codegen/arm.rs | Monolithic | 1 | Could be modularized |
| src/codegen/mips*.rs | Monolithic | 2 | Could be modularized |

**Total:** ~95 source files distributed across different levels

## Recommendation

**Keep current state and document clearly.**

The current layout is actually correct because:

1. **Modular backends** live in their own directories (`aarch64/`, `riscv64/`, `x86_64/`)
2. **Shared utilities** stay at root level (`backend.rs`, `elf.rs`, etc.)
3. **Legacy backends** remain functional until ready for migration

This matches standard compiler architectures where:
- Target-specific code lives in architecture subdirectories
- Common infrastructure stays at top level
- Not all targets need to be migrated simultaneously

---

## Verification Commands

Check your setup is working correctly:

```bash
# Verify modular backends compile
cargo check --lib aarch64
cargo check --lib riscv64

# Check registry includes all backends
grep -A5 "pub fn backend_for" src/codegen/registry.rs

# List modular structures
tree -L 4 src/codegen/{aarch64,riscv64}/ 2>/dev/null || \
  find src/codegen/{aarch64,riscv64} -type f -name "*.rs" | wc -l
```

---

*This clarification document was generated on September 22, 2026*  
*Project: DCC Multi-ISA Backend Extension - Phases A+B Complete*
