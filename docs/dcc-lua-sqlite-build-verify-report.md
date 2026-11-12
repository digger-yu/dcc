# DCC — Lua 5.5.1 + SQLite: build & verification results

**Verdict (re-checked 2026-11-12, dcc `90b0370c` + the working-tree
`-shared`/`--whole-archive`/frame-depth changes, `sha256 = c8b7bc82…`; gcc 13.3.0).**
dcc builds Lua 5.5.1 and SQLite 3.53.4/3.54.0 from source at `-O0`…`-O3`, and both
binaries run. The **Lua official suite (`all.lua`) passes at all four levels on all
three verification paths** (`final OK !!!`, `rc=0`, 550 / 416 / 328 lines) **with all
five `libs/*.so` dynamic modules built by dcc itself** (§3.4), and after noise
normalisation its output is **line-identical to the `gcc -O2` baseline in all 12
comparisons** (§4). §2 shows the four levels are genuinely distinct on the command
line, not four copies of `-O2`. SQLite has been run against its **official TCL suite
(`test/quick.test`, 618 691 tests) to completion at all four levels**: every level
reaches the suite's own summary line with `5 errors out of 618 691 tests`, and the
failing-name set is **identical to the gcc baseline's** (those five also fail under gcc,
§4.4) — `RESULT: PASS`, no new failure at any level. **Nineteen complete upstream test
files (2 302 tests)** also pass end to end with **byte-identical result lines** at all
four levels: `select1..5` (470), twelve `mmap`/`wal`/`where`/`join`/`index` files
(1 606) and `session1`/`session3` (226, from a fixture rebuilt with
`SQLITE_ENABLE_SESSION` — the stock `testfixture` skips the whole family, §4.5); the
upstream workload (`speedtest1 --verify`) produces **the same query-result content hash
as gcc at all four levels**, and `rtree`/`json`/`star` — which `--verify` cannot score —
get an equivalent non-`--verify` criterion (identical step sequence, identical `.dump`
content hash, `integrity_check = ok`; §4.5). The defects these suites exposed — ten in
total, nine miscompiles (two of them only became reachable once the earlier ones were
fixed) plus one ASan-only codegen defect — are all fixed and each now has a regression
case (§4.4's table); what remains unverified is listed in §7 and is a boundary of
*coverage* (the rest of the tree's 1 196 `.test` files, the heavy files `quick.test`
skips, other targets, musl), not a known break. Runtime performance is measured
separately in §5 (`scripts/bench_lua_sqlite.sh`, clean rebuild per level) and is
**not** part of this correctness verdict.

One gap left open by the previous revision is closed: `-shared` produced no loadable
shared object — it was an alias for `-fbuiltin-linker`, whose `.so` writer has no
dynamic exports — so the module side of "dynamic module loading" had to be gcc-built.
`-shared` now selects only the output form and links through the host GNU `ld -shared`
(no host C compiler is started), so the five `libs/*.so` are dcc artifacts and the
2026-11-12 re-run below used them.

This file records final results only: build commands, result tables, the
reproduce script and the open gaps.

---

## 1. Sources and environment

| Item | Value |
|---|---|
| dcc | this repository `90b0370c` plus the working-tree changes (`-shared` / `--whole-archive` in `src/cli.rs`, `src/driver.rs`, `src/link/host_ld.rs`; frame-depth scan in `src/codegen/x86_64/emit/func_setup.rs`), `cargo build --release` → `target/release/dcc`, `sha256 = c8b7bc82…` |
| Lua | 5.5.1 (`~/lua-5.5.1`) |
| Lua tests | `~/lua-5.5.1-tests` (official suite, `all.lua`) |
| SQLite | 3.53.4 amalgamation (`~/sqlite`, `sqlite3.c` + `shell.c`) for §3.2/§5; 3.54.0 **source tree** (`~/sqlite-src-3540000`, from `scripts/setup_sqlite_suite.sh`) for the upstream suite |
| SQLite tests | `test/quick.test` in that tree (1196 `.test` files, 618 691 test cases); needs `tcl8.6-dev` |
| Host | Linux x86-64, 4 cores; reference compiler `gcc 13.3.0` |
| Known host gap | no `libreadline-dev`: only `libreadline.so.8` exists, not the unversioned `libreadline.so` (see §3.4) |

## 2. Are the four `-O` levels real?

Lua's `src/Makefile` puts the optimisation flag *before* the user flags:

```make
CFLAGS= -O2 -Wall -Wextra $(SYSCFLAGS) $(MYCFLAGS)     # line 10
```

so `MYCFLAGS="-std=gnu99 -O0"` expands to `-O2 -Wall -Wextra -std=gnu99 -O0`
(verified with `make echo MYCFLAGS=...`). The four levels are therefore only
meaningful if the compiler lets the **last** `-O` win. Both do:

* **gcc** — last `-O` wins (documented behaviour).
* **dcc** — `src/cli.rs` parses the flags in a single left-to-right loop and
  assigns `opt` each time it sees `-O0|-O|-O1|-O2|-O3|-Ofast|-Os`, so the last
  occurrence is the one that survives. Pinned by the integration test
  `repeated_opt_flag_last_wins` (`tests/integration/glue_misc.rs`), which also
  asserts `-O0` and `-O2` are *byte-different* — otherwise those equalities would
  hold vacuously even if the optimiser ignored `-O` altogether.

The two build forms are not the same, and only one of them leans on last-wins
(captured with `make -n -B all CC=$DCC …` in `src/`, i.e. the real recipe line):

```sh
# §6 reproduce path — MYCFLAGS appends: two -O on the line, last one wins
… dcc -O2 -Wall -Wextra  -std=gnu99 -O0  -c -o lapi.o lapi.c

# check_lua_suite_diff.sh — overrides CFLAGS wholesale (a command-line variable
# replaces the Makefile assignment), so exactly one -O reaches the compiler:
… dcc -O0 -Wall -Wextra -std=gnu99 -DLUA_USE_LINUX -c -o lapi.o lapi.c
```

The gate therefore does not depend on the last-wins rule at all; §6 keeps the
`MYCFLAGS` form because that is what upstream documents, and the hashes below
cover the difference it makes.

Measured (same input, dcc and gcc, object files hashed):

```
dcc -O2 -O0  == dcc -O0        (27f71f33…, 1672 B)
dcc -O2 -O3  == dcc -O3        (e64965c5…, 1704 B)
dcc -O2      != both           (427624d0…, 1704 B)
gcc -O2 -O0  == gcc -O0        (b9e18c67…)
gcc -O2 -O3  == gcc -O3        (0056a465…)
```

End-to-end confirmation on the real workload — clean rebuild of Lua at each
level, then `fib(32)` (§5):

| | `-O0` | `-O1` | `-O2` | `-O3` |
|---|---|---|---|---|
| dcc | 309 ms | 309 ms | 269 ms | 267 ms |

The level changes the generated code, so “all four levels” is not `-O2` four
times. (Caveats: the trailing flag is the only thing that varies between the
four builds — `-std=gnu99` also lands after `-O2`, but in every level; and on
this workload dcc only gains between `-O1` and `-O2`.)

## 3. Build commands

### 3.1 One recipe for both verification paths

```bash
DCC=~/dcc/target/release/dcc
cd ~/lua-5.5.1
make clean
make linux CC="$DCC" MYCFLAGS="-std=gnu99 -O2"
```

`make linux` (not `make posix`) is the right base: it adds `-DLUA_USE_LINUX`
(which pulls in `LUA_USE_POSIX`, `LUA_USE_DLOPEN` and `LUA_READLINELIB`) and
links with `-Wl,-E -ldl`, so `package.loadlib` works. `posix` defines only
`LUA_USE_POSIX` and has no `-Wl,-E`, i.e. dynamic modules are unusable — the
two targets are **not** interchangeable.

`-std=gnu99` may go either in `CC` or in `MYCFLAGS`; both end up on the compile
line and dcc, like gcc, does not care about the position (the Makefile's own
`CC= gcc -std=gnu99`, line 9). This document uses `MYCFLAGS` because overriding
`CC` would otherwise silently drop that default `-std=gnu99`.
`scripts/check_lua_suite_diff.sh` uses a third form — it assigns `CFLAGS=` on the
make command line, which replaces the Makefile's `CFLAGS` wholesale, so `-std=gnu99`
is carried there and is *not* also appended to `CC` (§2 shows the resulting single
`-O` line).

### 3.2 SQLite

Single invocation, both translation units plus the link:

```bash
cd ~/sqlite
$DCC -O2 -o sqlite3_dcc shell.c sqlite3.c -lpthread -ldl -lm
```

dcc delegates linking to the host linker, accepts several `.c` files per
invocation, and splits `$CC`/`$LD` into shell words, so it works as a drop-in
`CC`.

The upstream `testfixture` is *not* built that way: `main.mk` compiles and links all
88 translation units in **one** command, and dcc's multi-file entry keeps every unit
in one process, so the build is OOM-killed (`make … error 137`, i.e. SIGKILL) before
`sqlite3.c`
(9.5 MB source, 2.7 GB peak RSS alone) finishes. `scripts/check_sqlite_suite.sh`
therefore captures the recipe's real argument list from `make` and compiles the units
**one process per translation unit**, then links once; its header records the method
and the caveat above.

### 3.3 The three verification paths

* **Path A** — `ltests` build: extra internal assertions, `lua_*` exported via
  `-Wl,-E`, `T` available. Runs the full suite (~550 lines, including
  `main.lua` and non-portable tests).
* **Path B** — stock `make linux` build run as `-e "_U=true" all.lua`: what a
  user gets. User mode sets `_U`, which makes `all.lua` also set `_port` and
  `_soft`; `main.lua` returns immediately (`if _port then return end`, line 6),
  so path B prints ~328 lines and **skips** the stand-alone-interpreter tests,
  the failed-write test and the dynamic-module tests. Path B therefore proves
  that a stock dcc build runs the suite; it does not cover those areas.
* **Path B-full** — the same stock `make linux` binary, but running plain
  `all.lua` (no `_U=true`), i.e. the non-portable tests are *not* skipped
  (~416 lines, 27–28 test files). It costs one extra run per level and covers
  the area path B gives up; `scripts/check_lua_suite_diff.sh` runs all three.

### 3.4 `libs/*.so` (dcc-built) and `LD_LIBRARY_PATH=/tmp/rllib`

The suite's dynamic-module tests load `libs/lib1.so`, `lib11.so`, `lib2.so`,
`lib21.so`, `lib2-v2.so`. All five are **dcc artifacts**, built by the test tree's
own makefile with dcc as `CC` (only the `-O2` level applies; the makefile hard-codes
it):

```bash
cd ~/lua-5.5.1-tests/libs
make LUA_DIR=~/lua-5.5.1/src CC="$DCC"  # dcc -Wall -O2 -I… -fPIC -shared -o libN.so libN.c
```

dcc compiles each `libN.c` itself (`-fPIC` is honoured by the code generator) and
links the `.so` through the host GNU `ld -shared` **directly**; the host C compiler
is not started for any of the five builds:

```
$ make LUA_DIR=~/lua-5.5.1/src CC=$DCC
/root/dcc/target/release/dcc -Wall -O2 -I/root/lua-5.5.1/src -fPIC -shared -o lib1.so lib1.c
dcc: lib1.so (shared, via host ld)
… (same shape for lib11.so, lib2.so, lib21.so, lib2-v2.so ← lib22.c)
```

`-shared` used to be an alias for `-fbuiltin-linker`. The built-in linker's `.so`
writer has no dynamic exports (`.dynsym` entries are written with `st_name = 0` and
`.dynamic` carries no `DT_HASH`/`DT_GNU_HASH`), so such an artifact `dlopen`ed but
`dlsym()` found nothing; worse, it rejected the modules' undefined references
(`lua_*`, `lib1_export`, `luaopen_lib2`) that are undefined **by design**. `-shared`
now selects only the output form, and `-fbuiltin-linker -shared` is a hard error
(`E0320`, "cannot emit a loadable shared object") instead of a silent one.

What the dcc-built modules contain (`-O2 -fPIC`, sysv + GNU hash tables, no `TEXTREL`):

| module | `T luaopen_*` | `U lua_*` | `DT_NEEDED` |
|---|---|---|---|
| `lib1.so` | 1 (`luaopen_lib1_sub`) | 8 | `libc.so.6` |
| `lib11.so` | 1 (`luaopen_lib11`) | 0 | `libc.so.6` |
| `lib2.so` | 1 (`luaopen_lib2`) | 4 | `libc.so.6` |
| `lib21.so` | 1 (`luaopen_lib21`) | 0 | `libc.so.6` |
| `lib2-v2.so` | 1 (`luaopen_lib2`) | 12 | `libc.so.6` |

The table is a gate now, not prose: `scripts/check_shared_object.sh` rebuilds the
five modules with dcc and asserts per artifact `ET_DYN` / no `PT_INTERP`, exactly one
`T luaopen_*` with the expected name, the expected count of `U lua_*`, `DT_GNU_HASH`,
`DT_SONAME`, no `TEXTREL`, `DT_NEEDED` libc, and a successful `dlopen(RTLD_LAZY)` +
`dlsym` in a minimal C host. Current run (2026-11-12, `sha256(dcc) = c8b7bc82…`): **30 assertions
pass, 0 fail, 1 skip (musl)** — `RESULT: PASS`.

The undefined `lua_*` are resolved at load time from the executable's exported
symbols (`make linux` links with `-Wl,-E`), and `lib11.so` resolves `lib1_export`
from `lib1.so`. Path A exercises the chain: `attrib.lua:291` (`package.loadlib` of
`lib11`, which must return `"exported"`), `attrib.lua:296` (`require"lib2-v2"`) and
`main.lua:251` (`lua -l lib2-v2`). Deleting `libs/*.so` makes path A fail at
`main.lua:251`, while path B still passes. The suite never `dlopen`s two of the five
(`lib2.so`, `lib21.so`); the script closes that blank: `lib2.so` loads in a real Lua
host (its entry writes its two arguments to globals `x`/`y`), and `lib21.so` loads
with `LD_PRELOAD=lib2.so` (its entry calls `luaopen_lib2` and checks the same globals).
The core behaviour is also pinned by four integration cases in
`tests/integration/c_link_debug.rs`: `shared_library_is_loadable_and_exports_symbols`,
`shared_library_leaves_undefined_symbols_for_runtime_binding`,
`builtin_linker_rejects_shared_output` and
`whole_archive_applies_to_explicit_archive_paths`.

Two `-shared` combination boundaries are covered by the same script:

* `-flto -shared` — all five modules built with `-fPIC -O2 -flto -shared
  -Wl,-soname,…` keep the same ELF shape and load in the minimal host. dcc accepts
  `-flto` and still links through host `ld -shared`; this pins that the combination
  is neither refused nor rerouted, not that an LTO pass merges the module.
* `-shared --whole-archive` — two checks. The real shape (`lib11.c` + `lib1.a`)
  links, exports `T lib1_export` from the archive and still resolves at load time;
  a semantic control archives `w1.o` (referenced) + `w2.o` (unused): with
  `-Wl,--whole-archive` both are `T` in `.dynsym`, without it `w2` is absent. The
  control found a real CLI defect — `--whole-archive` was tracked only for `-lNAME`
  inputs, so an explicit `path/lib.a` inside the bracket silently kept default
  archive semantics and unused members never entered the `.so`. Fixed in
  `src/cli.rs`, `src/driver.rs` and `src/link/host_ld.rs` (`whole_archive_inputs`);
  the integration case above pins both arms.

`/tmp/rllib` is a workaround for a missing dev symlink, and only path A needs
it:

```bash
mkdir -p /tmp/rllib
ln -sf /lib/x86_64-linux-gnu/libreadline.so.8 /tmp/rllib/libreadline.so
```

Lua 5.5.1 does not link readline; `lua.c` `dlopen`s the *unversioned*
`"libreadline.so"` at start-up and calls `lua_warning` if that fails. Without
`libreadline-dev` only `libreadline.so.8` exists, so the `dlopen` fails. In
`main.lua`'s readline test the second half runs with `-E` (which ignores
`LUA_READLINELIB`), so the default name is used and a warning is printed;
`ltests` makes unexpected warnings fatal, which aborts the run. Path B is not
affected: it skips `main.lua` and a plain build does not turn that warning into
an error.

