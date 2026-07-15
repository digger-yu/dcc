# Next Steps: Linux 6.6 Kbuild / scripts/mod

This document tracks follow-up work after `-S` assembly output and Kbuild
`DEFINE()` macro codegen landed.

## 1. Regenerate devicetable offset artifacts on remote

Clear the compiler cache and rebuild the offset table assembly and header
under Linux 6.6:

```bash
rm -rf ~/.dcc/cache
cd ~/linux-6.6
rm -f scripts/mod/devicetable-offsets.s scripts/mod/devicetable-offsets.h
make CC=~/dcc-agent-linux66/target/release/dcc scripts/mod/devicetable-offsets.h
```

Verify `scripts/mod/devicetable-offsets.s` is ASCII text and contains
`.ascii "->SIZE_*"` / `.ascii "->OFF_*"` lines. Confirm
`scripts/mod/devicetable-offsets.h` defines symbols such as
`SIZE_cdx_device_id`.

## 2. Build file2alias.o with dcc (host / modpost)

`file2alias.c` is compiled as a host tool (`HOSTCC`). A direct dcc attempt
failed because standard headers were not found on the test VM
(for example `stdbool.h` under `/usr/include`).

Actions:

- Locate GCC internal include directories on the build host
  (`gcc -print-file-name=include`, `/usr/lib/gcc/.../include`).
- Teach dcc default host include search (or document required `-isystem`
  flags) so host tools like `file2alias.c` can compile without `-ffreestanding`.
- Rebuild with kernel host flags:

```bash
cd ~/linux-6.6/scripts/mod
~/dcc-agent-linux66/target/release/dcc \
  -Wp,-MMD,.file2alias.o.d -Wall -Wmissing-prototypes \
  -Wstrict-prototypes -O2 -fomit-frame-pointer -std=gnu11 \
  -c -o file2alias.o file2alias.c
```

Do **not** set `HOSTCC=dcc` globally until fixdep and other host bootstrap
tools compile; keep `HOSTCC=gcc` for `scripts/basic/fixdep`.

## 3. Complete scripts/mod host link (modpost)

After `file2alias.o`, `modpost.o`, and `sumversion.o` build with dcc:

```bash
make CC=~/dcc-agent-linux66/target/release/dcc scripts/mod/modpost
```

Fix any remaining parser/codegen gaps surfaced by modpost sources.

## 4. Kernel prepare0 / bounds.s blockers

Forcing removal of `devicetable-offsets.s` during a partial `make` run
can pull in unrelated prepare steps (for example `kernel/bounds.s`) that
still fail on dcc (ellipsis/colon syntax in generated bounds checks).

Track and fix bounds.c / asm generation separately; avoid full `make`
reconfig prompts by keeping an existing `.config` and building narrow
targets only.

## 5. Suggested verification checklist

- [x] `cargo test --release` (22 integration tests green)
- [x] `scripts/remote_feature_check.sh` (O1/MD/san/LTO/GlobalAsm/`-S`)
- [x] `scripts/test_kernel_features.sh` (7/7)
- [x] `devicetable-offsets.s` is assembler text, not ELF (needs `autoconf.h`)
- [ ] `devicetable-offsets.h` has 200+ `#define` entries (via full make)
- [ ] `file2alias.o` builds with dcc on remote (parser gaps remain)
- [ ] `modpost` host binary links successfully
- [x] Document host include path requirements in KERNEL_BUILD_GUIDE.md
- [x] Document autoconf.h prerequisite in sync_remote_build.sh
- [x] Fix CRLF on shell scripts synced from Windows
- [x] `kernel/bounds.s` compiles with dcc (GNU case range)
- [x] `-S` emits custom sections and decodes lock cmpxchg (not `.byte`)
- [x] `-g` emits `.debug_*` / `.BTF` sections
- [x] `-flto` emits `.llvm.lto` summary section
- [x] `-MD` / `-O1` / sanitizer / GlobalAsm integration tests
- [x] Host GCC include path for modpost headers
