# DCC — Lua 5.5.1 + SQLite: build & verification results

**Verdict (re-checked 2026-10-27, dcc `918b9ae7`, gcc 13.3.0).**
dcc builds Lua 5.5.1 and SQLite 3.53.4/3.54.0 from source at `-O0`…`-O3`, and both
binaries run. The **Lua official suite (`all.lua`) passes at all four levels on all
three verification paths** (`final OK !!!`, `rc=0`), and after noise normalisation its
output is **line-identical to the `gcc -O2` baseline in all 12 comparisons**
(§4). §2 shows the four levels are genuinely distinct on the command line, not four
copies of `-O2`. SQLite has been run against its **official TCL suite
(`test/quick.test`, 618 691 tests) to completion at all four levels**: every level
reaches the suite's own summary line with `5 errors out of 618 691 tests`, and the
failing-name set is **identical to the gcc baseline's** (those five also fail under gcc,
§4.4) — `RESULT: PASS`, no new failure at any level. Five complete upstream test files
(470 tests) also pass end to end with **byte-identical result lines**, and the upstream
workload (`speedtest1 --verify`) produces **the same query-result content hash as gcc at
all four levels** (§4.5). The defects these suites exposed — nine in total, eight
miscompiles (two of them only became reachable once the earlier ones were fixed) plus
one ASan-only codegen defect — are all fixed and each now has a regression case
(§4.4's table, §C.4–§C.15); what remains unverified is listed in §7, and
is about *coverage* (families `quick.test` itself excludes, other targets), not a known
break. Runtime
performance is measured separately in §5 (`scripts/bench_lua_sqlite.sh`, clean rebuild
per level) and is **not** part of this correctness verdict.

This file records final results only: build commands, result tables, the
reproduce script and the open gaps.

---

## 1. Sources and environment

| Item | Value |
|---|---|
| dcc | this repository `e8424e94`, `cargo build --release` → `target/release/dcc` |
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
**one process per translation unit**, then links once; method and caveats are in
`report/dcc-lua-sqlite-optimizer-rounds-zh.md` §C.2 (one-off product, not in git).

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

### 3.4 `libs/*.so` and `LD_LIBRARY_PATH=/tmp/rllib`

The suite's dynamic-module tests load `libs/lib1.so`, `lib11.so`, `lib2.so`,
`lib21.so`, `lib2-v2.so`. Those are built by the test tree's own makefile with
**gcc**, not dcc:

```bash
cd ~/lua-5.5.1-tests/libs
make LUA_DIR=~/lua-5.5.1/src CC=gcc      # gcc -Wall -O2 -fPIC -shared …
```

so the *host* program under test is dcc-built, while the modules are gcc-built
and leave `lua_*` undefined (12 undefined `lua_*` symbols in `lib2-v2.so`),
resolved at load time from the executable's exported symbols. In other words
the "dynamic module loading" coverage is *dcc host + gcc modules*; the `.so`
side is not a dcc artifact. Path A does load them: deleting `libs/*.so` makes
path A fail at `main.lua:251` (`lua -l lib2-v2`), while path B still passes.

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

Re-run 2026-10-27 (final compiler `918b9ae7`) with `scripts/check_lua_suite_diff.sh`
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
own `--selftest` (45 checks, half of them "must be red") passes.

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

Runs of 2026-10-27 with `scripts/check_sqlite_suite.sh` (source tree 3.54.0, gcc `-O2`
baseline), all four levels on **one and the same compiler binary** (`918b9ae7`). A
warning-only cleanup afterwards (visibility downgrades + dead-binding removal, no codegen
change) moved the binary to `33527302`; it was checked against `918b9ae7` by recompiling
9 fixtures × 4 levels and comparing the object bytes — 36/36 identical — so the readings
below still describe the current compiler. The
levels came from two consecutive invocations on one output directory — `LEVELS="2 3"`
then `LEVELS="0 1"` — so that the two levels that had been crashing were measured first
instead of queueing behind the ~50 minutes that `-O0`/`-O1` plus a second gcc baseline
take; each level's own
`run-dcc-O{L}.log` is the evidence for its row. The criterion is the **set of failing test
names** relative to the baseline, so host-dependent upstream failures are not charged
to dcc.

| Build | suite outcome | failing names | new vs baseline |
|---|---|---|---|
| `gcc -O2` (baseline) | completes: **5 errors out of 618 691 tests**, 1428 test files, ≈ 9 min | `recoverfault-{1,2}-oom-{persistent,transient}.430/.431`, `zipfile-25.0` | — |
| dcc `-O0` (`918b9ae7`) | **completes**, no summary-line difference | the same five names | **0** |
| dcc `-O1` (`918b9ae7`) | **completes**, no summary-line difference | the same five names | **0** |
| dcc `-O2` (`918b9ae7`) | **completes**: `5 errors out of 618 691 tests` (first level-2 run ever to reach the summary line) | the same five names | **0** |
| dcc `-O3` (`918b9ae7`) | **completes**: `5 errors out of 618 691 tests` | the same five names | **0** |

Readings:

* Build and link succeed at all four levels (88 translation units each, one process
  per unit); `testfixture` is 13 MB and starts.
* **All four levels now run the whole suite with zero failures that the gcc baseline
  passes** (`RESULT: PASS`, identical failing-name sets). Getting there took the separate
  backend defects listed below, all of them silent miscompiles that only the upstream
  suite exposes:

  | defect | what the suite showed | state |
  |---|---|---|
  | `extern` re-declaration dropping storage; `(*fnptr[])(…)` array dimension; block-scope `extern` address | suite could not run at all (io-error injection, built-in vtabs absent) | fixed (§C.4) |
  | nested-call `call` site mis-aligned `rsp`; same-named `static` in sibling blocks | `rc=139` mid-suite; `trace3-1.2` wrong trace type | fixed (§C.9) |
  | `char buf[N] = "literal"` inside a nested block copied the *pointer* bytes | 20 `zipfile-*` failures (`zipfileGetMode` read `@` for `-rw-r--r--`) | fixed (§C.10.1) |
  | second-level subscript over a pointer field skipped a dereference | 9 `walfault-10-*`: `wrong # of entries in index` | fixed (§C.10.2) |
  | ModRM/SIB base field 101 at `mod=00` emitted *no displacement bytes* | `-O2`/`-O3` died at start-up with a wild table base | fixed (§C.10.3) |
  | two inlined copies of one jump table shared the hoisted `%rbp` base (a position-keyed static binding picked the wrong declaration) | `-O2`/`-O3` aborted after 105 report lines of `walfault.test` | fixed (§C.11) |
  | ASan store check received the address left in `%rax` instead of `&lhs` | `-fsanitize=address` miscompiles (was the `#[ignore]`d case) | fixed (§C.12) |
  | read of `[NULL+0x50]` inside `sqlite3VdbeExec` | `-O2`/`-O3` abort at `join3-1.34`, i.e. after ~250k report lines | fixed (§C.14) |
  | LICM hoisted a member read whose subscript is recomputed every iteration (`aQ[iMid].iFirst`) | `-O2`/`-O3` abort at `qrf01-1.10` after **396 363** report lines, wild address read inside `sqlite3_qrf_wcwidth()` | fixed (§C.15) |

  H was *not* a read-side defect: `-O2` mis-booked `MASKBIT(i)` = `((Bitmask)1) << i` as a
  32-bit shift, so `WhereLoop.maskSelf` came back `0xffffffff80000000` for cursor 31 and `0`
  from 32 up. The planner then opened one cursor twice and never opened the last one, so the
  generated byte code executed `OP_Column` on a `NULL` cursor. A 3-second reproducer now
  replaces the old 2-minute window: `EXPLAIN SELECT * FROM t1,…,t34` (33 tables are fine, 34
  is where it breaks). The evidence chain — a gcc-built SIGSEGV forensics object attached to
  the *unmodified* `-O2` fixture, `offsetof` probes as the offset source of truth, and
  three levels of `fprintf` injection — is in §C.14. Note `DCC_BLOCKOPT=0` is *not*
  usable as a control: with the machine-level block optimizer off, `-O2` already misruns
  `index6-1.11`, so the off-arm is not a valid baseline (and the same verdict on
  `run_ssa_passes` on the big window is what hid H for a day — see §C.14 lesson 2).

* All four levels now reach the suite's own summary line (§4.4). What used to be the
  finding — “everything past the `-O2`/`-O3` abort point is unverified” — is closed by
  defect H (§C.14, a 64-bit shift mis-booked as 32-bit, which made the query planner emit
  a byte code that never opened the last cursor) and defect I (§C.15, LICM hoisting a
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
  Root causes and file/line sites are in
  `report/dcc-lua-sqlite-optimizer-rounds-zh.md` §C.4 / §C.9 / §C.10 / §C.11 (one-off, not in git).

### 4.5 SQLite: whole test files and a workload content hash

§4.4 cannot answer two questions, because its criterion is the *set of failing test
names*: does an individual upstream `.test` file pass **end to end**, and does a whole
workload produce the **same query results** as gcc? `scripts/check_sqlite_verify.sh`
answers both at all four levels: the per-file result lines are compared byte for byte
against the gcc tree's run, and the workload is reduced to one content hash.

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
* `speedtest1` (upstream `test/speedtest1.c`, testset `main,cte,orm,fp`) must produce the
  **same hash as gcc at every level**, not merely at the same level: `-O3` may change
  speed, not results. Reference: `65386787` steps /
  `199446be4f62d73f91240c50190f13d92702ca1a3a208e39`, 47 step lines, reproduced by every
  dcc level. `json`/`star` are excluded because they emit only the empty hash
  `0 0e12171d…`; `rtree` is excluded because **even the gcc reference** does not finish
  step 111 under `--verify` (both binaries still running after 150 s, `rc=124`) — an
  upstream load property, not a dcc difference.
* The whole run is tied to one compiler binary: `sha256(dcc) =
  918b9ae722b3e230be45d95cba58858badea68c8ae00e93e7368a52103bb9be2`, recorded at the start
  and re-checked at the end, so a rebuild in the middle of a run fails the verdict instead
  of silently mixing two compilers.
* **All four levels are always covered.** If a level's `testfixture` object set is
  missing, the script builds it on the spot (`BUILD_ONLY=1`, into its own output
  directory) and then runs it; there is no "skipped level" path left in the script. The
  four columns above come from one run in which `-O2` and `-O3` were built on demand.
* Wall-clock in that log is not a benchmark; §5 is.

## 5. Performance (current build)

Clean rebuild per level on an otherwise idle 4-core host, `bash`'s `EPOCHREALTIME`.
The SQLite workload is now the upstream **`speedtest1`** (testset `main,cte,orm,fp`,
same `--verify` run as §4.5) instead of the hand-written 20 000-statement script used by
earlier revisions: `speedtest1` is both the load and its own correctness criterion, while
the old script could only be timed. The two rows come from different measurements and
are labelled as such:

| Workload | gcc -O2 | gcc -O3 | dcc -O0 | dcc -O1 | dcc -O2 | dcc -O3 |
|---|---|---|---|---|---|---|
| Lua `fib(32)`, best-of-5 (`bench_lua_sqlite.sh`) | 113 ms | — | 308 ms (2.7×) | 308 ms (2.7×) | 268 ms (2.4×) | 266 ms (2.4×) |
| SQLite `speedtest1 --verify`, best-of-3 (`check_sqlite_verify.sh`) | 7 080 ms | — | 25 100 ms (3.5×) | 24 951 ms (3.5×) | 19 897 ms (2.8×) | 19 800 ms (2.8×) |

`—` = not a column of this table: `bench_lua_sqlite.sh` also builds a `gcc -O3` reference
for the SQLite arm (5 916 / 6 002 ms vs dcc `-O2`/`-O3` 17 789 / 17 881 ms, i.e. **3.0×**
with *bare* flags), but that pair is not comparable with the row above — the row quotes
`check_sqlite_verify.sh`, which reuses the suite's own compile flags and whose reference
arm is `gcc -O2` only.

Reading: `-O3` buys dcc nothing on either workload (266 ms vs 268 ms on Lua, 19 800 ms vs
19 897 ms on `speedtest1` — both within noise), so `-O2` is the level to compare, at
**2.4×** gcc on `fib(32)` and **2.8×** on `speedtest1`. For both workloads the level
response is `-O0` ≈ `-O1` → `-O2`; dcc's `-O1` is essentially the `-O0` code here.

The SQLite row is measured with the suite's own compile flags (the same objects §4.5
runs), not with a bare `gcc -O2 sqlite3.c`: absolute milliseconds are therefore not
comparable with a plain amalgamation build, while the gcc/dcc ratio is measured under
identical flags.

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

# 0. test modules for dynamic loading (gcc-built on purpose, see §3.4)
( cd "$TESTS/libs" && rm -f all *.so && make LUA_DIR="$LUA/src" CC=gcc )

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

# 5. Performance (§5)
ITERATIONS=5 bash ~/dcc/scripts/bench_lua_sqlite.sh
```

## 7. What is not verified

- SQLite's upstream suite **completes at all four levels** under dcc, with failing-name
  sets identical to the gcc baseline (§4.4: `5 errors out of 618 691 tests`, the four
  `recoverfault` OOM names plus `zipfile-25.0`, which also fail under gcc). What is
  *still* not covered is what `quick.test` itself excludes — the heavier
  `mmap*.test` / `session*.test` / `wal*.test` families are not run by it at any level,
  for dcc or for gcc.
- SQLite's `-O0…-O3` row in §4 is a *smoke query* on the amalgamation shell: identical
  output on one query is not a correctness claim. The correctness evidence for SQLite is
  §4.4 (complete run at all four levels) plus §4.5 (complete, but only 5 of the tree's
  1196 `.test` files and one workload).
- `test/evidence/` and `sqllogictest` do not exist in this workflow: the former is absent
  from the 3.54.0 release and the latter is a separate project. Nothing in §4.5 stands in
  for them beyond `select1..5.test` (§4.5 lists the three naming points).
- `speedtest1`'s `rtree` testset and the `json`/`star` testsets carry no criterion here:
  `rtree` does not finish under `--verify` even with gcc, and the other two emit only the
  empty hash `0 0e12171d…`. The 470 upstream tests are 5 of 1196 files.
- Two `-O2`/`-O3` miscompiles that the completed runs exposed are **fixed**:
  *H* — a 64-bit shift whose left operand is a cast literal was re-derived as `int` during
  SSA→AST rebuild, so `MASKBIT(i)` (`((Bitmask)1) << i`) was mis-booked and the query
  planner emitted a byte code that never opened the last cursor (§C.14, case
  `shift_of_cast_literal_keeps_64bit_width`); *I* — LICM hoisted `aQ[iMid].iFirst` out of a
  loop because the member-read branch of the invariance test never looked at the subscript,
  so `sqlite3_qrf_wcwidth()` indexed with an uninitialized slot (§C.15, case
  `licm_does_not_hoist_over_loop_variant_subscript`).
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
  the complete `quick.test` above (all four levels), the 470 upstream tests of §4.5, and
  one upstream workload (`speedtest1`), whose timing is the §5 SQLite row.
- Targets other than x86-64 Linux (`ptrdiff_t` / integer widths differ).
- All Lua/SQLite links here go through the host cc; `-fbuiltin-linker` was not
  exercised with `-Wl,-E` / `--export-dynamic`.
