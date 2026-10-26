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
#   功能与一致性判据跑四档；性能线只测 -O2/-O3（对应文档引用的两个档位）。
#
# 判据（退出码）:
#   * 任一档构建失败                → 记 BUILD-FAIL，exit 1
#   * 参考（gcc）构建失败           → 记 REF-BUILD-FAIL，**不**再做 REF-DIFF
#   * 任一档功能输出与 gcc -O2 参考字节不一致 → 记 DIFF，exit 1（四档间一致
#     + 与 gcc 同档一致，否则就是误编译）
#   * 任一计时样本非 0 退出         → 记 BENCH-FAIL，exit 1（不给崩溃样本计时）
#   * 性能数字本身只**报告**，不判绿（机器噪声大，见 §6.3）
#
# 依赖（非工具，是测试数据；缺失即显式报错退出，不做静默 SKIP）:
#   * bash >= 4（关联数组）
#   * 计时源：bash >= 5 的 EPOCHREALTIME（首选，零 fork）或 GNU date +%N
#   * Lua 源码树（含 src/Makefile 与 src/luafull.lua，目标 posix）
#     默认 /root/lua-5.5.1
#   * SQLite  amalgamation 源码树（含 shell.c + sqlite3.c）默认 /root/sqlite
#   * 参考编译器 gcc，可用 REFCC 覆盖（单条命令名，不带空格）
#
# 用法:
#   DCC=/path/dcc LUA_SRC=/p/lua SQLITE_SRC=/p/sqlite OUT=/tmp/x ITERATIONS=5 \
#     REFCC=gcc bash scripts/bench_lua_sqlite.sh
# 默认: DCC=<repo>/target/release/dcc, REFCC=gcc
#
# 每次运行都会重建 "$OUT.artifacts"，不复用上一次的二进制（否则本次构建失败
# 时会拿旧产物计时）。
# ===========================================================================
set -o pipefail
set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="${DCC_REPO:-$(cd "$HERE/.." && pwd)}"
DCC="${DCC:-$REPO/target/release/dcc}"
REFCC="${REFCC:-gcc}"
LUA_SRC="${LUA_SRC:-/root/lua-5.5.1}"
SQLITE_SRC="${SQLITE_SRC:-/root/sqlite}"
OUT="${OUT:-/tmp/bench_lua_sqlite.txt}"
ITERATIONS="${ITERATIONS:-5}"

# ---- 环境检查 ----
if [ -z "${BASH_VERSINFO:-}" ] || [ "${BASH_VERSINFO[0]}" -lt 4 ]; then
  echo "bash >= 4 required (associative arrays); got: ${BASH_VERSION:-not bash}" >&2
  exit 1
fi
case "$ITERATIONS" in
  '' | *[!0-9]*)
    echo "ITERATIONS must be a positive integer: '$ITERATIONS'" >&2
    exit 1
    ;;
esac
if [ "$ITERATIONS" -lt 1 ]; then
  echo "ITERATIONS must be >= 1: $ITERATIONS" >&2
  exit 1
fi

if [ ! -x "$DCC" ]; then
  echo "DCC not found or not executable: $DCC" >&2
  exit 1
fi
if ! command -v "$REFCC" >/dev/null 2>&1; then
  echo "reference compiler not found: $REFCC" >&2
  exit 1
fi
if [ ! -f "$LUA_SRC/src/Makefile" ]; then
  echo "Lua source tree missing: $LUA_SRC/src/Makefile" >&2
  exit 1
fi
if [ ! -f "$LUA_SRC/src/luafull.lua" ]; then
  echo "Lua functional script missing: $LUA_SRC/src/luafull.lua" >&2
  exit 1
fi
if [ ! -f "$SQLITE_SRC/shell.c" ] || [ ! -f "$SQLITE_SRC/sqlite3.c" ]; then
  echo "SQLite amalgamation missing: $SQLITE_SRC/{shell.c,sqlite3.c}" >&2
  exit 1
fi

