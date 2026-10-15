# DCC

> A tiny C compiler written in Rust: own preprocessor, IR, optimizer, back ends, assembler and linker — no LLVM.

## What is this

dcc compiles C (C89 … C23 plus GNU extensions) and a C++11–23 subset straight into object
files and executables. Every layer is in this one crate: preprocessing, parsing, typed IR,
an SSA optimizer, per-architecture mini-assemblers, ELF/COFF writers and an ELF/PE linker.

It is meant for the boring, demanding part of systems programming — kbuild, bootloaders,
freestanding code, toolchain test suites — where a fast, reviewable compiler beats a clever one.

> "Make doesn't strictly require GCC, but you obviously need a C compiler to build C code.
> Try dcc; it's noticeably faster."
>
> "This is a personal research project. It is not production-ready. Bugs and missing
> features are known. Issues and patches are welcome, but I cannot promise timely
> responses."

Status: **alpha**. Proven so far: whole-tree `make CC=dcc` on Linux 7.1 x86_64 → `vmlinux`,
bzImage and a QEMU boot reaching `/init`; self-hosting `dcc0 → dcc1 → dcc2`; GCC torture
suite 99.92 % pass (§Testing).

| | |
|---|---|
| Languages | C89/99/11/17/23 + GNU, C++11–23 subset, inline asm |
| IR | typed IR + lightweight SSA |
| Objects | ELF64, ELF32, COFF; executables ELF (exec/PIE/shared) and PE32+ |
| Back ends | x86-64, AArch64, RISC-V 64, ARM (ARM/Thumb), MIPS 32/64 |
| Targets | Linux (glibc/musl), freestanding, bare-metal triples, Windows (COFF + PE32+) |
| Extras | LTO, DWARF 4/5 + BTF + eh_frame/ORC, sanitizers, PGO/GCOV/AutoFDO/LBR/MemProf/Topdown, OpenMP·CUDA·HIP offload (`.dcc.offload` + PTX/AMDGCN), PCH, header + incremental caches, self-hosting diff |

## Why no LLVM

Recorded in [`docs/adr.md`](./docs/adr.md); in short:

- **Build cost.** LLVM is ~1 M lines of C++ and tens of minutes to bootstrap. dcc is one
  `cargo build --release`, and `dcc -c` writes the object file directly from its own
  mini-assembler — no `as`, no LLVM-IR round trip, one pass per translation unit.
- **The workload does not need it.** The target audience builds with `-O0`…`-O2`; what matters
  there is correct codegen for exotic GNU-isms (statement expressions, `asm goto`, per-CPU
  segments, `__int128`, nested functions) and kbuild-level flag compatibility, not the last few
  percent of vectorizer quality.
- **Own IR = own guarantees.** Output must be byte-deterministic, because self-hosting is
  verified by diffing objects (`--bootstrap-compare`). A shared back end would hide the passes
  that decide that.
- **The low layers are the point.** Writing the ELF/COFF/PE containers, the `.dcc.lto` bitcode,
  BTF, ORC unwinding, IFUNC resolvers and the mini-assemblers is what makes kernel and
  bootloader builds possible at all.

Cost paid: fewer optimizations, and per-architecture gaps that must be tracked explicitly
instead of being inherited. Those gaps are published by artifact probe, not by claim —
[`docs/backend_feature_matrix.md`](./docs/backend_feature_matrix.md).

Related decisions: Rust, not C++ (ADR-001); AST-as-IR (ADR-002); global type registry so the
kernel's ~30 k header includes are parsed once (ADR-003); `-std=gnu11` default to match the
kernel (ADR-004); `mmap` I/O (ADR-006); cross-target from day one (ADR-007).

## Quick start

```bash
cargo build --release                    # -> target/release/dcc
target/release/dcc hello.c -o hello && ./hello

make test                                # Tier-0 gates (see Testing)
make bootstrap                           # dcc0 -> dcc1 -> dcc2, byte + semantic comparison
bash scripts/setup_torture.sh            # sparse-fetch gcc.c-torture (~2 k + ~1.7 k tests)
bash scripts/torture_test.sh all
```