## 4. Verification results

Re-run 2026-11-12 (compiler `90b0370c` + the working-tree changes, `sha256 = c8b7bc82…`)
with `scripts/check_lua_suite_diff.sh`
(`LEVELS="0 1 2 3" VARIANTS="ltests plain user"`); every level is a clean rebuild, the
reference is the same script's `gcc -O2` tree. "identical" below means **after noise
normalisation, line for line** (§4.1), and every run also ends in `final OK !!!` with
`rc=0`; the progress-dot counts are the script's own "no test was skipped" reading.

| Project / path | `-O0` | `-O1` | `-O2` | `-O3` |
|---|---|---|---|---|
| Lua path A (ltests, `all.lua`) | identical, 550 lines, 12 992 dots | identical, 550, 12 992 | identical, 550, 12 992 | identical, 550, 12 992 |
| Lua path B-full (stock build, full `all.lua`) | identical, 416 lines, 1 735 dots | identical, 416, 1 735 | identical, 416, 1 735 | identical, 416, 1 735 |
| Lua path B (stock build, `-e "_U=true"`) | identical, 328 lines, 1 597 dots | identical, 328, 1 597 | identical, 328, 1 597 | identical, 328, 1 597 |
| Lua `gcc -O2` reference (same three paths) | — | — | 550 / 416 / 328 lines, 12 992 / 1 735 / 1 597 dots (two passes,差 0) | — |
| SQLite 3.54.0 `testfixture` compile+link | OK | OK | OK | OK |
| SQLite `test/quick.test` (§4.4) | completes, 0 new vs baseline | completes, 0 new vs baseline | completes, 0 new vs baseline | completes, 0 new vs baseline |
| SQLite 3.53.4 amalgamation compile+link + smoke query | OK | OK | OK | OK |
| SQLite `select1..5.test` — 470 tests (§4.5) | 470/470, result lines identical | 470/470, identical | 470/470, identical | 470/470, identical |
| SQLite family samples — 1 606 tests (§4.5) | 1 606/1 606, result lines identical | 1 606/1 606, identical | 1 606/1 606, identical | 1 606/1 606, identical |
| SQLite `session1`+`session3` — 226 tests (§4.5) | 226/226, identical | 226/226, identical | 226/226, identical | 226/226, identical |
| SQLite `speedtest1` `rtree`/`json`/`star` — steps + `.dump` hash (§4.5) | = gcc | = gcc | = gcc | = gcc |
| SQLite `speedtest1 --verify` content hash (§4.5) | = gcc | = gcc | = gcc | = gcc |

