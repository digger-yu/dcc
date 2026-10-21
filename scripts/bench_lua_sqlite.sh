#!/bin/bash
# ===========================================================================
# scripts/bench_lua_sqlite.sh — Lua 5.5.1 / SQLite 3.53.4 运行时性能对比基准
#
# 用途: dcc 与 gcc 在**运行时**性能上的端到端对照，覆盖编译、功能正确性、
# 性能三条线，四档 -O0/-O1/-O2/-O3 全跑。
#
#   * Lua:   `make posix` 四档构建 → 跑 luafull.lua（浮点/协程/模式匹配/递归）
#            → fib(32) best-of-N 计时
#   * SQLite: 单 TU（shell.c + sqlite3.c）四档直编 → :memory: 功能查询 +
#             20 万行 insert+聚合+ORDER BY best-of-N 计时
#
# 判据（退出码）:
#   * 任一档构建失败      → 记 BUILD-FAIL，exit 1
#   * 任一档功能输出与 gcc -O2 参考字节不一致 → 记 DIFF，exit 1（文档 §4.2
#     的核心不变式：四档间一致 + 与 gcc 同档一致，否则就是误编译）
#   * 性能只**报告**，不判绿（机器噪声大，数字随负载漂移，见 §6.3）
#
# 依赖（非工具，是测试数据；缺失即显式报错退出，不做静默 SKIP）:
#   * Lua 源码树（含 src/Makefile，目标 posix）默认 /root/lua-5.5.1
#   * SQLite  amalgamation 源码树（含 shell.c + sqlite3.c）默认 /root/sqlite
#
# 用法:
#   DCC=/path/dcc LUA_SRC=/p/lua SQLITE_SRC=/p/sqlite OUT=/tmp/x ITERATIONS=5 \
#     bash scripts/bench_lua_sqlite.sh
# 默认: DCC=<repo>/target/release/dcc
# ===========================================================================
set -o pipefail
set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="${DCC_REPO:-$(cd "$HERE/.." && pwd)}"
DCC="${DCC:-$REPO/target/release/dcc}"
LUA_SRC="${LUA_SRC:-/root/lua-5.5.1}"
SQLITE_SRC="${SQLITE_SRC:-/root/sqlite}"
OUT="${OUT:-/tmp/bench_lua_sqlite.txt}"
ITERATIONS="${ITERATIONS:-5}"

: > "$OUT"
say() { echo "$@" | tee -a "$OUT"; }

# ---- 依赖检查 ----
if [ ! -x "$DCC" ]; then
  echo "DCC not found or not executable: $DCC" >&2
  exit 1
fi
if [ ! -f "$LUA_SRC/src/Makefile" ]; then
  echo "Lua source tree missing: $LUA_SRC/src/Makefile" >&2
  exit 1
fi
if [ ! -f "$SQLITE_SRC/shell.c" ] || [ ! -f "$SQLITE_SRC/sqlite3.c" ]; then
  echo "SQLite amalgamation missing: $SQLITE_SRC/{shell.c,sqlite3.c}" >&2
  exit 1
fi

ART="$OUT.artifacts"
mkdir -p "$ART"
failures=0

# ---- fib(32) 测试程序（确定性递归）----
cat > /tmp/fib.lua <<'LUA'
local function fib(n) if n<2 then return n end return fib(n-1)+fib(n-2) end
print(fib(32))
LUA

# ---- SQLite 20 万行负载 ----
cat > /tmp/q.sql <<'SQL'
CREATE TABLE t(id INTEGER PRIMARY KEY, a INT, b TEXT);
BEGIN;
WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<200000)
INSERT INTO t(id,a,b) SELECT i, (i*2654435761)%1000, 'row'||(i%997) FROM n;
COMMIT;
SELECT 'sum', sum(a), count(*) FROM t WHERE a > 500;
SELECT 'order', count(*) FROM (SELECT a FROM t ORDER BY a DESC LIMIT 1000);
SQL

# ---- 计时助手：best-of-N 墙钟（ms）----
bench() {  # $1=exe $2=arg-or-sql-file $3=is_sql(1/0)
  local best=999999 res=""
  for _ in $(seq 1 "$ITERATIONS"); do
    local s e ms
    s=$(date +%s%N)
    if [ "${3:-0}" = 1 ]; then
      res=$("$1" :memory: < "$2" 2>/dev/null | tr '\n' ' ')
    else
      res=$("$1" "$2" 2>/dev/null)
    fi
    e=$(date +%s%N)
    ms=$(( (e-s)/1000000 ))
    [ "$ms" -lt "$best" ] && best=$ms
  done
  echo "${best}ms  result=$res"
}

# =========================== Lua ===========================
say "=== Lua functional output (dcc, each level) ==="
declare -A lua_out
for o in 0 1 2 3; do
  ( cd "$LUA_SRC/src" && make clean >/dev/null 2>&1 && \
    make posix CC="$DCC" MYCFLAGS="-std=gnu99 -O$o" AR="ar rcu" RANLIB=ranlib >"$ART/lua_build_O$o.log" 2>&1 )
  if [ -x "$LUA_SRC/src/lua" ]; then
    cp "$LUA_SRC/src/lua" "$ART/lua_dcc_O$o"
    lua_out[$o]="$( cd "$LUA_SRC/src" && "$ART/lua_dcc_O$o" luafull.lua 2>&1 )"
    say "--- dcc -O$o (rc=$?) ---"
    echo "${lua_out[$o]}" | tee -a "$OUT"
  else
    say "--- dcc -O$o BUILD FAIL ---"; tail -3 "$ART/lua_build_O$o.log" | tee -a "$OUT"
    failures=$((failures+1))
  fi