Building needs a Rust 2021 toolchain **and** a host C compiler: the `zstd` crate compiles C
sources through `zstd-sys`. Host `cc` / `ld` / `objcopy` are used when dcc delegates linking
(the default for `ET_EXEC`) and by a few gate scripts.

## Usage

`dcc [options] file…` — the authoritative list is `src/cli.rs`; `--help` prints the summary.

| Group | Flags |
|---|---|
| Actions | `-c` `-S` `-E` `-r` `-o <f>` `-fsyntax-only` `-dump-ast` `-dM` `-j<N>`/`--jobs` |
| Language | `-std=c89…c23` `-std=gnu89…gnu23` `-std=c++11…c++23` `-x c\|c++\|assembler…` `-ffreestanding` `-fpermissive` |
| Optimizer | `-O0` `-O1` `-O2` `-O3` `-Os`(= `-O2`) `-Ofast`(= `-O3 -ffast-math`) `-flto` `-fipa-sra` |
| Debug | `-g0`…`-g3` (`-g` = `-g2`) `-gdwarf-4`/`-gdwarf-5` `-gsplit-dwarf` |
| Target | `--target <triple>` `--sysroot=<dir>` `-march=` `-mtune=` `-mcpu=` `-m32` `-mthumb` `-mfloat-abi=soft\|softfp\|hard` `-mno-red-zone` `-mno-sse` `-mno-sse2` |
| Preprocessor | `-D` `-U` `-I` `-isystem` `-iquote` `-idirafter` `-include` `-nostdinc` `-MD -MMD -MP -MF -MT -MQ` `-Wp,<opts>` `-trigraphs` `-fpch-create=<f>` `-fpch-include=<f>` |
| Linker | `-pie` `-shared` `-static` `-l` `-L` `-u` `-e` `-T <script>` `--whole-archive` `-Wl,--exclude-libs=` `-Wl,--dynamic-linker=` `-Wl,--build-id=` `-Wl,--gc-sections` `-Wl,--gdb-index` `-z relro\|now\|execstack\|max-page-size=N` `-rpath` `--vdso=<f>` `-fuse-ld=dcc\|gold\|lld\|bfd` `-fbuiltin-linker` |
| Instrumentation | `-fsanitize=address\|kernel-address\|hwaddress\|undefined\|thread\|memory\|dataflow\|cfi\|fuzzer` `-fsanitize-coverage=` `-fprofile-generate` `-fprofile-use` `-ftest-coverage` `-fprofile-arcs` `-fauto-profile` `-flbr-profile` `-fmemory-profile` `-ftopdownmetrics` `-fopenmp` `-fcuda` `-fhip` `-finstrument-functions` `-pg` `-mfentry` |
| Diagnostics | `-fdiagnostics-color=always\|never\|auto` `-fdiagnostics-format=github\|clang\|msvc\|vi\|sarif` `--bootstrap-compare=<ref.o>` `-v` `--profile-parse` |
| Emitters | `--emit-module-file=<f.bmf>` (C++ modules) `--cxx-native=auto\|on\|off` `--asm-fallback=gas\|error` `--incremental`/`-fcached` |
| Accepted, no effect | `-pipe`, `-specs=`, most `-W…`, and `--output-format=binary\|hex\|ihex` (parsed; not wired to the link step yet, output stays ELF) |

Honesty rules the CLI follows: an unrecognized `-f…` warns instead of being swallowed;
`-fstrict-aliasing` warns because TBAA is not implemented (`-fno-strict-aliasing` is silent —
that already is the behavior); inline asm that cannot be encoded is `E0311` unless
`--asm-fallback=gas` is given.

## Example

```c
#include <stdio.h>

int fib(int n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }

int main(void) {
    printf("dcc: fib(20) = %d\n", fib(20));
    return 0;
}
```

```console
$ dcc -O2 -o hello hello.c && ./hello
dcc: fib(20) = 6765

$ dcc -O2 -S -o hello.s hello.c      # assembly, no assembling
```

Cross compiling (`-c` writes the target object directly; system headers need a `--sysroot`
for anything but freestanding code):

