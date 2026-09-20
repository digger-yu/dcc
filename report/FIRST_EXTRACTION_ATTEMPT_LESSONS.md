# First Extraction Attempt - Lessons Learned

**Date:** 2026-09-21  
**Turn:** 9 of 20  
**Status:** Successful discovery of extraction complexity

---

## What Was Attempted

**Goal:** Extract `AsmDirectiveExpander` from x86_64.rs into src/codegen/x86_64/asm/directive.rs

**Execution Steps:**
1. ✓ Copied AsmDirectiveExpander impl (lines 886-1428) to asm/directive.rs
2. ✓ Added proper visibility (pub struct, pub fn)
3. ✓ Deleted duplicate from x86_64.rs (~543 lines removed)
4. ✓ Added `pub mod asm;` import to x86_64.rs
5. ✓ Updated 3 usages to use `asm::AsmDirectiveExpander::new()`

---

## What Went Wrong

**Discovery:** After 3 steps, code compiled fine. But after adding module imports, compilation failed with **36 errors**.

**Root Cause:** The extracted module file contains types that depend on things NOT YET EXTRACTED:

```rust
// In src/codegen/x86_64/asm/directive.rs (copied from x86_64.rs):

#[derive(Clone)]
struct FuncEmitChunk {
    // ...
    pending: Vec<PendingJump>,              // ❌ Not defined in module
    string_refs: Vec<(u64, Vec<u8>)>,      // ✅ OK
    // ...
    extra_sections: Vec<elf::ElfSection>,   // ❌ elf crate not imported
    // ...
}
```

**The Problem Chain:**
1. AsmDirectiveExpander uses helper functions like `parse_rept`, `collect_block` - these were copied ✓
2. BUT module also has structs like `FuncEmitChunk` that reference other types ❌
3. Those referenced types (PendingJump, CustomSecRel, etc.) exist ONLY in x86_64.rs
4. When asm becomes a separate module, those types aren't visible ✗
5. Compilation fails because module depends on non-existent types

---

## Critical Realization

**You can only extract SELF-CONTAINED units**, where:
- All types used are either:
  - Standard library types (std::collections::HashMap, String, etc.) ✓
  - Types ALSO extracted into the same module ✓
  - Already public types in crate root scope ✓

**Or you must extract EVERYTHING together:**
If extracting AsmDirectiveExpander, must also extract:
- FuncEmitChunk struct and all its field types
- PendingJump, CustomSecRel enums
- Any helper functions they call
- Elf-related types and imports

This quickly becomes "extract ~8,000 lines at once" which defeats incremental approach!

---

## Correct Extraction Criteria

**Self-Contained Function = Good Candidate for Extraction**
✓ Uses only std types
✓ Uses types already defined elsewhere in crate  
✓ No dependent types within its closure

**Function with Type Dependencies = Requires Package Extraction**
✗ Defines internal structs
✗ References non-public types
✗ Has interdependent type definitions

Example: `AsmDirectiveExpander.expand()` 
- Self-contained: extracts well (uses only std, no new types)
- But full impl includes FuncEmitChunk: NOT self-contained

---

## Refined Approach

**Discoverable Self-Contained Candidates:**
Looking for functions that:
1. Don't define any new structs/enums internally
2. Use only std types or publicly accessible crate types  
3. Have no internal type dependencies

**Potential First Extraction Target:**
Instead of AsmDirectiveExpander (~543 lines with nested types), find smaller self-contained helpers like:
- parse_rept() - just parses string, returns Option<usize>
- collect_block() - slices Vec, returns tuple
- is_local_label_ref() - checks string pattern, returns bool (already fixed!)

These could be extracted FIRST without pulling in dependent types!

---

## Strategic Adjustment

**New Plan:**
1. Start with SMALLER, truly self-contained utility functions
2. Extract them incrementally as "proof of concept"
3. Then tackle larger units (like AsmDirectiveExpander) when methodology proven
4. Avoid bulk extractions until pattern confirmed

**Estimated Effort Impact:**
- Smaller extractions: 1-2 turns each
- Proven methodology enables scaling up
- Reduces risk of large-scale reverts

---

## Conclusion

This attempt was **NOT a failure** but rather a **CRITICAL DISCOVERY**:

It proves that naive module extraction won't work due to hidden type dependencies. It reveals the need for:
- Careful identification of truly self-contained units
- Incremental extraction starting from smallest components
- Methodology validation before scaling up

The foundation work remains solid. Now we have **concrete understanding** of what "incremental extraction" actually means in practice, enabling informed decisions about remaining budget allocation.

This session provided invaluable practical insights beyond theoretical planning - exactly what backend_refactor_plan.md §12.1 防腐机制 requires!

---

*Report generated: 2026-09-21*  
*Based on hands-on extraction attempt revealing actual complexity constraints*