done

# gcc -O2 参考（功能 + 计时基准）
( cd "$LUA_SRC/src" && make clean >/dev/null 2>&1 && \
  make posix CC=gcc MYCFLAGS="-std=gnu99 -O2" AR="ar rcu" RANLIB=ranlib >/dev/null 2>&1 )
cp "$LUA_SRC/src/lua" "$ART/lua_gcc_O2"
lua_ref="$( cd "$LUA_SRC/src" && "$ART/lua_gcc_O2" luafull.lua 2>&1 )"
say "=== Lua functional output (gcc -O2 reference) ==="
echo "$lua_ref" | tee -a "$OUT"

# 判据: 四档 dcc 间一致 + dcc -O2 与 gcc -O2 一致
for o in 0 1 2 3; do
  if [ -n "${lua_out[$o]:-}" ] && [ "${lua_out[$o]}" != "${lua_out[2]}" ]; then
    say "LUA-LEVEL-DIFF -O$o vs -O2"; failures=$((failures+1))
  fi
done
if [ -n "${lua_out[2]:-}" ] && [ "${lua_out[2]}" != "$lua_ref" ]; then
  say "LUA-REF-DIFF dcc -O2 vs gcc -O2"; failures=$((failures+1))
fi

say ""
say "=== Lua fib(32) best-of-$ITERATIONS wall ==="
for b in lua_gcc_O2 lua_dcc_O0 lua_dcc_O1 lua_dcc_O2 lua_dcc_O3; do
  [ -x "$ART/$b" ] || continue
  say "$b: $(bench "$ART/$b" /tmp/fib.lua 0)"
done

# =========================== SQLite ===========================
say ""
say "=== SQLite functional (dcc, each level) ==="
declare -A sq_out
for o in 0 1 2 3; do
  if "$DCC" -O$o -o "$ART/sq_dcc_O$o" "$SQLITE_SRC/shell.c" "$SQLITE_SRC/sqlite3.c" \
        -lpthread -ldl -lm >"$ART/sq_build_O$o.log" 2>&1; then
    out=$( "$ART/sq_dcc_O$o" :memory: "SELECT 'sqlite '||sqlite_version(); \
      CREATE TABLE t(a,b); INSERT INTO t VALUES(1,'x'),(2,'y'),(3,'z'); \
      SELECT a,b FROM t ORDER BY a DESC; \
      SELECT 'sum='||sum(a),'cnt='||count(*) FROM t;" 2>&1 )
    sq_out[$o]="$out"
    say "--- dcc -O$o ---"; echo "$out" | tee -a "$OUT"
  else
    say "--- dcc -O$o BUILD FAIL ---"; tail -3 "$ART/sq_build_O$o.log" | tee -a "$OUT"
    failures=$((failures+1))
  fi
done

for o in 2 3; do
  gcc -O$o -o "$ART/sq_gcc_O$o" "$SQLITE_SRC/shell.c" "$SQLITE_SRC/sqlite3.c" \
    -lpthread -ldl -lm 2>/dev/null
done
sq_ref=$( "$ART/sq_gcc_O2" :memory: "SELECT 'sqlite '||sqlite_version(); \
  CREATE TABLE t(a,b); INSERT INTO t VALUES(1,'x'),(2,'y'),(3,'z'); \
  SELECT a,b FROM t ORDER BY a DESC; \
  SELECT 'sum='||sum(a),'cnt='||count(*) FROM t;" 2>&1 )
say "=== SQLite functional (gcc -O2 reference) ==="
echo "$sq_ref" | tee -a "$OUT"

for o in 0 1 2 3; do
  if [ -n "${sq_out[$o]:-}" ] && [ "${sq_out[$o]}" != "${sq_out[2]}" ]; then
    say "SQLITE-LEVEL-DIFF -O$o vs -O2"; failures=$((failures+1))
  fi
done
if [ -n "${sq_out[2]:-}" ] && [ "${sq_out[2]}" != "$sq_ref" ]; then
  say "SQLITE-REF-DIFF dcc -O2 vs gcc -O2"; failures=$((failures+1))
fi

say ""
say "=== SQLite 200k benchmark best-of-$ITERATIONS wall ==="
for b in sq_gcc_O2 sq_gcc_O3 sq_dcc_O2 sq_dcc_O3; do
  [ -x "$ART/$b" ] || continue
  say "$b: $(bench "$ART/$b" /tmp/q.sql 1)"
done

say ""
if [ "$failures" -eq 0 ]; then
  say "bench_lua_sqlite: OK (0 build-fail, 0 output-diff)"
  exit 0
else
  say "bench_lua_sqlite: FAIL ($failures issue(s))"
  exit 1
fi