```console
$ dcc --target aarch64-linux-gnu -O2 -c -o add.o add.c
$ dcc --target riscv64-linux-gnu -O2 -c -o add.o add.c
$ dcc --target x86_64-pc-windows-msvc -c -o add.obj add.c   # COFF
$ make CC=dcc HOSTCC=gcc -j"$(nproc)" bzImage               # whole kernel tree
```

## Architecture

```
source → preproc → parser/semantics → typed IR (the AST stays the single source of truth)
       → SSA build → pass pipeline (opt · loop · regalloc) → SSA rebuild
       → codegen (per-arch mini-assembler + DWARF/BTF/eh_frame writers)
       → object (ELF64 / ELF32 / COFF)
       → link (built-in ELF + PE, or host `cc` / `ld -r`)
```

Caches beside the pipeline: preprocessed-header cache `~/.dcc/cache`, PCH files, and the
incremental build cache `.dcc-cache/` (source + flag hash → skip unchanged files).

≈350 k lines of Rust in one crate, two binaries (`dcc`, `fixdep` — the Kbuild dependency tool).

| Path | Responsibility |
|---|---|
| `src/driver.rs` | phase orchestration, host-tool delegation, verbose reports |
| `src/cli.rs` | options, target triples, sanitizer/profile flags |
| `src/preproc.rs` | macros, includes, `#pragma`, `#embed`, header cache, PCH |
| `src/parser.rs` `src/pratt.rs` `src/semantics.rs` `src/ir.rs` | C front end and typed IR |
| `src/opt.rs` `src/ssa/` `src/loop_opt.rs` `src/ast_gvn.rs` `src/ipa_sra.rs` | AST + SSA optimizer |
| `src/codegen/<arch>/` | lowering, mini-assembler, ABI, per-arch ELF/COFF emission |
| `src/codegen/shared/` | back-end registry, capability `FeatureSet`, ELF writer, asm engine |
| `src/link/` | resolution, layout, relocation, PLT/GOT, `.gnu.hash`, symver, IFUNC, eh_frame, build-id, ar/DSO/vDSO, linker scripts, PE |
| `src/cpp/` | C++ lexer/parser/sema, mangling, templates, modules (BMF), coroutines; lowers to C text or to IR |
| `src/debug.rs` | DWARF 4/5, BTF/BTF.ext, `.eh_frame`/CFI, ORC, `.gdb_index`, split DWARF |
| `src/sanitizer.rs` | KASAN/HWASAN(MTE)/UBSAN/KCSAN/MSAN/DFSAN/CFI/KCOV instrumentation |
| `src/profile/` | PGO, GCOV, AutoFDO, LBR, MemProf, Topdown, perf events, offload |
| `src/lto.rs` `src/link/lto_bitcode.rs` | `.llvm.lto` text summary + `.dcc.lto` (`DBC\0`) cross-TU passes |
| `src/bootstrap.rs` | three-level (byte/struct/semantic) self-compile comparison |
| `src/mmap_io.rs` `src/node_pool.rs` `src/timing.rs` | I/O and allocation fast paths |

Two structural rules: the AST is the IR (each pass rewrites and re-reads the same source of
truth, `Module::type_info()` gives structured type queries), and capability claims are derived
from build artifacts — `scripts/probe_backend_features.sh` fails CI when the declared
`FeatureSet` and reality disagree.

## Supported C subset

Default `-std=gnu11` (matches the kernel). Full matrix: [`docs/c_standard.md`](./docs/c_standard.md).

