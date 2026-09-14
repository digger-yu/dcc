# Lua 5.5.1 Compilation with DCC

## Status: Compilation Success, Runtime Crashes (Multiple DCC Bugs)

### Summary
All 33 Lua 5.5.1 source files compile successfully with dcc.
The generated .o files link without errors.
However, the resulting binary crashes with SIGSEGV at runtime due to
multiple dcc code generation bugs discovered during analysis.

A gcc-compiled build with the same source runs correctly, confirming
the Lua source code is not at fault.

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

### DCC Bugs Discovered (Not Yet Fixed)

#### Bug 2: Stack Slot Collision
**Symptom**: In `luaS_new`, the temporary for `L->l_G` and the
local variable `i` share the same stack slot (-0x58(%r13)).
The computation of `i` overwrites the saved `g` pointer.
Later code loads the corrupted `g` and computes wrong addresses.

**Evidence**: GDB watchpoint shows -0x58(%r13) changes from
valid `g` (0x55555562cb38) to garbage (0x555500000010) after
`i = point2uint(str) % STRCACHE_N` is stored.

**Impact**: Affects `luaS_new` and likely many other functions
that use `G(L)` macro alongside local variables.

#### Bug 3: Address-of Struct Member (`&(ts)->contents`)
**Symptom**: `rawgetshrstr(ts) = (char*)(&(ts)->contents)` should
return `ts + offsetof(TString, contents)` = `ts + 24`. Instead dcc
generates code that dereferences `contents` as a pointer and adds
offset 11 (shrlen offset), producing garbage.

**Evidence**: Assembly shows `add $0xb,%rax; movsbq (%rax),%rax`
instead of `add $0x18,%rax` (24 = offsetof(TString, contents)).

**Impact**: All short string operations crash because `getstr()`
returns garbage pointers.

#### Bug 4: Systematic Code Generation Failures
**Evidence**: When compiling Lua with dcc, only `lstate.c` produces
a working .o file. All other 31 files produce .o files that crash
at runtime. The same source compiled with gcc -O0 works perfectly.

This suggests a fundamental issue in dcc's code generation that
affects most non-trivial C code.

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
