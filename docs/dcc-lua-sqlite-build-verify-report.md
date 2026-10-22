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
  **2.5x** on the Lua interpreter loop (was 4.4x five revisions ago) and
  **5.0-5.4x** on SQLite (was 6.4-7.1x). This is an honest result, not a
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

---

## 7. Summary

| Project | dcc compile | dcc link | functional run (-O0..-O3) | output consistency | perf vs gcc (-O2 / -O3) |
|---|---|---|---|---|---|
| Lua 5.5.1 | OK | OK | OK (all levels) | byte-identical to gcc, all levels | ~2.5x / ~2.6x slower |
| SQLite (amalgamation) | OK | OK | OK (all levels) | identical across -O0..-O3 | ~5.4x / ~5.0x slower |

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
   gcc (§6): roughly **2.5x** on the Lua interpreter loop (was 4.4x) and
   **5.0-5.4x** on a SQLite insert+query workload (was 6.4-7.1x). `-O3 ≈ -O2`
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
   at the top of this item are the ones measured on that build. That is the current state of the plan in
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