**The dot counts in this table are the suite's own progress markers, not benchmarks.**
Their purpose is the negative claim: one dot fewer would mean a test case never ran, so
the gate compares them against the reference (±16 flush-boundary slack, and the same gcc
tree measured twice to show the slack is real). `all.lua` also writes a `total time` in
CPU seconds that covers process start-up, `dofile`/I/O of ~200 test scripts, the
GC/allocator counters and (path A) the `LUA_DEBUG` internals; it is not comparable with
§5's `fib(32)` measurements and is therefore not quoted here. Performance is only
discussed in §5.

`nm -D lua | grep -c ' T lua_'` gives 103 on every path-A binary and 98 on the stock
builds; the runs above were taken on an otherwise idle host. The script's own verdict is
`RESULT: PASS（dcc 各档与 gcc -O2 在官方套件上归一化后逐行相同）` — 12/12 comparisons
identical, three self-noise checks (same gcc tree twice) identical, and the normaliser's
own `--selftest` (45 checks, half of them "must be red") passes. Every run in §4 — the
12 dcc builds and the gcc reference passes — used the dcc-built modules of §3.4, so the
`attrib.lua` loadlib block and `main.lua:251` inside the compared output are *dcc host +
dcc modules*, and the same modules also load in the gcc-built host.

### 4.1 Line counts and what counts as noise

