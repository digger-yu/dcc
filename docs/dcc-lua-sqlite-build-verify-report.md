# DCC — Lua 5.5.1 + SQLite: build & verification results

**Verdict (2026-10-26).** dcc builds Lua 5.5.1 and SQLite 3.53.4 from source at
`-O0`…`-O3`, and both binaries run. The **Lua official suite (`all.lua`) passes
at all four levels on both verification paths** (`final OK !!!`, `rc=0`).
SQLite has not been run against its test suite. Reported speed is ~2.4x (Lua)
and ~3.4x (SQLite) slower than `gcc -O2`; those timings predate the current
build and have not been re-measured.

This file records final results only: build commands, result tables, the
reproduce script and the open gaps.

---

## 1. Sources and environment

| Item | Value |
|---|---|
| dcc | this repository, `cargo build --release` → `target/release/dcc` |
| Lua | 5.5.1 (`~/lua-5.5.1`) |
| Lua tests | `~/lua-5.5.1-tests` (official suite, `all.lua`, 28 files) |
| SQLite | 3.53.4 amalgamation (`~/sqlite`, `sqlite3.c` + `shell.c`) |
| Host | Linux x86-64, system gcc as reference |

## 2. Build commands

```bash
# dcc itself
cd ~/dcc && cargo build --release

# Lua (posix build: no readline dependency)
cd ~/lua-5.5.1/src
make clean
make posix CC=~/dcc/target/release/dcc MYCFLAGS="-std=gnu99 -O2" \
     AR="ar rcu" RANLIB=ranlib
# -O0 / -O1 / -O3: same command with the level changed in MYCFLAGS

# SQLite: single invocation, both translation units plus the link
cd ~/sqlite
~/dcc/target/release/dcc -O2 -o sqlite3_dcc shell.c sqlite3.c -lpthread -ldl -lm
```

Both produce a running binary at all four levels. dcc delegates linking to the
host linker, accepts several `.c` files per invocation, and splits `$CC`/`$LD`
into shell words, so it works as a drop-in `CC`.

## 3. Verification results

| Project | compile | link | `luafull.lua` smoke | official suite (`all.lua`) | vs gcc `-O2` |
|---|---|---|---|---|---|
| Lua 5.5.1 | OK, all levels | OK | identical to gcc, all levels | **PASS on both paths at all levels** (`final OK !!!`) | ~2.4x slower (bench, earlier build) |
| SQLite 3.53.4 | OK, all levels | OK | identical across levels | not run | ~3.4x slower |

"Both paths" means (A) the `ltests` build of `all.lua` (extra internal
assertions, `lua_*` symbols exported) and (B) the stock `make linux CC=dcc`
build running `-e "_U=true" all.lua`, which is what a user gets. Both were run
at `-O0`…`-O3` and both end in `final OK !!!` with `rc=0`; path A prints about
550 lines, gcc about 551. Every remaining difference from gcc is a random seed,
a pid, a timestamp, a temp path, an allocation count, a memory statistic, or
where the progress dots land. Both paths also exercise `libs/lib2-v2.so`, i.e.
dynamic module loading.

Regression signal: `scripts/check_opt_equivalence.sh` over 410 C fixtures
reports **0 OPT-DIFF** (identical stdout and exit code at `-O0`…`-O3`). All 60
COMPILE-FAIL fixtures are attributed — 40 have no `main`, 16 need a second
translation unit, 4 are rejected by gcc under the same flags — and none is
unattributed (`CF-UNKNOWN 0`).

## 4. Performance (indicative, earlier build)

`fib(32)` in the Lua interpreter, and a 200k-insert + range-aggregate +
`ORDER BY` workload in SQLite, best-of-N wall time on an idle host:

| Workload | gcc -O2 | dcc -O2 | dcc -O3 |
|---|---|---|---|
| Lua `fib(32)` | 111 ms | 267 ms (~2.40x) | 266 ms (~2.40x) |
| SQLite 200k | 86 ms | 291 ms (~3.38x) | 296 ms (~3.44x) |