| Standard | Covered |
|---|---|
| C89/C90 | including K&R definitions (auto-detected) |
| C99 | compound literals, designated initializers, VLA, `long long`, `_Bool`, `inline`, block-scoped declarations |
| C11 | `_Static_assert`, `_Atomic`, anonymous struct/union, `_Generic`, `_Thread_local`, `_Noreturn` |
| C17 | same path as C11 |
| C23 | `typeof`/`typeof_unqual`, `constexpr`, `bool`/`true`/`false` keywords, `nullptr`, `static_assert` without message, enum underlying type, `[[attributes]]`, `_BitInt(N)`, digit separators, `#embed`, `__has_embed`, `%b` |
| GNU | `__int128`, statement expressions, `__VA_ARGS__`/`__VA_OPT__`, nested functions, computed goto, `asm goto`, `&&label`, full `__attribute__` family, `__declspec` translation, `_Complex`/`_Decimal`/`_Fract`/`_Accum`, `__seg_fs`/`__seg_gs`, `__builtin_*` (~300 recognized; an unknown one is a hard `E0018` in x86-64 codegen) |
| Inline asm | AT&T constraints (`r m =r +m & i 0..9`, named operands), file-scope `asm`, `asm goto`; x86-64 up to AVX-512/AMX/APX, AArch64 NEON+SVE, RISC-V C/D/V/Zicsr |

C++ is a subset on purpose: classes, inheritance (incl. virtual), virtual functions + vtable and
RTTI, operators, exceptions, lambdas, templates with real monomorphization and partial
specialization, concepts/SFINAE, coroutines, modules with ODR checks, and ~40 standard headers.
Both paths land in the same C back end — there is no separate C++ code generator. Details in
[`docs/support_features.md`](./docs/support_features.md).

## Known limitations

Authoritative lists: [`docs/missing_features.md`](./docs/missing_features.md) (with the evidence
behind each entry) and [`docs/backend_feature_matrix.md`](./docs/backend_feature_matrix.md).

- **Cross back ends are not x86-64.** AArch64/RISC-V/ARM/MIPS pass arithmetic, float, bitfields,
  varargs, DWARF and `.eh_frame` probes, but fail on cross-TU call symbols (`CoreCall`), TLS
  sections, `-flto` bitcode and `-fsanitize=*`; `-fstack-protector` errors out on AArch64/RISC-V.
- **`-S` is real assembly only on x86-64** (partly ARM). AArch64/RISC-V/MIPS write a
  machine-code size comment plus a stub; on those targets use `-c`, which encodes the
  instructions directly. `--asm-fallback=gas` covers unencodable *inline* asm, and
  `scripts/check_asm_text_parity.sh` keeps the two emitters aligned where the back end declares
  `GasTextOutput`.
- **No TBAA.** The optimizer always assumes conservative aliasing; `-fstrict-aliasing` says so.
- **`target_clones` variants are byte-identical** — there is no per-function ISA context yet.
- **Unsupported platforms fail in two different ways.** `riscv32-*` parses and then stops with
  `E0399` (no code generator); `powerpc*`, `loongarch64`, `wasm32`, `avr` and similar arch names
  are rejected during triple parsing (`unknown arch`). Darwin triples parse cleanly and currently
  produce an **ELF** object, because there is no Mach-O writer yet. No back end exists for
  wasm32, AVR/MSP430/Xtensa, PowerPC or LoongArch64.
- **Windows**: COFF objects build fine and PE32+ executables link through the built-in linker
  (`-fbuiltin-linker`), but there is no DLL import table and no Windows CRT, so linking against
  real libraries is unverified locally.
- **`--output-format=binary|hex|ihex` is parsed, not wired**: the converters in
  `src/output_format.rs` have no link-step caller, so linked output is still ELF.
- **No `ld.so`** — dynamic loading is delegated to the host by design.
- Address-taking nested functions use a stack trampoline, which needs an executable stack.
- `mips64` ELF64 symtab misplaces the first null symbol by 25 bytes, so UND-based probes there
  are inconclusive, and no `mips64-*-as` is available in CI.

## Testing

```bash
cargo test --lib                      # 2471 unit tests (2 ignored)
cargo test --release                  # all top-level targets: integration 1094, errors 26,
                                      # profile 53, reproducibility 8, cpp ABI cross-check 9, …
make test                             # Tier-0: cargo build --tests, cargo test --release,
                                      # fixture gate + the gate's own self-test
make test-all                         # Tier-0 plus test_all.sh, -O equivalence, REX hygiene,
                                      # pipe structure, disasm coverage, cross golden files
```

GCC `gcc.c-torture`, measured 2026-10-15 at `0d9e692c`:

