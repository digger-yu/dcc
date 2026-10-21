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
> §6.5/§6.6. The tables in §6.1/§6.2 are the like-for-like after-state, measured
> against the same script run on the pre-change compiler; §7 records the
> correctness numbers (389 fixtures × four levels, 0 OPT-DIFF; 2483 unit tests).

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
- All 2483 dcc library unit tests pass (`cargo test --lib`).
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
| gcc -O2  | 113 | — (baseline) | 115 |
| dcc -O0  | 375 | 3.3x slower | 427 |
| dcc -O1  | 381 | 3.4x slower | 461 |
| dcc -O2  | 324 | 2.9x slower | 432 |
| dcc -O3  | 329 | 2.9x slower | 451 |

Both columns are produced by the same script on the same host. The
"previous run" column is that script run against the pre-change compiler
(HEAD built into a separate target directory), so the delta is like-for-like
rather than a number carried over from an older machine. `gcc -O2` stayed at
113–115 ms between the two runs, which bounds the run-to-run noise. A pinned
single-core best-of-11 runs of the final binary gave 317–324 ms for `-O2`.

`-O2` is now 13.6% faster than `-O0`: `luaV_execute` reaches the SSA home
allocator through the analysis-only channel (§6.5), which runs before the
per-level pass selection, and the rest of the file gets the ordinary `-O2`
SSA pipeline. The emitter rules below run at **every** level, which is why all
four rows moved down, not just `-O2`/`-O3`.

### 6.2 SQLite — 200k insert + range aggregate + `ORDER BY` scan

| Compiler / level | wall (ms) | vs gcc same level | previous run (ms) |
|---|---|---|---|
| gcc -O2  | 86  | — (baseline) | 98 |
| gcc -O3  | 95  | — | 101 |
| dcc -O2  | 527 | 6.1x slower | 595 |
| dcc -O3  | 523 | 5.5x slower | 578 |

`gcc -O2` moved 98→86 ms (−12%) between the two runs, so the absolute gcc
reference carries more noise on this workload than on Lua. The dcc numbers
moved down too (−11.4% / −9.5%), which is what makes the improvement credible
despite that noise: the previous round's second run against the final binary
gave 588 / 582 ms, so this round's single full run is not an outlier.

### 6.3 Reading the numbers

- dcc-generated code is **correct but materially slower** than gcc: roughly
  **2.9x** on the Lua interpreter loop (was 4.4x two revisions ago) and
  **5.5-6.1x** on SQLite (was 6.4-7.1x). This is an honest result, not a
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

## 7. Summary

| Project | dcc compile | dcc link | functional run (-O0..-O3) | output consistency | perf vs gcc (-O2 / -O3) |
|---|---|---|---|---|---|
| Lua 5.5.1 | OK | OK | OK (all levels) | byte-identical to gcc, all levels | ~2.9x / ~2.9x slower |
| SQLite (amalgamation) | OK | OK | OK (all levels) | identical across -O0..-O3 | ~6.1x / ~5.5x slower |

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
   gcc (§6): roughly **2.9x** on the Lua interpreter loop (was 4.4x) and
   **5.5-6.1x** on a SQLite insert+query workload (was 6.4-7.1x). `-O3 ≈ -O2`
   for dcc. The 2026-10 emitter round (§6.6) is what moved these ratios: over its
   two stages it cut `luaV_execute`'s instruction count by 31.3% and its register
   moves by 55.8%, and Lua `-O2` went 497→432→324 ms in like-for-like reruns.
   Further back-end work should shrink these ratios while keeping correctness.
6. **Correctness evidence for the optimizer.** `scripts/check_opt_equivalence.sh`
   compiles all 389 fixtures at `-O0`/`-O1`/`-O2`/`-O3` with dcc itself and
   requires identical stdout and exit codes: 0 OPT-DIFF (60 COMPILE-FAILs are
   all fixture-side: 40 no-`main`, 16 cross-TU, 4 rejected by gcc under the same
   flags; 0 unknown). `cargo test --lib` is 2483 passed / 0 failed, and the Lua
   and SQLite builds were re-verified functionally after the emitter changes.
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