Both compilers print the same number of lines on every path (550 / 416 / 328), and after
normalisation the effective output is 456 / 326 / 268 lines. The normaliser
(`scripts/lua_suite_norm.sh`, 20 noise categories, each with a value-range check) rewrites
random seeds, pids, temp paths, timings, GC counters and progress dots; anything not in one
of those categories stays in the diff. Two instruments keep it honest, both run by the
script: the **same gcc tree run twice** must normalise to the same text (that is how the
"progress dots attach to the start of the next printed line" and the `done (with N kills)`
classes were found), and four **injected semantic differences** must all come back red.
`--selftest` (45 cases, half of them "must be red") runs before any build.

Two readings that used to look like differences and are not:

- `total time` / per-file `time:` / GC counters / seeds / pids — normalised away.
- The `main.lua` SIGINT case reports how many signals it managed to send before the child
  died (`done (with 2 kills)` vs `… 3 kills`); upstream itself only asserts `n >= 2`
  (`main.lua:595`), so it is a race, and the normaliser keeps that same lower bound as a
  value check instead of silently swallowing the line.

### 4.2 Conditional lines (why the raw count can be 549–551)

Counts move between 549 and 551 because `all.lua` has
two conditional prints:

- `testing failed write` (`files.lua:718`) runs only when `T.nonblock` is
  non-NULL, i.e. when `ltests.c` itself is compiled with
  `LUA_USE_POSIX`/`LUA_USE_LINUX`. Without that define the failed-write and
  non-blocking `/dev/full` tests are skipped while `rc` stays 0 and the run
  still ends in `final OK !!!`.
- `Lua warning: #time difference from previous test: ±x%` (`all.lua:281`)
  runs only when the working directory still holds a `time-debug.txt` from an
  earlier run whose total time differs by more than 5%. Reusing a directory
  across builds therefore adds the line (observed: +180.1% and −66.4%).

Both are **line-count** variation only: with a clean run directory (the script copies the
test tree per tree and deletes any leftover `time*.txt`) neither appears, and §4.1's
comparison is line-for-line identical on all twelve dcc-vs-gcc pairs. dcc `-O0` and `-O3`
differ only in the temp-file name and the random seed.

### 4.3 Regression signal

`scripts/check_opt_equivalence.sh` over 410 C fixtures reports **0 OPT-DIFF**
(identical stdout and exit code at `-O0`…`-O3`). All 60 COMPILE-FAIL fixtures
are attributed — 40 have no `main`, 16 need a second translation unit, 4 are
rejected by gcc under the same flags — and none is unattributed
(`CF-UNKNOWN 0`).

### 4.4 SQLite official TCL suite (`test/quick.test`)