| Suite | Total | Pass | Fail | Skip | Rate |
|---|---|---|---|---|---|
| compile | 2016 | 2008 | 0 | 8 | 100 % |
| execute | 1697 | 1689 | 3 | 5 | 99.82 % |
| **all** | **3713** | **3697** | **3** | **13** | **99.92 %** |

The three runtime failures are `921019-1.c`, `pr92904.c`, `va-arg-pack-1.c`.
`scripts/torture_gate.sh` compares against `tests/skip/torture_known_fail_*.txt` and only
allows that set to shrink.

| Gate | Command / CI job |
|---|---|
| GCC `gcc.dg` subset | `scripts/gcc_dg_test.sh` · `gcc-dg.yml` |
| Diagnostic snapshots | `cargo test --test errors` (`DCC_UPDATE_SNAPSHOTS=1` to refresh) |
| Back-end capability probe | `scripts/probe_backend_features.sh` · `check_feature_matrix.yml` |
| `-c` vs `-S` byte parity | `scripts/check_asm_text_parity.sh` · `check_asm_text_parity.yml` |
| Disassembly + reproducibility | `tests/disasm_snapshot.rs`, `tests/reproducibility.rs` |
| Kernel build + QEMU boot | `scripts/kernel_full_check.sh`, `scripts/qemu_boot_matrix.sh` · `kernel-bench.yml` |
| Self-hosting | `make bootstrap` |

Feature → fixture → test index is generated into [`docs/test_coverage.md`](./docs/test_coverage.md);
contribution rules for tests live in [`tests/README.md`](./tests/README.md).

## Roadmap

Full milestone history: [`docs/roadmap.md`](./docs/roadmap.md). Done: C89–C23 + GNU, C++ subset
with modules/coroutines/ODR, six code-generating back ends, built-in ELF+PE linker, LTO,
sanitizers, DWARF/BTF/objtool support, the whole profiling stack, self-hosting, Linux 7.1
x86_64 full tree → vmlinux + bzImage + QEMU + `/init`, and GCC-tree subsets (libiberty,
libcpp, generator programs).

**Now**

- Cross back ends: make extern declarations linkable (`CoreCall`), fix the IR shapes that still
  raise `E0312` (e.g. `for`-initializers on AArch64/RISC-V).
- AArch64/RISC-V: TLS sections, sanitizer instrumentation, `-flto` bitcode, stack protector.
- `mips64` ELF64 symtab defect; LTO Pass 7 (`no_semantic_interposition`) actually implemented.

**Next**

- TBAA wired into the optimizer, with real `-fstrict-aliasing` semantics.
- Per-function ISA context so `target_clones` variants differ; PE import tables.
- Higher C++ native-path coverage (`--cxx-native=on` falling back rarely) and cc1/cc1plus convergence.
- Profile runtime helpers for AArch64/RISC-V.

**Later**

- Distribution binaries per architecture, byte-identical self-hosting pinned in CI, real GPU CI
  for the offload path.

## Contributing

Start with [`.github/CONTRIBUTING.md`](.github/CONTRIBUTING.md): the Tier-0 gate list, where
tests and fixtures go, the debugging switches, the commit-message convention, and the two rules
that shape the code — a capability may only be claimed if a probe can see it in the build
artifact, and nothing may fail silently.

See also [`.github/SECURITY.md`](.github/SECURITY.md) — a compiler crash or a miscompile is filed
as a normal bug, not reported privately — and
[`.github/CODE_OF_CONDUCT.md`](.github/CODE_OF_CONDUCT.md). Issues and patches are welcome;
responses may be slow.

## License

Apache License 2.0 — see [`LICENSE`](./LICENSE). The front end, IR, optimizer, assemblers,
object writers and linker are all in this repository. Only three crates are load-bearing at
build time: `miniz_oxide` and `zstd` (debug-section compression) and `anyhow` (one legacy site
in the MIPS back end) — plus a host C compiler, because `zstd-sys` builds C sources. See
[`docs/adr.md`](./docs/adr.md) ADR-005 for the dependency policy and the declared-but-unused
entries still to drop.
