#!/usr/bin/env bash
# 全量差分扫描 tests/fixtures/c/*.c：逐字符比较 gcc 与 dcc 的 stdout。
# 用法: scripts/scan_all_fixtures.sh <dcc 二进制> <输出清单文件>
# 输出每行: 状态<TAB>文件名
#   MATCH      一致
#   MISMATCH   stdout 或退出码不一致
#   GCCFAIL    gcc 自身编译失败
#   DCCFAIL    dcc 编译失败
set -u
DCC=${1:-/root/dcc/target/release/dcc}
OUT=${2:-/tmp/scan_result.txt}
GCC=${GCC:-gcc}
WORK=/tmp/dcc-scan-all.$$
mkdir -p "$WORK"
: > "$OUT"
for f in /root/dcc/tests/fixtures/c/*.c; do
  b=$(basename "$f" .c)
  if ! "$GCC" -std=gnu17 -no-pie -O0 -o "$WORK/$b.gcc" "$f" -lm >"$WORK/g.log" 2>&1; then
    printf 'GCCFAIL\t%s\n' "$f" >> "$OUT"; continue
  fi
  if ! "$DCC" -std=gnu23 -o "$WORK/$b.dcc" "$f" >"$WORK/d.log" 2>&1; then
    printf 'DCCFAIL\t%s\n' "$f" >> "$OUT"; continue
  fi
  rm -f "$WORK/$b.gcc.out" "$WORK/$b.dcc.out"
  ( cd "$WORK" && timeout 10 "./$b.gcc" > "$b.gcc.out" 2>&1 ); g=$?
  ( cd "$WORK" && timeout 10 "./$b.dcc" > "$b.dcc.out" 2>&1 ); d=$?
  if [ "$g" = "$d" ] && cmp -s "$WORK/$b.gcc.out" "$WORK/$b.dcc.out"; then
    printf 'MATCH\t%s\n' "$f" >> "$OUT"
  else
    printf 'MISMATCH\t%s\n' "$f" >> "$OUT"
  fi
done
rm -rf "$WORK"
echo "total=$(wc -l < "$OUT") match=$(grep -c '^MATCH' "$OUT") mismatch=$(grep -c '^MISMATCH' "$OUT") gccfail=$(grep -c '^GCCFAIL' "$OUT") dccfail=$(grep -c '^DCCFAIL' "$OUT")"