Re-run 2026-11-12 with `scripts/check_sqlite_suite.sh` (source tree 3.54.0, gcc `-O2`
baseline), all four levels on **one and the same compiler binary**
(`sha256 = c8b7bc82…`, recorded at the start and re-checked at the end), in one
invocation `LEVELS="0 1 2 3"`; each level's own `run-dcc-O{L}.log` is the evidence for
its row. The criterion is the **set of failing test names** relative to the baseline, so
host-dependent upstream failures are not charged to dcc. This supersedes the 2026-10-27
reading (compiler `918b9ae7`); the re-run exists because the frame-depth fix in
`src/codegen/x86_64/emit/func_setup.rs` changes code generation, which the earlier
`-shared`-only argument did not cover.

| Build | suite outcome | failing names | new vs baseline |
|---|---|---|---|
| `gcc -O2` (baseline) | completes: **5 errors out of 618 691 tests**, 1428 test files, ≈ 9 min | `recoverfault-{1,2}-oom-{persistent,transient}.430/.431`, `zipfile-25.0` | — |
| dcc `-O0` (`c8b7bc82`) | **completes**: `5 errors out of 618 691 tests` | the same five names | **0** |
| dcc `-O1` (`c8b7bc82`) | **completes**: `5 errors out of 618 691 tests` | the same five names | **0** |
| dcc `-O2` (`c8b7bc82`) | **completes**: `5 errors out of 618 691 tests` | the same five names | **0** |
| dcc `-O3` (`c8b7bc82`) | **completes**: `5 errors out of 618 691 tests` | the same five names | **0** |

Readings:

* Build and link succeed at all four levels (88 translation units each, one process
  per unit); `testfixture` is 13–16 MB and starts.
* **All four levels now run the whole suite with zero failures that the gcc baseline
  passes** (`RESULT: PASS`, identical failing-name sets). Getting there took the separate
  backend defects listed below, all of them silent miscompiles that only the upstream
  suite exposes:

  | defect | what the suite showed | state |
  |---|---|---|
  | `extern` re-declaration dropping storage; `(*fnptr[])(…)` array dimension; block-scope `extern` address | suite could not run at all (io-error injection, built-in vtabs absent) | fixed |
  | nested-call `call` site mis-aligned `rsp`; same-named `static` in sibling blocks | `rc=139` mid-suite; `trace3-1.2` wrong trace type | fixed |
  | `char buf[N] = "literal"` inside a nested block copied the *pointer* bytes | 20 `zipfile-*` failures (`zipfileGetMode` read `@` for `-rw-r--r--`) | fixed |
  | second-level subscript over a pointer field skipped a dereference | 9 `walfault-10-*`: `wrong # of entries in index` | fixed |
  | ModRM/SIB base field 101 at `mod=00` emitted *no displacement bytes* | `-O2`/`-O3` died at start-up with a wild table base | fixed |
  | two inlined copies of one jump table shared the hoisted `%rbp` base (a position-keyed static binding picked the wrong declaration) | `-O2`/`-O3` aborted after 105 report lines of `walfault.test` | fixed |
  | ASan store check received the address left in `%rax` instead of `&lhs` | `-fsanitize=address` miscompiles (was the `#[ignore]`d case) | fixed |
  | read of `[NULL+0x50]` inside `sqlite3VdbeExec` | `-O2`/`-O3` abort at `join3-1.34`, i.e. after ~250k report lines | fixed |
  | LICM hoisted a member read whose subscript is recomputed every iteration (`aQ[iMid].iFirst`) | `-O2`/`-O3` abort at `qrf01-1.10` after **396 363** report lines, wild address read inside `sqlite3_qrf_wcwidth()` | fixed |
  | frame `temp_slots` scanned no `switch` body, so `max_expr_depth` stayed at its floor and a spilled temporary overwrote a parameter slot | the §4.5 workload: `speedtest1 --testset orm` `rc=139` at step 110 at every level (`sqlite3VdbeSerialGet` read address `0x100`) | fixed |

  H was *not* a read-side defect: `-O2` mis-booked `MASKBIT(i)` = `((Bitmask)1) << i` as a
  32-bit shift, so `WhereLoop.maskSelf` came back `0xffffffff80000000` for cursor 31 and `0`
  from 32 up. The planner then opened one cursor twice and never opened the last one, so the
  generated byte code executed `OP_Column` on a `NULL` cursor. A 3-second reproducer now
  replaces the old 2-minute window: `EXPLAIN SELECT * FROM t1,…,t34` (33 tables are fine, 34
  is where it breaks). Locating it took a gcc-built SIGSEGV forensics object attached to
  the *unmodified* `-O2` fixture, `offsetof` probes as the offset source of truth, and
  three levels of `fprintf` injection. Note `DCC_BLOCKOPT=0` is *not*
  usable as a control: with the machine-level block optimizer off, `-O2` already misruns
  `index6-1.11`, so the off-arm is not a valid baseline (and the same verdict on
  `run_ssa_passes` on the big window is what hid H for a day).

* All four levels now reach the suite's own summary line (§4.4). What used to be the
  finding — “everything past the `-O2`/`-O3` abort point is unverified” — is closed by
  defect H (a 64-bit shift mis-booked as 32-bit, which made the query planner emit
  a byte code that never opened the last cursor) and defect I (LICM hoisting a
  member read whose subscript is recomputed every iteration, which made
  `sqlite3_qrf_wcwidth()` index a table with an uninitialized slot). Single-file
  reproductions of the families that used to abort:

  ```bash
  cd /tmp/sqlite_suite/run/src && su -s /bin/bash sqltest -c \
    'cd /tmp/sqlite_suite/run/src && ./testfixture test/walfault.test'   # or oserror/join3/qrf01
  ```

* Before those fixes, io-error/`Tcl_LinkVar` machinery and the built-in virtual tables were
  silently absent from dcc builds, so an *incomplete* suite run used to be the finding, not
  a regression: io-error fault injection only really executes after those were fixed.

