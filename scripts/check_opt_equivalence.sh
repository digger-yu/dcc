#!/bin/bash
# ===========================================================================
# scripts/check_opt_equivalence.sh — 优化档位执行等价不变式门禁
#
# 判据: 同一份源码用 dcc 自己编 -O0 / -O2 / -O3, 三档的 **stdout 与退出码必须完全
# 一致**。为什么以 -O0 为基准而不是 gcc: 部分夹具的绝对期望值本就与 gcc 不同
# (需要特定运行时支持 / 故意 abort / 多 TU 链接), 那些是"运行库覆盖缺口"而不是
# 优化器缺陷; 而优化器的正确性定义恰好就是「不改行为」。
#
# 为什么必须有这一条: `make test` 的 Tier 0 不跑 -O 档位 (`tests/integration/opt.rs`
# 里的 m16_* 「优化测试」全部零参数 = -O0), 因此一批静默误编译 (LICM 把 while 体
# 掏空成死循环、GVN 无 kill 机制、并行发射标号撞名、假 hint 造成 SEGV) 能在门禁
# 全绿的情况下长期存在。本轮就是靠这个扫描发现并修完了它们。
#
# 输出: 只报 OPT-DIFF / COMPILE-FAIL; COMPILE-FAIL 先作为存量债报告不置红
# (根因是夹具自身的运行时依赖, 需逐个定性), OPT-DIFF 非空即 exit 1。
# 用法: DCC=... DCC_REPO=... JOBS=2 bash scripts/check_opt_equivalence.sh
# ===========================================================================
set -o pipefail
DCC=${DCC:-/root/dcc/target/release/dcc}
REPO=${DCC_REPO:-/root/dcc}
OUT=${OUT:-/tmp/opteq2}
jobs=${JOBS:-2}
timeout_s=${TIMEOUT_S:-20}
mkdir -p "$OUT"

probe() {
  local f=$1
  local tag
  tag=$(echo "$f" | sed "s|$REPO/tests/fixtures/||; s|/|_|g")
  local base_rc="" 
  for L in 0 2 3; do
    if ! "$DCC" -O$L -std=gnu2x -I "$REPO/tests/fixtures/c" -I "$REPO" "$f" \
         -o "$OUT/${tag}_d$L" >"$OUT/${tag}_d$L.build" 2>&1; then
      echo "COMPILE-FAIL $tag -O$L" >> "$OUT/summary.txt"
      return 0
    fi
    ( cd "$OUT" && timeout "$timeout_s" "./${tag}_d$L" > "${tag}_d$L.out" 2>/dev/null )
    rc=$?
    if [ "$L" = 0 ]; then
      base_rc=$rc
      mv "$OUT/${tag}_d0.out" "$OUT/${tag}_base.out" 2>/dev/null
    else
      if [ "$rc" -ne "$base_rc" ] || ! cmp -s "$OUT/${tag}_base.out" "$OUT/${tag}_d$L.out"; then
        echo "OPT-DIFF $tag -O$L base_rc=$base_rc opt_rc=$rc" >> "$OUT/summary.txt"
      fi
    fi
  done
  return 0
}
export -f probe; export DCC REPO OUT timeout_s

: > "$OUT/summary.txt"
find "$REPO/tests/fixtures/c" "$REPO/tests/fixtures/opt" -name '*.c' | sort \
  | xargs -P "$jobs" -I{} bash -c 'probe "$@"' _ {}
total=$(find "$REPO/tests/fixtures/c" "$REPO/tests/fixtures/opt" -name '*.c' | wc -l)
diffs=$(grep -c '^OPT-DIFF' "$OUT/summary.txt" || true)
fails=$(grep -c '^COMPILE-FAIL' "$OUT/summary.txt" || true)
echo "=== 语料总数: $total"
echo "=== OPT-DIFF: $diffs   COMPILE-FAIL: $fails"
if [ "$diffs" -gt 0 ]; then
  grep '^OPT-DIFF' "$OUT/summary.txt" | sort | head -40
  echo "OPT-DIFF 非空 → FAIL (优化器改变了程序行为)"
  exit 1
fi
if [ "$fails" -gt 0 ]; then
  grep '^COMPILE-FAIL' "$OUT/summary.txt" | sort | head -10
  echo "注: COMPILE-FAIL 不置红 (夹具自身依赖), 但不得增长"
fi
echo "check_opt_equivalence: OK (0 OPT-DIFF)"
exit 0
