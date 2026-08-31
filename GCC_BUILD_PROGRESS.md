# GCC 15.3.0 Build Progress with dcc

## Status: In Progress

## Summary
Compiling GCC 15.3.0 using only dcc (no gcc). The build process fixes C++ lowering issues as they are encountered.

## Completed Milestones

### libcpp (GCC preprocessor library) - COMPLETE
All libcpp source files compile successfully with dcc:
- lex.cc (most complex - 24000+ lines preprocessed)
- charset.cc, directives.cc, errors.cc, expr.cc, files.cc
- identifiers.cc, init.cc, line-map.cc, macro.cc, mkdeps.cc
- pch.cc, symtab.cc, traditional.cc

### Key Fixes Applied

#### 1. C++ Parser Improvements
- `const` prefix handling in qualified type detection
- Function pointer member detection for keyword return types
- `__attribute__` suffix skipping in function pointer members
- Elaborated type `class Name` vs class definition disambiguation
- Error recovery in class/struct body parsing (`skip_to_member_end`)

#### 2. AST Lowering Improvements
- **Typedef tracking** (M30.3): Added `typedef_aliases` map to track top-level typedef declarations. Prevents typedef names like `linenum_arith_t` from being treated as class types.
- **Typedef emission**: Emit typedef declarations in lowered output with skip list for hardcoded types and self-referential typedef detection.
- **`__attribute__` stripping**: Remove `__attribute__((...))` from lowered output since C backend doesn't support this syntax.
- **Nested class handling**: Emit nested classes BEFORE enclosing struct definition to prevent post-class emissions (constructors, typedefs, offsetof) from appearing inside struct body.
- **Forward declarations**: Add typedef forward declarations for GCC libcpp types (cpp_reader, cpp_hashnode, cpp_token, etc.) and POSIX types (rlim_t, rlim64_t).

#### 3. Driver Improvements
- Allow AST lowering with parser errors (threshold: ≤5 errors or non-empty decls)
- Enhanced C++ detection patterns

## Current Blockers (gcc directory)

### genhooks.cc
- `htab_create(...)` global initializer not a constant expression (C++ dynamic init vs C static init)
- C string functions (`strlen`, `strcmp`, etc.) being mangled as C++ functions

### sort.cc
- Similar C++ to C lowering issues

### Common Issues
- C++ dynamic initialization of globals needs conversion to runtime init
- C function name mangling (functions declared in `extern "C"` blocks still get mangled)
- Some system types still missing forward declarations

## Build Command
```bash
cd /root/gcc-15.3.0/build
export PATH="/root/dcc/target/release:$PATH"
make -j4
```

## Commit History (dcc repository)
1. `cpp/parser: fix const prefix, function pointer members, elaborated types`
2. `cpp/lower: add GCC built-in type forward declarations`
3. `driver: allow AST lowering with parser errors`
4. `docs: update GCC build progress`
5. `cpp/lower: add typedef tracking and attribute stripping`
6. `cpp/lower: add GCC libcpp forward declarations`
7. `cpp/lower: emit nested classes before enclosing struct`
8. `cpp/lower: remove __fsid_t from builtin types injection`
9. `cpp/lower: add rlim_t/rlim64_t forward declarations`

## Next Steps
1. Fix C++ dynamic initialization of globals (convert to runtime init functions)
2. Fix C function name mangling (respect `extern "C"` blocks)
3. Add more system type forward declarations as needed
4. Continue fixing gcc directory compilation errors