`-O3 ≈ -O2` for dcc on both workloads; gcc behaves the same way.

**Caveat.** These readings come from `scripts/bench_lua_sqlite.sh` and were
taken **before** the current build; the script has not been independently
re-verified and has not been re-run since. Use them as "how fast an earlier
build ran", not as a current performance claim.

## 5. Reproduce

```bash
DCC=~/dcc/target/release/dcc

# 1. Lua smoke test (same output at every level)
cd ~/lua-5.5.1/src
for o in 0 1 2 3; do
  make clean
  make linux CC="$DCC" MYCFLAGS="-std=gnu99 -O$o" >/dev/null 2>&1
  ./lua luafull.lua
done

# 2. Official suite, path B: stock build, user mode (expect: final OK !!!)
#    This is what `make CC=dcc` gives a normal user (ltests off).
for o in 0 1 2 3; do
  cd ~/lua-5.5.1 && make clean >/dev/null 2>&1
  make linux CC="$DCC -std=gnu99" MYCFLAGS="-O$o" >/dev/null 2>&1 || exit 1
  mkdir -p /tmp/mkbin-$o && cp src/lua /tmp/mkbin-$o/lua
  rm -rf /tmp/suiteU-$o && cp -a ~/lua-5.5.1-tests /tmp/suiteU-$o
  ( cd /tmp/suiteU-$o && \
    PATH=/tmp/mkbin-$o:$PATH LD_LIBRARY_PATH=/tmp/rllib \
    /tmp/mkbin-$o/lua -e "_U=true" all.lua )
done

# 3. Official suite, path A: ltests, full (expect: final OK !!!, ~550 lines)
T=~/lua-5.5.1-tests/ltests
for o in 0 1 2 3; do
  mkdir -p /tmp/lt-$o && cd /tmp/lt-$o
  for f in ~/lua-5.5.1/src/*.c; do
    "$DCC" -c -O$o -DLUA_USE_LINUX -I"$HOME/lua-5.5.1/src" -I$T \
        -DLUA_USER_H='"ltests.h"' "$f" -o "$(basename "${f%.c}").o" || exit 1
  done
  "$DCC" -c -O$o -I$T -I"$HOME/lua-5.5.1/src" \
      -DLUA_USER_H='"ltests.h"' $T/ltests.c -o ltests.o
  ar rcu liblua.a *.o
  # -Wl,-E must survive dcc's argument handling: without it libs/*.so cannot
  # resolve lua_* symbols. Check: nm -D lua | grep -c ' T lua_'  (expect 103)
  "$DCC" -o lua lua.o liblua.a -Wl,-E -ldl -lm
  # all.lua's dofile() uses loadfile(), which ignores package.path, so the cwd
  # must contain main.lua — run in a pristine copy of the test tree, not in it.
  rm -rf /tmp/suite-$o && cp -a ~/lua-5.5.1-tests /tmp/suite-$o
  ( cd /tmp/suite-$o && \
    PATH=/tmp/lt-$o:$PATH LD_LIBRARY_PATH=/tmp/rllib lua all.lua )
done

# 4. SQLite (smoke only; no upstream suite yet)
cd ~/sqlite
for o in 0 1 2 3; do
  "$DCC" -O$o -o sq_$o shell.c sqlite3.c -lpthread -ldl -lm
  ./sq_$o :memory: "CREATE TABLE t(a); INSERT INTO t VALUES(1),(2),(3); \
      SELECT 'ok '||sum(a)||'/'||count(*) FROM t;"   # expect: ok 6/3
done
```

## 6. What is not verified

- SQLite has no upstream test-suite run under dcc; only the query checks above.
- Performance of the current build (the §4 numbers predate it). The only
  current timing available is suite wall time (dcc 11.5 s vs gcc 3.8 s), which
  is not a throughput measurement.
- Targets other than x86-64 Linux (`ptrdiff_t` / integer widths differ).
- `-fbuiltin-linker` does not understand `-Wl,-E` / `--export-dynamic`; the
  passthrough only covers the host-cc link path. All Lua/SQLite builds here
  use the host cc, so results are unaffected.
