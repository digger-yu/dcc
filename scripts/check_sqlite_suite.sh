#!/bin/bash
# ===========================================================================
# scripts/check_sqlite_suite.sh — SQLite 官方 TCL 套件（gcc 基线 vs dcc 各档）
#
# 判据:
#   * 基线 = gcc 编译的 testfixture 跑同一套件。SQLite 官方套件在部分环境下
#     本来就有失败项（权限、平台、可选扩展），那些不算 dcc 的问题。
#   * 判红条件（三者任一）:
#       DCC-BUILD-FAIL  dcc 编不出 testfixture
#       SUITE-INCOMPLETE 套件没跑到汇总行（崩溃/超时，日志里没有
#                        "N errors out of M tests"）
#       SUITE-DIFF      dcc 的失败集合里出现了 gcc 没有的测试名
#   * gcc 自己也失败的测试不判红 —— 否则门禁从第一天起就长期红。
#
# 为什么必须非 root: test/attach.test 的 attach-6.2 会把文件 chmod 0000 再
# 断言打不开。root 无视权限，该用例直接 puts "Tests do not work when run as
# root" 并 exit 1，整套提前结束。脚本因此在 root 下自动降权到 $SUITE_USER。
#
# 为什么用源码树副本: 构建会往树里写 Makefile/产物/临时数据库，直接在
# $SQLITE_SRC 里跑会污染源树，也过不了"运行用户需要写权限"这一关。
#
# 依赖: bash>=4, tclsh + tcl.h + tclConfig.sh (tcl8.6-dev), unzip,
#       $SQLITE_SRC（由 scripts/setup_sqlite_suite.sh 准备）
#
# 用法（手工执行，不挂 CI 触发器，和 torture 一样）:
#   bash scripts/setup_sqlite_suite.sh
#   bash scripts/check_sqlite_suite.sh
#   LEVELS="0 1 2 3" bash scripts/check_sqlite_suite.sh
#   SUITE="test/quick.test test/veryquick.test" bash scripts/check_sqlite_suite.sh
# ===========================================================================
set -o pipefail

DCC=${DCC:-/root/dcc/target/release/dcc}
REFCC=${REFCC:-gcc}
SQLITE_SRC=${SQLITE_SRC:-$HOME/sqlite-src-3540000}
OUT=${OUT:-/tmp/sqlite_suite}
SUITE=${SUITE:-test/quick.test}
LEVELS=${LEVELS:-2}
SUITE_USER=${SUITE_USER:-sqltest}
WITH_TCL=${WITH_TCL:-}
BUILD_TIMEOUT_S=${BUILD_TIMEOUT_S:-3600}
TEST_TIMEOUT_S=${TEST_TIMEOUT_S:-3600}

mkdir -p "$OUT" || exit 1
SUMMARY="$OUT/summary.txt"
: > "$SUMMARY"
say() { echo "$*" | tee -a "$SUMMARY"; }
fails=0

die() {
  say "ERROR: $*"
  exit 2
}

# ---- 依赖检查 -------------------------------------------------------------
case "${BASH_VERSINFO[0]:-0}" in
  '' | 0 | 1 | 2 | 3) die "bash >= 4 required" ;;
esac
[ -x "$DCC" ] || die "DCC not executable: $DCC"
command -v "$REFCC" >/dev/null 2>&1 || die "reference compiler missing: $REFCC"
command -v tclsh >/dev/null 2>&1 || die "tclsh missing (apt-get install tcl-dev)"
# 发行版把 tclConfig.sh 放在 /usr/lib/tcl8.6 或 /usr/lib/<triplet>/tcl8.6，
# 不同版本不一致，这里直接找，找不到才报错。
if [ -z "$WITH_TCL" ]; then
  cfg=$(find /usr/lib /usr/local/lib -maxdepth 3 -name tclConfig.sh 2>/dev/null | head -1)
  [ -n "$cfg" ] && WITH_TCL=$(dirname "$cfg")
fi
[ -n "$WITH_TCL" ] && [ -f "$WITH_TCL/tclConfig.sh" ] ||
  die "tclConfig.sh not found (apt-get install tcl-dev)"
if [ -z "$(find /usr/include /usr/local/include -name tcl.h 2>/dev/null | head -1)" ]; then
  die "tcl.h missing (apt-get install tcl-dev)"
fi
[ -f "$SQLITE_SRC/configure" ] || die "no configure in $SQLITE_SRC"
[ -d "$SQLITE_SRC/test" ] || die "no test/ in $SQLITE_SRC (run scripts/setup_sqlite_suite.sh)"
[ -n "$SUITE" ] || die "SUITE is empty"

n_test=$(find "$SQLITE_SRC/test" -maxdepth 1 -name '*.test' | wc -l)
say "suite source: $SQLITE_SRC ($n_test .test files)"
say "run: SUITE='$SUITE' LEVELS='$LEVELS'"

# ---- 工作副本 + 降权 ------------------------------------------------------
WORK="$OUT/run"
rm -rf "$WORK"
mkdir -p "$WORK" || die "cannot create $WORK"
say "copying source tree to $WORK/src ..."
cp -a "$SQLITE_SRC" "$WORK/src" || die "copy failed"

