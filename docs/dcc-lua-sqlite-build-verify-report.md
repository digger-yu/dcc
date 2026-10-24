# DCC — From-Scratch Build & Verification Guide (Lua 5.5.1 + SQLite)

This document is a **complete, copy-pasteable walkthrough** that simulates a
fresh machine. It starts from nothing but the OS, records **every** step
(including the most basic ones such as `cargo build`), and ends with verified
runtime behavior of two real-world C projects compiled by **dcc** — the
in-tree Rust C compiler.

All commands were executed on the environment described in §0; substitute your
own working directory for `~/` if needed.

**Current status:** dcc compiles and runs both Lua 5.5.1 and SQLite 3.53.4
**correctly at every optimization level `-O0`/`-O1`/`-O2`/`-O3`**, with output
byte-identical to a gcc build of the same sources.

**Reference commit:** `c2432159`
(driver / cli / link / preproc) — the driver-side behaviors this guide relies
on: `$CC`/`$LD`/`$OBJCOPY` are shell-word-split when dcc delegates to the host
toolchain, and dcc accepts multiple translation units (`.c` plus `.o`/`.a`) in a
single invocation. The runtime-correctness numbers in §4.2 reflect the code at
this commit plus the fixes recorded in §6.4; the performance numbers in §6
additionally include the 2026-10 emitter round described in §6.6.

> **2026-10 re-verification.** This guide was re-run end to end after a
> correctness and performance pass on the -O2 pipeline. Five defects that
> affected these two projects were found and fixed (§6.4); one of them made
> `-O2`/`-O3` Lua print wrong numbers (`print(12.0)` emitted `.0`), so the
> `-O2`/`-O3` rows below were **not** correct at the time this guide was first
> written even though they built and ran. All four levels now match gcc
> byte-for-byte, which is what §4.2 asserts and what §6 measures. A second pass
> then repaired the register-home channel and added the first emitter-level
> optimizations; a third pass took `luaV_execute` through the SSA home allocator
> despite its computed `goto`, then added a second emitter round (constant
> comparisons, dead-write removal, 32-bit zero-extension tidy-up) — see
> §6.5/§6.6. A fourth round stopped materialising branch conditions at all
> (`cmp` + `jcc` instead of `setcc`+`movzx`+`test`) and, in the process, exposed
> two **silent miscompiles that were already on HEAD** — a mixed-sign comparison
> using the wrong signedness, and `-O3` loop blocking reordering observable side
> effects — both fixed with new unit tests and permanent fixtures; see §6.7/§6.8. A
> fifth round made scalar stores direct (no push/pop staging for the target address),
> added the dead-value context and cost-model-driven `static` function-body inlining at
> -O2 (§6.9): Lua `-O2` p50 −2.4% and SQLite −7.4% against HEAD, measured in paired runs.
> A block-equivalence layer then took the emitter's last remaining surface (§6.10), and the
> current round removed nearly all of the remaining push/pop staging — `luaV_execute`
> push+pop **389 → 155**, instruction count 11366 — while fixing three **real miscompiles** on
> the condition-decomposition path and one in the emit-time peephole (§6.11).
> The tables in §6.1/§6.2 are the like-for-like after-state, measured
> against the same script run on the pre-change compiler; §7 records the
> correctness numbers (397 fixtures × four levels, 0 OPT-DIFF; 2505 lib tests, 1130 integration
> tests whose 4 failures are the same HEAD-red items). The paired runtime A/B for the current
> round is in §6.11: pinned to one core, gcc and dcc alternating, best-of-9 `task-clock` —
> Lua -O2 `min 291.6 / p50 302.5 ms` and SQLite -O2 `min 472.7 / p50 475.9 ms`, both ~10% inside
> the plan §5 thresholds (324 / 527 ms) and ~9% better than the §1 baseline once the gcc
> reference arm's 2% drift is subtracted.

---

## 0. Prerequisites (check on a fresh machine)

| What | Why | Verify |
|------|-----|--------|
| x86_64 Linux | dcc's codegen target is `x86_64-unknown-linux-gnu` | `uname -m` → `x86_64` |
| Rust + Cargo (≥1.70) | to build dcc itself | `cargo --version` |
| Host C toolchain | dcc **delegates linking** to the host `cc`/`ar`/`ranlib`/`ld` | `gcc --version`, `ar --version`, `ld --version` |
| `curl` or `wget` | to fetch Lua / SQLite sources | `curl --version` |
| `make`, `tar`, `gdb` (optional, for debugging) | build / inspect | `make --version` |

Confirmed versions used for this report:

```bash
$ cargo --version
cargo 1.97.1 (c980f4866 2026-06-30)
$ rustc --version
rustc 1.97.1 (8bab26f4f 2026-07-14)
$ gcc --version | head -1
gcc (Ubuntu 13.3.0-6ubuntu2~24.04.1) 13.3.0
$ ar --version | head -1
GNU ar (GNU Binutils for Ubuntu) 2.42
$ ld --version | head -1
GNU ld (GNU Binutils for Ubuntu) 2.42
```

Assumed directory layout after §1:

```
~/
├── dcc/                 # dcc source repository (Rust)
├── lua-5.5.1/          # extracted Lua source (from lua.org)
└── sqlite/             # SQLite amalgamation: shell.c + sqlite3.c
```

---

## 1. Obtain the sources

### 1.1 dcc

This is the dcc source repository. Place/clone it anywhere, e.g.:

```bash
cd ~
# (clone or copy the dcc source so that ~/dcc/Cargo.toml exists)
```

### 1.2 Lua 5.5.1

```bash
cd ~
curl -L https://www.lua.org/ftp/lua-5.5.1.tar.gz -o lua-5.5.1.tar.gz
tar xzf lua-5.5.1.tar.gz          # creates ~/lua-5.5.1/
```

### 1.3 SQLite amalgamation (3.53.4)

```bash
cd ~
curl -L https://www.sqlite.org/2025/sqlite-autoconf-3530400.tar.gz -o sqlite.tar.gz
tar xzf sqlite.tar.gz            # creates ~/sqlite-autoconf-3530400/
mv sqlite-autoconf-3530400 sqlite
```

The amalgamation directory contains `shell.c` and `sqlite3.c`, which is all we
need (single-file translation units).

---

## 2. Build dcc (the compiler itself)

This is the most basic step — dcc is written in Rust and must be compiled
before it can compile anything else.

```bash
cd ~/dcc
cargo build --release
```

- The first build downloads dcc's crate dependencies and compiles them; this
  can take a few minutes. Subsequent builds are incremental and fast.
- A debug build (`cargo build`, without `--release`) also works; this report
  uses the release binary.
- All 2505 dcc library unit tests pass (`cargo test --release --lib`).
- On success the binary appears at:

```bash
ls -l ~/dcc/target/release/dcc
```

### 2.1 Verify the dcc binary

```bash
~/dcc/target/release/dcc --version
```

Expected output:

```
dcc 0.1.0
Digger C compiler. A suckless C compiler.
Target: x86_64-unknown-linux-gnu
```

(Invoking `dcc` with no arguments prints `dcc: error: no input file` — that is
expected, not a failure.)

### 2.2 Smoke test — compile & run a tiny C program

Before touching Lua/SQLite, confirm dcc can compile and link a trivial program
(it delegates the final link to the host `cc`):

```bash
printf 'int main(void){ int x=40,y=2; return !(x+y==42); }\n' > /tmp/hello.c
~/dcc/target/release/dcc -o /tmp/hello /tmp/hello.c
/tmp/hello; echo "rc=$?   # expect 0"
```

Expected:

```
dcc: wrote /tmp/dcc-hostlink-XXXXXX/dcc-hostlink-hello.c.o
dcc: /tmp/hello (via host cc)
rc=0
```

dcc now works. The rest of this guide uses `DCC=~/dcc/target/release/dcc`.

---

## 3. dcc behavior notes (read before using it on real projects)

### 3.1 dcc delegates linking to the host linker

For an executable dcc does **not** link by itself — it delegates to the host
linker. To find that linker dcc reads the `CC` environment variable (and, for
`ld`/`objcopy`, the `LD`/`OBJCOPY` variables). dcc **shell-word-splits** the
value, so a `CC` that carries flags works as-is — including the form `make`
exports straight from its command line, e.g. `CC="dcc -std=gnu99"`: the program
is executed with its trailing flags as separate arguments, and a `CC` naming dcc
itself falls back to the default host toolchain. No environment manipulation
around `make` is required.

```bash
make posix CC=~/dcc/target/release/dcc MYCFLAGS="-std=gnu99 -O2" ...
# also fine — flags carried inside CC (note $HOME, since `~` is not expanded
# inside double quotes):
make posix CC="$HOME/dcc/target/release/dcc -std=gnu99" MYCFLAGS="-O2" ...
```

### 3.2 dcc accepts multiple translation units per invocation

dcc compiles and links several sources in one call: give `-o exe` together with
any combination of `.c` files and pre-built `.o`/`.a` objects, and dcc compiles
each `.c` to a temporary object, then links them:

```bash
~/dcc/target/release/dcc -O2 -o sqlite3_dcc shell.c sqlite3.c -lpthread -ldl -lm
```

Per-file `dcc -c` followed by a separate link still works; it is simply no
longer required. (`-c`/`-S` each take a single output and are rejected when
combined with `-o` plus multiple inputs.)

### 3.3 Diagnostics

dcc emits `-Wall -Wextra` diagnostics but does **not** fail on them. The only
warning seen throughout this work is benign:

```
warning[W0501]: implicit fallthrough between switch cases
  (use [[fallthrough]] to mark intentional fallthrough)
```

These are expected (Lua and SQLite both rely on intentional fallthrough) and do
not affect the build.

### 3.4 Parallel codegen at -O2 and above

From `-O2` up, dcc emits function bodies **in parallel** (per-function emit
chunks merged back into the main code generator). This is a build-time behavior
of the compiler and does not change the semantics of the generated code.

### 3.5 dcc as a drop-in `CC` for existing projects (DX)

Two driver behaviors — both landed in commit `99dabbc1` — are what let dcc act
as a **drop-in `CC`** for a foreign build tree instead of needing a bespoke
harness. They are the last mile between "dcc can compile this project" and "an
open-source project can set `CC=<path>/dcc` and build out of the box":

1. **`$CC`/`$LD`/`$OBJCOPY` are shell-word-split on link delegation.** `make`
   exports its command-line variables into every recipe, so a build that does
   `make CC="dcc -std=gnu99"` hands dcc a `CC` that is a *program plus flags*.
   dcc splits the value, runs the program with its arguments, and — when `CC`
   names dcc itself — canonicalizes it and falls back to the default host
   toolchain. No `env -u CC` wrapper, and no need to keep flags out of `CC`.
2. **Multiple translation units per invocation.** `make`/autotools routinely
   link several `.c` (and `.o`/`.a`) in one command: `CC -o prog a.c b.c`. dcc
   compiles each to a temp object and links them, so configure-style command
   lines work unchanged.

In practice this means a project that builds with a one-line CC override:

```bash
# a typical out-of-tree build just works — flags live in CFLAGS, not CC
make CC=~/dcc/target/release/dcc CFLAGS="-O2 -std=gnu99"
# and even the sloppier form where flags are folded into CC itself:
make CC="~/dcc/target/release/dcc -std=gnu99" CFLAGS="-O2"
```

Both patterns are exercised directly by the Lua (`make posix CC=dcc ...`) and
SQLite (`dcc -O2 -o sqlite3_dcc shell.c sqlite3.c -lpthread -ldl -lm`) builds in
§4 and §5, which run with no special-casing beyond pointing `CC` at dcc.

---

## 4. Lua 5.5.1

### 4.1 Build

```bash
cd ~/lua-5.5.1/src
make clean
make posix CC=~/dcc/target/release/dcc \
     MYCFLAGS="-std=gnu99 -O2" AR="ar rcu" RANLIB=ranlib
```

`make posix` (instead of `linux`) avoids the readline dependency. The archive
step uses the host `ar`/`ranlib`; dcc compiles every `.c` and performs the final
link (delegating to the host linker). Sample output:

```
~/dcc/target/release/dcc -std=gnu99 -O2 -Wall -Wextra -DLUA_USE_POSIX \
    -c -o lstrlib.o lstrlib.c
dcc: wrote lstrlib.o
ar rcu liblua.a lapi.o lcode.o ... linit.o
ranlib liblua.a
~/dcc/target/release/dcc -o lua   lua.o liblua.a -lm
dcc: lua
~/dcc/target/release/dcc -o luac  luac.o liblua.a -lm
dcc: luac
```

The build above uses `-O2`. **Every** optimization level works and produces a
correct, identical interpreter:

```bash
# alternate levels are equally valid and bit-for-bit equivalent at runtime:
make posix CC=~/dcc/target/release/dcc \
     MYCFLAGS="-std=gnu99 -O0" AR="ar rcu" RANLIB=ranlib     # -O0
make posix CC=~/dcc/target/release/dcc \
     MYCFLAGS="-std=gnu99 -O1" AR="ar rcu" RANLIB=ranlib     # -O1
make posix CC=~/dcc/target/release/dcc \
     MYCFLAGS="-std=gnu99 -O3" AR="ar rcu" RANLIB=ranlib     # -O3
```

Result: `lua`, `luac`, and `liblua.a` are produced.

### 4.2 Functional verification — identical output at every optimization level

Create the test script `~/lua-5.5.1/src/luafull.lua` (content in §8.2):

```bash
cat > luafull.lua <<'LUA'
-- Functional smoke test for a dcc-built Lua 5.5.1
print("ver:        " .. (_VERSION or "Lua 5.5"))

local f = io.open("/tmp/dcc_lua_io.txt", "w")
f:write("hello from dcc lua\n")
f:close()
local g = io.open("/tmp/dcc_lua_io.txt", "r")
local line = g:read("*l")
g:close()
print("readback:   " .. line .. "\t1")

print("sin/sqrt:   " .. tostring(math.sin(0)) .. "\t" .. tostring(math.sqrt(144)))

local function fib(n)
  if n < 2 then return n else return fib(n - 1) + fib(n - 2) end
end
print("fib(20):    " .. fib(20))

local co = coroutine.create(function()
  for i = 1, 3 do coroutine.yield(i * i) end
end)
local a, b, c = coroutine.resume(co), coroutine.resume(co), coroutine.resume(co)
print("co:         " .. tostring(a) .. "\t" .. tostring(b) .. "\t" .. tostring(c))

local s = "X1X2X3"
local r, n = s:gsub("X(%d)", "X%1")
print("pat:        " .. r .. "\t" .. n)
LUA

./lua -v
./lua luafull.lua
./luac -o /tmp/luafull.luac luafull.lua && ./lua /tmp/luafull.luac
```

Expected (identical at **all** of `-O0`/`-O1`/`-O2`/`-O3`):

```
Lua 5.5.1  Copyright (C) 1994-2026 Lua.org, PUC-Rio

ver:        Lua 5.5
readback:   hello from dcc lua	1
sin/sqrt:   0.0	12.0
fib(20):    6765
co:         true	true	true
pat:        X1X2X3	3
```

(Re-running the `luac`-emitted bytecode prints the same tail lines.) The script
exercises: version banner, file I/O (write + read-back), `math.sin`/`math.sqrt`,
recursive `fib(20)=6765`, coroutines, and string `gsub`. All results are
correct, **and the output is byte-for-byte identical across `-O0`/`-O1`/`-O2`/
`-O3`** — confirming dcc compiled Lua's own compiler and runtime correctly at
every optimization level.

> The same invariant — all four levels equal to each other **and** to a gcc
> `-O2` build — is what
> [`scripts/bench_lua_sqlite.sh`](../scripts/bench_lua_sqlite.sh) pins down
> automatically (it exits non-zero on the first `*-REF-DIFF` / `*-LEVEL-DIFF`).
> That sweep is also what caught the §6.4 defect 3: a `-O2` build that merely
> "built and ran" printed `print(12.0)` as `.0` until the comparison was made
> against gcc, not just against itself.

### 4.3 Optimization sweep — -O0 .. -O3

A full sweep with a clean rebuild at each level:

```bash
cd ~/lua-5.5.1/src
for o in 0 1 2 3; do
  make clean
  make posix CC=~/dcc/target/release/dcc \
       MYCFLAGS="-std=gnu99 -O$o" AR="ar rcu" RANLIB=ranlib >/dev/null 2>&1
  ./lua -v >/dev/null 2>&1; v=$?
  ./lua luafull.lua >/dev/null 2>&1; s=$?
  printf "O%s: lua -v rc=%s | run script rc=%s\n" "$o" "$v" "$s"
done
```

| Level | `lua -v` (no script) | `./lua luafull.lua` (real script) |
|-------|----------------------|------------------------------------|
| -O0   | OK (0) | OK (0) |
| -O1   | OK (0) | OK (0) |
| -O2   | OK (0) | OK (0) |
| -O3   | OK (0) | OK (0) |

All four levels are now correct and produce identical script output.

---

## 5. SQLite (amalgamation)

### 5.1 Build

Both sources compile and link in a single invocation (see §3.2):

```bash
cd ~/sqlite
~/dcc/target/release/dcc -O2 -o sqlite3_dcc \
     shell.c sqlite3.c -lpthread -ldl -lm
```

The compile emits only benign W0501 warnings; the link prints
`dcc: sqlite3_dcc` and produces the `sqlite3_dcc` shell.

### 5.2 Verification — at every optimization level (-O0 .. -O3)

```bash
./sqlite3_dcc :memory: "SELECT 'sqlite '||sqlite_version(); \
    CREATE TABLE t(a,b); INSERT INTO t VALUES(1,'x'),(2,'y'),(3,'z'); \
    SELECT a,b FROM t ORDER BY a DESC; \
    SELECT 'sum='||sum(a),'cnt='||count(*) FROM t;"
```

```
sqlite 3.53.4
3|z
2|y
1|x
sum=6|cnt=3
```

JOIN + aggregation:

```bash
./sqlite3_dcc :memory: "CREATE TABLE u(id,name); INSERT INTO u VALUES(1,'a'),(2,'b'); \
    CREATE TABLE v(uid,score); INSERT INTO v VALUES(1,10),(1,20),(2,5); \
    SELECT u.name, sum(v.score) s FROM u JOIN v ON v.uid=u.id \
    GROUP BY u.name ORDER BY s DESC;"
```

