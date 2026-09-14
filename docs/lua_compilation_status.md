# Lua 5.5.1 Compilation with DCC

## Status: ✅ Compilation Success, ⚠️ Runtime Issue

### Achievements
- **All source files compile successfully** using dcc compiler
- Generated object files and library (liblua.a) without errors
- Successfully linked into `lua` and `luac` executables

### Compiled Components
```
CORE files (all successful):
lapi.c ✓ lcode.c ✓ lctype.c ✓ ldebug.c ✓ ldo.c ✓ ldump.c ✓ 
lfunc.c ✓ lgc.c ✓ llex.c ✓ lmem.c ✓ lobject.c ✓ lopcodes.c ✓ 
lparser.c ✓ lstate.c ✓ lstring.c ✓ ltable.c ✓ ltm.c ✓ 
lundump.c ✓ lvm.c ✓ lzio.c ✓

LIB files (all successful):
lauxlib.c ✓ lbaselib.c ✓ lcorolib.c ✓ ldblib.c ✓ liolib.c ✓ 
lmathlib.c ✓ loadlib.c ✓ loslib.c ✓ lstrlib.c ✓ ltablib.c ✓ 
lutf8lib.c ✓ linit.c ✓

Main programs:
lua.c ✓ luac.c ✓
```

### Known Issues

#### 1. Runtime Segmentation Fault (Under Investigation)
**Symptom**: After successful compilation and linking, running `./lua -v` causes immediate segmentation fault.

**GDB Analysis**:
```
Program received signal SIGSEGV, Segmentation fault.
0x00005555555b30b0 in internshrstr ()
#0  0x00005555555b30b0 in internshrstr ()
#1  0x00007fffffffbf48 in ?? ()
#2  0x00007ffff7cad8f2 in __GI___libc_malloc ()
#3  0x00007ffff7c2a28b in __libc_start_main_impl (main=0x555555568f00 <main>, argc=-7680, argv=0x1, ...)
```

**Key Observations**:
- Crash occurs in `internshrstr()` (string interning function)
- `argc=-7680` in libc_start_main suggests stack corruption or incorrect parameter passing
- The crash happens very early, likely during Lua state initialization

**External Function Calls**: ✅ **VERIFIED CORRECT**
- DCC generates proper ELF relocations (R_X86_64_PLT32) for external function calls
- Linker correctly resolves call targets
- Test program with external function calls works perfectly after linking
- Symbol table and relocation entries are correctly generated

**Current Hypothesis**: ✅ **INVESTIGATION IN PROGRESS**

**Findings so far**:
- Function pointer calls work correctly (verified with test programs)
- Parameter passing is correct (ud, NULL, LUA_TTHREAD=8, sizeof(global_State)=0x658)
- luaL_alloc returns valid pointer (0x5555556442a0)
- lua_newstate returns valid pointer (0x7709189a0)
- Crash occurs in internshrstr() very early in Lua initialization

**Next steps**:
- Need to trace exact crash location with more detailed debugging
- May be related to how Lua initializes internal data structures
- Possible issue with struct layout or member access in DCC-generated code

**Status**: Active debugging - external calls verified correct, investigating internal Lua state corruption

#### 2. Previous Bug Fix Applied
**Issue**: Function pointer identifier arguments in aggregate calls were not generating store instructions.

**Fix Location**: `/root/dcc/src/codegen/x86_64.rs` around line 8495-8506

**Change Summary**:
```rust
// Before: Only loaded data to register but didn't store it
self.load_stack(0, var_off);
match *loc {
    ArgLoc::Stack(off) => self.store_rsp_qword(0, off),
    _ => {
        self.store_rsp_qword(0, stage);  // Using wrong register!
        staged.push((*loc, stage, false));
        stage += 8;
    }
}

// After: Properly load to r11 and store accordingly
self.load_stack(11, var_off); // r11 = data
match *loc {
    ArgLoc::Stack(off) => {
        self.store_rsp_qword(11, off);  // Direct to caller's stack slot
    },
    _ => {
        self.store_rsp_qword(11, stage);  // Stage for passing as argument
        staged.push((*loc, stage, false));
        stage += 8;
    }
}
```

**Rationale**: When an identifier (like a function name) is used as an aggregate argument:
1. For VLA variables: Load base address first, then access data chunks
2. For global variables: Load from absolute addresses  
3. **For regular local identifiers**: Must generate both the load AND subsequent store operations

### Verification Tests Performed

#### Test 1: Struct Return Passing
```c
Point get_point();
void use_point(Point p);
use_point(get_point());  // Aggregate function call
```
✅ Result: DCC generates correct code (call → store → load → call)

#### Test 2: Function Pointer Arguments
```c
void process_func(void (*fp)(int));
process_func(simple_func);
```
✅ Result: Compilation successful, but need runtime verification

### Next Steps for Debugging Segfault

1. **Test minimal Lua program**: Try running a simple Lua script instead of just `-v`
2. **Check startup code**: Verify crt.o initialization and `_start` symbol
3. **Use GDB**: Attach debugger to identify crash location
   ```bash
   gcc -g lua.c liblua.a -o lua_debug -lm -Wl,-E -ldl
   gdb ./lua_debug
   (gdb) break main
   (gdb) run
   ```
4. **Compare with GCC binary**: Run original GCC-built lua to verify logic issue
5. **Check ABI compliance**: Ensure all calling conventions match SysV AMD64 ABI

### Build Script Used

```bash
#!/bin/bash
cd /root/lua-5.5.1/src
DCC=/root/dcc/target/release/dcc

# Compile all .c files
for f in *.c; do
    $DCC -c "$f" -o "${f%.c}.o" -DLUA_USE_LINUX
done

# Create archive
ar rcu liblua.a *.o

# Compile main programs  
$DCC -c lua.c -o lua.o
$DCC -c luac.c -o luac.o

# Link with GCC (required for proper startup)
gcc -o lua lua.o liblua.a -lm -Wl,-E -ldl
gcc -o luac luac.o liblua.a -lm -Wl,-E -ldl
```

### Documentation References
- See `docs/development_practice_specification/` for debugging methodology
- See memory about "GCC torture test failure analysis" for systematic approach
- x86_64 backend implementation in `/root/dcc/src/codegen/x86_64.rs`

---
**Last Updated**: 2026-09-14
**Build Environment**: Linux 24.04, DCC Rust compiler