RUN=()
if [ "$(id -u)" -eq 0 ]; then
  if ! id "$SUITE_USER" >/dev/null 2>&1; then
    useradd -m "$SUITE_USER" || die "cannot create user $SUITE_USER"
    say "created user: $SUITE_USER"
  fi
  chown -R "$SUITE_USER" "$WORK" || die "chown failed"
  # -u BASH_ENV/-u ENV: 调用方的 shell 启动钩子常指向 root 私有路径，降权后
  # 每次 bash 启动都会 "权限不够"，还会把 configure 的编译器探测带偏。
  RUN=(runuser -u "$SUITE_USER" -- env -u BASH_ENV -u ENV)
  say "dropping privileges to: $SUITE_USER (SQLite refuses to run as root)"
else
  say "running as: $(id -un)"
fi

as_user() { # $1 = 在 $WORK/src 下执行的 shell 片段
  if [ "${#RUN[@]}" -eq 0 ]; then
    bash -c "$1"
  else
    "${RUN[@]}" bash -c "$1"
  fi
}

SRC="$WORK/src"
say "configure ..."
as_user "cd '$SRC' && ./configure --with-tcl='$WITH_TCL'" >"$OUT/configure.log" 2>&1 ||
  die "configure failed, see $OUT/configure.log"

# ---- 构建与运行 -----------------------------------------------------------
build() { # $1=cc $2=cflags $3=log
  rm -f "$SRC/testfixture"
  as_user "cd '$SRC' && timeout $BUILD_TIMEOUT_S make testfixture CC='$1' CFLAGS='$2'" \
    >"$3" 2>&1
  [ -x "$SRC/testfixture" ]
}

# 失败集合: 只取测试名。行格式为 "! xxx-1.2 expected: ... got: ..."，
# 汇总行 "! N errors out of M tests" 用数字前缀排除。
extract_fails() {
  grep -E '^! ' "$1" 2>/dev/null |
    grep -vE '^! +[0-9]+ errors out of' |
    sed -E 's/^! +//' |
    awk '{print $1}' |
    sort -u
}

complete() { grep -qE '[0-9]+ errors out of [0-9]+ tests' "$1" 2>/dev/null; }

run_suite() { # $1=tag
  rm -f "$SRC/test.db" "$SRC/test.db-journal" 2>/dev/null
  as_user "cd '$SRC' && timeout $TEST_TIMEOUT_S ./testfixture $SUITE" >"$OUT/run-$1.log" 2>&1
  rc=$?
  extract_fails "$OUT/run-$1.log" >"$OUT/fails-$1.txt"
  if ! complete "$OUT/run-$1.log"; then
    say "SUITE-INCOMPLETE $1 (rc=$rc) — no summary line, see $OUT/run-$1.log"
    fails=$((fails + 1))
    return 1
  fi
  echo "$rc"
}

say ""
say "=== baseline: $REFCC -O2 ==="
if ! build "$REFCC" "-O2 -g" "$OUT/build-ref.log"; then
  die "$REFCC could not build testfixture, see $OUT/build-ref.log"
fi
run_suite "ref" >/dev/null
ref_n=$(wc -l <"$OUT/fails-ref.txt")
say "baseline failures: $ref_n"

declare -A RESULT=()
for L in $LEVELS; do
  say ""
  say "=== dcc -O$L ==="
  if ! build "$DCC" "-O$L -g" "$OUT/build-dcc-O$L.log"; then
    say "DCC-BUILD-FAIL -O$L (see $OUT/build-dcc-O$L.log)"
    tail -3 "$OUT/build-dcc-O$L.log" | sed 's/^/  /' | tee -a "$SUMMARY"
    RESULT[$L]="BUILD-FAIL"
    fails=$((fails + 1))
    continue
  fi
  if ! run_suite "dcc-O$L" >/dev/null; then
    RESULT[$L]="INCOMPLETE"
    continue
  fi
  comm -13 "$OUT/fails-ref.txt" "$OUT/fails-dcc-O$L.txt" >"$OUT/newfails-O$L.txt"
  new_n=$(wc -l <"$OUT/newfails-O$L.txt")
  if [ "$new_n" -gt 0 ]; then
    say "SUITE-DIFF -O$L: $new_n test(s) fail under dcc but pass under $REFCC"
    sed 's/^/  /' "$OUT/newfails-O$L.txt" | tee -a "$SUMMARY"
    RESULT[$L]="DIFF($new_n)"
    fails=$((fails + 1))
  else
    say "ok -O$L (baseline failures $ref_n, no new failures)"
    RESULT[$L]="ok"
  fi
done

# ---- 汇总 -----------------------------------------------------------------
say ""
say "=== summary ==="
say "$(printf '%-12s %-10s %-10s %s' tag build fails-new status)"
ref_fails="$ref_n"
for L in $LEVELS; do
  st=${RESULT[$L]:-skipped}
  say "$(printf '%-12s %-10s %-10s %s' "dcc -O$L" "$([ "$st" = BUILD-FAIL ] && echo fail || echo ok)" \
    "$(echo "$st" | sed -n 's/^DIFF(\(.*\))$/\1/p')" "$st")"
done
say "$(printf '%-12s %-10s %-10s %s' "$REFCC -O2" ok "$ref_fails" "baseline")"

if [ "$fails" -eq 0 ]; then
  say "RESULT: PASS"
  exit 0
fi
say "RESULT: FAIL ($fails issue(s))"
exit 1