### 4.5 SQLite: whole test files and a workload content hash

§4.4 cannot answer two questions, because its criterion is the *set of failing test
names*: does an individual upstream `.test` file pass **end to end**, and does a whole
workload produce the **same query results** as gcc? `scripts/check_sqlite_verify.sh`
answers both at all four levels: the per-file result lines are compared byte for byte
against the gcc tree's run, the workload is reduced to one content hash, and the three
testsets `--verify` cannot score get an equivalent content-hash criterion.

Three naming points first, because three terms that prompted this section do not map onto
the 3.54.0 release tree:

* `sqllogictest` is a **separate project**, not part of the SQLite release, and the
  release's `select*.test` files are **TCL** scripts — only `testfixture` runs them. The
  upstream path used here is `./testfixture test/selectN.test`.
* 3.54.0 has **no `test/evidence/`** and no `evid*.test` (`find . -iname '*evid*'` is
  empty), so that item has nothing to execute: it is absent, not skipped.
* `-verify` is `speedtest1 --verify` ("run additional verification steps"): it prints
  `Verification Hash: <steps> <sha>`, a content hash over every query result of the run.
  `main.mk`'s `src-verify` / `tclextension-verify` / `verify-source` targets are
  **source-integrity** checks and say nothing about query results.

Criteria and readings:

* `select1..5.test` = 199 + 21 + 91 + 124 + 35 = **470 tests**. Per file the script
  compares with `cmp` the `... Ok` / `... FAILED` / `... ERROR` result lines against the
  gcc tree's run, and in addition requires `rc=0` and the suite's own
  `0 errors out of N tests` with `N > 0`. The summary line alone would accept a
  wrong-but-not-crashing query; the result-line comparison does not. All 20
  file×level combinations produce **identical result lines** (198/20/90/123/34 of them),
  and every run ends with its own `0 errors out of 199/21/91/124/35 tests`.
* Twelve more complete upstream files with the same criterion, **1 606 tests**:
  `mmap1` 85, `mmap2` 153, `mmap3` 9, `wal2` 266, `wal3` 490, `wal9` 9, `where2` 107,
  `where3` 106, `join` 199, `join5` 53, `index` 121, `index2` 8. The first six are the
  `mmap`/`wal` families (`quick.test` does include them, §7); the last six are the grouped
  `where`/`join`/`index` samples. All 48 file×level combinations are identical, `0 errors`
  each.
* `ext/session` gets its own fixture, because the official one cannot run it: the
  `testfixture` recipe compiles without `-DSQLITE_ENABLE_SESSION` /
  `-DSQLITE_ENABLE_PREUPDATE_HOOK` (`main.mk` passes those only to the shell's
  `SHELL_OPT`), so `session1..9` stop at `ifcapable !session` without executing a case —
  under gcc as well. The script recompiles four TUs (`sqlite3.c`, `src/test_config.c`,
  `src/test_tclsh.c`, `ext/session/test_session.c`) with those two defines per level and
  runs `session1` (**211 tests**) and `session3` (**15**): identical result lines at all
  four levels, `0 errors out of 211/15`.
* `speedtest1` (upstream `test/speedtest1.c`, testset `main,cte,orm,fp`) must produce the
  **same hash as gcc at every level**, not merely at the same level: `-O3` may change
  speed, not results. Reference: `65386787` steps /
  `199446be4f62d73f91240c50190f13d92702ca1a3a208e39`, 47 step lines, reproduced by every
  dcc level. `json`/`star` emit only the empty hash `0 0e12171d…` under `--verify`, and
  `rtree` **even under gcc** does not finish step 111 (both binaries still running after
  150 s, `rc=124`), so those three get a criterion without `--verify`: run the testset,
  `cmp` the step sequence with the gcc run (timing stripped: **12** steps for `rtree`,
  **8** for `json`, **5** for `star`), then compare the SHA-256 of `sqlite3 .dump` of the
  final database (**`b36c386a…`**, **`5a70fefb…`**, **`248453ac…`**) and require
  `PRAGMA integrity_check = ok`. All four levels match gcc on all three, and the dump is
  produced by a gcc-built shell, not by the dcc binary under test.
* The whole run is tied to one compiler binary: `sha256(dcc) =
  c8b7bc825e85d8508654715186bf2eff8e54cf7023540398db8dc6e2f21ee00a`, recorded at the start
  and re-checked at the end, so a rebuild in the middle of a run fails the verdict instead
  of silently mixing two compilers.
* **All four levels are always covered.** If a level's `testfixture` object set is
  missing, the script builds it on the spot (`BUILD_ONLY=1`, into its own output
  directory) and then runs it; there is no "skipped level" path left in the script. The
  four columns above come from one run in which all four object sets were built on
  demand, and the session fixture recompiled its four TUs per level.
* Wall-clock in that log is not a benchmark; §5 is.

## 5. Performance (current build)

Clean rebuild per level on an otherwise idle 4-core host, `bash`'s `EPOCHREALTIME`.
The SQLite workload is the upstream **`speedtest1`** (testset `main,cte,orm,fp`, same
`--verify` run as §4.5): it is both the load and its own correctness criterion.

| Workload | gcc -O2 | gcc -O3 | dcc -O0 | dcc -O1 | dcc -O2 | dcc -O3 |
|---|---|---|---|---|---|---|
| Lua `fib(32)`, best-of-5 (`bench_lua_sqlite.sh`) | 113 ms | — | 314 ms (2.8×) | 312 ms (2.8×) | 258 ms (2.3×) | 262 ms (2.3×) |
| SQLite `speedtest1 --verify`, best-of-5 (`bench_lua_sqlite.sh`, bare flags) | 6 013 ms | 6 068 ms | — | — | 15 919 ms (2.6×) | 15 685 ms (2.6×) |

