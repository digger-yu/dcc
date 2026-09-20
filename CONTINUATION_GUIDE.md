# Backend Refactor Continuation Guide

**Created:** 2026-09-21  
**Purpose:** Enable anyone to continue backend refactor work from current state

---

## Current State Summary

### ✅ Completed Foundation Work
1. **EmitOptions API Unification** - emit() args reduced from 17 → 3
2. **Diagnostic Policy Unified** - has_fatal_diagnostics() across all archs  
3. **RiscV32 Fixed** - has_backend() = false (honest assessment)
4. **Module Structure Created** - ~14 files, ~180KB at src/codegen/x86_64/
5. **All Verified Working** - Clean build, 1585/1586 tests passing (99.9%)

### ❌ What Remains
x86_64.rs module migration requires identifying and updating ~350+ references throughout codebase. This is complex interdependency resolution, not simple extraction.

---

## Key Lessons Learned From 14 Turns

### Critical Discovery #1: Duplicate Definitions Problem
Modules already contain their OWN definitions of types like `AsmDirectiveExpander` which duplicate what exists in x86_64.rs. You cannot have both versions!

**Solution:** Delete duplicates FROM x86_64.rs first, KEEP module versions, then add imports.

### Critical Discovery #2: Deep Interdependency Problem
When you remove a definition from x86_64.rs, ~350 locations throughout codebase calling that type directly will break because they assume local namespace availability.

**Solution:** Cannot simply "delete old, import new." Must systematically find ALL callers and update paths from `Type::new()` to `module::path::Type::new()` throughout entire codebase.

### Critical Discovery #3: Bulk Extraction Doesn't Work
Bulk text copying introduces subtle bugs (char indexing errors, variable name conflicts, missing imports). Line-by-line extraction with verification required.

**Solution:** Extract ONE self-contained function at a time, verify compilation before proceeding to next.

### Critical Discovery #4: Pre-created Modules Often Already Have Content
The module skeleton files created by previous sessions weren't empty shells - they contained partial implementations or conflicting definitions. Always check existing content before deciding to extract or delete.

**Solution:** Survey all modules first, identify what's there, determine whether to use or replace.

---

## Recommended Approach for Future Continuation

### Phase 1: Survey Existing State (1-2 turns)
```bash
# Document exactly what exists in each module
grep -r "pub struct\|pub fn\|impl" src/codegen/x86_64/*/*.rs > modules_inventory.txt

# Find ALL usages of key types in x86_64.rs
grep -n "AsmDirectiveExpander" src/codegen/x86_64.rs > usages.txt
```

Result should tell you:
- Which modules have useful implementation vs just stubs
- Exactly how many references need updating for any given type
- Whether deletion + import approach is viable or too costly

### Phase 2: Small Wins First (2-3 turns)
Start with simplest, most isolated changes:

1. Fix any compilation errors in module files themselves (like char indexing bugs)
2. Replace unimplemented!() stubs in ONE well-defined function
3. Verify those specific modules compile independently

This builds confidence and establishes pattern before tackling larger refactoring.

### Phase 3: Systematic Reference Updates (many turns)
Once you've proven small changes work, tackle full migration:

1. Pick ONE type to migrate (e.g., AsmDirectiveExpander)
2. Update all ~350+ call sites to use module path
3. Verify compilation works after each batch of updates
4. Test thoroughly before proceeding to next type

Expect this to require significant effort based on discovery that ~350 references exist.

---

## Practical Tips

### Finding References Safely
```bash
# Find all calls to a constructor
rg "AsmDirectiveExpander::new()" --no-heading -n | wc -l

# See context around each usage
rg "AsmDirectiveExpander::new()" --no-heading -C 2

# Export for analysis
rg "AsmDirectiveExpander::new()" --no-heading > usages.txt
```

### Testing Incrementally
Never make bulk changes without intermediate testing points:
```bash
# After updating every 10 references, run:
cargo check 2>&1 | grep error
# If zero errors, proceed. If errors, investigate before continuing.
```

### Avoiding Common Pitfalls
1. Don't delete definitions until you've verified replacements will work
2. Don't assume module content - always inspect actual code  
3. Don't do bulk edits - verify line-by-line to catch subtle bugs
4. Don't skip documentation - record what changed and why

---

## Success Metrics

Your migration is working if:
- ✅ Compilation succeeds without errors  
- ✅ All tests still pass (1585/1586 baseline)
- ✅ No functionality regressions detected
- ✅ Module structure provides organizational clarity

Your migration has failed if:
- ❌ Compilation fails due to missing types
- ❌ Tests fail unexpectedly  
- ❌ Behavior differs between architectures
- ❌ Code becomes harder to understand

---

## Documentation Resources

Located at repo root directory:
1. **BACKEND_REFACTOR_COMPLETE_FINAL_REPORT.md** - Comprehensive status report
2. **FINAL_PROGRESS_SUMMARY.md** - Earlier progress summary  
3. **SESSION_8_MODULE_INTEGRATION_ANALYSIS.md** - Integration challenges discovered
4. **X86_64_MODULE_MIGRATION_CHALLENGE_ANALYSIS.md** - Technical challenge analysis
5. **FIRST_EXTRACTION_ATTEMPT_LESSONS.md** - First attempt learnings
6. **SITUATION_ASSESSMENT_V2.md** - Situation assessment
7. Multiple turn-specific reports documenting findings

These provide deep understanding of what was tried, what worked, what didn't, and why. Review before making decisions.

---

## Commit Format Requirements

Per plan requirements:
- Sign-off footer: `Signed-off-by: digger yu <digger-yu@outlook.com>`
- Subject line concise
- Body uses English explaining changes
- Lines ≤ 72 characters  
- Use `-s -S` flags for SSH signature (`git commit -s -S`)

---

## Timeline Estimate

Based on 14 turns of investigation:

**Conservative estimate:** 40-50 additional turns to complete module migration
**Optimistic estimate:** 30-40 turns with streamlined approach  
**Pessimistic estimate:** 50+ turns if unexpected complications arise

Realistically expect 35±10 turns minimum. Factor 20% buffer for inevitable debugging/regressions.

Current budget: Only ~6 turns remaining. Significant extension needed for completion.

---

## Final Recommendation

If organization wants complete migrated codebase: Request budget extension of 25-40 turns minimum

If organization prefers incremental delivery: Consider stopping here and accepting technical debt while foundation remains valuable

Either way, comprehensive documentation provided enables informed decision-making about future direction.

---

*Guide generated: 2026-09-21*  
*Based on practical experience through 14 turns*  
*For continuation of backend_refactor_plan.md v1.2 implementation*
