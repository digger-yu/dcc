#!/usr/bin/env bash
# scripts/perf_pass_stats.sh —— 优化 pass 体检：-O2 vs -O1 的**命中率**与**耗时**。
#
# 背景：`-O1` 相对 `-O0` 的编译时间差距（内核全树实测 ≈10×）要落到具体 pass 上，
# 才能回答「删 pass」还是「补 cost model」。两个读数来源：
#   1. `DCC_PASS_STATS=1`（`src/pass_probe.rs`）：每个 pass 的墙钟时间 + 是否真改了
#      模块（AST 层用结构指纹，SSA 层用 pass 自己的 bool 返回值）。
#   2. `PROF=1` 时叠一层 `perf record`（需要带符号的 dcc：`strip = "symbols"` 的
#      release 产物拿不到 pass 级符号），用于交叉核对耗时分布。
#
# 用法：
#   bash scripts/perf_pass_stats.sh <build-dir> <kernel-src> <out-dir> [obj ...]
#   PROF=1 bash scripts/perf_pass_stats.sh /root/pvh-lmc3 /root/linux-pvh-src /root/pass-stats
#
# 注意：
#   * 本脚本会 `rm -f` 目标 .o 强制重编，**不要**在正在进行的 kbuild 上跑；
#   * 每个 (obj, level) 一次单文件 `make -j1`，墙钟里含 kbuild 的依赖检查开销，
#     所以绝对时间只用于「-O2/-O1 相对比较」，pass 内部耗时看 PASSSTAT 的 ms；
#   * 默认对象覆盖面：内存分配 / 调度 / 格式化 / 文件系统 / 网络 / fork / 页分配。
set -uo pipefail

BUILD=${1:?用法: perf_pass_stats.sh <build-dir> <kernel-src> <out-dir> [obj ...]}
KSRC=${2:?缺 <kernel-src>}
OUT=${3:?缺 <out-dir>}
shift 3 || true
OBJS=("$@")
if [ ${#OBJS[@]} -eq 0 ]; then
    OBJS=(
        mm/memory.o
        mm/page_alloc.o
        kernel/sched/core.o
        kernel/fork.o
        lib/vsprintf.o
        fs/open.o
        fs/read_write.o
        net/core/skbuff.o
    )
fi

CC_WRAP=/root/dcc/scripts/dcc-kernel-cc.sh
HOST_WRAP=/root/dcc/scripts/dcc-host-wrapper.sh
mkdir -p "$OUT"

build_one() { # <obj> <level>
    local obj=$1 lvl=$2
    rm -f "$BUILD/${obj%.o}.o"
    env DCC_PASS_STATS=1 make -s -C "$KSRC" O="$BUILD" \
        CC="$CC_WRAP" HOSTCC="$HOST_WRAP" HOSTLD=ld \
        KCFLAGS="-O$lvl" -j1 "$obj"
}

for obj in "${OBJS[@]}"; do
    name=$(echo "${obj%.o}" | tr '/' '_')
    for lvl in 2 1; do
        log="$OUT/$name.O$lvl.log"
        t0=$(date +%s%N)
        if [ "${PROF:-0}" = "1" ]; then
            perf record -F 499 -o "$OUT/$name.O$lvl.perf.data" -- \
                bash -c "$(declare -f build_one); BUILD='$BUILD' KSRC='$KSRC' \
                    CC_WRAP='$CC_WRAP' HOST_WRAP='$HOST_WRAP' build_one '$obj' '$lvl'" \
                >"$log" 2>&1
        else
            build_one "$obj" "$lvl" >"$log" 2>&1
        fi
        rc=$?
        t1=$(date +%s%N)
        printf '%s\t-O%s\t%d\t%d\n' "$obj" "$lvl" $(( (t1 - t0) / 1000000 )) "$rc" \
            >>"$OUT/timing.tsv"
        # PASSSTAT 行留在 per-TU 日志里；汇总阶段统一收。
        if [ $rc -ne 0 ]; then
            echo "!! $obj -O$lvl 失败（rc=$rc），日志: $log" >&2
        fi
    done
done

# ---- 汇总：每个 -O 档一张表，按耗时降序 ----
for lvl in 2 1; do
    raw="$OUT/passstats-O$lvl.tsv"
    {
        printf 'pass\tcalls\thits\thit_pct\tms\thit_ms\tmiss_ms\n'
        grep -h '^PASSSTAT|' "$OUT"/*.O$lvl.log 2>/dev/null |
            awk '
            BEGIN { FS = "|" }
            {
                p = ""; c = 0; h = 0; ms = 0; hm = 0
                for (i = 2; i <= NF; i++) {
                    split($i, kv, "=")
                    k = kv[1]; v = kv[2]
                    if (k == "pass") p = v
                    else if (k == "calls") c = v + 0
                    else if (k == "hits") h = v + 0
                    else if (k == "ms") ms = v + 0
                    else if (k == "hit_ms") hm = v + 0
                }
                if (p == "") next
                C[p] += c; H[p] += h; MS[p] += ms; HM[p] += hm
                if (!(p in seen)) { seen[p] = 1; order[++n] = p }
            }
            END {
                for (i = 1; i <= n; i++) {
                    p = order[i]
                    pct = (C[p] > 0) ? 100 * H[p] / C[p] : 0
                    printf "%s\t%d\t%d\t%.1f\t%.3f\t%.3f\t%.3f\n",
                        p, C[p], H[p], pct, MS[p], HM[p], MS[p] - HM[p]
                }
            }' | sort -t "$(printf '\t')" -k5,5nr
    } >"$raw"
    echo "== -O$lvl 汇总: $raw"
done

# ---- prof 模式：把每个 perf.data 的 top 符号并到一张表 ----
if [ "${PROF:-0}" = "1" ]; then
    prof="$OUT/perf-top.tsv"
    : >"$prof"
    for f in "$OUT"/*.perf.data; do
        [ -e "$f" ] || continue
        base=$(basename "$f" .perf.data)
        perf report -i "$f" --stdio -g none --sort symbol 2>/dev/null |
            awk -v tu="$base" '
            /^#/ { next }
            /^$/ { next }
            { sub(/^[ \t]+/, ""); printf "%s\t%s\n", tu, $0 }' >>"$prof"
    done
    echo "== perf top 符号: $prof"
fi

echo "== 墙钟（含 kbuild 开销）: $OUT/timing.tsv"