`—` = not measured: the Lua arm times all four dcc levels, the SQLite arm only `-O2` /
`-O3`; its `-O0` / `-O1` arm is covered by `check_sqlite_verify.sh` below.

Reading: `-O3` buys dcc nothing on either workload (262 ms vs 258 ms on Lua, 15 685 ms vs
15 919 ms on `speedtest1` — both within noise), so `-O2` is the level to compare, at
**2.3×** gcc on `fib(32)` and **2.6×** on `speedtest1`. On the Lua arm the level response
is `-O0` ≈ `-O1` → `-O2`; the same shape shows on SQLite under the suite's own flags,
`check_sqlite_verify.sh`'s best-of-3 reading of 2026-11-12: 23 393 / 21 871 / 21 619 /
21 806 ms for dcc `-O0`…`-O3`. Those absolute numbers are not comparable with the
bare-flag row above (different compile flags), and the gcc reference arm there is
`gcc -O2` only.

**Measurement validity.** Every level is a clean rebuild: `make` does not track
`CFLAGS`, so deleting only the binary would reuse the previous level's `.o`
files, and the gcc reference would then measure dcc code. `bench_lua_sqlite.sh`
runs `make clean` per build; the numbers above come from clean builds.

## 6. Reproduce

```bash
DCC=~/dcc/target/release/dcc
LUA=~/lua-5.5.1
TESTS=~/lua-5.5.1-tests
SQ=~/sqlite

# 0. test modules for dynamic loading (dcc-built, see §3.4)
( cd "$TESTS/libs" && rm -f all *.so && make LUA_DIR="$LUA/src" CC="$DCC" )

# 0b. only path A needs this (see §3.4): unversioned libreadline.so
mkdir -p /tmp/rllib
ln -sf /lib/x86_64-linux-gnu/libreadline.so.8 /tmp/rllib/libreadline.so

# 1. Path A: ltests, full suite (expect: final OK !!!, 550 lines)
T="$TESTS/ltests"
for o in 0 1 2 3; do
  d=/tmp/lt-$o
  rm -rf "$d" && mkdir -p "$d" && cd "$d"          # no stale .o from last run
  for f in "$LUA"/src/*.c; do
    "$DCC" -c -O$o -DLUA_USE_LINUX -I"$LUA/src" -I"$T" \
        -DLUA_USER_H='"ltests.h"' "$f" -o "$(basename "${f%.c}").o" || exit 1
  done
  # the platform define matters here too: without it T.nonblock is NULL and
  # files.lua silently skips the failed-write tests (§4.1)
  "$DCC" -c -O$o -DLUA_USE_LINUX -I"$T" -I"$LUA/src" \
      -DLUA_USER_H='"ltests.h"' "$T/ltests.c" -o ltests.o
  ar rcs liblua.a $(ls *.o | grep -v -E '^(lua|luac)\.o$')   # no lua.o/luac.o
  # -Wl,-E is required: without it libs/*.so cannot resolve lua_* symbols.
  # Check: nm -D lua | grep -c ' T lua_'  (expect 103)
  "$DCC" -o lua lua.o liblua.a -Wl,-E -ldl -lm
  s=/tmp/suite-$o
  rm -rf "$s" && cp -a "$TESTS" "$s"
  rm -f "$s"/time*.txt          # else all.lua may add the timing-diff warning
  # Absolute path is required (main.lua re-runs "$progname", also under
  # `env -i`), and the directory must be on PATH (files.lua calls bare `lua`).
  # dofile() ignores package.path, so run inside the copy, not in the tree.
  ( cd "$s" && PATH="$d:$PATH" LD_LIBRARY_PATH=/tmp/rllib "$d/lua" all.lua )
done

# 2. Path B: stock build, user mode (expect: final OK !!!, 328 lines)
for o in 0 1 2 3; do
  cd "$LUA" && make clean >/dev/null 2>&1
  make linux CC="$DCC" MYCFLAGS="-std=gnu99 -O$o" >/dev/null 2>&1 || exit 1
  d=/tmp/mkbin-$o
  rm -rf "$d" && mkdir -p "$d" && cp src/lua "$d/lua"
  s=/tmp/suiteU-$o
  rm -rf "$s" && cp -a "$TESTS" "$s"
  rm -f "$s"/time*.txt
  ( cd "$s" && PATH="$d:$PATH" "$d/lua" -e "_U=true" all.lua )
done

# 3. SQLite smoke query (the shell built from the amalgamation)
cd "$SQ"
for o in 0 1 2 3; do
  "$DCC" -O$o -o sq_$o shell.c sqlite3.c -lpthread -ldl -lm
  ./sq_$o :memory: "CREATE TABLE t(a); INSERT INTO t VALUES(1),(2),(3); \
      SELECT 'ok '||sum(a)||'/'||count(*) FROM t;"   # expect: ok 6/3
done

# 4. Upstream suites — the scripted way (serial; they share one machine-wide lock)
bash ~/dcc/scripts/setup_sqlite_suite.sh
LEVELS="0 1 2 3" TEST_TIMEOUT_S=5400 bash ~/dcc/scripts/check_sqlite_suite.sh
LEVELS="0 1 2 3" VARIANTS="ltests plain user" bash ~/dcc/scripts/check_lua_suite_diff.sh
bash ~/dcc/scripts/lua_suite_norm.sh --selftest     # 45 checks, half of them "must be red"

# 4b. SQLite: complete test files + workload content hash (§4.5)
#     Builds any missing -O1/-O3 object set itself (BUILD_ONLY), never skips a level;
#     expect RESULT: PASS and the same sha256(dcc) at the top and bottom of summary.txt.
LEVELS="0 1 2 3" bash ~/dcc/scripts/check_sqlite_verify.sh

# 4c. dcc-built shared objects: ELF structure + real loading (§3.4)
#     expect RESULT: PASS (30 checks); a missing musl toolchain is a SKIP, not a pass.
bash ~/dcc/scripts/check_shared_object.sh

# 5. Performance (§5)
ITERATIONS=5 bash ~/dcc/scripts/bench_lua_sqlite.sh
```