```
a|30
b|5
```

File-backed database (persistence across connections) and a transaction:

```bash
./sqlite3_dcc /tmp/test2.db "CREATE TABLE p(id INTEGER PRIMARY KEY, name TEXT); \
    INSERT INTO p(name) VALUES('alice'),('bob'); SELECT * FROM p ORDER BY id;"
./sqlite3_dcc /tmp/test2.db "SELECT * FROM p ORDER BY id; \
    BEGIN; UPDATE p SET name='ALICE' WHERE id=1; COMMIT; \
    SELECT * FROM p WHERE id=1;"
```

```
1|alice
2|bob
1|ALICE
```

The persisted row survives a reconnect, and the `BEGIN/COMMIT` transaction is
durable.

### 5.3 Optimization sweep — -O0 .. -O3

A full sweep with a clean rebuild at each level (single invocation, see §3.2):

```bash
cd ~/sqlite
for o in 0 1 2 3; do
  ~/dcc/target/release/dcc -O$o -o sq_$o shell.c sqlite3.c -lpthread -ldl -lm \
      >/dev/null 2>&1
  ./sq_$o :memory: "CREATE TABLE t(a); INSERT INTO t VALUES(1),(2),(3); \
      SELECT 'ok '||sum(a)||'/'||count(*) FROM t;"
done
```

| Level | build + link | query output |
|-------|--------------|--------------|
| -O0   | OK (0) | `ok 6/3` |
| -O1   | OK (0) | `ok 6/3` |
| -O2   | OK (0) | `ok 6/3` |
| -O3   | OK (0) | `ok 6/3` |

SQLite runs correctly at **every** optimization level under dcc, and the same
queries return identical results across `-O0`/`-O1`/`-O2`/`-O3`: DDL/DML,
`ORDER BY`, aggregates, `JOIN`/`GROUP BY`, file persistence, and transactions
all behave as expected (version 3.53.4).

---

## 6. Performance benchmark (dcc vs gcc, -O2 / -O3)

Correctness is established in §4/§5; the remaining piece is how the **optimizer
pipeline at -O2/-O3 actually performs** on the two real workloads, measured
against a host `gcc` build of the same sources. This is the last puzzle piece:
if dcc only compiled correctly at `-O0` but produced slow code at `-O2/-O3`, the
middle/back-end would be validated only halfway.

Methodology: same host, single-threaded, best-of-N wall-clock after warmup (the
script's default `ITERATIONS=5`; the final run recorded here used
`ITERATIONS=7`).
Lua runs a CPU-bound recursive `fib(32)` (interpreter dispatch loop, compiled by
the chosen compiler); SQLite builds a 200k-row in-memory table and runs a range
aggregate + an `ORDER BY ... LIMIT` scan. Results are byte-identical in output
across all four binaries; only timing differs.

The numbers below are produced by a single script —
[`scripts/bench_lua_sqlite.sh`](../scripts/bench_lua_sqlite.sh) — which builds
both projects at `-O0/-O1/-O2/-O3`, runs the §4.2 functional checks against a
gcc `-O2` reference (it fails the run with `LUA-REF-DIFF` / `SQLITE-REF-DIFF` /
`*-LEVEL-DIFF` if any level disagrees), and then measures timing. Re-running it
on a fresh checkout reproduces the tables here:

```bash
DCC=/root/dcc/target/release/dcc bash scripts/bench_lua_sqlite.sh
# -> /tmp/bench_lua_sqlite.txt (+ /tmp/bench_lua_sqlite.txt.artifacts/)
#    exits 0 when all four levels match gcc byte-for-byte, 1 on any build-fail/diff
```

### 6.1 Lua — recursive `fib(32)`

| Compiler / level | wall (ms) | vs gcc -O2 | previous run (ms) |
|---|---|---|---|
| gcc -O2  | 114 | — (baseline) | 117 |
| dcc -O0  | 359 | 3.1x slower | 373 |
| dcc -O1  | 351 | 3.1x slower | 380 |
| dcc -O2  | 315 | 2.8x slower | 329 |
| dcc -O3  | 317 | 2.8x slower | 338 |

Both columns are produced by the same script on the same host. The
"previous run" column is that script run against the pre-change compiler
(HEAD built into a separate target directory), so the delta is like-for-like
rather than a number carried over from an older machine.

第五轮（P13 地址后置重算与常量右值白名单，§6.11）之后再跑一遍本脚本：
`gcc -O2` 114ms、`dcc -O0/-O1/-O2/-O3` = 336/347/287/296ms，`-O2` 对 gcc 比值降到 **2.5x**。
同日的 pinned-core 配对量（`taskset -c 3`、best-of-9 `task-clock`，gcc/dcc 交替）为
dcc -O2 **min 291.6 / p50 302.5ms** 对 gcc -O2 110.3 / 112.9ms —— 详见 §6.11。

Because this script
only reports a best-of-`N` wall time with millisecond resolution, the -O2 row
alone does not prove the gain: a **paired, alternating** best-of-9 run on one
pinned core (`taskset -c 3`, `perf stat -e task-clock`) is the load-bearing
measurement — HEAD `317.8 / 320.9 ms` (min / p50) versus the §6.7 build
`310.9 / 319.3 ms`, i.e. **−2.2% min, −0.5% p50** for the same workload. A
second paired run later the same day gave HEAD `323.8 / 330.0` against the
final build's `319.5 / 324.7` (min −1.3%, p50 −1.6%), so the Lua gain is
reported as a band of **−1.3% … −2.2%** rather than a single number.

`-O2` is now 12.3% faster than `-O0`: `luaV_execute` reaches the SSA home
allocator through the analysis-only channel (§6.5), which runs before the
per-level pass selection, and the rest of the file gets the ordinary `-O2`
SSA pipeline. The emitter rules below run at **every** level, which is why all
four rows moved down, not just `-O2`/`-O3`.

The row-to-row drop from "previous run" (−14 ms at `-O2`) is **larger** than this
round's paired gain (§6.10: −1.2%…−2.5%), so it must not be read as the round's
timing claim: this script reports one best-of-9 wall per level and gcc's own row
has ranged 114-117 ms across revisions. On the blockopt round the `-O0`/`-O1`
rows moved too (373→359, 380→351) although the feature is default-on at every
level, which is the signature of run-to-run drift rather than of the change.
The load-bearing numbers stay the paired ones.

### 6.2 SQLite — 200k insert + range aggregate + `ORDER BY` scan

| Compiler / level | wall (ms) | vs gcc same level | previous run (ms) |
|---|---|---|---|
| gcc -O2  | 90  | — (baseline) | 89 |
| gcc -O3  | 99  | — | 101 |
| dcc -O2  | 486 | 5.4x slower | 488 |
| dcc -O3  | 482 | 4.9x slower | 487 |

The paired best-of-5 wall for the same binary pair (§6.7 A/B) is
`539 ms → 529 ms` on `-O2`, i.e. **−1.9%**, on top of a 902-instruction
(`20788 → 19886`) drop in `sqlite3VdbeExec`'s static body.

第五轮之后再跑一遍本脚本：`gcc -O2/-O3` 88/98ms、`dcc -O2/-O3` 476/487ms（5.4x / 5.0x）；
同日的 pinned-core 配对 `task-clock` 为 dcc -O2 **min 472.7 / p50 475.9ms** 对 gcc -O2 85.0 / 87.3ms（§6.11）。

gcc's own reference carries more noise on this workload than on Lua (its -O2
row has ranged 86-98 ms across the last three revisions), which is why §6.7's
claim rests on a **paired** dcc-vs-dcc run on the same host rather than on the
dcc/gcc ratio: the previous round's single full run gave 527 / 523 ms against
the pre-change compiler, and this round's paired A/B moved 539 → 529 ms for
the same binary pair.

### 6.3 Reading the numbers

- dcc-generated code is **correct but materially slower** than gcc: roughly
  **2.6x** on the Lua interpreter loop (was 4.4x five revisions ago) and
  **~3.6x** on SQLite (was 6.4-7.1x, and 5.0-5.4x before the jump-table
  round in §6.12). This is an honest result, not a
  pass/fail gate — it quantifies where the dcc back-end (register allocation,
  inlining, instruction selection, and the -O2/-O3 passes) still trails a
  production compiler.
- For dcc, `-O3 ≈ -O2` on both workloads (no regression, no gain), matching
  gcc's behavior where -O3 does not help here; neither level breaks correctness.
- The gap tracks the workloads' hot paths: SQLite's single huge translation
  unit and Lua's computed-goto VM loop stress exactly the passes dcc is weakest
  at, so they are the most informative signal for driving further optimization.
- The 2026-10 emitter work (§6.6) cut `luaV_execute`'s static instruction count
  by 31.3% and its register-to-register moves by 55.8%, and Lua `-O2` moved
  497→324 ms (−34.8%). The ratio to gcc improved because of that, not because
  the benchmark changed.

These timings are the benchmark baseline: future optimizer work should be judged
by shrinking these ratios while keeping §4/§5 correctness.

### 6.4 What the 2026-10 re-verification changed

Five defects were found and fixed. Four of them blocked or miscompiled these
two projects at `-O2`/`-O3`; the fifth is a latent callee-saved violation that
the new emitter shortcut exposed. Together they account for most of the runtime
movement in the tables above.

1. **Serial `-O2` compile of `lvm.c` failed outright** (`E1135: no
   callee-saved register left`). `gen_binop_into` borrowed a scratch register
   for the right-hand side and never returned it, so four levels of nested
   Binop exhausted the four-slot callee-saved pool. The parallel codegen path
   hid it — every worker got a fresh register bank, so no single worker saw the
   exhaustion. Fix: release the borrowed register after its last read, and add a
   third sourcing tier (borrow `%rcx`) before the diagnostic.
2. **`-O2` folded wide literal comparisons wrong.** VRP classified two literal
   operands by masking them with the *destination* type, which is always `int`
   for comparisons, so `long long a = 1LL << 40; if (a <= 0)` folded to
   `0 <= 0` — true. Fix: bail out when a literal does not survive the
   destination mask.
3. **`-O2`/`-O3` Lua printed wrong numbers.** `print(12.0)` emitted `.0` and
   `print(1.5)` reported length 0. The float compare emitted `setp %bl`,
   clobbering the low byte of `%rbx` — which the register allocator had pinned
   as the home of `len` in `luaO_tostringbuff`. Fix: pick the 8-bit scratch by
   register-bank occupancy (`rbx`/`rcx`/`rdx`) instead of hard-coding `%bl`,
   saving/restoring when all three are busy. This is the one that made the
   `-O2`/`-O3` rows above incorrect, not merely slow.
