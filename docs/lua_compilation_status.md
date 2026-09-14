# Lua 5.5.1 Compilation with DCC

## Status: Compilation Success, Runtime Crashes (Multiple DCC Bugs)

### Summary
All 33 Lua 5.5.1 source files compile successfully with dcc.
The generated .o files link without errors.
The runtime crash moved from luaS_new to insertkey after fixes.
Binary search identified 5 files with remaining dcc codegen bugs:
lapi.c, ltable.c, lgc.c, llex.c, lcode.c.
Replacing these 5 with gcc-compiled objects makes Lua work,
confirming the Lua source is correct and bugs are in dcc.

A gcc-compiled build with the same source runs correctly.

### DCC Bug Fixes Applied

#### Fix 1: Forward Reference Incomplete Struct Type Resolution
**File**: `src/codegen/x86_64.rs`, `src/ast.rs`

**Root Cause**: When `lua_State` is defined before `global_State`,
it stores `struct global_State *l_G` with an incomplete struct type
(empty fields list). Later when `global_State` is fully defined via
`typedef struct global_State { ... } global_State;`, the type stored
in `lua_State`'s field is NOT updated.

So `L->l_G->strt` uses the incomplete type which has no fields,
causing `find_field` to return None and the field offset (48) is lost.
The generated code misses `add $48` for `&G(L)->strt`.

**Fix**: Added `complete_struct_types: HashMap<String, Type>` to
the Codegen struct. Populated during `compile()` by scanning
`Decl::StructDef`, `Decl::UnionDef`, and `Decl::Typedef`. Added
`resolve_complete_struct_ty()` helper that looks up complete struct
definitions when the type has empty fields. Applied at all 4 call
sites of `member_target_ty`.

#### Fix 2: Struct Member Multi-Dim Array Dimension Order
**File**: `src/parser.rs`

**Root Cause**: In `parse_fields()`, struct member array dimensions
were wrapped without reversal. For `int g[2][3]` as a struct
member, dcc produced `Array(Array(int, 2), 3)` (3 elements of
int[2]) instead of the correct `Array(Array(int, 3), 2)` (2
elements of int[3]). Variable declarations already reversed
correctly in `parse_decl_specifiers`.

This caused `TString *strcache[53][2]` to have stride 424
(53*8) instead of 16 (2*8) for `strcache[i]`.

**Fix**: Collect all dimensions into a Vec, reverse, then wrap
(matching the variable declaration path).

#### Fix 3: element_stride Semantic Layer
**File**: `src/semantics.rs`

**Root Cause**: `element_stride()` returned `inner.size()` for
`Type::Array(inner, _)` instead of `elem_ty.size()`. For
`Array(Ptr(TString), Const(2))`, it returned 8 (pointer size)
instead of 16 (2 * 8).

**Fix**: Return `elem_ty.size()` directly.

### DCC Bugs Discovered (Not Yet Fixed)

#### Bug 4: Code Generation Failures in 5 Core Files
**Evidence**: Binary search replacing dcc .o files with gcc
versions identified 5 files with codegen bugs:
- lapi.c, ltable.c, lgc.c, llex.c, lcode.c

Replacing all 5 with gcc-compiled objects makes Lua run
correctly. The remaining 28 dcc-compiled .o files work fine.

Symptoms include:
- insertkey: mainpositionTV returns wrong node address
- luaH_setint: wrong argument passing or computation
- Various crashes in string/table operations

### Verification
```bash
# gcc build works correctly:
gcc -c -O0 -I/root/lua-5.5.1/src -o /tmp/lua_obj/file.o file.c
# Result: Lua 5.5.1 runs, print("Hello") works

# dcc build crashes:
dcc -c -I/root/lua-5.5.1/src -o /tmp/lua_obj/file.o file.c
# Result: SIGSEGV in luaS_new or other functions
```

### Build Commands Used
```bash
DCC=/root/dcc/target/release/dcc
LUA_SRC=/root/lua-5.5.1/src
for f in $LUA_SRC/*.c; do
    $DCC -c -I$LUA_SRC -o /tmp/lua_obj/$(basename ${f%.c}.o) $f
done
gcc -o /tmp/lua_test /tmp/lua_obj/*.o -lm -ldl
```

---
**Last Updated**: 2026-09-14
**Build Environment**: Linux 24.04, DCC Rust compiler