## 7. What is not verified

The list below is a boundary of **coverage** or of *evidence scope*, not a known break;
§4.4 and §4.5 give the evidence for everything that *has* run. The final bullets record
defects these runs did find — all fixed and pinned by regression cases.

- 19 of the tree's 1 196 `.test` files are executed as complete files with byte-identical
  result lines (§4.5) — `select1..5` (470 tests), twelve `mmap`/`wal`/`where`/`join`/
  `index` samples (1 606) and `session1`/`session3` (226) — **2 302 tests** in total. The
  remaining 1 177 files are covered only by `quick.test`'s failing-name set (§4.4), not
  file by file.
- `quick.test` is **not** a blanket exclusion of the family tests: its
  `permutations.test` list `allquicktests` *includes* `mmap1..3`, `wal2`, `wal3` and
  `wal9` (they are inside the 618 691 count), and §4.5 runs all six end to end. What it
  *does* exclude are individual heavy files — `mmap4`, `walcrash*`, `walslow`,
  `walprotocol`, `walthread`, `sessionbig*`, `sessionB` — and none of those is run here.
- `ext/session` is a separate story: the official `testfixture` is built without
  `SQLITE_ENABLE_SESSION`/`SQLITE_ENABLE_PREUPDATE_HOOK` (`main.mk` gives those only to
  the shell's `SHELL_OPT`), so `session1..9` stop at `ifcapable !session` without
  executing a case — under gcc as well. §4.5 rebuilds the four TUs that carry the switch
  and runs two of the nine as complete files; the other seven and the heavy variants are
  not run.
- `speedtest1`'s `rtree`/`json`/`star` testsets carry only the weaker §4.5 criterion
  (identical step sequence, identical final-database `.dump` hash, `integrity_check =
  ok`, `--size 10`). `--verify` cannot score them: `json`/`star` print the empty hash and
  `rtree` does not finish even under gcc. Final-content equality is not the per-query
  hash, and `rtree` at full size remains unmeasured.
- `-shared` is verified on x86-64 Linux with GNU `ld` only. `scripts/check_shared_object.sh`
  is the gate (§3.4): five modules, `-flto -shared`, `-shared --whole-archive`, real
  `dlopen`/`dlsym` (including `lib2.so`/`lib21.so`, which the Lua suite never loads).
  `-flto` is pinned as “accepted, same ELF shape, loadable”, not as “an LTO pass merges
  the module”. The musl arm is a **SKIP** — this host has no musl toolchain — so the
  README's musl claim is unverified here: an environment gap, not a pass. The built-in
  linker still cannot emit a loadable `.so` (`-fbuiltin-linker -shared` is `E0320`).
- The SQLite `-O0…-O3` row in §4 is a *smoke query* on the amalgamation shell: identical
  output on one query is not a correctness claim. SQLite correctness rests on §4.4
  (complete `quick.test`, failing-name sets) plus §4.5 (2 302 complete-file tests, one
  workload hash, three non-`--verify` testsets).
- `test/evidence/` and `sqllogictest` do not exist in this workflow: the former is absent
  from the 3.54.0 release and the latter is a separate project. Nothing in §4.5 stands in
  for them beyond the files above.
- Targets other than x86-64 Linux (`ptrdiff_t` / integer widths differ).

### Defects these runs found (all fixed)

- Two `-O2`/`-O3` miscompiles that the completed runs exposed are **fixed**:
  *H* — a 64-bit shift whose left operand is a cast literal was re-derived as `int` during
  SSA→AST rebuild, so `MASKBIT(i)` (`((Bitmask)1) << i`) was mis-booked and the query
  planner emitted a byte code that never opened the last cursor (case
  `shift_of_cast_literal_keeps_64bit_width`); *I* — LICM hoisted `aQ[iMid].iFirst` out of a
  loop because the member-read branch of the invariance test never looked at the subscript,
  so `sqlite3_qrf_wcwidth()` indexed with an uninitialized slot (case
  `licm_does_not_hoist_over_loop_variant_subscript`).
- The frame-depth defect the §4.5 workload found is **fixed**: `max_expr_depth` never
  walked `switch` bodies (nor `StmtExpr` / compound-literal initialisers), so
  `temp_slots` stayed at its floor and a spilled temporary overwrote a parameter slot —
  `speedtest1 --testset orm` died with `rc=139` at step 110 at every level (§4.4's
  table, case `switch_case_expression_depth_fits_temp_slots`).
- The ASan-only codegen defect that used to be open here is **fixed**: the store check for a
  member/array-element assignment received the address left in `%rax` by the previous
  instruction instead of `&lhs` (three causes: a load check wrapped around the store target,
  `Expr::AddrOf` silently emitting nothing for a shape it didn't recognise, and
  `gen_lvalue_addr` not seeing through the instrumentation's comma). It is now pinned by two
  running cases — `asan_aggregate_member_assign_store_addr` (formerly `#[ignore]`d) and
  `asan_array_element_member_store_addr` (three-line form: `A[0].f = 10;`).
- The `recoverfault` OOM cases fail under the reference compiler too, so they say nothing
  about dcc; `zipfile-25.0` is a host zlib behaviour difference.
- Deep-corpus evidence: paths A/B/B-full cover the Lua official suite; the SQLite side is
  the complete `quick.test` above (all four levels), the 2 302 complete-file tests of
  §4.5, and the upstream `speedtest1` workload in four configurations
  (`main,cte,orm,fp` under `--verify`, plus `rtree`/`json`/`star` without it), whose
  timing is the §5 SQLite row.
