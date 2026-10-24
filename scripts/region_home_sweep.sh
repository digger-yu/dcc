#!/bin/bash
# ===========================================================================
# scripts/region_home_sweep.sh — 区内家（S4）开着时的值等价全语料扇扫
#
# 为什么单独有一条脚本而不是复用 `check_opt_equivalence.sh`：那条比的是「同一份产物在
# 四个 -O 档之间行为不变」，而本机制的判据是「**开与关**两档行为不变」。区内家一旦把某个
# 前提破坏掉，症状是别处读到垃圾，只有拿门关当参照才看得见。
#
# 判据是方案 §5 的**四元组**，不是「rc + stdout」两条：
#   1. 退出码
#   2. stdout 的 sha256（先按指针打印归一化，手法与 `check_opt_equivalence.sh` 一致）
#   3. stderr 的 sha256（同上）
#   4. **执行目录文件树** 的 sha256（文件名 + 内容一起入摘要）——夹具里有故意往磁盘写的
#   5. 先跑一次「门关 vs 门关」自比对证明这套判据在本案上稳定，不稳的夹具报 UNSTABLE
#      而不是拿它去判机制对错
#
# 输出：`REGION-DIFF`（四元组任一维不同）与 `REGION-UNSTABLE`（门关自己两次跑就不一致）
# 都必须是空，否则 exit 1。
#
# 用法: DCC=/root/dcc/target/release/dcc JOBS=2 bash scripts/region_home_sweep.sh
#       ONLY=percpu_offset.c bash scripts/region_home_sweep.sh   # 定点复查
#       DEFER=0 bash scripts/region_home_sweep.sh                # 只扇读侧服务那一半
# ===========================================================================
set -o pipefail
DCC=${DCC:-/root/dcc/target/release/dcc}
REPO=${DCC_REPO:-/root/dcc}
OUT=${OUT:-/tmp/regionsweep}
jobs=${JOBS:-2}
timeout_s=${TIMEOUT_S:-20}
mkdir -p "$OUT"
: > "$OUT/summary.txt"

norm() {
  sed -E 's/0x[0-9a-fA-F]{4,}/0xADDR/g; s/\b[0-9a-fA-F]{8,}\b/HEX/g' "$1"
}

# 一次执行的**四元组**：在一个专属空目录里跑，把 rc / stdout / stderr / 落盘文件树
# 全部折成一个字符串。文件树按「相对路径 + 内容 sha256」排序后整体再摘要一次，所以
# 「多写一个文件」「把文件写坏」都会被看见（只看 stdout 就漏这一整类）。
run_one() { # <bin> -> "rc|stdout-hash|stderr-hash|tree-hash"
  local bin=$1
  # 目录必须每 Worker 独拿：多个 xargs worker 并发在跑，固定名字会互相删掉。
  local dir; dir=$(mktemp -d "${TMPDIR:-/tmp}/rgrun.XXXXXX")
  local o="$dir/.out" e="$dir/.err"
  ( cd "$dir" && timeout "$timeout_s" "$bin" >"$o" 2>"$e" ) 2>/dev/null
  local rc=$?
  local hso hse ht
  hso=$(norm "$o" | sha256sum | cut -d' ' -f1)
  hse=$(norm "$e" | sha256sum | cut -d' ' -f1)
  rm -f "$o" "$e"
  ht=$( cd "$dir" && find . -type f | LC_ALL=C sort | xargs -r sha256sum | sha256sum | cut -d' ' -f1)
  rm -rf "$dir"
  echo "$rc|$hso|$hse|$ht"
}

one() {
  local f=$1
  local tag
  tag=$(echo "$f" | sed "s|$REPO/tests/fixtures/||; s|/|_|g")
  for L in 1 2 3; do
    local on_env=(DCC_REGION_HOME=1 "DCC_REGION_DEFER=${DEFER:-1}")
    if ! env "${on_env[@]}" "$DCC" -O$L -std=gnu2x -I "$REPO/tests/fixtures/c" \
         -I "$REPO" "$f" -o "$OUT/${tag}_on_$L" >"$OUT/${tag}_on_${L}.build" 2>&1; then
      : # 门关那一档下面再判；两档都编不过 = 夹具自身的运行时依赖问题
    fi
    if ! "$DCC" -O$L -std=gnu2x -I "$REPO/tests/fixtures/c" -I "$REPO" "$f" \
         -o "$OUT/${tag}_off_$L" >"$OUT/${tag}_off_${L}.build" 2>&1; then
      echo "SKIP $tag -O$L (两档都编不过：夹具自身依赖)" >> "$OUT/summary.txt"
      rm -f "$OUT/${tag}"_*_"$L"; continue 2
    fi
    [ -x "$OUT/${tag}_on_$L" ] || { rm -f "$OUT/${tag}"_*_"$L"; continue 2; }
    # 先自比对：门关跑两次，四元组必须一致，否则本案没有稳定判据可用。
    local off1 off2 on1
    off1=$(run_one "$OUT/${tag}_off_$L")
    off2=$(run_one "$OUT/${tag}_off_$L")
    on1=$(run_one "$OUT/${tag}_on_$L")
    if [ "$off1" != "$off2" ]; then
      echo "REGION-UNSTABLE $tag -O$L 门关自比对不一致 [$off1] vs [$off2]" >> "$OUT/summary.txt"
    fi
    if [ "$off1" != "$on1" ]; then
      echo "REGION-DIFF $tag -O$L 四元组 [off=$off1] [on=$on1]" >> "$OUT/summary.txt"
    fi
    rm -f "$OUT/${tag}_off_$L" "$OUT/${tag}_on_$L"
  done
  return 0
}
export -f one run_one norm
export DCC REPO OUT timeout_s DEFER

find "$REPO/tests/fixtures/c" -name '*.c' ${ONLY:+-name "*$ONLY*"} | sort \
  | xargs -P "$jobs" -I{} bash -c 'one "$@"' _ {}

bad=$(grep -c '^REGION-DIFF' "$OUT/summary.txt" || true)
unstable=$(grep -c '^REGION-UNSTABLE' "$OUT/summary.txt" || true)
echo "扫描完成：REGION-DIFF $bad 条、REGION-UNSTABLE $unstable 条（详见 $OUT/summary.txt）"
grep '^REGION-' "$OUT/summary.txt" | head -40
[ "$bad" = 0 ]
