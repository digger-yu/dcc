# GCC 15.3.0 Build Progress with dcc

## Status
- libcpp: ✅ Fully compiled
- gcc/build (generator programs): 3/4 compiled
  - ✅ genhooks.o
  - ✅ gengenrtl.o  
  - ✅ genchecksum.o (compiles, link errors remain)
  - ❌ sort.o (parser bug: function body leak)

## Key Fixes Applied

### Parser Fixes
- const prefix handling in qualified types
- Function pointer member detection
- Elaborated type class Name disambiguation
- Class member parse error recovery

### AST Lower Fixes
- Typedef alias tracking (typedef_aliases map)
- Nested class emission before enclosing struct
- Aggregate initialization (InitList) direct brace emission
- Dynamic global initializer conversion with brace depth tracking
- C function demangling (_ZN6strlenEv -> strlen)
- Attribute stripping (__attribute__)

### Type System Fixes
- GCC builtin types injection with duplicate detection
- Forward declarations: cpp_reader, htab_t, rlim_t, etc.
- Conditional cookie_io_functions_t injection
- Struct typedef duplicate detection (} type; pattern)

## Remaining Issues
1. sort.cc: Parser fails to parse gcc_qsort/gcc_sort_r function bodies
   - Function parameters all named `_`
   - Body statements leaked to global scope
   - Root cause: Under investigation - likely related to function typedef handling
2. genchecksum: Link errors for missing runtime functions
   - dccpp_make_shared, dccpp_shared_ptr_inc/dec
   - dccpp_swap_bytes, strlen, strncmp, memcmp (C library name mangling)
