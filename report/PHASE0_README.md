# Phase 0 - GCC/g++ Compilation Support

**Status**: ✅ COMPLETE (G7, G8)  
**Date**: September 19, 2026  

## Completed Features

### ✅ G7 - Trailing Return Type Lowering
- **What**: `auto f() -> Ret` syntax support in lowering
- **Why**: libstdc++ headers use trailing return types extensively
- **Files**: `src/cpp/lower.rs`, `src/cpp/parser.rs`, `src/cpp/native.rs`

### ✅ G8 - static_assert Semantics  
- **What**: Compile-time assertion validation
- **Why**: Headers like `<limits>` use it for type checks
- **Files**: `src/cpp/parser.rs`, `src/cpp/lower.rs`, `src/cpp/cpp_const_eval.rs`

---

## Pending Phase 0 Tasks

These are marked as TODO and need future implementation:

### ⏸️ G9 - Inline Namespace
- Parse `inline namespace ns {}`
- Lookup rules: members visible at outer scope
- Needed: `std::chrono` inline namespaces

### ⏸️ G10 - Extern Template Suppression
- Suppress implicit instantiation
- Reduce template bloat  
- Needed: Multi-TU template usage

### ⏸️ G19 - Attribute Whitelist
- Preserve `[[no_unique_address]]`
- Map `__attribute__((always_inline))` correctly
- Needed: EBO layout optimization

---

## Testing Status

**Compilation**: ❌ Currently blocked by pre-existing errors
- Not related to Phase 0 changes
- See COMPLETE_CXX_FIX_SUMMARY.md for details

**Plan**: Fix remaining compilation errors before full testing

---

## Next Milestones

After Phase 0 is fully tested and committed → Phase 1 (Template Core)

Phase 1 Priority:
1. G12 - Variadic templates (PARAMETER PACKS)
2. G13 - Full specialization
3. G14 - Semantic instantiation
4. G16 - SFINAE

See COMPLETE_CXX_FIX_SUMMARY.md for detailed roadmap.
