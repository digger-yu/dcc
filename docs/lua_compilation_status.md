# Lua 5.5.1 Compilation with DCC

## Status: Lua 5.5.1 Builds and Runs Successfully

### Summary
All 34 Lua 5.5.1 source files compile successfully with dcc.
The generated object files link successfully with gcc into both `lua`
and `luac` executables. Source execution, bytecode generation, and
bytecode execution all pass the current smoke tests.

The previously documented Bug 4 is fixed. The remaining observed
warnings are for `__builtin_huge_val` being assumed as `int` in
`lmathlib.c` and `lstrlib.c`; these warnings do not block the current
Lua build or smoke-test execution.

### Latest Discovery (2026-09-15)
The remaining crash in `luac` was caused by DCC's x86-64 SysV indirect
call lowering.

In `ldump.c`, `dumpBlock` performs:

```c
D->status = (*D->writer)(D->L, b, size, D->data);
```

The assignment code saved the `D->status` lvalue address on the stack
before emitting the indirect call. `emit_sysv_call()` stored the indirect
callee function pointer in a scratch slot, but `frame_bytes` did not
reserve that extra slot. As a result, the scratch write overlapped the
caller temporary holding the saved lvalue address. After the call
returned, the store used the function return value as the destination
address and crashed.

GDB evidence before the fix:
- Crash in `dumpBlock` immediately after `call *%r11`
- Faulting instruction: `mov %eax,(%rcx)`
- `rcx = 3` instead of `&D->status`

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

#### Fix 4: Signed 32-bit Struct Member Loads
**File**: `src/codegen/x86_64.rs`

**Root Cause**: Loading a signed 32-bit integer struct member emitted
`mov r32, [mem]`, which zero-extends into the 64-bit register. Lua's
lexer stores `EOZ` as `-1` in the signed `int` field `ls->current`; the
zero-extended value became `0x00000000ffffffff` instead of
`0xffffffffffffffff`, causing token comparisons to fail.

**Fix**: For signed 4-byte member loads, emit `movsxd r64, r/m32`.
Unsigned 4-byte member loads still use `mov r32, r/m32`.

#### Fix 5: Indirect Call Callee Scratch Slot Allocation
**File**: `src/codegen/x86_64.rs`

**Root Cause**: `emit_sysv_call()` stores the callee of indirect calls
in a scratch stack slot after parameter staging slots. The frame-size
calculation only reserved register argument staging and hidden sret
slots, but not this indirect-callee scratch slot. In expressions such as
`lhs = (*fp)(...)`, that unreserved slot could overwrite a caller
temporary saved immediately above the call frame.

**Fix**: Include one extra frame slot when `direct.is_none()` so the
indirect-callee scratch storage is inside the reserved call frame and
cannot alias caller temporaries.

### Bug 4 Resolution

#### Bug 4: Code Generation Failures in Core Lua Files
**Status**: Fixed by the signed-member-load and indirect-call scratch
slot fixes.

Earlier binary-search evidence identified failures involving:
- `lapi.c`, `ltable.c`, `lgc.c`, `llex.c`, `lcode.c`

The final blocking runtime failure reproduced in `luac` through
`ldump.c::dumpBlock`, where an indirect writer callback was used inside
an assignment. Fixing the indirect call scratch slot eliminated that
crash. Combined with the signed-member-load fix for `llex.c`, the Lua
source and bytecode smoke tests now pass with all Lua objects compiled
by dcc.

### Verification

```bash
# Build dcc
cargo build --release

# Compile all Lua source files with dcc
DCC=/root/dcc/target/release/dcc
LUA_SRC=/root/lua-5.5.1/src
mkdir -p /tmp/lua_dcc_bug4
for f in $LUA_SRC/*.c; do
    base=$(basename "$f" .c)
    $DCC -c -I$LUA_SRC -o /tmp/lua_dcc_bug4/${base}.o "$f"
done

# Link lua and luac separately
libobjs="/tmp/lua_dcc_bug4/lapi.o /tmp/lua_dcc_bug4/lauxlib.o \
/tmp/lua_dcc_bug4/lbaselib.o /tmp/lua_dcc_bug4/lcode.o \
/tmp/lua_dcc_bug4/lcorolib.o /tmp/lua_dcc_bug4/lctype.o \
/tmp/lua_dcc_bug4/ldblib.o /tmp/lua_dcc_bug4/ldebug.o \
/tmp/lua_dcc_bug4/ldo.o /tmp/lua_dcc_bug4/ldump.o \
/tmp/lua_dcc_bug4/lfunc.o /tmp/lua_dcc_bug4/lgc.o \
/tmp/lua_dcc_bug4/linit.o /tmp/lua_dcc_bug4/liolib.o \
/tmp/lua_dcc_bug4/llex.o /tmp/lua_dcc_bug4/lmathlib.o \
/tmp/lua_dcc_bug4/lmem.o /tmp/lua_dcc_bug4/loadlib.o \
/tmp/lua_dcc_bug4/lobject.o /tmp/lua_dcc_bug4/lopcodes.o \
/tmp/lua_dcc_bug4/loslib.o /tmp/lua_dcc_bug4/lparser.o \
/tmp/lua_dcc_bug4/lstate.o /tmp/lua_dcc_bug4/lstring.o \
/tmp/lua_dcc_bug4/lstrlib.o /tmp/lua_dcc_bug4/ltable.o \
/tmp/lua_dcc_bug4/ltablib.o /tmp/lua_dcc_bug4/ltm.o \
/tmp/lua_dcc_bug4/lundump.o /tmp/lua_dcc_bug4/lutf8lib.o \
/tmp/lua_dcc_bug4/lvm.o /tmp/lua_dcc_bug4/lzio.o"
gcc -o /tmp/lua_dcc_bug4/lua /tmp/lua_dcc_bug4/lua.o $libobjs -lm -ldl
gcc -o /tmp/lua_dcc_bug4/luac /tmp/lua_dcc_bug4/luac.o $libobjs -lm -ldl

# Runtime smoke tests
/tmp/lua_dcc_bug4/lua /tmp/lua_dcc_bug4/smoke.lua
/tmp/lua_dcc_bug4/luac -o /tmp/lua_dcc_bug4/smoke.luac /tmp/lua_dcc_bug4/smoke.lua
/tmp/lua_dcc_bug4/lua /tmp/lua_dcc_bug4/smoke.luac
```

Observed output:

```text
smoke ok        500500  720
smoke ok        500500  720
```

Additional regression check:

```bash
cargo test indirect_call_assignment_preserves_lvalue_address
```

Result: passed.

Full `cargo test` currently compiles and runs, but still has 5 unrelated
pre-existing failures in floating-point/vector tests:
- `double_complex_abi_run_returns_zero`
- `fenv_access_rounding_mode_affects_runtime_arith`
- `float_mod_run_returns_zero`
- `m2_5_float`
- `vector_size_cast_in_expression_run_returns_zero`

---
**Last Updated**: 2026-09-15
**Build Environment**: Linux 24.04, DCC Rust compiler