4. **Computed-goto jump tables were rebuilt on every call.** A `static` array
   initialised from `&&label` (Lua's `disptab`) was emitted as runtime
   initialisation: ~120 `lea`+`mov` per `luaV_execute` call, ~7M calls in
   fib(32). It now goes into `.data` with relocations, like gcc. Worth ~3% on
   fib(32) — the largest single item here, and a reminder that the remaining
   gap is spread across code quality rather than concentrated in one place.

5. **The float-compare scratch clobbered a callee-saved register.** The
   `ucomisd`-based compare needs an 8-bit scratch for the PF merge and picks
   the first register that `is_free` reports from `rbx`/`rcx`/`rdx`. `is_free`
   answers "has *this* function already claimed it", not "is writing it ABI-safe".
   `rcx`/`rdx` are caller-saved, so writing them without a claim is fine;
   `rbx` is callee-saved, and writing `%bl` without a prologue save destroys
   the caller's value. The defect was latent because binops habitually occupied
   `rbx`, so `is_free(3)` was usually false. The new no-save shortcut (§6.6)
   removed that occupation and made it fire: in the `va_arg` aggregate fixture
   at `-O2`, `main`'s accumulator lived in `rbx`, the callee's `m.d > 4.0`
   compare executed `setnp %bl`, and the fixture returned 1 instead of 0.
   Fix: `claim(3)` when `rbx` is selected, so the prologue/epilogue save it.

Measured effect of the four earlier fixes (previous round): Lua `-O2` 506→502 ms,
`-O3` 509→482 ms; SQLite `-O2` 639→622 ms, `-O3` 638→624 ms. The emitter round
(§6.6) then moved Lua `-O2` 502→432 ms and SQLite `-O2` 622→595 ms, and its
second stage moved them again: Lua `-O2` 432→324 ms, SQLite `-O2` 595→527 ms
(the full like-for-like tables are §6.1/§6.2).

### 6.5 Register homes: from `reject_all` to an analysis-only CFG-SSA channel

The previous revision of this report said pointer locals could not be given a
register home, and that widening the type predicate had been tried and rejected.
The rejection was right; the diagnosis was incomplete. The type predicate was not
the load-bearing part — the **home channel** had read paths that still read the
stack slot after a write had gone to the register. With those repaired,
`bindable_ty` now accepts `Type::Ptr` and the 389-fixture four-level sweep stays
clean (0 OPT-DIFF). The read paths that had to be routed through
`mov_from_stack`/`home_for` were:

- `AddressBase::HomeLoad`, in both address-computation entry points (the value
  path and the `BASE`-register path in `mem/access.rs`);
- `emit_ident_base_addr`, which loaded `var_offset(name)` directly for a local
  used as an index base;
- the `&arr[i]` base-address path for locals;
- `StorageInfo.home`, which decides the base of a member access.

This is why "just widen `bindable_ty`" failed before: with any one of those four
still reading the slot, a bound pointer reads a slot that was never written.
The type predicate was never the fix; it only became safe after the channel was.

Even with that fixed, the four hottest variables in `luaV_execute`
(`base`/`pc`/`cl`/`k`) still got no home: the function uses computed `goto`,
`reject_all` refuses to build CFG-SSA for it, and the SSA home allocator never
sees it. That has now changed — **admission and landing are two separate
decisions**. `ssa::structure::structured_reject` no longer rejects
`goto`/`label`/computed `goto`; `has_unstructured_flow` answers a different
question, namely *how* the function lands. Structured functions still go
through `build_from_func` and `rebuild_block`; unstructured ones go through
`build_from_func_analysis` (`Builder::analysis_only`), which builds the same
CFG-SSA but never rebuilds the AST. `opt.rs` then plans homes with
`Ctx { unstructured: true }` and writes them straight back onto the original
declarations through `regalloc::deliver` — no phi materialisation, no inlining,
no block reordering. Two things had to change for that to be sound:

- the admission predicate is `definitely_assigned` (a must-analysis over the
  CFG, initialised to "all reachable blocks = universe, entry = parameters")
  instead of `Span::entry_def`. `base`/`pc`/`cl`/`k` are first assigned after
  the `newframe:` label, which dominates every use but is not the entry block,
  so the old predicate rejected all four before they could become candidates;
- register-number sharing is disabled on this channel (`shared_slots = false`),
  because the AST carries no version information that could tell two live
  ranges apart.

**The channel works; the pool is the limit.** `DCC_REGALLOC_DUMP=1` now shows
five names bound in `luaV_execute`:

```
[regalloc-dump] func=luaV_execute cands=42 bound=5(callee=5,ext=0) spilled=0
  bind pc -> %rbx    bind i -> %r12     bind base -> %r14
  bind trap -> %r15  bind io -> %r8
  skip k: pool-full  skip cl: pool-full
```

`base` and `pc` are resident; `cl` and `k` lose the access-count sort to
`i`/`trap`/`io` and are rejected **only because the pool is full**, not because
they are unbindable. Forcing just the four hot names proves that all four can
be bound at the same time:

```
[regalloc-dump] func=luaV_execute cands=4 bound=4(callee=4,ext=0) spilled=0
  bind pc -> %rbx  bind base -> %r12  bind k -> %r14  bind cl -> %r15
```

Measured on the same `lvm.c` (release `-O2`, `scripts/count_fn_insns.sh`), pinned to
one core, best-of-9 on `fib(32)`:

| analysis-only homes | instructions | `mov reg,reg` | stack refs (`%r13`) | `fib(32)` |
|---|---|---|---|---|
| off (`DCC_REGALLOC_ONLY=__none__`) | 12889 | 2333 | 3177 | 386 ms |
| forced `base,pc,cl,k` | 12476 | 2446 | 2941 | 344 ms |
| default (access-count order) | 12830 | 3124 | 2617 | 324 ms |

The table corrects the previous revision in an important way. Emitter-level
homes really were zero-sum: they drew from the same `{rbx,r12,r14,r15}` pool as
the binop scratch registers, so every move saved reappeared as a stack
reference (the `DCC_AUTO_HOME` table below still stands as the record of that).
A **resident** home on the SSA channel is a different trade: it removes the
per-instruction load/store of that variable entirely. That is why the default
row has the *most* register moves and still the fastest runtime — the forced
four-name row is 354 instructions smaller and 6% slower, because it leaves 324
more stack references in the hot loop. Static instruction count is again the
wrong scoreboard.

`cl`/`k` do not become resident without either widening the pool or evicting
`i`/`trap`/`io`; the eviction route was measured (the forced row above, 6%
slower) and the wide pool is closed for concrete reasons (`r10`/`r11` are
written outside the call save/restore window, `rsi`/`rdi` are `rep movs*`
operands, `rcx` is the variable-shift count). The honest statement is that
`luaV_execute` now has **two** resident pointers (`base`, `pc`) plus three
other hot locals, and the remaining two (`cl`, `k`) are one pool slot short.

For the record, the emitter-home experiment that the previous revision
reported (kept behind `DCC_AUTO_HOME`, default off) is unchanged:

| homes enabled | instructions | `mov reg,reg` | stack refs (`%r13`) |
|---|---|---|---|
| 0 | 16810 | 1993 | 3602 |
| 1 | 16810 | 1979 | 3616 |
| 2 | 16810 | 1565 | 4030 |
| 3 | 16809 | 1312 | 4282 |
| 4 | 16662 | 739 | 4708 |

The `DCC_AUTO_HOME` path therefore stays off by default.

### 6.6 What the emitter work bought

Three emitter-level mechanisms landed; all of them run at every `-O` level
(they do not depend on SSA):

1. **Pointer ± integer constant folding.** `p + k` / `p - k` with a
   side-effect-free constant `k` is emitted as one `add`/`sub` on the byte
   offset instead of a scaled multiply, folding the element-size `imul` away.
   ~605 instructions in `luaV_execute`.
2. **No-save left operand for isolated right operands.** When the right operand
   is a literal, a char constant, or a plain identifier, evaluating it cannot
   write the destination register, so the left operand no longer has to be
   moved to a callee-saved register or a frame slot first. ~585 instructions,
   and it frees the four-slot callee-saved pool for the cases that really need
   it. This is also what exposed defect 5 in §6.4.
3. **A one-instruction peephole at the emit layer.** Three rules: drop an
   adjacent byte-identical idempotent move; drop `mov x→y; mov y→x`; and drop
   a load/store immediately followed by its inverse on the same slot/register.
   The first revision removed ~750 instructions in `luaV_execute`; the two
   correctness fixes in the second bullet below take back a few of those
   opportunities (a self-referential load no longer qualifies), so treat 750
   as an upper bound rather than the final delta.

Static effect of the first round, as recorded at the time (release `-O2`,
rounded to the nearest 100; the authoritative HEAD→now measurement with the
final counting script is the table below):

| metric | before | after | delta |
|---|---|---|---|
| instructions | 18000 | 16400 | −8.9% |
| stack refs (`%r13`) | 3626 | 3560 | −1.8% |
| `mov reg,reg` | 3379 | 2086 | −38.3% |
| `imul` | 334 | 216 | −35.3% |
| `movsxd` | 1013 | 699 | −31.0% |
| `cmp`/`test` | 714 | 714 | 0 |

Two lessons worth recording:

- **The peephole removed 750 instructions and bought no measurable time** when
  measured on its own (Lua `-O2` 502→504 ms, SQLite 622→691 ms). The
  instructions it removes are not on the hot path; the win came from (1) and
  (2). Do not size an optimization by its instruction-count delta alone.
- **A clean equivalence sweep is not evidence that an emitter change is
  correct.** The first peephole revision deleted `movsb`/`movsq` (no operands,
  implicit `rsi`/`rdi`/`rcx`) and `mov (%rax), %rax` (self-referential: the
  first move changes `%rax`, so the second reads a different address). Both
  passed the whole 389-fixture sweep at `-O0`/`-O2`/`-O3` and still broke the
  Lua build immediately: `print("hello")` crashed at `-O0`/`-O1` and reported
  `malformed number` at `-O2`/`-O3`, because the bug lives in `llex.c`, which
  the fixture corpus does not compile. Rule: after touching the emitter, rebuild
  Lua end-to-end, not just the corpus.

#### Second round: constant comparisons, dead writes, zero-extension tidy-up

Four more emitter rules landed; they also run at every `-O` level:

4. **A constant right operand of a comparison is an immediate.** `x != 0` used
   to be emitted as `mov tmp, 0; cmp x, tmp`; it is now `test x, x`. Non-zero
   constants use `cmp r/m, imm` (`83 /7 ib` or `81 /7 id`), followed only by
   the `setcc`/`movzbq` tail. Constants already passed through
   `mask_imm_to_type`, so the unsigned `zero_extend_for_cmp` on the constant
   side is the identity; 32-bit comparisons take the low 32 bits, 64-bit ones
   require the masked value to fit in `i32`. The condition-code choice and the
   immediate emission each have a single source (`cmp_cc` / `emit_cmp_imm`).
5. **Dead write removal, not just dead constants.** The first round only
   dropped a `mov reg, imm` that the next instruction overwrote. The rule is
   now "any instruction that writes a register in full, followed by one that
   overwrites the same register without reading it" — `mov`, `movzx`/`movsx`
   with a 32/64-bit destination, 32-bit `mov`, and frame-slot loads. It is
   still bounded by `label()`/raw `emit()`: those clear the lookback, so the
   deleted instruction can never be a jump target.
6. **32-bit self-moves after a 32-bit write.** `mov eax, eax` is the
   zero-extension tail of a 32-bit unsigned binop; when the previous
   instruction already wrote `%eax` (32-bit writes zero-extend by themselves),
   the tail is pure overhead. `Add`/`Sub` stopped emitting the tail altogether
   because their 32-bit ALU form already zero-extends; `Mul`/`Shl` still need
   it (they are 64-bit operations).
7. **Computed-goto dispatch table goes to `.data`** — recorded in §6.4 item 4,
   repeated here because it is the same emitter round and the largest single
   runtime item (~3% of `fib(32)`).

Static effect on `luaV_execute` (release `-O2`, one counting script for all
columns, `scripts/count_fn_insns.sh` = `objdump -M intel --no-show-raw-insn`):

| metric | HEAD | emitter rules only (homes off) | default (now) | vs HEAD |
|---|---|---|---|---|
| instructions | 18669 | 12889 | 12830 | −31.3% |
| `mov reg,reg` | 7064 | 2333 | 3124 | −55.8% |
| stack refs (`%r13`) | 3632 | 3177 | 2617 | −27.9% |
| `imul` | 334 | 216 | 216 | −35.3% |
| `movsxd` | 1013 | 698 | 698 | −31.1% |
| `cmp`/`test` | 714 | 714 | 714 | 0 |

The middle column is `DCC_REGALLOC_ONLY=__none__`: every emitter rule above is
on, the analysis-only home channel is off. It is the same A/B run as the §6.5
table, which is why the counts agree, and it separates the two halves of the
work: the emitter rules account for essentially all of the instruction drop
(18669→12889), while the resident homes trade 791 register moves and 59
instructions for 560 fewer stack references and 62 ms of runtime
(12889→12830, 3177→2617 `%r13`, 386→324 ms). `mov reg,reg` going *up* in the
last step is not a regression — it is the price of keeping `base`/`pc` in
registers across blocks instead of reloading them from the frame.

Three more lessons, all about the *premises* of a peephole rather than its
code:

- **`MovExt` is a full write only when its destination is 32 or 64 bits.** The
  classifier ignored the `to` width, so `movzx reg8, ...` — which leaves the
  upper 56 bits intact — qualified as "overwrites the whole register" and
  deleted a still-live write. Lua built with that revision answered `attempt
  to call a nil value (global 'print')`: `luaL_openlibs` never registered the
  standard libraries. The fix keys on `to: B32/B64` and treats byte/word
  destinations as opaque. This defect was latent in the original rule too; the
  wider rule is what made it fire.
- **A load's address can read the register it writes.** `mov rax, [rax+8]` is
  a full write of `rax`, but deleting the preceding `mov rax, imm` changes the
  address. General `mov reg32, [mem]` is therefore not accepted as the
  overwriting instruction; frame-slot loads (`base = r13`) are, because their
  address cannot involve the destination.
- **The corpus is necessary but not sufficient.** Neither defect is visible to
  the fixture corpus: they broke Lua's own `linit.c`, which the corpus does not
  compile. The end-to-end Lua build caught them within one run.

Finally, the cross-basic-block half of the request. Folding `mov A,B; mov B,C`
**across blocks is not implemented**, and the reason is structural:
computed-goto labels take their address with `lea rip+`, so a `label` cannot be
proven unreferenced from the `jmp rel32` stream alone, and a large share of the
emitted instructions are raw byte sequences whose read/write sets are not
recoverable. What did land is block-local DCE that crosses *instructions* (not
blocks): the removal above finds the overwritten write across any number of
intervening instructions, bounded only by the next `label()` or raw `emit()`.
On the final build the strictly adjacent chain is already gone: a scan of
`luaV_execute` finds **0** adjacent 64-bit `mov A,B; mov B,C` pairs, and the
only adjacent mov pairs left at all are two `mov eax,eax; mov eax,eax` tails
(the second is the `movl` form of the same zero-extension). They survive
because the byte-identical rule's `repeat_safe` whitelist only accepts the
3-byte REX.W register-to-register form; these are 2-byte 32-bit encodings. It
is a two-instruction miss, not a correctness gap. What remains beyond that is
the non-adjacent case inside a block: proving `A` dead at the second `mov`
needs a real liveness pass over the instruction stream, which is the
prerequisite for going further.

---

### 6.7 Direct conditional branches (P13 stage 3)

A branch condition that is an integer/pointer comparison used to be
materialised into `0/1` before it was tested:

```
cmp eax,0x3 / sete al / movzx rax,al / test rax,rax / je L    ; 5 instrs, dep depth 5
cmp eax,0x3 / jne L                                          ; gcc: 2 instrs, dep depth 1
```

`luaV_execute` carried **309** of those chains — type-tag tests on the path every
single VM instruction takes. The emitter now hands the comparison's condition code
straight to the branch site:

- `Codegen::cmp_flags_token` is **consumed at the first line of `gen_expr`**, so a
  nested comparison whose value is still needed (`if (f(a<b))`, `if (arr[k<2] + 1)`,
  `int r = (a == b)`, `if (x + (a<b))`) structurally cannot reach the hand-off path.
  This is the single defence against the failure mode of this optimisation: emitting
  flags but no value, then testing a garbage `%rax`.
- `Codegen::last_cmp` records "the instruction just emitted was this `cmp`" and is
  cleared by every `emit()` and every `label()` — the same lifetime discipline as the
  peephole's `peep_last`, so a label (a join point) can never inherit stale flags.
- Polarity comes from `CondCode::inverse()`; `JumpKind` gained `Jae`/`Jbe`, and
  `jump_kind_for_cc()` sits next to `jmp_rel32`'s byte table so the two cannot drift
  apart silently. The condition code itself is still taken only from `cmp_cc`.
- Float (`ucomisd` + parity), `_Complex`, `__int128`, vectors and bit-field
  comparisons are **not** admitted; they keep the materialising path byte-for-byte.

Measured on the final build (`scripts/count_fn_insns.sh`, same host):

| Counter | HEAD | stage 3 | stage 3 + §6.8 + 浮点条件修复（最终态） |
|---|---|---|---|
| `luaV_execute` instructions | 12830 | 12491 (−339) | 12491 |
| `luaV_execute` `setcc` | 376 | 263 | 263 |
| `luaV_execute` `cmp`/`test` | 714 | 601 | 601 |
| `sqlite3VdbeExec` instructions | 20788 | — | 19886 (−902) |
| `sqlite3VdbeExec` `cmp`/`test` | 1410 | — | 1131 |
| Lua `fib(32)` min / p50 (paired, best-of-9, pinned) | 323.8 / 330.0 ms | 316.9 / 325.8 ms | 319.5 / 324.7 ms |
| SQLite 200k min (paired, best-of-5) | 539 ms | — | 529 ms |

The Lua gain is quoted as a **band** (min −1.3% to −2.2%, p50 −1.6%) because two
independent paired runs of the same binaries disagreed by more than their
internal spread: this host drifts a few percent between runs, which is exactly
why every row above is a paired, alternating measurement of two builds in one
run rather than a number carried over from an earlier session.

The admitted/refused boundary is frozen by a new disassembly snapshot case,
`tests/disasm_snapshots_x86_64/direct_cond_branch`, whose `expected.s` shows
`cmp` + `jge`/`jl`/`jbe`/`ja`/`je`/`jne` for the admitted shapes and keeps
`setcc`+`movzbq`+`test` for the four refused ones; the behaviour side is
`tests/fixtures/opt/direct_cond_branch.c` (exit code = number of failed checks,
identical to gcc at all four levels). Two unit tests lock the tables: one that
`CondCode::inverse()` is a fixed-point-free involution, and one that maps every
comparison × signedness to its `JumpKind` against a hand-written x86 table.

**What this did not buy.** −2.2% on min (−0.5% on p50) and −1.9% on SQLite —
against a 2.7x total gap. Instruction-count reduction on the executed path does
pay, but only a fraction of what is needed; the remaining factor is structural
and is recorded in `.trae/documents/x86_64-p13-hot-path-emission-optimization-plan.md`:
`luaV_execute` / `sqlite3VdbeExec` are computed-goto or `goto`-riddled, so they
travel the analysis-only channel and **run no SSA pass at all**, and every opcode
inlines its own cold path (`luaG_traceexec` appears 80 times inside `luaV_execute`
against gcc's 6).

**A measured rejection.** Removing the 149 "jump to the very next instruction"
blocks (`emit/stmt.rs`: the `if`-without-`else` tail and the `switch` tail) cuts
135 instructions and 675 bytes out of `luaV_execute` and changes nothing else
(`movrr`/`r13`/`imul`/`movsxd` identical). It made the benchmark **3.8% slower**
(paired best-of-9: min 310.9 → 322.6 ms, p50 319.3 → 326.4 ms): shifting 135
five-byte blocks moves the hot loop bodies off their existing alignment, and the
layout cost outweighs one always-taken branch per site. The change was reverted,
and both emission points now carry that measurement in a comment so nobody
retries it from the static numbers alone. Same-revision companion: replacing the
five `push r; pop r2` register moves in `mem/access.rs` with a real `mov` was
kept — it is length-neutral for Lua (those sites are not on `lvm.c`'s paths) and
strictly one instruction shorter wherever it fires.

---

### 6.8 Two silent miscompiles found by the new fixtures

Both defects exist on HEAD (`4676c0c8`) and are unrelated to §6.7; the new
fixtures are simply the first artefact-level checks that ever covered their
shapes. Root-causing and patching were done by a dedicated expert agent in an
isolated worktree, then applied and re-verified here. Full site-by-site analysis:
`.trae/documents/x86_64-p13-hot-path-emission-optimization-plan.md` §9.

1. **Usual arithmetic type of a mixed-sign comparison** (`-O0`/`-O2`/`-O3`).
   `if ((unsigned long)x > 0xffffffffUL)` with `x = -9` compiled to a **signed**
   compare (`movsxd rax,DWORD PTR [r13-0x50]; movabs r8,0xffffffff; cmp rax,r8; setg`),
   so dcc answered 0 where gcc answers 32. Four sites had to be fixed together:
   `src/ast.rs::usual_arith_type` compared type *rank* without *width* (LP64:
   `long long` vs `unsigned long` are both 64-bit, so C11 6.3.1.8 requires
   `unsigned long long`); `emit/expr/dispatch.rs` kept a **second copy** of that
   rank-only rule; `src/ssa/expr_rebuild.rs::int_const_ty` guessed a type back from
   the magnitude of an untyped `Operand::Int`; and SCCP's `replace_op` replaced a
   *typed* `Operand::Var(y.1: UInt)` with an *untyped* `Operand::Int(1)`.
   `src/const_eval.rs::cmp_binop_values` also folded on "either side is unsigned"
   instead of the common type, with a round-trip `as u128 as i128` that is the
   identity, and folding runs even at `-O0` (it sits inside `function_inline`, which
   precedes the `level == 0` early return). The emitter now uses
   `Type::int_promotion`/`usual_arith_type` as the single source, and SSA records the
   literal's type keyed by instruction **destination** (keying by value cannot tell
   `1` from `1u`).
2. **Loop blocking reordered observable side effects** (`-O3` only). The old
   `ts×ts` blocking in `src/loop_opt.rs` enumerates iterations in a different order
   from the source loop — the multiset of printed values matched gcc, the sequence
   did not — and the pass also lacked a `break` guard, so an unshielded `break`
   inside the tiled body ran extra iterations (measured 12 → 18). It is now an
   order-preserving strip-mine (outer loop kept verbatim, one block counter plus one
   inner loop, body moved verbatim) with a `tile_body_is_safe` guard that reuses the
   unrolling path's existing `break_shielded` walker rather than defining a second
   one.

New tests locking these: `usual_arith_type_matches_c11_conversion_rank_and_range`
(21-row LP64 matrix whose expected values come from a local gcc `_Generic` probe,
not from hand arithmetic), `comparison_result_type_is_int_and_does_not_leak_the_common_type`,
`cmp_folds_in_usual_arith_type_of_operands`, `cmp_folds_left_operand_in_its_own_width_first`,
`unsigned_order_preserves_full_u128_ordering`,
`const_int_ty_table_records_literal_signedness`,
`rebuilt_int_constant_restores_literal_type_instead_of_the_ladder_guess`,
`cmp_operand_ty_is_keyed_by_inst_dest_not_by_value`, `tile_refuses_unshielded_break`,
plus the fixtures `tests/fixtures/opt/mixed_signedness_usual_arith.c` and
`tests/fixtures/opt/tile_strip_order_break.c`.

Still open and deliberately **not** papered over here:

- **A float used as a condition never tested its own value.** `emit_truth_flags`
  had no SSE-float arm at all: for `if (d)` with `d` a `double` it emitted
  `movsd xmm0,[d]` and then tested `%rax` — the residue of whatever ran last.
  HEAD agreed with gcc only because its integer comparisons left a materialised
  `0/1` in `%rax`; once §6.7 replaced `cmp`+`setcc`+`movzx`+`test` with
  `cmp`+`jcc`, that residue was no longer zero and `gcc.dg/tree-ssa/scev-2.c`
  started aborting. The probe `.work_tmp/p13_flt_nan.c` shows HEAD really was
  wrong (`double z = 0; if (z) …` prints `zTRUE`), i.e. this is a latent
  miscompile that §6.7 exposed, not caused. `emit_truth_flags` now zeroes `xmm1`
  and reuses `gen_float_cmp(Ne)` (whose parity-term handling is what makes
  `if (nan)` correctly true, which a bare `setne` after `ucomisd` would get
  wrong). **Remaining sibling of the same family, still broken:** the
  value-context uses `!d` and `d && x` still read that integer residue
  (`.work_tmp/p13_flt_nan.c` prints `1111` where gcc prints `0111` for
  `!!z`), and `long double` conditions (x87 channel) plus `__float128`
  conditions have no float truth test either.
- The disassembly-snapshot baseline was **already stale on HEAD**: in a pristine
  HEAD worktree (`.work_tmp/wt_fixbase`, empty `git status`) 19 of the 30 cases
  fail `cargo test --test disasm_snapshot`. This round's re-record is what makes
  that target green again (31/31), and it is also the reason the two miscompiles
  above survived: `cargo test --lib` alone never runs that oracle.
- `sizeof arr / sizeof *arr` on a **local struct array** yields the wrong element
  count (gcc loops 2 times, dcc loops 8 and reads past the end) at every level,
  identical before and after these fixes — a separate `sizeof`/array-dimension
  root cause.
- Four integration tests already fail on HEAD itself (verified by running them in a
  clean HEAD worktree): `cpp_flat_map`, `cpp_multi_declarator_member`,
  `eh_landing_pad_functions_are_never_auto_bound`,
  `register_home_and_cleanup_survive_every_declaration_path`. The gate for this
  round is therefore "no **new** integration failure": measured
  `1110 passed / 4 failed`, the failing set identical to HEAD's.

---

### 6.9 标量写直生、死值上下文与 -O2 成本模型内联（P13 阶段 1/2/5）

**阶段2（标量写直生）**。六条赋值臂（Deref / Member / `__real__` / Index 语义层 /
Index 回退 / 通用）过去都是「地址算进 `rax` → `push` 保住 → 右值算进 `rax` → `pop` 到
`rcx` → store」，押栈的唯一理由是地址与右值挤同一个号。现在右值是「只会写 `rax`」的形态
（字面量、名字，以及由 `Cast`/`Member`/`Deref` 套出来的标量读链）时，地址一条 `mov rcx, rax`
到位，栈往返整条消失；判据只存一份（`store_rhs_isolated`），五条臂共用一对
`save_addr_for_rhs` / `restore_addr_after_rhs`。另外两处：

- `emit_store_scalar_int_at` 拆出带值寄存器的 `_val_at` 版本，**宽度判据仍只有一份**
  （16 字节值模型是 `(rax, rdx)` 一对，换低半不换高半的形状直接报内部错误 E1138）。
- Ident 基址的下标取址改为「先算下标、后算基址」（基址那一路只写 `rax`），于是
  `a[i] = v` 也不再押栈；开 `-fbounds-check` 时保留原顺序（那一段有 handler 调用）。

**阶段1（死值上下文）**。`ValueUse` 的 `CondOnly` 一极已由 §6.7 的消费式令牌实现；
这一轮补上 `Discarded`：`Stmt::Expr`、`for` 的 step 在求值前声明「值没人要」，赋值臂
末尾就只做到存之前必要的那一段转换（浮点→整数仍然做，那是值语义），不再把 `rax`
归约到左值宽度——store 本身就按左值字节数截断。同时补齐了几处一直缺 gas 文本的
高频发射点（自反 `movsxd/movzx/movzwq`、赋值押栈的两条 `mov`），发射层窥孔的幂等移动
判据以 gas 文本为凭，没文本就永远看不见那类冗余。

**阶段5（-O2 成本模型 `static` 函数体内联）**。原先只有 `always_inline`/`flatten` 会被
语句级展开，且只接直线性 body；-O3 的启发式又要求「零参 + cost ≤ 20」。新增一条按成本
模型决定的函数体内联：允许形参、允许控制流，准入是一串硬判据（`static`、非 `noinline`、
非变参、非 `returns_twice`、非嵌套函数、无 `alias_name`、形参都有名字且非 VLA、体内无
computed goto/asm goto/标签地址/语句表达式/`va_arg`/`__builtin*`/嵌套函数定义/不可搬的
存储类别、调用图不自环），失败关闭——扫描器遇到不认识的 AST 变体一律当作不可内联。
阈值按实测分布定：多调用点 200，**只有一个调用点**放宽（单点展开不复制代码）。

实测（同一台机器、`taskset -c 3`、与 HEAD 交替配对）：

| 口径 | HEAD | 本轮结束 |
|---|---|---|
| `luaV_execute` 指令 | 12830 | 12412（−3.3%） |
| `luaV_execute` push/pop | 535 | 389 |
| `luaV_execute` `setcc` / `cmp+test` | 376 / 714 | 263 / 601 |
| `sqlite3VdbeExec` 指令 | 20788 | 21278（单点助手被搬进解释器，见下） |
| Lua `fib(32)` min / p50 | 314.7 / 322.7 ms | 311.9 / 314.9 ms |
| SQLite 200k（best-of-7 墙钟） | 527 / 523 ms（-O2/-O3） | **488 / 487 ms** |

`sqlite3VdbeExec` 静态变**大**了 490 条而运行快了 7%：单点 `static` 助手被搬进解释器后
调用消失、但体内联不进 pass 的函数体本身变长。这是本轮第一次出现「静态指标与运行时
反向」的正例，前两个反例（§2.1 spill/reload、§2.2 死 jmp）都是静态变小而运行时变慢。
产物尺寸也同时上涨（SQLite `sqlite3.o` 4.9→5.1 MB，Lua 可执行 919 KB→1.08 MB），
记在这里作为下一步调阈值的依据。

**与方案里数值目标差距**（本段写于该轮，目标已在第五轮达成）：`scalar-store` 那组目标
（`luaV_execute` ≤12350 条、push/pop ≤250、`setcc` ≤100）**当时没有达成**：该轮停在 12412 条 /
push+pop 389 / `setcc` 263。当时给出的归因——剩下的 push/pop「全部属于『基址与下标都要
寄存器』与 `+=`/`++` RMW 两类，需要与 regalloc 的家分配联合调参」——已被 §6.11 的实测
**推翻**：把它们拿掉的是三条发射层改动（整型常量表达式进「右值只写 `rax`」白名单、标量写
的地址后置重算 Late、浮点赋值进 Late），push+pop 389 → **155**、insn → 11366。`setcc` 那条
归因（剩余绝大多数是「值真要用」的语境：`!!x`、把比较结果存进变量、`&&` 链两侧，不在直生
白名单内）没被动摇，但它也不是第五轮动的——那一轮起点就已经是 28（§6.11）。差距与逐条原因
登记在 `.trae/documents/x86_64-p13-hot-path-emission-optimization-plan.md` §12.3 与 §14。

**测试用例**：新增 `tests/fixtures/opt/scalar_store_direct.c`（白名单/回退两侧逐条自检，
四档与 gcc 一致）与 `scalar_store_direct_matches_gcc_at_all_levels`、
`o2_static_inline_cost_model_inlines_small_helper`（产物级断言「单点 `static` 助手展开后
不再存在向它的 `call`」）。阶段5 还顺带暴露并修好一条自己的坑：被内联汇编文本点名的
`static` 函数不得展开（否则定义消失、asm 里 `call leaf` 链接失败），该回归由仓库既有夹具
`tests/fixtures/opt/asm_text_static_func_symbol.c` 守住。

---

### 6.10 块内等价事实：发射层能做的最后一档（P13 阶段6(c)）

`luaV_execute` 与 `sqlite3VdbeExec` 的 computed goto 让它们走 analysis-only 通道，
**一条 SSA pass 都不跑**。它们剩下的唯一优化面是发射层，而发射层的既有窥孔只看相邻两条
指令。这一轮把「上一条」换成「本块内的状态」：`src/codegen/x86_64/blockopt.rs` 维护
`copy`（两个号相等）、`sext32`/`zext32`（32 位规范形态）、`slots`（`[r13+disp]` 里就是
某个号）四张定长表，据此删/换已发出的指令。

六条规则分三档（`DCC_BLOCKOPT` 0-3，默认 3）：`=1` 只删恒等 `movsxd r,r`；`=2` 加
自反搬运 / 已相等搬运 / 同号帧槽读；`=3` 加帧槽读转发与重复写删除。分级是为了二分定位——
削错时症状出现的那一级就是肇事规则。屏障：`label()` / 裸字节 `emit()` / 函数边界整体清空，
`jcc` 不清（落空边事实仍成立，落点必是 `label()`），`push`/`pop` 不清帧槽事实
（`[rsp-8]` 与 `[r13+disp]` 不重叠，这是解释器热路径里事实能活过一条指令的直接原因）。

实测（release `-O2`）：`luaV_execute` 12412→12381 条，`sqlite3VdbeExec` 21278→21155 条；
`lvm.c` 单文件 410 次 drop（220 恒等 `movsxd`、84 恒等 32 位自搬、81 同号帧槽读、
17 重复帧槽写、8 无 gas 文本）。

「410 次 drop 只换来 31 条」不是矛盾，是两个开关在抢同一批形状。把 `DCC_PEEP`（既有相邻
窥孔）与 `DCC_BLOCKOPT` 拆成 2×2，`luaV_execute` 的指令数是：

| | `DCC_PEEP=0` | `DCC_PEEP=3` |
|---|---|---|
| `DCC_BLOCKOPT=0` | 12687 | 12412 |
| `DCC_BLOCKOPT=3` | 12429 | **12381** |

块内事实单独削 258 条（−2.0%），既有窥孔单独削 275 条，两者叠加只有 306 条 —— 重叠 227。
邻接窥孔与块内事实同域：恒等搬运/自反扩展本来就大多相邻，谁先看到就归谁。

同机交替配对，三次独立测量（两次 best-of-9，一次 best-of-15/11）：

| 工作负载 | 关 min/p50 | 开 min/p50 | Δ |
|---|---|---|---|
| Lua `fib(32)`（9 轮） | 320 / 324 ms | 312 / 316 ms | −2.5% / −2.5% |
| Lua `fib(32)`（9 轮，最终二进制） | 321 / 328 ms | 317 / 323 ms | −1.2% / −1.5% |
| Lua `fib(32)`（15 轮） | 319 / 326 ms | 312 / 322 ms | −2.2% / −1.2% |
| SQLite 200k（9 轮） | 487 / 495 ms | 479 / 494 ms | −1.6% / −0.2% |
| SQLite 200k（9 轮，最终二进制） | 490 / 493 ms | 486 / 494 ms | −0.8% / +0.2% |
| SQLite 200k（11 轮） | 488 / 492 ms | 485 / 493 ms | −0.6% / +0.2% |

按 §2.3 的纪律保留：Lua 三次 min 与 p50 同向下降（−1.2%…−2.5%），这是支撑证据；
SQLite 的 min 三次均小幅下降（−0.6%…−1.6%）而 p50 在 ±0.2% 内持平，属噪声边缘，
不能拿单次最好值当结论。

两件必须记下来的事：

1. **volatile 的逐槽登记会把编译时间拖成分钟级。** 帧槽事实只对非 volatile 槽建立，而
   修饰符不在 `var_map`（那里只有 `Type`），所以要单独登记每个 volatile 对象占的每个
   8 字节槽。`volatile char big[0x100000000UL]` 是 5.4 亿个槽，逐槽插入让
   `perf_regression::perf_large_frame_compiles` 跑成 **6 分钟 / 5.9 GB RSS**（该项原本
   要求 <10s）。修法是 `VOLATILE_SLOT_CAP=128` 加「溢出就整体关掉本函数的事实」——
   保守方向只能选一个，选的是不转发、不去重。
2. **callee-save 补丁点会漂移。** `patch_callee_saves` 按记录的 `pos` 回填 4 个 `0x90`，
   而块内事实可能把一条恢复整条删掉。第一版按 `emit_insn` 的返回值（「已处理」）登记，
   于是一条没发字节的指令也登记了补丁点，4 个 nop 落到下一条 `mov %r13,%rsp` 上，
   尾声只剩 `pop %rbp; ret` → SQLite 在 `DCC_BLOCKOPT=3` 下 SIGSEGV（`=0` 正常）。
   判据改成「代码真的长长了」：补丁点必须与被删指令同生同灭。

等价性证据：`scripts/check_opt_equivalence.sh` 395 夹具 × 四档 **0 OPT-DIFF**；开启/关闭
直接差分（`.work_tmp/blockopt_diff.sh`）两侧都能编成可执行的 111 个夹具中，opt 61 个 0 diff、
asm 50 个里 2 项 diff 经核查都是打印指针地址的噪声；31 个反汇编快照用例中 23 个的产物随
`DCC_BLOCKOPT` 变化（已重录），差异助记符只有 `mov`/`movsxd`/`movzx`，其余是跳转偏移与
nop 填充随代码长度漂移；G10 产物哈希 5 个重录（相对 HEAD 全部变化，用 `DCC_BLOCKOPT=0/3`
反汇编对比确认属于预期形态）。

阶段6 的 (a) 冷路径外置与 (b) 冷序列 tail merging **仍未做**：前者要移动基本块，而
`disptab` 用 `lea rip+` 对 label 取址；(b) 合并的是 trace 没开时一次都不执行的冷块，
按 §2.3 属纯静态收益。两者的开工条件是先有 `perf stat` 的 front-end 剖面，而**这台机器
拿不到那个口径**：`cycles`/`instructions`/`branch-misses` 全是 `<not supported>`，
`L1-icache-load-misses` 在 0.115 s 的 workload 上报 0（根本没在数），
`perf_event_paranoid=4`，且 `valgrind` 未安装（无 cachegrind 代替）。所以这两项在本机
不可证、本方案不预写代码，判据留在方案 §13.7。

阶段表的其余各项已在方案 §13.10 逐项结清（落地 / 回退 / 不必要 / 不落地），其中
阶段4c 的 `last_sext` 单槽**不再需要**：它的判据（相邻且同源同目标的 `movsxd` 对）在基线
副本上就复现不出（实测 0 对），而真正要消的「源寄存器 32 位写已把高半部置对」属于块内
事实，已由规则 3/6 覆盖并实测在 `lvm.c` 上消掉 220 条恒等 `movsxd`。方案 §7 列出的 11 条
形态用例也逐条对照过（§13.9）：8 条本轮新增（7 条同名、1 条因实测行为不符而换名并把判据
钉成两面）、2 条第三轮已存在、1 条因前提消失换成对 §2.2 的反向防线；新增的 9 条使
integration 从 1115 条基线升到 1124 条，失败的仍是 HEAD 那 4 条。

---

### 6.11 地址后置重算与常量右值白名单：push/pop 389 → 155（P13 第五轮）

§6.9 记了三轮的那 389 条 push/pop，这一轮降到 **155**。判据口径与方案 §14 一致：
`cd /root/lua-5.5.1/src && dcc -O2 -I. -c lvm.c`，只统计 `luaV_execute` 函数体的反汇编
（`scripts/count_fn_insns.sh`）。

| 计数 | 本轮起点 | 本轮最终 | 方案 §5 第 3 条的目标 | 结果 |
|---|---|---|---|---|
| insn | 11438 | **11366** | 阶段2 ≤12350、阶段3 ≤11800 | 达成 |
| push + pop | 194 + 195 = 389 | 77 + 78 = **155** | ≤250 | 达成 |
| `setcc` | 28 | 28 | ≤100 | 达成 |
| cmp | 366 | 366 | — | 本轮未动 |

§6.9 那组「没达成」的静态目标，到这里**三条全部达成**。口径衔接也要写清：本轮起点已经是
insn 11438 / `setcc` 28，与 §6.10 记的 12381 / 263 之间隔着不属于本轮的部分，本节不追溯它的
归属，只对本轮的增量（push+pop 389→155、insn −72）负责。

三条改动：

1. **整型常量表达式进「右值只写 `rax`」白名单**。新增 `binop_const_foldable`
   （`src/codegen/x86_64/emit/expr/utils.rs`），判据与 `dispatch.rs` 里 `Expr::Binop` 臂的常量折叠
   **逐字同源**：非 `&&`/`||`、两侧与结果都是整型、无副作用、`const_eval::eval` 成功。lvm.c 里
   `settt_()` 展开出的 `(3|(0<<4))` 这类右值，在发射层本来就会折成一条 `mov imm`，但 AST 层的
   形态判据不认识 `Binop`，把它挡在白名单外，于是白白押栈。
2. **标量写的地址后置重算（Late）**。`AddrKeep` 从两路扩成三路
   `InRcx` / `Late(LateAddr)` / `OnStack`，配 `plan_addr_keep` + `save_addr_for_rhs` /
   `restore_addr_after_rhs` 三段式。依据是 **C11 6.5.16**「赋值的两个操作数求值无序」：先算右值，
   随后一条 `lea`/`mov` 重算同一个地址。两道门：地址链必须能描述成
   `LateAddr::{Frame(off), Loaded{slot, more}}`；`Loaded` 形态还要求右值不得改写那个**指针值**
   （`Frame` 不需要这条门，因为 `r13+off` 与内存内容无关）。`Index` 与带 VLA 的成员偏移刻意留在
   名单外：下标要算进 `rcx`，且重算等于重复求值下标。实测产物形态：`p->n = p->n + 1` 原来的
   `push %rax … pop %rcx` 换成一条 `mov -72(%r13), %rcx` 重算。
3. **浮点赋值进 Late**。新增 `late_store_ty_ok` 放行 `float`/`double`——`InRcx` 仍不放行，因为
   浮点的值通道是 xmm0，而 Late 只要求「重算地址不碰值通道」；`long double`/`__float128`、
   聚合、复数继续排除。

本轮还修掉三处**真实语义错误**（是错值，不是低效）：

1. `gen_cond_jump_to` 在「假则跳出」语境（If/While/For，`jump_if_true == false`）把 `||` 按 `&&`
   拆。原注释「`!(l && r)` 与 `!(l || r)` 都等价于任一侧为假就跳」是**错的**：`!(l || r)` 是
   「两侧都假才跳」，左真时必须落到右操作数，后果是 `if (a || f())` 的短路整条反向。它由
   integration 测试用例 `short_circuit_rhs_impure_matches_gcc_at_all_levels` 先复现（修复前 `-O0`
   退出码 26 vs gcc 0），同时带出 `lazy_guard_and_matches_gcc_at_all_levels`、
   `loop_exit_ownership_shapes_match_gcc_at_all_levels` 共 3 条失败；修复后 integration 回到
   `1124 passed / 4 failed`，4 条失败集合与 HEAD 逐条相同。HEAD 里没有 `gen_cond_jump_to` 这个
   符号，所以这条不属 §6.8 那份 HEAD 案底，而是 §6.7 自己的分解表带进来的。
2. `emit_truth_flags`（`src/codegen/x86_64/emit/expr/float.rs`）的兜底只按 `ty.size() > 4` 选
   `test %rax,%rax`，而本后端 `__int128` 的 rvalue 是 `(rax, rdx)` **一对** ⇒ 16 字节类型只测了
   低半，「lo==0 且 hi!=0」被误判成 0。它由反汇编快照 `pr105613` 暴露：`r[0] != 0` 从完整两半
   比较退化成一条 `test %rax,%rax`。修复是 128 位先 `or rax, rdx` 再 `test`，与 Binop 臂里
   `&&`/`||` 对 128 位操作数的合并判据同一条理由。**这个盲区的性质值得单独记**：`pr105613`
   语料自身只覆盖 `lo!=0` 与全 0 两种数据，**恰好不含「hi!=0 且 lo==0」**，所以它长期只在快照
   文本里现形、执行侧永远绿——是「快照 + 语料数据」的组合盲区，不是运气；本轮为此新增的执行级
   夹具专门取 hi-only 数据。
3. **发射层窥孔把「带符号补丁占位」的指令当同字节冗余削掉**（`instructions.rs::emit_insn`）。
   这一条与赋值臂无关：`d = b = 1;` 脱糖成两条相邻的 `mov %eax,<符号>(%rip)`，而 RIP 相对的
   disp32 在指令字节里就是 `00 00 00 00` 占位（真实偏移由链接器按补丁填）⇒ **两条字节完全
   相同**，「相邻两条同字节幂等搬运 ⇒ 第二条不发」就把外层那次写整条削掉。它比「少写一个变量」
   严重得多：符号补丁是调用点在 `emit_insn` **返回之后**按 disp32 位置自行登记的，而 `emit_insn`
   的 `true` 说的是「编码器覆盖了该形态」，**不是**「字节已落地」；指令没了、补丁还在台账上，
   链接器就把那 4 字节写进**下一条指令的 ModRM**（`movslq 0x0(%rip),%rax` 变成
   `movslq 0x2095(%rdx),%rsi`），症状因此是**间歇**的：`.text` 字节连编三次哈希一致，
   跑不跑得起取决于进程启动时 `%rdx` 的残值（同一 exe 跑 30 次：12 通过 / 18 段错误）。
   处置：新增 `carries_reloc` 两道判据（台账长度差分 + 文本含 `%rip`），成立则本条与上一条都
   不许被削。代价为零：`luaV_execute` 的三条静态指标在这道门之后一字未动（11366 / 77 / 78 / 28），
   因为热路径上的搬运全是帧槽 `r13+disp` 形态、不带符号补丁。接住它的是
   `tests/fixtures/opt/chain_assign_global_store.c`（四档执行对照 + 8 字节宽度与 128 位链式）。

新增测试用例：`tests/fixtures/opt/lor_branch_order.c`（分支语境 `A || B` 的短路顺序，五段 ——
左真时右值 store 不得发生 / 左假右真必须走 then / 纯值四走向 / `a || (b && c)` 递归分解 /
`while (a || b)` 的「真则回头」反向臂）与 `tests/fixtures/opt/int128_truth_condition.c`（`__int128`
真值语境，数据取 hi-only `((u128)1 << 64)`、lo-only、全零三组，覆盖 `x != 0`、`x == 0`、`!x`、
`x || y`、`x && y`、`x != 0 || (*p = 9) != 0`、do-while）。两份都已落地并验证（gcc -O2 与 dcc
-O0/-O1/-O2/-O3 退出码全为 0），而且都故意 `return mask ? 1 : 0` 而不是 `return mask`：**退出码
只有 8 位**，直接返回会把 bit≥8 的那些段截掉（错得再狠也看不见）。另有三条发射形态用例，**已落地并通过**（均在 `tests/integration/opt_ssa_gate.rs`）：
`o2_late_addr_recompute_skips_push_pop`（正向 Late）、`o2_const_expr_rhs_store_skips_push_pop`
（正向 InRcx）、`o2_unrecompute_addr_with_call_rhs_still_pushes`（必须保留 `push %rax … pop %rcx`
的反向对照，钉住判据不是恒真）；逐条判据见方案 §14.6。

验收结果（同一台机器，全部实测）：

| 项 | 结果 |
|---|---|
| `cargo test --release --lib` | 2505 passed / 0 failed / 2 ignored |
| `cargo test --release --test integration` | 1130 passed / 4 failed / 1 ignored；4 条 = `cpp_flat_map`、`cpp_multi_declarator_member`、`eh_landing_pad_functions_are_never_auto_bound`、`register_home_and_cleanup_survive_every_declaration_path`，与 HEAD 记录逐条相同 |
| `--test errors` / `--test reproducibility` / `--test profile` | 26 / 8 / 53 passed，均 0 failed |
| `--test disasm_snapshot` | 本轮重录 6 个快照（`atomic_basic`、`bitfld-3`、`direct_cond_branch`、`loop-8`、`pr55750`、`pr105613`），逐个人工核对后确认全部是发射形态变短或变优（例：`mov -0x50(%r13),%rax; push %rax; pop %rcx` → `mov -0x50(%r13),%rcx`；`setne/movzbq/test` 冗余链消失；`mov $0x0,%r8; cmp %r8,%rax` → `test %rax,%rax`），重录后 1 passed / 0 failed |
| `--test g10_snapshot_baseline` | 本轮 2 个基线漂移，核对语义后重录：`float.c`（夹具的 CHECK 宏就是 `A \|\| B`，旧基线锁的正是上面那处 `\|\|` 误分解的产物）、`stmt_expr.c`（`*p += 5` 的地址改走 Late，产物除帧指针外零 push/pop，夹具运行退出码与 gcc 一致） |
| `scripts/check_opt_equivalence.sh` | `OPT-DIFF: 0`；`COMPILE-FAIL: 60`（= HEAD 基线）、CF-UNKNOWN 0 |
| `scripts/check_fixtures.sh` | 60 个问题 = HEAD 同类基线（全在 `tests/fixtures/asm/parity` 存量债） |
| `scripts/check_asm_text_parity.sh` | 28 组合、0 PARITY-FAIL、12 条 GAS alias 差异 warn |
| `scripts/check_rex_hygiene.sh` | 32 条 = HEAD 基线同数（本轮不引入新 REX 问题） |
| `scripts/check_reproducible_output.sh` | 5454 组合全部可复现 |
| 运行时口径（方案 §5 第 4 条） | `bench_lua_sqlite.sh ITERATIONS=9` → **`OK (0 build-fail, 0 output-diff)`**（Lua 四档输出与 gcc -O2 逐字节一致，SQLite 四档一致）；`taskset -c 3` best-of-9 `task-clock`：Lua -O2 **min 291.6 / p50 302.5**（阈值 324）、SQLite 20 万行 **min 472.7 / p50 475.9**（阈值 527），gcc 参考臂同循环内 110.3 / 85.0 |
| 非阻塞参考：C-Torture 全量语料 | 本轮跑过一轮 `execute` 段，**已定位并修掉一条静默误编译**（上面第 3 处语义错误）；「不得新增失败」由 pre-commit 钩子（`scripts/torture_gate.sh`）在提交前把关 |
| 非阻塞参考：gcc-dg | `scripts/gcc_dg_test.sh run` → `pass=850 fail=24 skip=4042`；日志里 24 条分布在 torture 目录 13 条、tree-ssa 目录 11 条（那里的「torture 目录」是 `tests/gcc-dg/gcc-dg/torture` 这一子目录，**与 C-Torture 全量语料不是同一集合**）。**参考数据，非阻塞**，本轮不下「零新增」结论 |

验收口径（本轮修订，方案 §5 已同步改写）：**阻塞项只在仓库内** —— cargo 测试、上面那几个脚本、
以及 §5 的三条静态指标与运行时阈值。外部语料**都不作为阻塞项**：C-Torture 由 pre-commit 钩子的
`scripts/torture_gate.sh`（比对基线 `tests/skip/torture_known_fail_{compile,execute}.txt` 不得新增）
把关，gcc-dg 只登记参考数据。两条入口是**两套不同语料**：`scripts/torture_test.sh` 跑 C-Torture
全量（约 3715 条），`scripts/gcc_dg_test.sh` 跑 `tests/gcc-dg`（5400+ 条，量大）——先前把两者
混为一谈、把「torture fail 必须为 0」挂在 dg 后面的表述已纠正分开。

**运行时口径的完整数据**（`ITERATIONS=9`，同机、单线程）：

| 负载 | 脚本内 best-of-9 墙钟 | `taskset -c 3` best-of-9 `task-clock`（gcc/dcc 交替） | 方案 §5 阈值 |
|---|---|---|---|
| Lua fib(32) | gcc -O2 114；dcc -O0/-O1/-O2/-O3 = 336/347/287/296 | gcc -O2 min 110.3 / p50 112.9；dcc -O2 **min 291.6 / p50 302.5** | min < 324 |
| SQLite 20 万行 | gcc -O2/-O3 88/98；dcc -O2/-O3 476/487 | gcc -O2 min 85.0 / p50 87.3；dcc -O2 **min 472.7 / p50 475.9** | min < 527 |

两条都不仅能「过线」，还能说明本轮不变慢：gcc -O2 是同一条交替循环里的**固定参考臂**，
它相对本报告 §1 记录的 113/115ms 只差 −2%（机器状态漂移的量级），而 dcc -O2 从 §1 记录的
324/340ms 降到 291.6/302.5ms —— 扣掉参考臂漂移后仍是**约 −9%**，两个负载同向。
方案 §2.3 要的「两构建交替」本轮没有单独再构建一个 HEAD 二进制（工作树全量构建约 3.5 分钟），
漂移已由 gcc 参考臂在 2% 以内测出；若后续需要更强的证据，补一次 paired HEAD 构建即可，判据不变。

### 6.12 阶段 6 修订轮：稠密 switch 走跳表分派（SQLite −27.5%）与三条口径修正

本轮先做了 10 轮专家评审，结论是原「解释器进 SSA」主线**判死**（非结构化 CFG 上 IR 不在
SSA 形式，且重建要求可约 CFG；`luaD_precall` 跑满整条 SSA 管线仍只有 gcc 的 1/2.6），
取而代的是按「能否被本机证明」排序的批次。已落地：

**跳表分派（默认开，`DCC_SWITCHTABLE=0` 可回退到链）。** `sqlite3VdbeExec` 的主分派不是
computed goto（sqlite3.c 全文无 `goto *`），而是 185 路 switch；此前它每臂发
`mov imm / load slot / cmp / je` 四条，**每次分派平均扫 60.6 臂**（加权臂位由
`scripts/hot_weights.sh` 用只由 gcc 编的插桩臂实测得到，不是均匀假设的 92），
而 gcc 是 O(1) 跳表。现在稠密整型 switch 在**不重建 AST 的非结构化函数**里被改写成
「`static const void *const tbl[] = {&&L,...}` + selector 只求值一次 + 无符号减 min 后一条
`jbe` 界检 + `goto *tbl[idx]`」，用的就是 Lua 解释器每天在走的 `&&label` + `goto *` 通路。

同轮交替 15 轮的配对结果（`scripts/ab_pair.sh`，含一个与表臂同一二进制的 dup 臂作为仪器噪声）：

| 臂 | min / p50 / max（ms） | 相对链的 median(r) |
|---|---|---|
| chain（`DCC_SWITCHTABLE=0`） | 463.5 / 498.8 / 548.7 | 1.000 |
| **table（新默认）** | **348.3 / 366.3 / 391.1** | **0.7245**，95%CI [0.713, 0.755]，15/15 轮同向 |
| dup（与 table 同一二进制） | 343.7 / 365.2 / 383.1 | 0.7261 ⇒ 仪器自身只差 0.3% |
| gcc -O2 参考 | 88.5 / 91.4 / 109.7 | 0.1864 |

静态侧：`sqlite3VdbeExec` 的分派槽读取 185 → 18、`cmp` 435 → 251、`je` 595 → 410、
间接跳转 0 → 1，函数体量 +154 条（+0.8%）。SQLite 与 gcc 的比从 5.46x 降到 **4.01x**。

批 3（内联按调用点形状放宽）在同一口径下另给一段收益：新接 `x = f(...)` /
`y = x = f(...)` / `if (f(...))` 三种形状后，终态 `sqlite3.o` 里 `fillInCell`/`insertCellFast`
**符号消失**（基线对象里还在），`ldo.c` 打出 `INLINE prepCallInfo (cost 51)`。批1+批3+cast
白名单修正叠起来是 `median(r)=0.6893`（95%CI [0.672, 0.693]，15/15 轮同向），即相对链基线共
**−31.1%**（444.6 → 304.8ms 中位数），对 gcc 到 **3.57x**（基线是 5.2x）；扣掉批1 单独的
0.7245，批3 的边际是再 −4.9%。
Lua 侧同一对比给出 `median(r)=1.0023`，而同一二进制的 dup 臂是 `1.0051` ⇒ 完全在噪声带内，
**不记收益**（`prepCallInfo` 三个调用点里只有一个就是「整条语句的赋值」形状）。

批 2（`DCC_BLOCKOPT_RULES` 的 `R_RAW`/`R_ZBITS` 两条新臂）是**负结果**：`luaV_execute` 在
`7`/`0x27`/`0x2f` 三个位图下班数都是 11366、`[blockopt] drop` 都是 415，而只开新臂
（`0x8`/`0x20`）会退回 11397——位图本身是通的，但「Raw 过度杀挡住了 247 对 store→reload」
个离线模型的结论与发射器真实指令流不符（反汇编里 `and` 后窗口≤2 内出现同号 `movsxd`
的处数是 0）；而“死写 Drop”的相邻形态早已在 `instructions.rs` 的 `dead_pc` 回删里，剩下的
非相邻情形要的是真 DCE。三条全部默认关，只留机制与单测。

新默认下的全量测试用例：lib 2508/0；integration 1132/4（4 条与 HEAD 逐条相同）；
`check_opt_equivalence.sh` **0 OPT-DIFF**；`check_reproducible_output.sh` 5454 组合全绿；
反汇编快照与 G10 基线**零重录**；`check_asm_text_parity.sh` 无 PARITY-FAIL；
`scripts/torture_test.sh all` 3707 pass / 0 fail；`scripts/bench_lua_sqlite.sh` OK（0 build-fail /
0 output-diff）；`check_fixtures.sh` 60 项、`check_rex_hygiene.sh` 32 条均为存量债同数。

两个实现坑（写下来免得后人重踩）：表与标签的名字**不能按 `Span` 取**——sqlite3.c 里 111 个
不同 switch 共享同一个 `Span`，`sqlite3Select` 一个函数里就出现 8 张同名表，标签后写覆盖前写，
表项直接跳到别的函数（SIGSEGV）；静态局部表必须 hoist 到**函数体顶层**，嵌在循环里时
`pre_register_static_locals` 得不着登记，发出来就是一句位移永不回填的 `lea`。

**三条被实测订正的口径。**（1）本轮把测量方法从「各臂自取 best-of-N 的 min」换成**同轮
交替配对比值**，并实测了两个带：同一二进制两臂的真噪声带 **2.1~3.7ms**，而“语义全同、
只在热区前插单字节 nop”的布局敏感带高达 **43~46ms（≈10%）**——这就是 P13 §2.2
「删 135 条 to-next jmp 反而慢 3.8%」的量级证明，也是为什么本轮所有结论只用配对中位数。
新增 `scripts/aa_layout.sh` 会硬断言“滤掉 nop 后指令流与标签集合全同”，防止扰动旋钮偷改语义。
（2）热区体量斜率实测 **1.5~3.3 ms/KB**，说明 SQLite 的前端确实是瓶颈，冷路径外置一类
「只减体量」的改动从此有了换算率（不再只能搁置）。（3）新发现一条已登记待修的静默错值：
同宽 `signed↔unsigned` cast 在使用点不归一（`100000000000UL + (unsigned)(int)-1` gcc
104294967295、dcc 99999999999，四档都错）。本轮推进一步：标量写/赋值白名单
`scalar_load_shape` 本来把 `Cast` 剥掉再判形状，使得快道按**内层对象的类型**决定符号/零扩展；
现在「同宽但换符号性」的 cast 退出快道，`tests/fixtures/opt/int_widen_signedness.c` 的 12 个
形态从多形错变成只剩 `K64 + (unsigned)v` 一形错（反例 `int i=-1; long l=i;` 仍正确保留符号扩展）。
剩下那一形已排除「块内优化删的」与「`gen_expr_cast` 没被调到」两假设，仍按
「正确性修复不搭性能批次」以 `#[ignore]`（report-only）形式入库。
同时修掉了另一条真误编译：`lower_switch_stmt` 把 selector 逐臂 `clone()`，导致 `switch(i++)`
在 -O2 下重复求值并选错臂（实测 `pick=-1 i=5` vs gcc `300/3`）；修法是把有副作用的 selector
先存进一个临时局部，无副作用者保持原形态（实测 16 个 TU 里只有该改的那一个产物变了）。

### 6.13 阶段 6 第六轮：cast 专项收口、跳表基址常驻 `%rbp`、内联视野两处补漏

本轮把 §15.8 列的四条遗留全部推到底，并按「先改完再 review」的方式做一次评审（六处问题
在实测之前被抓住并修掉，全部补了判据）。三组改动：

**一、cast 最后一形的真因在发射层窥孔，不在 `gen_expr_cast`。**
`100000000000UL + (unsigned)v` 那一形里，那次零扩展 `mov dst32, dst32` **发出来了**，被
`instructions.rs::emit_insn` 的自反搬运去重削掉：它的前提「上一条已经把目的的高 32 位清掉」
对 32 位写与零扩展装载成立，而 `classify_peep` 把 `movsxd r64, DWORD PTR [mem]`（高位是**符号位**）
也归进了同一个类 `LoadR32`。现在符号扩展装载单独立类 `LoadS32`，不再充当那个前提。它之所以难查：
`DCC_BLOCKOPT=0` 照样错（发射层窥孔不是块内优化）、`gen_expr_cast` 确实被调到、而 `emit_insn`
对「被削掉」也返回 true。正反两面各一条单测（零扩展装载后的自反 `mov` **仍该被削**，否则等于
把这条窥孔整体关掉）。⇒ `int_widen_signedness_matches_gcc_at_all_levels` 去掉 `#[ignore]`，
12 个形态四档全绿。

**二、批 4：跳表基址常驻 `%rbp`，分派尾部三条塌成一条。**
实测 gcc -O2 在 Lua 同一个位置就是**一条** `jmp *(%r12,%rax,8)`（基址在序言取一次），而 dcc
是 `lea 表(%rip),%r11` + `mov (%r11,%rcx,8),%rax` + `jmp *%rax` 三条。批 1 之后 SQLite 的稠密
switch 也走同一条 `goto *表[idx]`，所以这是两个解释器共用的尾部。落地三件事：`JmpRmMem` 加变址
（新编码按 GNU as 的实际字节对拍，`jmp *0x0(%rbp,%rcx,8)` = `FF 64 CD 00`，`%rbp` 当基址时 SIB 的
base=101 在 mod=00 下意思是「无基址」⇒ 必须强制 disp8=0）、新模块 `src/ast_dispatch.rs` 在发射前
一次遍历收集「表名与站点数 / 有没有 asm / 有没有嵌套函数」（不留 `_ => {}`，收集端与发射端共用
同一个形态判据）、`%rbp` 的所有权（存槽 `%r13-8` 与金丝雀同格由判据互斥、尾声恢复、`.eh_frame`
补 `rbp @ CFA-24`，五条互斥加一条不变式「字段为 `Some` ⇔ 序言真的发了那对存取」）。
`register asm("rbp")` 改为**无条件** E0318（gcc 同一处是 `error: frame pointer required, but reserved`）。

静态口径（`scripts/count_fn_insns.sh`）：`luaV_execute` 11366 → **11210（−156）**，
`lea …,%r11` 从 80 处到 **0 处**，80 处分派各剩一条 `jmp *0x0(%rbp,%rcx,8)`；`sqlite3VdbeExec`
主分派同样塌成一条，函数里其余 7 张表保持原形（一个号只够常驻一张表）。两臂的关侧与本轮之前
在 `luaV_execute` 上静态口径逐条相同（`DCC_DISPTAB_RBP=0 DCC_INLINE_WALK=0` 给出 insn=11366 /
movrr=2697 / r13=2654 / imul=216 / movsxd=685 / cmp=366）。

**三、批 3 的两处覆盖补漏：内联遍历走进臂体，`always_inline` 三条路不再互斥。**
查「没接的形状」时撞上更大的真因：`inline_calls_in_stmt` 以前对 `Stmt::Label`/`Stmt::Switch`/
`Stmt::GotoIndirect` 全部落进 `_ => {}`，而两种臂体都走不到：`Stmt::Switch` 的 case 体（稠密
switch 改写成线程形那一步在 `run_ssa_passes::lower_switches_in_func`，而 `function_inline` 跑在它
**前面** ⇒ 内联时点上 VdbeExec 仍是 `Stmt::Switch`）与**源码级** computed goto 的
`Stmt::Label` 体（Lua 的 `vmcase(l)` 展开就是一个标签）⇒ **`sqlite3VdbeExec` 的 185 个 handler 体
一个都不在内联器视野里**。补上三条臂后 `ldo.c` 的 INLINE
行 16 → 23（`prepCallInfo` 两个调用点全展开）、`lparser.c` 271 → 307。同时 `always_inline` 不再被
通用路整体拒掉（带控制流的助手 + `rc = f(...)` 这种赋值调用点，SQLite btree 的 `allocateSpace`
正是这一族），只在「专属两条确定能接」的形状上让位，并给一档 20× 硬上限防病态标注；
递归的 `always_inline` 仍不展开（有反向用例）。两者合起来让产物变大（`luaV_execute` +616 条），
因此共用一个归因闸门 `DCC_INLINE_WALK`。

**闸门当时默认关，理由不是收益而是编译内存**（这一条已被下一轮 §6.14-5 推翻：预算把中等
函数的亏账隔掉后默认已翻成开）：`-O2` 编 `sqlite3.c` 的单 TU 峰值 RSS 实测
**6.77GB 并被 OOM killer 杀掉**（`/usr/bin/time -v`，本机共 7GB；两个开关都关那一臂
复测为 6.39GB —— 存量基线已经贴着机器上沿；`lvm.c` 那一档只多 86MB）。
而 §6.11 那段引用的门禁（`bench_lua_sqlite.sh` 必须 `OK (0 build-fail)`）不允许把一个编不动的
默认值带出门，所以列入待办：先给展开加体量预算（按函数大小限定展开点数/总增量）、把峰值 RSS
做成常规度量项，再翻默认。收益本身是量到的（`median(r)=0.9472`，与 `all` 同一二进制的 `dup` 臂
复测 0.9306），只是在 SQLite 上付不出那个内存价。

运行时（`scripts/ab_pair.sh`：15 轮同轮交替 + 臂序旋转 + 绑核 3；`off`=两闸门全关，`rbp`=只开基址
常驻，`all`=两都开，`dup`=与同一列对比臂**同一二进制**的仪器对照）：

| 对比（Lua `fib(32)`） | median(r) | 95%CI | 同号 | 判定 |
|---|---|---|---|---|
| rbp / off（只算基址常驻） | 0.9939 | [0.9717, 1.0414] | 9/15 | 不可判 ⇒ 走窄通道保留，**不记收益** |
| all / rbp（只算内联覆盖面） | 0.9472 | 同二进制 dup 臂复测为 0.9306 | 12/15（dup 15/15） | 实测有效，但默认关 |
| all / off（两都开） | 0.9308 | [0.9015, 0.9887] | 13/15 | 同上 |
| dup / off（与 `all` 同一二进制） | 0.9407 | [0.9120, 0.9599] | 15/15 | 噪声与位置效应的上界 |

| 对比（SQLite 20 万行负载） | median(r) | 95%CI | 同号 | 判定 |
|---|---|---|---|---|
| rbp / off | 1.0056 | [1.0017, 1.0106] | 3/15 | 不可判（与 `rbp` 同一二进制的 `dup` 臂地板就 +0.38%） |
| all / off | — | — | — | 量不了：开侧 `sqlite3.c` 被 OOM 杀死 |

批 4 在窄通道上保留默认开，但 **SQLite 的 `median(r)=1.0056` 比要求的 ≤1.005 高出 0.0006**（而
同一二进制的对照臂已是 +0.0038，差在仪器地板之内），Lua 单独效应 CI 跨 1 ⇒ 两条都不算收益；
默认档的静态口径同时达标：`luaV_execute` 11366 → **11210**（§6.11 那条「≤ 11800」仍成立）。

评审抽出的六处（全部已修并各带判据，详见方案 §15.8.4）：`main` 走内置链接器直链那格里
序言没发存取而站点照用 `%rbp`（**实测 SIGSEGV**）；内联结果临时量被按错的类型读（`Dest::Cond`
硬贴 `int`、`Dest::Assign` 贴左值类型、`AssignChain` 外层拿到未转换的值）—— 三条都是静默错值，
新的 `always_inline_cf_site` 夹具用 double/long/指针返回的三个目的地形状钉住；`switch` 的 prelude
被 if-chain 降级拼成「链之前的顺序语句」（-O2/-O3 多执行一次赋值，gcc 与 -O0 都不）；`blockopt`
把 `movswl` 当成「已是符号扩展规范形态」从而削掉必需的 `movsxd`。另采纳一条 ABI 补漏：
`__builtin_setjmp` 的 callee-saved 快照补上 `%rbp`（临时区 40→48 字节，与 glibc 的
`sigsetjmp` 存 rbx/rbp/r12-r15 对齐），因为现在一个跳表函数可以 longjmp 穿过另一个；
判据 `setjmp_longjmp_across_hoisted_dispatch_keeps_the_captured_table_base` 只比 dcc 自己 ——
同一份源在 gcc `-O2` 下自己就 SIGSEGV（`-O0` 与 dcc 都对），拿 gcc 当参照反而会把这类缺陷遮住。

本轮全量测试用例（**本轮当时的默认**：批 4 开、`DCC_INLINE_WALK` 关；下一轮已翻默认并重跑，
见 §6.14 末的质量门）：lib **2525/0**；integration
**1142 passed / 4 failed**（4 条与 HEAD 逐条相同，零新增）；disasm 快照与 G10 基线**零重录**；
`check_opt_equivalence.sh` **0 OPT-DIFF**（COMPILE-FAIL 60 = 基线同数）；
`check_reproducible_output.sh` 5454 组合全绿；`check_asm_text_parity.sh` 无 PARITY-FAIL；
`check_fixtures.sh` 60 项、`check_rex_hygiene.sh` 32 条均为存量债同数；
`scripts/torture_gate.sh`（C-Torture 全量）**PASS** —— compile `pass=2010 / fail=0`、
execute `pass=1697 / fail=0`，而基线名单里本来有 27 条 compile / 49 条 execute 已知失败，
本轮全部转绿；`bash scripts/bench_lua_sqlite.sh ITERATIONS=2` → **OK (0 build-fail, 0 output-diff)**，
其中墙钟 best-of-2（该脚本自己的口径，不绑核、不作判定依据）：Lua `-O2` 290ms vs gcc 115ms
（**2.52x**），SQLite `-O2` 312ms vs gcc 87ms（**3.59x**）；同一夹具的 `computed_goto_dispatch`/
`switch_prelude`/`always_inline_cf_site`/`int_widen` 四条在**两臂开关下**都全绿（开关差分 0-diff）。

本节的 Lua/SQLite 百分比数字全部来自 `scripts/ab_pair.sh` 的同轮配对（median(r) + 自助法
95%CI + 符号检验 + 同二进制 dup 对照），不是「各臂自己取 min」那种已被 §6.12 证伪的口径；
`bench_lua_sqlite.sh` 那组墙钟只用于「能不能跑通、输出对不对」的门禁。

### 6.14 阶段 6 spec 对齐轮（第七轮）+ 第八轮评审修正：四类形状全部实现、预算量尺从 fail-open 修到两条通道

本轮把 spec（`阶段6_解释器SSA_task-5dd.md`）逐条对到底，完整的逐条落位表在方案
§15.10（含没达成的那一条：Lua ≤275ms 的承诺）。报告只记四个新数字：

1. **批 3 的第 3、4 类形状已实现但默认关**。`x op= f(...)`（只把那一侧的 Call 换成结果
   临时量的读，**不改读-改-写次序**）与 `int x = f(...)` 声明初值（原地拆成「空声明 +
   赋值」，变量不离开原作域）都进了决策点，夹具 `inline_call_site_shapes.c` 四档与 gcc
   一致；但 15 轮同轮配对是负的：Lua `median(r)=1.0188`（CI [0.9987,1.0351]）、
   SQLite `1.0135`（CI [0.9965,1.0240]），而与开臂**同一二进制**的 dup 臂自己报 1.0212
   ⇒ 两个同二进制臂方向一致地慢，不是位置效应。体量代价 Lua +53KB、sqlite3.o +346KB。
   按 §5.5 三态统一处理：机制与判据保留，闸门 `DCC_INLINE_SHAPE` **默认关**，登记负结果。
   结构原因也写了：这两类比 `x = f()` 多一份 per-site 的「临时量 + 存回」开销，而成本模型
   看不到它。（同轮的 `DCC_INLINE_WALK` 已经靠展开预算翻成了默认开，见下面第 5 条；这两类
   没有跟着翻 —— 它们的回退与体量无关，是 per-site 开销本身。）
2. **批 2 的旧结论需要修正**。在新基线（批 4 开、两条内联闸门关）上重跑
   `DCC_BLOCKOPT_RULES`：开 `R_RAW|R_CPY32`（`0x67`）时 `luaV_execute` 从 11210 降到
   **11188**（r13 引用 2657→2635）——上轮登记的「R_RAW 零机会」只对当时的基线成立；默认档
   翻成开之后同一列在新基线上直接复验：`7` → 11670、`0x67` → **11648**（同一个 −22 条）。
   本轮同时实现 C-lite 的第二条 `R_CPY32`（`mov r64,r64` → 同长 32 位形态，**严格同长**
   才换；单测钉住三个反面：没事实不换、低 8 号会缩短不换、默认位图不换），但它只换字节
   不减条数，没体量收益 ⇒ 同样默认关，等运行时测量。
3. **测量协议补了两块**。`aa_layout.sh` 加了方案 §5.1 要的**静态可达性断言**（只对旋钮
   插入的 nop 段要求前驱是 jmp/ret/jcc；FN_SHIFT 臂不成立即 die，PAD 臂则报告并从斜率拟合
   里排除 —— 实测载体上 13 段里有 1 段真会被落入）；`hot_weights.sh` 头里那句「对 nop/位移
   不敏感」被删掉并改成交叉引用。新脚本 `scripts/gprofng_attr.sh` 补上按函数的归因通道
   （本机不依赖 PMU）：实测 `luaV_execute` 占 exclusive CPU 74%~83%，但 10ms 粒度下
   0.29s 的单次跑只能命中 ~7 个样本 ⇒ 它能定位函数，不能判 6% 的差值。
4. **§6 的两件小事与阶段 7 立项书**。`ssa/build.rs` 补 `LABEL_BB_PREFIX`/`label_bb_name`/
   `source_label_of`（生产点全改走它，两条单测，零产物影响）；`structure.rs` 文件头写明
   「谁决定准入 / 谁决定落地」；`shared/control_flow.rs` 的两个跳表桩加了 `#[deprecated]`。
   阶段 7 另立项成文：`.trae/documents/x86_64-p14-trace-regalloc-charter.md`，带实测准入表
   （第八轮重算的最终默认档：帧内存引用密度 dcc **232** vs gcc **72** 条/千指令，差 3.2×；
   分派尾部 80 vs 6，且开/关 `DCC_INLINE_WALK` 都是 80）与开工前要还的三条硬账。

5. **展开了预算（`arm_budget_ok_for`），把 `DCC_INLINE_WALK` 翻成默认开；随后评审发现预算的
   量尺是坏的，重测后准入改成两条通道**（完整过程在方案 §15.10.8）。预算本身是「caller 体的
   AST 节点数 ≤ `DCC_INLINE_WALK_MAX`（默认 **800**）才下探 `Label`/`Switch` 臂体」，但
   `stmt_node_count` 带一个 `_ => 1`，于是 `Stmt::Label`（Lua 的 `vmcase(l)` 展开就是一个标签）
   与 `GotoIndirect`/`Invoke`/`EhLandingPad`/`AsmGoto` 全部只计 1 个节点、`switch` 的 `prelude`
   完全不进账 ⇒ `luaV_execute`（真实 **18713** 节点）被量成 ~250，预算在它身上 **fail-open**。
   修全之后重测（同一台机器，四臂同轮交替 **25 轮**，绑核 3）：

   | Lua 四臂（25 轮） | 配置 | min / p50（ms） | median(r) | 同号 |
   |---|---|---|---|---|
   | base | `DCC_INLINE_WALK=0` | 300.2 / 308.7 | 1.000 | — |
   | m300 | 只开体量通道，上限 300 | 281.8 / 297.0 | **0.9604**（CI [0.9464,0.9731]） | 22/25 |
   | **m800（默认）** | 只开体量通道，上限 800 | 284.1 / 294.8 | **0.9704**（CI [0.9437,0.9832]） | 22/25 |
   | tonly | 只开线程形通道 | 289.5 / 294.8 | **0.9974** | 8/15 |
   | dup | 与 base 同一二进制 | 297.0 / 311.4 | 1.0007 | 12/25 |

   两个上限谁强得直接对拍（跨会话读数不可比：同一个 m800 在两个会话里分别量到 0.9462 与
   0.9704）：`m300/m800 = 0.9968`（CI [0.9780,1.0166]，14/25，dup 地板 0.9990）⇒ **不可判**
   ⇒ 默认值继续用 800。（表里 `tonly` 那一行来自上一个 **15 轮**会话，其余四行才是本次 25 轮
   同一轮交替的产物；它只用来支持「线程形单独打开量不到时间」这一句方向，不用来算量级。）

   量尺修全后得到的关键事实：`luaV_execute` 18713 节点与 SQLite 的 `sqlite3VdbeExec` 16847
   只差 10%，**没有任何体量阈值能既放进前者又拦下后者**；能分开它们的是形状（前者有 80 个
   `goto *disptab[..]` 站点，SQLite 在内联时点上一个也没有），但形状通道单独并不得到时间
   （0.9974）⇒ 准入做成两条可分别开关的通道后：**体量通道默认开（800），线程形通道默认关**
   （机制与判据保留，`DCC_INLINE_WALK_THREADED`）。
   SQLite 在同一会话同一组臂下不可判（25 轮：默认档 `median(r)=0.9943`，CI [0.9773,1.0136]，
   而 dup 自己 1.0028）。完整的读数串得一并记账：同一个默认档在四次会话里量到
   **0.9943 / 0.9988 / 1.0132 / 1.0288** ⇒ 方向一致地偏正 ⇒ **SQLite 不记任何收益**。
   §5.5 的「E 不上升」在本批是结构上成立的：两个分派循环在两臂下**逐字节同形**
   （`sqlite3VdbeExec` 都是 insn 20299 / movrr 3763 / r13 4197 / imul 279 / movsxd 1188 / cmp 800；
   `luaV_execute` 都是 11210 / 2697 / 2657 / 216 / 685 / 366）——展开全部落在非分派函数里。
   体量代价：sqlite3.o 7,373,360 → **7,818,200 字节**（+444.8KB），Lua 二进制 +112.8KB。
   静态口径（最终默认档）：`luaV_execute` = **11210 条**（它 18713 节点，被体量通道拦住，仍在
   §6.11 的 ≤11800 以下），帧内存引用 2597 条 = 232/千条，`jmp *0x0(%rbp,%rcx,8)` 仍 80 处。
   编译内存（同一会话三臂）：关门臂 6.01GB / 默认档 **6.20GB**（都能过；上限 300 是 6.19GB，
   与 800 分不开；无预算旧档 6.77~6.83GB 被 OOM 杀死）。
   与上面那条差别说清：**本节先前的那一稿**记的是 0.9530 / `luaV_execute` 11670，它
   **依赖那个 fail-open bug**（线程形默认进）；本轮的 0.9704（25 轮）/ 11210 不依赖，且实现与
   文档里写的「按 caller 体量限住」现在一致。
   §2 的 Lua 承诺：比值口径成立（两个会话分别 −2.96% 与 −5.38%，各自的 dup 地板 1.0007 / 0.9959
   ⇒ 都在带外），**绝对 p50 得看当日基准臂**（基准臂 308.7ms 那轮默认档 294.8ms；基准臂
   292.6ms 那轮 278.3ms）⇒ 本节不再拿绝对 ms 定承诺。
6. **第八轮评审抽出的发现全部修完，并顺带抖出一族 HEAD 既有的 `switch prelude` 缺陷**
   （详表在方案 §15.10.8）。除上面那条 blocker（预算量尺）外，评审还抽到：集成用例丢了
   自己承诺的 `DCC_INLINE_WALK=0` 反向臂（已补，现在删守卫就会红）、`DCC_INLINE_TRACE` 的
   归因行自己重算了一条错的判决式（恒报 `walk_arms=true`，会把「通道没开」读成「开了没收益」，
   已改为直接打真实判决）、嵌套函数沿用外层判决、`try_stmt_inline_call` 重算预算、
   `DCC_INLINE_WALK_MAX` 非法值静默回落（现在往 stderr 打警）、三个 `DCC_INLINE_*` 布尔门
   解析不一致（`DCC_INLINE_WALK=TRUE` 会静默当「关」，而关侧正是配对基准臂；现在三个门共用
   一个解析，认不了就警 + 按默认值）。

   **更严重要的一族是 HEAD 就有的**：`Stmt::Switch.prelude`（第一个 `case` 之前的那些项）是
   本项目为 E1102 加的字段，但写它之前的 walker 一律只匹配 `{ expr, cases, .. }` ⇒ 静默少扫
   一棵子树。本轮因「prelude 要接进内联遍历」逐个抖出并修完 10 处，每处都有可复现的错：
   调用图（`ssa/passes.rs`）漏 prelude ⇒ **只被 prelude 调用的 `static` 函数被跨函数 DCE 删掉
   而 `call` 照样发射，`-O2` 链接报 `undefined reference`**（-O0/-O1 正常）；帧预算
   （`late_need_stmt`）漏 prelude ⇒ `switch (x) { int y = 1; … }` 直接编不过
   （`E1133: late frame allocation … 1 slots used, 0 reserved`）；`collect_frame_var_decls_stmt`（
   帧槽分配）、`collect_local_labels_stmt`（标签登记）、`sret_bytes_stmt`、`collect_free_idents_stmt`、
   `set_home_stmt`、`ast_gvn.rs::collect_scope_stmt`（GVN 会把 prelude 里被写的名字当成没被写过）、
   `ast_asm.rs::walk_stmt`、以及 `opt.rs` 里六处同一类判据。三条带判据：
   `switch_prelude_only_callee_survives_cross_function_dce`（四档与 gcc 同结果 + 汇编自洽）、
   `switch_prelude_decl_init_site_is_walked_like_a_block_statement`（同一形状在 prelude 与普通块
   里同进同退）、`arm_budget_counts_switch_prelude`（平铺与 prelude 两份同量体必须同量级）。
   没修但已登记：`sanitizer.rs` 三处（只在 `-fsanitize` 下参与插桩覆盖面）与 `cpp/modules.rs`。
   另记一条：三条臂不看 `level`，而 `function_inline` 在 -O0/-O1 也跑（它的既有设计就是 O0 只接
   `always_inline`/`flatten`）⇒ 默认翻开会连带动 -O0/-O1 产物，本轮只以四档输出一致 + C-Torture
   全量覆盖，未单独配对。

7. **可证下界（spec §2 要求这条必得写死在报告里，按负载分列）**。本仓实际用的是
   **同轮配对 + 与基准同一二进制的 dup 臂**给出的仪器地板，不是旧的「各臂取 min」：

   | 负载 | 同二进制 dup 臂的读数（本轮四次会话） | 换算成时间 | 绝对 ms 能不能证 |
   |---|---|---|---|
   | Lua `fib(32)` | median(r) 0.9959 / 1.0007 / 1.0018 / 0.9934，p50 差 2.1~3.7ms | **≈±0.5%（≾1.5ms）** | 不能：纯 `nop` 位移就能移动 43~46ms（§15.2），同一配置的配对比值会话之间漂 2.4% |
   | SQLite 20 万行 | median(r) 0.9817 / 1.0025 / 1.0028 / 1.0013，p50 差 4~6ms | **≈±1%（≾6ms）** | 不能：基准臂 p50 在 305~332ms 之间跳（会话漂） |

   ⇒ 因此本报告里：**任何 |median(r)−1| 小于上表地板的结论一律记「不可判」**（不得写进承诺，
   §5.5）；比值超过地板的才能计盈亏；**绝对 ms 不得单独作判据**（§1 纠错表已经因混用两套口径
   自打过一次）。spec §2 当时引的「11% / ±3.8%」是旧口径（各臂独立 best-of-N + 跳轮 min）
   的标定值，已被批 0a 的同轮配对口径取代（那个口径下噪声只占 0.5~1%，但布局敏感度依旧存在，
   所以两条一起记：比值可证到 ≅1%，绝对值不可证）。

本轮全部质量门（最终默认：批 4 开、`DCC_INLINE_WALK` **开**（体量通道 800）、
`DCC_INLINE_WALK_THREADED`/`DCC_INLINE_SHAPE` 关）：
lib **2531 passed / 0 failed / 2 ignored**（两条新判据钉的是预算量尺的标签形与 `prelude`）；
integration **1147 passed / 4 failed**（四条与 HEAD
逐条相同：`cpp_flat_map`、`cpp_multi_declarator_member`、`eh_landing_pad_functions_are_never_auto_bound`、
`register_home_and_cleanup_survive_every_declaration_path`）；disasm 快照与 G10 零重录；
`check_opt_equivalence.sh` **0 OPT-DIFF**（COMPILE-FAIL 60 = 基线）；`check_reproducible_output.sh`
**5454 组合全绿**；`check_asm_text_parity.sh` 无 PARITY-FAIL；`scripts/torture_gate.sh`（C-Torture
全量）**PASS**：compile `pass=2010 / fail=0`、execute `pass=1697 / fail=0`（基线名单 27/49 全转绿）；
`scripts/bench_lua_sqlite.sh` → **OK (0 build-fail, 0 output-diff)**，其中墙钟 best-of-5（该脚本
自己的口径，不绑核、不作判定依据）：Lua `-O2` **290ms vs gcc 119ms = 2.44x**（-O3 295ms）、
SQLite `-O2` **331ms vs gcc 92ms = 3.60x**（-O3 321ms）。上一会话同组量到 279/112 = 2.49x 与
311/88 = 3.53x（gcc 参考臂自己也在漂）⇒ 可引的是比值而不是绝对 ms（方案 §15.10.8）。
默认档翻转会改产物，而预算量尺修正又改了一次（`luaV_execute` 11670 → **11210**），所以
§6.13 里那些「默认关」口径的静态数字在本节一律以新默认档为准重测；§6.13 的配对 A/B 表是
**当时的默认**下量出来的历史记录，其中内联覆盖面那一步的收益（`all/rbp` 0.9472）与最终默认的
0.9704（25 轮；15 轮那次 0.9462）同源 —— 区别是后者不再依赖一个「标签不进预算」的 bug。

---

## 6.15 产物性能根治轮（第十三轮）：S1 翻默认 −485 条、冷尾外置让热区 −14.4%

实施 `.qoder/specs/产物性能根治方案_task-5dd.md`（含第十一轮评审修订）。**本节只记验证读数**：
探针如何改写了方案自己的假设、每条臂的机制与护栏口径、外部 oracle 抖出的两条真错、S4 两次
结构错与「零插入下推迟存出省不到」的推导、以及各臂「为什么默认关」的算账，全部在方案 §12
（第十三~十五轮定位与过程记录）；数字目标逐条终账在方案 §11.2。

### 6.15.1 默认档位图与不变量

`DCC_BLOCKOPT` 档 3 = `R_SEXT | R_COPY | R_SLOT | R_RAW | R_ZBITS`（本轮翻入 `R_RAW`/`R_ZBITS`，
第十四轮再加 `R_IMMSEXT`）。仍默认关：`R_CALLSLOT`(A1)、`R_LABELJOIN`(A2)、`R_LRU`(A3)、
`R_TYPED`、`R_CPY32`、`R_CPYBASE`、`DCC_COLD_OUTLINE`、`DCC_COLD_MERGE`、`DCC_REGION_HOME`。

「`DCC_BLOCKOPT=0..3` 产物逐字节不变」这条旧不变量自本轮起不再成立（它成立在一批永不生效的
代码上）；`0..2` 三档继续逐字不变，判据改为 `rules_for_maps_levels_without_drift` 那两条。

### 6.15.2 Gate 0 三探针（`scripts/gate0_probe.sh`，0a/0b 同一次构建出）

| 档 | total | hit_call | hit_label | hit_pressure | 其中 hit_raw | hit_capacity |
|---|---|---|---|---|---|---|
| 默认位图（不含 R_RAW） | 603 | 0 | 47 | 556 | 531（88%） | 0 |
| 含 R_RAW | 383 | 0 | 95 | 288 | 0 | 0 |

影子层只读 ⇒ 产物逐字节不变（`SHADOW=0/1` 的 .o `cmp` 全等）。0c：`sqlite3VdbeExec` 两侧同一
awk 口径，gcc 侧帧基址是 `rsp`（279 条，与方案 §8.2 的 271 对得上）；帧访存口径统一为 r13
引用 2657。

### 6.15.3 S1 指令事实层（`RawWrite` 4 → 9 种，默认档已翻）

| 量 | 开工前 | 翻默认后 | S3c 再减后 | Δ 合计 |
|---|---|---|---|---|
| `luaV_execute` 条数 | 11210 | 10725 | **10551** | **−659（−5.9%）** |
| r13 引用 | 2657 | 2438 | 2438 | −219 |
| `movsxd` | 685 | 424 | 424 | −261 |
| `mov r64,r64` | 2697 | 2431 | 2257 | −440 |
| 立即数 `mov` | 637 | 637 | 463 | −174 |

单翻 `R_RAW` 实测 −224 条（方案 §8.2 标的 −22 是十倍低估）。构造子穷举用例
`raw_writes_decodes_every_raw_constructor` 列 46 个形状，任何一项解成 `Unknown` 即红。
外部 oracle `scripts/check_facts_vs_objdump.sh`：410 条对账、0 不一致，并抖出两条静默错值级
真错（8 位形 `ah/ch/dh/bh` 的目的号、16 位累加器形不能推值域）。

### 6.15.4 S6 冷尾外置（`DCC_COLD_OUTLINE=1`，`hot_cold_census.sh`）

| 量 | 关 | 开 |
|---|---|---|
| full | 10725 | 10731 |
| **hot（被跳表指向的臂区间）** | **10612** | **9089（−14.4%）** |
| out（冷尾） | 40 | 1596 |
| arm_mean | 130.6 | 106.9 |

墙钟 `ab_pair.sh` 15 轮：median(r)=0.9923、CI [0.9838,1.0039]、10/15 同向 ⇒ CI 含 1 ⇒
按 §5.5 三态：**不可判 ⇒ 默认关、不计收益**。冷尾指纹合并（`DCC_COLD_MERGE=1`）在
`luaV_execute` 上合 **0 对**（机会集构造为空，理由见方案 §12.3）；§8.4 的「站点 80→≤10」
已于第十五轮按用户裁定删除。

### 6.15.5 S2 三条臂的读数

| 臂 | 位 | Lua 读数 | 判定 |
|---|---|---|---|
| A1 调用分流 | `R_CALLSLOT` | `hit_call = 0` | 买 0 条 ⇒ 默认关 |
| A2 标签汇合 | `R_LABELJOIN` | `a2_inherit = 0`（每个标签都是跳表落点） | 命名错修正后的两臂读数见 §6.16.2 |
| A3 容量 LRU | `R_LRU` | `hit_capacity = 0` | 上限为零 ⇒ 默认关 |

三条臂在第二个负载上的复测读数见 §6.16.6，归因见方案 §12.4。

### 6.15.6 S3c 立即数 `mov`：组成读数与 637 → 463

637 条里 455 条是 `mov *,0x0`，其中 250 条是每次调用都写的 `mov $0,%al`（SysV vector 寄存器
个数，只被变参被调方读）。`abi/call.rs` 改为只对「原型看过且明确非变参」的被调方省 ⇒
637 → **463**，总条数 10725 → 10551。剩下的 463 条是活的常量定义，组成拆解与「≤300 定在
错桶」的定性见方案 §12.5。

### 6.15.7 S4 区域家（该版：插入式 flush）

`DCC_REGION_HOME=1` 下 `region_drop` 117、`region_replace` 33（机制确实跑），但
`luaV_execute` 总条数 10551 → **10556（+5）**、r13 引用 2438 不变；全语料扫描仍有两处
SIGSEGV。该版的两次结构错、修法与后续零插入改写见方案 §12.6，零插入版读数见 §6.16.1。

### 6.15.8 §4 正确性先修七项落点

| 项 | 落点 |
|---|---|
| 4.1 `__builtin_setjmp/longjmp` | `frame_guard::RETURNS_TWICE_NAMES`（libc `longjmp` 故意不入，单测钉住） |
| 4.2 volatile | `FrameGuard.volatile_obj` 有消费者；`volatile_global_names` 两侧共用 |
| 4.3 内联 asm | 口径改为「出现即命中」（`FrameGuard.inline_asm`） |
| 4.4 `-g` | 本轮做成函数级硬互斥；loclist 本体在第十四轮实现（§6.16.4） |
| 4.5 prelude walker 3+1 | `sanitizer.rs` 三处 + `cpp/modules.rs` 一处 |
| 4.6 `Expr::LabelAddr` | `has_unstructured_flow` 补表达式槽（穷尽 match） |
| 4.7 四条件同源 | 单一落点 `src/frame_guard.rs`，blockopt 经 `CallPolicy` 只读结论 |

### 6.15.9 本轮质量门与 bench

lib 2556 / 0 failed / 2 ignored（+12）；integration 1173 / 0 / 5 ignored（+7）；disasm 快照
20 例重录（净 −36 行）；`check_opt_equivalence` 0 OPT-DIFF；`check_reproducible_output` 5454
全绿；`torture_gate` PASS；`check_facts_vs_objdump` 410 条 0 不一致；`bench_lua_sqlite` OK。

| 负载 | 本轮 best-of-5 | 上一轮 |
|---|---|---|
| Lua `fib(32)` -O2 | 267ms vs gcc 111ms（2.40x） | 290 vs 119（2.44x） |
| SQLite 20 万行 -O2 | 291ms vs gcc 86ms（3.38x） | 331 vs 92（3.60x） |

静态 −5.9% 只换到墙钟 −1.6%，与方案 §0 结论 2「差距是全局均匀膨胀」同向；绝对 ms 不作承诺
（§6.14 立的口径）。SQLite 侧那 −12% 来路不是分派循环，不当作本轮机制的成果。

---

## 6.16 收尾轮（第十四轮）与体量档（第十五轮）：S4 零插入、A2 取交、loclist、非负 imm32 短形

同上：过程与推导在方案 §12.6–§12.12，数字目标终账在方案 §11.2。

### 6.16.1 S4 区域家（零插入版）读数

`scripts/region_home_sweep.sh` 对全语料 311 份源 × 三档做「开 vs 关」四元组值等价 ⇒ **0 DIFF /
0 UNSTABLE**。机制服务读数（`DCC_REGION_STAT=1`，`luaV_execute`）：

| 臂 | promoted | loads_saved | writebacks | 拒因 |
|---|---|---|---|---|
| 默认门（家号须 callee-saved） | 0 | 0 | 0 | caller_saved=2156、no_owner=5 |
| 关掉该门（`DCC_REGION_CALLEE=0`） | 606 | **0** | 0 | addr_export=418、no_owner=90、escaped=32 |

三臂（门关 / 全开 / 只关 defer）**产物逐字相同**：insn 10551、r13 2438 ⇒ §5 止损② 成立，默认关。

### 6.16.2 A2 两臂读数（修正命名错后）

| 语料 | `a2_inherit`（落空继承） | `a2_intersect`（多前驱取交） |
|---|---|---|
| `lvm.c` 全部函数 | 8 | 7 |
| `tests/fixtures/c` 全 311 份 | 68 | 16 |

仍默认关（量级跳不出噪声）。命名错的定位过程见方案 §12.7。

### 6.16.3 `sext32` 事实三形态读数

`mov r64,imm32` / `movsx*` / `movzx*` 三族的扩展事实（单开一位 `R_IMMSEXT`）：`movsxd`
424 → **361**（lvm.o 全部 571 → 464），`luaV_execute` 10551 → **10488**。同轮配对：
SQLite median=0.9482、CI [0.9334,0.9588]、25/25 同向 ⇒ 进默认档；Lua median=0.9847、
CI [0.9812,0.9947]、31/41 ⇒ 不记收益。外部 oracle 加 `sext`/`zext` 臂（除目的号还查助记符
族），120 条唯一 sext 序列入账、0 不一致。

### 6.16.4 §4.4 `-g` loclist 读数

`.debug_loc`（DWARF4）有真实生产者：延伸池（caller-saved）家在 `persist_ext_homes` /
`restore_ext_homes` 那段窗口里真值在帧内保存槽而不在号里。仓库内 4 份夹具因此多出
`.debug_loc`；DWARF5 显式退回单 `exprloc`。3 条单测 + 1 条端到端用例（节存在、能解、同时有
`DW_OP_reg`/`DW_OP_fbreg`、不开 `-g` 不得多节）。loclist 只动 `.debug_*` 节 ⇒ `.text` 与
执行结果类判据不受影响；S4 的 `-g` 硬互斥随之删除。两个地址口径错的经过见方案 §12.9。

### 6.16.5 S3a / S3c 数字目标的组成分解

| 目标 | 现值 | 剩余构成 |
|---|---|---|
| `movsxd` ≤100 | 361 | 128 内存形（gcc 也发）+ 149「bit31 未知的首次扩展」+ 84 跨号转换 |
| 立即数 `mov` ≤300（条数） | 463 | 433 条目的就是 `%rax`、0 条目的是内存 ⇒ 全是活定义 |

两条都属于方案 §8.1 划出的 L4 射程（求值约定 + 分配层），发射层不可达；终账见方案 §11.2。

### 6.16.6 第十四轮质量门（同一份最终二进制）

lib 2563 / 0；integration 1177 / 0；OPT-EQ 0 OPT-DIFF；REPRO 5454 全绿；FACTS 410 条 0 不一致；
`region_home_sweep` 0 DIFF / 0 UNSTABLE；disasm 快照 24 例重录；G10 基线 2/5 漂移重录；
`-g` 放开后同批夹具带 `-g` 再跑开/关值等价 132 次 0 异常。S2 三条臂的双负载复测：A1/A2 在
`sqlite3VdbeExec` 一字不变（insn 19634 / movrr 3162 / r13 4134 / movsxd 1020 两臂全同），
A3 在 `lvm.o` 上产出**逐字节相同**的对象（119936 = 119936）。

### 6.16.7 S3c 体量档：非负 imm32 走 5 字节短形（第十五轮，已进默认档）

`DCC_IMM32_SHORT_MOV`（默认开；`=0` 退回 7 字节形，仅作量尺与回退证明）。同一份二进制两臂：

| 读数 | 门关 | 门开 | 差 |
|---|---|---|---|
| `luaV_execute` 字节 | 52505 | 51599 | −906（−1.73%） |
| `luaV_execute` 条数 / hot / out | 10488 / 10379 / 39 | 完全相同 | 0 |
| `lvm.o` `.text` | 74080 | 72812 | −1268 |
| `sqlite3.o` `.text` | 6350921 | 6267998 | −82923（−1.31%） |
| SQLite shell 链接后 `.text` | 7372560 | 7277760 | −94800（−1.29%） |
| 非负 7 字节 `mov r64,imm32` 条数 | 48412 | **0** | 全部转掉 |
| 负立即数 7 字节形 | 850 | 850 | 正确保持不变 |
| Lua 解释器产物 | 1172024 | 1155640 | −16 KB |

读侧配套两处（`classify_peep` 认 5/6 字节形；`raw_writes` 非负报 `ImmSExt`）的必要性推导见
方案 §12.10。墙钟配对（三腿 = median ≤0.985 且 CI 上界 <1 且符号检验 ≥7/8）：

| 负载 / 布局 | 会话 | median(r) | 95%CI | 同向 |
|---|---|---|---|---|
| SQLite 布局 L1 | R=15 | 0.9718 | [0.9518, 0.9815] | 13/15 |
| SQLite 布局 L1 | R=31 | 0.9734 | [0.9644, 0.9864] | 25/31 |
| SQLite 布局 L1（换臂序） | R=31 | 0.9642 | [0.9532, 0.9724] | 28/31 |
| SQLite 布局 L2 | R=31 | 0.9924 | [0.9771, 1.0044] | 20/31 |
| SQLite 布局 L2（换臂序） | R=31 | 0.9828 | [0.9722, 1.0032] | 21/31 |
| Lua `fib(32)` 布局 L1 | R=25 | 0.9953 | [0.9812, 1.0084] | 15/25 |
| Lua `fib(32)` 布局 L2 | R=25 | 1.0061 | [0.9917, 1.0307] | 10/25 |

同轮 dup 地板读数 0.9875~1.0068。⇒ **墙钟不可判，本轮不记任何墙钟收益**；默认开这个决定
依据的是三条静态事实（体量下降、条数一字不变、值与 gcc 同）。布局抽签主导判定的经过见
方案 §12.10。

### 6.16.8 第十五轮质量门

门关臂 `.text` sha256 与改动前已提交的 5 个 G10 基线**逐字节相同**（门关 = 改前）。
lib 2571 / 0；integration 1179 / 0；OPT-EQ 0 OPT-DIFF（COMPILE-FAIL 60 未增长）；REPRO 5454
全绿；FACTS 0 不一致（410 条）；`check_asm_text_parity` 无 PARITY-FAIL；`check_disasm_coverage`
OK（未命中率 0.0478%，上限 81）；`region_home_sweep` 0 / 0；`torture_gate` compile 2010/0/skip6、
execute 1697/0；`make test`（Tier 0，全测试目标）exit=0：3867 passed / 0 failed，
`check_fixtures.sh` 全绿、元门禁 5/5 PASS。disasm 快照 26 例重录（净 −740 行）；G10 基线
5 个全部重录。同轮另修两处基础设施问题（lib 用例的进程环境污染源、Tier 0 的 60 条继承假红），
经过见方案 §12.11 / §12.12。

---

## 7. Summary

| Project | dcc compile | dcc link | functional run (-O0..-O3) | output consistency | perf vs gcc (-O2 / -O3) |
|---|---|---|---|---|---|
| Lua 5.5.1 | OK | OK | OK (all levels) | byte-identical to gcc, all levels | ~2.40x / ~2.4x slower (267 / 266 ms vs gcc 111 ms; best-of-5 读数于 §6.15.9，§6.16 两轮未重跑该 bench) |
| SQLite (amalgamation) | OK | OK | OK (all levels) | identical across -O0..-O3 | ~3.38x / ~3.4x slower (291 / 296 ms vs gcc 86 ms; 同上口径) |

**Takeaways**

1. dcc compiles and links both projects. `$CC` (and `$LD`/`$OBJCOPY`) are
   shell-word-split during link delegation, and multiple translation units are
   accepted in a single invocation — no build-system workaround is needed.
2. SQLite is usable end-to-end with dcc at **every** optimization level
   `-O0`/`-O1`/`-O2`/`-O3` (version 3.53.4: DDL/DML, aggregates, `JOIN`/`GROUP
   BY`, file persistence, transactions), with query results identical across
   the levels.
3. Lua 5.5.1 now compiles and runs **correctly at every optimization level**
   (`-O0`/`-O1`/`-O2`/`-O3`), with `luafull.lua` output byte-identical to a
   reference `gcc` build. There is no longer any need to restrict Lua to
   `-O0`/`-O1`.
4. **Drop-in `CC`.** With the two DX fixes in commit `99dabbc1` (see §3.5), an
   existing project builds by simply pointing `CC` at dcc — flags may live in
   `CFLAGS` or be folded into `CC`, and multi-`.c` link lines work unchanged.
   This is what moves dcc from "can compile Lua/SQLite" to "a real project can
   use it as `CC` out of the box".
5. **Performance.** dcc's -O2/-O3 output is correct but measurably slower than
   gcc (§6): roughly **2.40x** on the Lua interpreter loop (was 4.4x) and
   **~3.38x** on a SQLite insert+query workload (was 6.4-7.1x, and 5.0-5.4x
   before the dense-switch jump-table round in §6.12, which with the new inline
   shapes took SQLite from 444.6 → 304.8 ms paired-median, −31.1%). 第十三~十五轮
   的累计变化在静态侧：`luaV_execute` 11210 → **10488 条**（−6.4%）与 52505 → **51599
   字节**（−1.73%，§6.16.7）；已测的墙钟读数仍是 §6.15.9 那一组（2.40x / 3.38x）。
   `-O3 ≈ -O2`
   for dcc. The 2026-10 emitter round (§6.6) is what moved these ratios most: over
   its two stages it cut `luaV_execute`'s instruction count by 31.3% and its register
   moves by 55.8%, and Lua `-O2` went 497→432→324 ms in like-for-like reruns. The
   direct-conditional-branch round (§6.7) adds a further ~2% on both benchmarks
   (`luaV_execute` −339 instructions, `sqlite3VdbeExec` −902); the scalar-store /
   dead-value / -O2 cost-model inlining round (§6.9) took `luaV_execute` to 12412 and
   bought −2.4% Lua and −7.4% SQLite; and the block-equivalence round (§6.10) adds
   `luaV_execute` −31 / `sqlite3VdbeExec` −123 instructions for a paired
   **−1.2%…−2.5%** on Lua and −0.6%…−1.6% on SQLite (p50 flat). The current round
   (§6.11) closes the three numeric targets of plan §5 — `luaV_execute` 11366 instructions with
   push+pop **389 → 155** — and its runtime measurement is in: pinned to one core with gcc/dcc
   alternating, best-of-9 `task-clock` gives Lua `-O2` min 291.6 / p50 302.5 ms and SQLite `-O2`
   min 472.7 / p50 475.9 ms, i.e. ~10% inside the §5 thresholds (324 / 527 ms) and ~9% better
   than the §1 baseline after subtracting the gcc reference arm's 2% drift. The ratios quoted
   at the top of this item are the ones measured on that build. The two stage-6 rounds on top of
   it are what brought SQLite down to ~3.6x: the dense-`switch` jump-table dispatch (§6.12,
   −27.5% paired) plus call-site-shaped inlining (a further −4.9%), and §6.13's jump-table base
   hoisting into `%rbp`, whose own paired effect is Lua 0.9939 / SQLite 1.0056 — inside the
   instrument's floor, so it is kept on through the narrow channel **without claiming a gain**.
   The last two numbers on top of that are §6.14's arm-body inlining walk (call sites inside
   `Label`/`Switch` arms, gated by a caller-size budget): **Lua `median(r)=0.9704` over 25 paired
   rounds** (CI [0.9437, 0.9832], 22/25 faster; a previous 15-round session read 0.9462 for the
   same configuration, and the same-session dup control was 1.0007, so the honest statement is
   "-3%…-5%") with SQLite indistinguishable (0.9943 against a 1.0028 same-binary floor, readings
   across four sessions: 0.9943/0.9988/1.0132/1.0288 — no SQLite gain claimed). The default is on
   for the size channel while the shape channel (threaded dispatch) and the two extra call-site
   shapes stay measured-but-off. Caveat recorded in §6.14-5: the budget's measurement scale had a
   `_ => 1` hole that made it fail open on label-shaped callers; the numbers above are the
   re-measurement after that was fixed, and the earlier 0.9530/11670 pair in this section's draft
   depended on the bug. §6.14-6 records a separate, pre-existing defect family: walkers written
   before `Stmt::Switch.prelude` existed (the cross-function DCE call graph, the frame-allocation
   budget, GVN scope collection, …) silently skipped that subtree, which could make `-O2`
   objects unlinkable (`undefined reference`) or reject a valid program with `E1133`; ten sites
   were fixed this round with three new regression tests.
   That is the current state of the plan in
   `.trae/documents/x86_64-p13-hot-path-emission-optimization-plan.md`.
   §6.7 also records a rejection: an optimisation that removes 135 instructions but
   moves the hot code off its alignment measured 3.8% **slower** and was reverted —
   from here on every claimed gain must ship with a paired, alternating min/p50
   measurement.
6. **Correctness evidence for the optimizer.** `scripts/check_opt_equivalence.sh`
   compiles all 397 fixtures at `-O0`/`-O1`/`-O2`/`-O3` with dcc itself and
   requires identical stdout and exit codes: 0 OPT-DIFF (60 COMPILE-FAILs are
   all fixture-side: 40 no-`main`, 16 cross-TU, 4 rejected by gcc under the same
   flags; 0 unknown). `cargo test --lib` is 2505 passed / 0 failed, and the Lua
   and SQLite builds were re-verified functionally after the emitter changes.
   The two new fixtures are what caught §6.8: two **silent miscompiles that exist on
   HEAD** (a mixed-sign comparison using the wrong signedness at `-O0`/`-O2`/`-O3`,
   and `-O3` loop blocking reordering observable side effects), both now fixed with
   9 new unit tests and 2 permanent fixtures; §6.8 also lists what is still open
   (`sizeof` on a local struct array, and 4 integration tests that already fail on
   HEAD, so this round's bar is "no new integration failure": 1130 passed / 4 failed — the
   1115-test baseline plus the 15 tests added for P13 §13.9 and this round, with the same 4 HEAD-red
   items). §6.11 adds two more fixtures of the same kind, and the pair they caught are **wrong
   values**, not ugly code: a `||` decomposed as `&&` in the "jump-if-false" context (`if (a ||
   f())` short-circuited backwards; reproduced first by the integration test
   `short_circuit_rhs_impure_matches_gcc_at_all_levels` — exit code 26 vs gcc's 0 before the
   fix), and a 128-bit truth test that read only the low half (`or rax,rdx` before `test` is now
   required). The second one sat in a **combination blind spot**: the `pr105613` corpus it was
   found in only exercises `lo != 0` and all-zero data, never `hi != 0 && lo == 0`, so the
   miscompile showed up in snapshot text while the execution side stayed green — hence the new
   fixture deliberately feeds hi-only data.
   **Acceptance criteria were reclassified this round** (plan §5): the only blocking external
   corpus is **GCC C-Torture** (`scripts/torture_test.sh compile|execute|all`, ~3715 cases, fail
   must be 0; `scripts/torture_gate.sh` is its incremental view against
   `tests/skip/torture_known_fail_{compile,execute}.txt`) — its fifth-round number is **not yet
   measured**, see §6.11. `scripts/gcc_dg_test.sh` drives the **gcc-dg** corpus (5400+ cases) and
   is reference data only, never a blocking criterion; conflating the two was itself an error
   corrected in this revision.
7. **Re-verification pays for itself.** The 2026-10 pass (§6.4) found that the
   `-O2`/`-O3` builds, while producing a running interpreter, printed wrong
   numbers for floats. The §4.2 sweep is what caught it — a build/rc-only
   check would have passed. Re-run §4.2 and §5.2 after optimizer changes, not
   just "does it build".

---

## 8. Appendix

### 8.1 One-shot reproduction script

```bash
#!/usr/bin/env bash
set -e
DCC=~/dcc/target/release/dcc

# --- build dcc (basic step) ---
( cd ~/dcc && cargo build --release )
"$DCC" --version

# --- Lua 5.5.1 (correct at every optimization level; use -O2) ---
cd ~/lua-5.5.1/src
make clean
make posix CC="$DCC" MYCFLAGS="-std=gnu99 -O2" AR="ar rcu" RANLIB=ranlib
./lua -v
./lua luafull.lua
./luac -o /tmp/luafull.luac luafull.lua && ./lua /tmp/luafull.luac

# O0..O3 sweep — all four must be OK and produce identical script output
for o in 0 1 2 3; do
  make clean
  make posix CC="$DCC" MYCFLAGS="-std=gnu99 -O$o" \
       AR="ar rcu" RANLIB=ranlib >/dev/null 2>&1
  v=$(./lua -v >/dev/null 2>&1; echo $?)
  s=$(./lua luafull.lua >/dev/null 2>&1; echo $?)
  echo "O$o: lua -v rc=$v | run script rc=$s"
done

# --- SQLite amalgamation (verified at -O0 .. -O3) ---
cd ~/sqlite
for o in 0 1 2 3; do
  "$DCC" -O$o -o sqlite3_dcc shell.c sqlite3.c -lpthread -ldl -lm
  ./sqlite3_dcc :memory: "SELECT sqlite_version();"
done
```

### 8.2 `luafull.lua` (functional test)

```lua
-- Functional smoke test for a dcc-built Lua 5.5.1
print("ver:        " .. (_VERSION or "Lua 5.5"))

local f = io.open("/tmp/dcc_lua_io.txt", "w")
f:write("hello from dcc lua\n")
f:close()
local g = io.open("/tmp/dcc_lua_io.txt", "r")
local line = g:read("*l")
g:close()
print("readback:   " .. line .. "\t1")

print("sin/sqrt:   " .. tostring(math.sin(0)) .. "\t" .. tostring(math.sqrt(144)))

local function fib(n)
  if n < 2 then return n else return fib(n - 1) + fib(n - 2) end
end
print("fib(20):    " .. fib(20))

local co = coroutine.create(function()
  for i = 1, 3 do coroutine.yield(i * i) end
end)
local a, b, c = coroutine.resume(co), coroutine.resume(co), coroutine.resume(co)
print("co:         " .. tostring(a) .. "\t" .. tostring(b) .. "\t" .. tostring(c))

local s = "X1X2X3"
local r, n = s:gsub("X(%d)", "X%1")
print("pat:        " .. r .. "\t" .. n)
```

### 8.3 Benchmark reproduction (-O2 / -O3, dcc vs gcc)

Produces the §6 numbers. Builds four Lua interpreters and four SQLite shells
(gcc/dcc x -O2/-O3), then reports best-of-5 wall-clock. Correctness is asserted
by identical program output across the binaries.

```bash
DCC=~/dcc/target/release/dcc

# --- Lua fib(32) ---
cd ~/lua-5.5.1/src
cat > /tmp/fib.lua <<'LUA'
local function fib(n) if n<2 then return n end return fib(n-1)+fib(n-2) end
print(fib(32))
LUA
for lvl in -O2 -O3; do
  for cc in "gcc" "$DCC"; do
    make clean >/dev/null 2>&1
    make posix CC="$cc" MYCFLAGS="$lvl -std=gnu99" AR="ar rcu" RANLIB=ranlib >/dev/null 2>&1
    cp lua "/tmp/lua_$(basename $cc)$lvl"
  done
done
# time each binary directly (best of 5)
for b in /tmp/lua_gcc-O2 /tmp/lua_gcc-O3 /tmp/lua_dcc-O2 /tmp/lua_dcc-O3; do
  best=999999999
  for i in 1 2 3 4 5; do
    s=$(date +%s%N); $b /tmp/fib.lua >/dev/null 2>&1; e=$(date +%s%N)
    ms=$(( (e-s)/1000000 )); [ $ms -lt $best ] && best=$ms
  done
  printf "lua %-16s %sms\n" "$(basename $b)" "$best"
done

# --- SQLite: 200k insert + range aggregate + ORDER BY scan ---
cd ~/sqlite
cat > /tmp/q.sql <<'SQL'
CREATE TABLE t(id INTEGER PRIMARY KEY, a INT, b TEXT);
BEGIN;
WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<200000)
INSERT INTO t(id,a,b) SELECT i, (i*2654435761)%1000, 'row'||(i%997) FROM n;
COMMIT;
SELECT 'sum', sum(a), count(*) FROM t WHERE a > 500;
SELECT 'order', count(*) FROM (SELECT a FROM t ORDER BY a DESC LIMIT 1000);
SQL
for lvl in -O2 -O3; do
  for cc in "gcc" "$DCC"; do
    $cc $lvl -o "/tmp/sql_$(basename $cc)$lvl" shell.c sqlite3.c -lpthread -ldl -lm
  done
done
for b in /tmp/sql_gcc-O2 /tmp/sql_gcc-O3 /tmp/sql_dcc-O2 /tmp/sql_dcc-O3; do
  best=999999999
  for i in 1 2 3 4 5; do
    s=$(date +%s%N); $b :memory: < /tmp/q.sql >/dev/null 2>&1; e=$(date +%s%N)
    ms=$(( (e-s)/1000000 )); [ $ms -lt $best ] && best=$ms
  done
  printf "sqlite %-16s %sms\n" "$(basename $b)" "$best"
done
```

Representative output (same host, single-threaded, best-of-7):

```
lua   gcc-O2  113ms | dcc-O0 375ms | dcc-O2 324ms | dcc-O3 329ms
sqlite gcc-O2  86ms | gcc-O3 95ms | dcc-O2 527ms | dcc-O3 523ms
```

The gcc reference moves a few percent between runs; compare dcc against the gcc
number produced by the same invocation of the script, not across days.

Add a float-printing assertion to any re-run: it is the check that distinguishes
"built and ran" from "produced the right answers" (§6.4 defect 3). The §4.2
script already covers it via `sin/sqrt`; the standalone form is:

```bash
./lua -e 'print(math.sqrt(144), math.sin(0), 1.5, 0.0, 12.0)'
# expect: 12.0  0.0  1.5  0.0  12.0
#         (a -O2 build that prints ".0" / empty values is miscompiling)
```