OUT_DIR="$(dirname "$OUT")"
[ -d "$OUT_DIR" ] || mkdir -p "$OUT_DIR" 2>/dev/null
: > "$OUT" || {
  echo "cannot write output file: $OUT" >&2
  exit 1
}

ART="$OUT.artifacts"
rm -rf "$ART" 2>/dev/null
mkdir -p "$ART" || {
  echo "cannot create artifact dir: $ART" >&2
  exit 1
}
failures=0
say() { echo "$@" | tee -a "$OUT"; }

# ---- 计时源：优先零 fork ----
# `date` 两次调用约 2 ms，会系统性抬高每个样本；EPOCHREALTIME 是 bash 内置
# 变量，微秒精度且无 fork。
NOW_US=0
if [ -n "${EPOCHREALTIME:-}" ]; then
  CLOCK="bash EPOCHREALTIME (us)"
  now_us() {
    local t=${EPOCHREALTIME/./}
    NOW_US=$((10#$t))
  }
elif [ "$(date +%N 2>/dev/null)" != "N" ] && [ -n "$(date +%N 2>/dev/null)" ]; then
  CLOCK="date +%s%N (+~2ms per sample)"
  now_us() { NOW_US=$(($(date +%s%N) / 1000)); }
else
  echo "no usable clock: need bash>=5 EPOCHREALTIME or GNU date +%N" >&2
  exit 1
fi

# ---- fib(32) 测试程序（确定性递归，结果应为 2178309）----
cat > "$ART/fib.lua" <<'LUA'
local function fib(n) if n<2 then return n end return fib(n-1)+fib(n-2) end
print(fib(32))
LUA
FIB_PROG="$ART/fib.lua"
FIB_EXPECT=2178309

# ---- SQLite 20 万行负载 ----
cat > "$ART/q.sql" <<'SQL'
CREATE TABLE t(id INTEGER PRIMARY KEY, a INT, b TEXT);
BEGIN;
WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<200000)
INSERT INTO t(id,a,b) SELECT i, (i*2654435761)%1000, 'row'||(i%997) FROM n;
COMMIT;
SELECT 'sum', sum(a), count(*) FROM t WHERE a > 500;
SELECT 'order', count(*) FROM (SELECT a FROM t ORDER BY a DESC LIMIT 1000);
SQL
SQL_LOAD="$ART/q.sql"

# ---- 计时助手：best-of-N 墙钟（ms）----
# 非 0 退出的样本跳过且不计入 best（崩溃往往是最快的一次）；失败记录写文件，
# 因为 bench 在 $( ) 子 shell 里跑，改不到外面的 failures。
: > "$ART/bench.fail"
bench() { # $1=exe $2=arg-or-sql-file $3=is_sql(1/0)
  local best=-1 res="" out="" rc ms i s e
  for ((i = 1; i <= ITERATIONS; i++)); do
    now_us
    s=$NOW_US
    if [ "${3:-0}" = 1 ]; then
      out=$("$1" :memory: < "$2" 2>"$ART/bench.err" | tr '\n' ' ')
    else
      out=$("$1" "$2" 2>"$ART/bench.err")
    fi
    rc=$?
    now_us
    e=$NOW_US
    if [ "$rc" -ne 0 ]; then
      echo "$(basename "$1") rc=$rc $(head -1 "$ART/bench.err" 2>/dev/null)" \
        >>"$ART/bench.fail"
      continue
    fi
    ms=$(((e - s) / 1000))
    res=$out
    if [ "$best" -lt 0 ] || [ "$ms" -lt "$best" ]; then
      best=$ms
    fi
  done
  if [ "$best" -lt 0 ]; then
    echo "FAILED (no successful run)"
  else
    echo "${best}ms  result=$res"
  fi
}

# =========================== Lua ===========================
say "=== Lua functional output (dcc, each level) ==="
declare -A lua_out=([0]="" [1]="" [2]="" [3]="")
declare -A LUA_RC=([0]=0 [1]=0 [2]=0 [3]=0)
for o in 0 1 2 3; do
  if (cd "$LUA_SRC/src" && rm -f lua &&
    make posix CC="$DCC" MYCFLAGS="-std=gnu99 -O$o" AR="ar rcu" \
      RANLIB=ranlib) >"$ART/lua_build_O$o.log" 2>&1 &&
    [ -x "$LUA_SRC/src/lua" ]; then
    cp "$LUA_SRC/src/lua" "$ART/lua_dcc_O$o"
    lua_out[$o]="$(cd "$LUA_SRC/src" && "$ART/lua_dcc_O$o" luafull.lua 2>&1)"
    LUA_RC[$o]=$?
    say "--- dcc -O$o (rc=${LUA_RC[$o]}) ---"
    echo "${lua_out[$o]}" | tee -a "$OUT"
  else
    say "--- dcc -O$o BUILD FAIL ---"
    tail -3 "$ART/lua_build_O$o.log" | tee -a "$OUT"
    failures=$((failures + 1))
  fi
done

# 参考（功能 + 计时基准）
lua_ref=""
if (cd "$LUA_SRC/src" && rm -f lua &&
  make posix CC="$REFCC" MYCFLAGS="-std=gnu99 -O2" AR="ar rcu" \
    RANLIB=ranlib) >"$ART/lua_build_ref.log" 2>&1 &&
  [ -x "$LUA_SRC/src/lua" ]; then
  cp "$LUA_SRC/src/lua" "$ART/lua_ref_O2"
  lua_ref="$(cd "$LUA_SRC/src" && "$ART/lua_ref_O2" luafull.lua 2>&1)"
  say "=== Lua functional output ($REFCC -O2 reference) ==="
  echo "$lua_ref" | tee -a "$OUT"
else
  say "=== REF-BUILD-FAIL $REFCC -O2 (see $ART/lua_build_ref.log) ==="
  tail -3 "$ART/lua_build_ref.log" | tee -a "$OUT"
  failures=$((failures + 1))
fi

# 判据: 四档 dcc 间一致 + dcc -O2 与参考一致
if [ -z "${lua_out[2]}" ]; then
  say "LUA-LEVEL/REF-DIFF skipped: -O2 baseline missing"
else
  for o in 0 1 2 3; do
    [ -n "${lua_out[$o]}" ] || continue
    if [ "${lua_out[$o]}" != "${lua_out[2]}" ]; then
      say "LUA-LEVEL-DIFF -O$o vs -O2"
      failures=$((failures + 1))
    fi
    if [ "${LUA_RC[$o]}" -ne "${LUA_RC[2]}" ]; then
      say "LUA-RC-DIFF -O$o (${LUA_RC[$o]}) vs -O2 (${LUA_RC[2]})"
      failures=$((failures + 1))
    fi
  done
  if [ -n "$lua_ref" ] && [ "${lua_out[2]}" != "$lua_ref" ]; then
    say "LUA-REF-DIFF dcc -O2 vs $REFCC -O2"
    failures=$((failures + 1))
  fi
fi

say ""
say "=== Lua fib(32) best-of-$ITERATIONS wall (clock: $CLOCK) ==="
# fib(32) 的输出是确定的；参考跑不对时，后面的毫秒数没有意义
if [ -x "$ART/lua_ref_O2" ]; then
  fib_got=$("$ART/lua_ref_O2" "$FIB_PROG" 2>&1)
  if [ "$fib_got" != "$FIB_EXPECT" ]; then
    say "FIB-SANITY-FAIL reference printed '$fib_got', expected $FIB_EXPECT"
    failures=$((failures + 1))
  fi
fi
for b in lua_ref_O2 lua_dcc_O0 lua_dcc_O1 lua_dcc_O2 lua_dcc_O3; do
  if [ ! -x "$ART/$b" ]; then
    say "$b: SKIP (binary missing)"
    continue
  fi
  say "$b: $(bench "$ART/$b" "$FIB_PROG" 0)"
done

# =========================== SQLite ===========================
SQ_PROBE="SELECT 'sqlite '||sqlite_version(); \
CREATE TABLE t(a,b); INSERT INTO t VALUES(1,'x'),(2,'y'),(3,'z'); \
SELECT a,b FROM t ORDER BY a DESC; \
SELECT 'sum='||sum(a),'cnt='||count(*) FROM t;"

say ""
say "=== SQLite functional (dcc, each level) ==="
declare -A sq_out=([0]="" [1]="" [2]="" [3]="")
for o in 0 1 2 3; do
  rm -f "$ART/sq_dcc_O$o"
  if "$DCC" -O$o -o "$ART/sq_dcc_O$o" "$SQLITE_SRC/shell.c" \
    "$SQLITE_SRC/sqlite3.c" -lpthread -ldl -lm >"$ART/sq_build_O$o.log" 2>&1 &&
    [ -x "$ART/sq_dcc_O$o" ]; then
    out=$("$ART/sq_dcc_O$o" :memory: "$SQ_PROBE" 2>&1)
    sq_out[$o]="$out"
    say "--- dcc -O$o ---"
    echo "$out" | tee -a "$OUT"
  else
    say "--- dcc -O$o BUILD FAIL ---"
    tail -3 "$ART/sq_build_O$o.log" | tee -a "$OUT"
    failures=$((failures + 1))
  fi
done

# 参考：性能线只需要 -O2/-O3
sq_ref=""
for o in 2 3; do
  rm -f "$ART/sq_ref_O$o"
  if ! "$REFCC" -O$o -o "$ART/sq_ref_O$o" "$SQLITE_SRC/shell.c" \
    "$SQLITE_SRC/sqlite3.c" -lpthread -ldl -lm >"$ART/sq_build_ref_O$o.log" 2>&1; then
    say "=== REF-BUILD-FAIL $REFCC -O$o (see $ART/sq_build_ref_O$o.log) ==="
    tail -3 "$ART/sq_build_ref_O$o.log" | tee -a "$OUT"
    failures=$((failures + 1))
  fi
done
if [ -x "$ART/sq_ref_O2" ]; then
  sq_ref=$("$ART/sq_ref_O2" :memory: "$SQ_PROBE" 2>&1)
  say "=== SQLite functional ($REFCC -O2 reference) ==="
  echo "$sq_ref" | tee -a "$OUT"
fi

if [ -z "${sq_out[2]}" ]; then
  say "SQLITE-LEVEL/REF-DIFF skipped: -O2 baseline missing"
else
  for o in 0 1 2 3; do
    [ -n "${sq_out[$o]}" ] || continue
    if [ "${sq_out[$o]}" != "${sq_out[2]}" ]; then
      say "SQLITE-LEVEL-DIFF -O$o vs -O2"
      failures=$((failures + 1))
    fi
  done
  if [ -n "$sq_ref" ] && [ "${sq_out[2]}" != "$sq_ref" ]; then
    say "SQLITE-REF-DIFF dcc -O2 vs $REFCC -O2"
    failures=$((failures + 1))
  fi
fi

say ""
say "=== SQLite 200k benchmark best-of-$ITERATIONS wall (clock: $CLOCK) ==="
for b in sq_ref_O2 sq_ref_O3 sq_dcc_O2 sq_dcc_O3; do
  if [ ! -x "$ART/$b" ]; then
    say "$b: SKIP (binary missing)"
    continue
  fi
  say "$b: $(bench "$ART/$b" "$SQL_LOAD" 1)"
done

# 计时样本失败发生在 bench 的子 shell 里，这里汇总
if [ -s "$ART/bench.fail" ]; then
  say "BENCH-FAIL: $(wc -l <"$ART/bench.fail") sample(s) exited nonzero"
  sed 's/^/  /' "$ART/bench.fail" | tee -a "$OUT"
  failures=$((failures + 1))
fi

say ""
if [ "$failures" -eq 0 ]; then
  say "bench_lua_sqlite: OK (0 build-fail, 0 output-diff)"
  exit 0
else
  say "bench_lua_sqlite: FAIL ($failures issue(s))"
  exit 1
fi
