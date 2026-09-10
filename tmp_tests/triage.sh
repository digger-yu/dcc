#!/usr/bin/env bash
# tmp_tests/triage.sh — 阶段0 triage: 对每个失败测试用 dcc 默认级别 vs -O0 分类
# 分类:
#   O0_FAIL      -O0 也失败 → 前端/codegen 语义错误
#   OPT_BUG      -O0 通过但默认级别失败 → 优化 pass bug
#   COMPILE_FAIL 编译直接失败 (记录 stderr)
set -u
DCC=/root/dcc/target/release/dcc
SUITE=/root/dcc/tests/gcc-torture/gcc/testsuite/gcc.c-torture/execute
OUT=/tmp/dcc-triage
mkdir -p "$OUT"

tests="$*"
if [ -z "$tests" ]; then
  tests="20000227-1 20000914-1 20010409-1 20020411-1 20020412-1 20021120-3 \
20030714-1 20040308-1 20040423-1 20041124-1 20041201-1 20050316-1 20050929-1 \
20061220-1 20070614-1 20230630-2 20230630-4 920908-2 921017-1 921215-1 960512-1 \
970217-1 990413-2 align-3 align-nest built-in-setjmp eeprof-1 medce-1 nestfunc-5 \
pr103209 pr104604 pr105613 pr108292 pr109040 pr111469-1 pr117432 pr122000 \
pr123625 pr126405-3 pr22061-1 pr39100 pr39228 pr41935 pr42614 pr44942 pr47237 \
pr51447 pr51877 pr56837 pr58984 pr60003 pr64242 pr65053-2 pr70460 pr70903 \
pr77767 pr78586 pr78622 pr79327 pr80153 pr82210 pr84169 pr84521 pr85156 \
pr85331 pr88904 pr91450-1 pr92904 pr94412 pr94524-1 pr94524-2 stdarg-1 \
stdarg-2 strlen-6 va-arg-pack-1"
fi

for t in $tests; do
  src="$SUITE/$t.c"
  [ -f "$src" ] || { echo "$t: NO_SOURCE"; continue; }
  # 编译错误捕获
  if ! "$DCC" -std=gnu11 -lm -o "$OUT/$t.default" "$src" 2> "$OUT/$t.compile_err"; then
    echo "$t: COMPILE_FAIL  (see $OUT/$t.compile_err)"
    continue
  fi
  rc=0; timeout 10 "$OUT/$t.default" >/dev/null 2>&1 || rc=$?
  if [ $rc -eq 0 ]; then
    echo "$t: PASS_NOW (fixed?)"
    continue
  fi
  mode="RUNTIME_FAIL"; [ $rc -eq 124 ] && mode="TIMEOUT"
  # -O0 对照
  if ! "$DCC" -std=gnu11 -O0 -lm -o "$OUT/$t.o0" "$src" 2> "$OUT/$t.o0_err"; then
    echo "$t: $mode O0_COMPILE_FAIL"
    continue
  fi
  rc0=0; timeout 10 "$OUT/$t.o0" >/dev/null 2>&1 || rc0=$?
  if [ $rc0 -eq 0 ]; then
    echo "$t: $mode OPT_BUG (O0 passes)"
  else
    m0="RUNTIME_FAIL"; [ $rc0 -eq 124 ] && m0="TIMEOUT"
    echo "$t: $mode O0_FAIL($m0) → frontend/codegen semantic bug"
  fi
done
