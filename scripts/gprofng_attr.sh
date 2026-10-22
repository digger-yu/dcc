#!/bin/bash
# ===========================================================================
# scripts/gprofng_attr.sh — 按**函数**归因运行时时间（方案 §5 第 8 条补的手段）
#
# 为什么还要第三个口径：本机 `perf` 的硬件计数器全不可用（`perf_event_paranoid=4`），
# 于是时间侧只剩两个数 —— 墙钟与 task-clock 的配对 A/B。它们都是**整个程序**的标量，
# 回答不了「省下的时间到底出自哪个函数」。批 4 那类改动（每次分派少两条）尤其吃这个：
# 整体差值落在噪声带里，而那 2 条确实只可能出现在 `luaV_execute` 里 —— 如果连
# 「luaV_execute 占多少」都量不到，改动与收益之间就永远只是一句推测。
# gprofng（binutils 2.42）用的是它自己的时钟采样，不要求 PMU；本机实测可用。
#
# 它**不是**判保留/回退的那个口径（那仍是 `ab_pair.sh` 的同轮配对比值）。本脚本只回答
# 「时间花在哪个函数上、两臂之间这个归属有没有移动」。
#
# 实测分辨率（写在这里免得后人误读）：clock profiling 的粒度下不到 10ms 以下
# （`-s 2` 不被接受，给的仍是 10ms 的样本），而 fib(32) 整跑只 0.29s ⇒ 单次采样只命中
# 约 7 个样本（0.07s 占 0.29s 的 24%），pct 能跳到一个十位数百分比。要拿到可用的归属，
# 就得把负载拉长（fib(34)）+ R 开大（6 次 = 约 60 个样本 ⇒ pct 的相对误差 ~13%）。
# 结论：这一路能回答「是不是 luaV_execute 的时间」（实测 74%~83%），回答不了 6% 的差值。
#
# 用法:
#   ARMS="off=/tmp/lua_off cand=/tmp/lua_all" ARG=/tmp/fib.lua \
#     [SYM=luaV_execute] [R=5] [CPU=3] [INTERVAL=2] [LIMIT=10] [OUTDIR=/tmp/gpa] \
#     bash scripts/gprofng_attr.sh
#   SYM 空 = 只打印每臂的 top 表；给了 SYM 就额外打印该函数的 exclusive 秒、占总计比例、
#   以及相对第一臂的比值/百分点差。
#   R = 每臂重复采样的次数。单次采样太少（fib(32) 只跑 290ms，默认 10ms 间隔只有两三个
#       样本，实测 pct 会跳到个位数百分比）；INTERVAL=2 + R=6 是一档可用的下限。
#
# 一个实现约束值得写下来：`gprofng collect -o X.er` **不能重复使用同一个实验名**
# （第二次直接 `name X.er is in use and cannot be updated`），所以每臂的 R 次采样各占
# 一个目录，聚合在本脚本里做（按函数名求和），而不是指望 gprofng 自己累加。
#
# 退出码：0 正常；3 = 本机没有 gprofng（这个手段用不了，不算失败）。
# ===========================================================================
set -o pipefail
set -u

ARMS="${ARMS:-}"
ARG="${ARG:-}"
SYM="${SYM:-}"
R="${R:-5}"
CPU="${CPU:-3}"
INTERVAL="${INTERVAL:-2}"
LIMIT="${LIMIT:-10}"
OUTDIR="${OUTDIR:-/tmp/gprofng_attr}"

die() { echo "gprofng_attr: $*" >&2; exit 1; }
command -v gprofng >/dev/null 2>&1 || {
    echo "gprofng_attr: NOT-AVAILABLE（PATH 里没有 gprofng；这个手段用不了，不算失败）" >&2
    exit 3
}
[ -n "$ARMS" ] || die "ARMS=\"name=path [name=path ...]\" 必须给"
command -v taskset >/dev/null 2>&1 || CPU=""
run_pin() { if [ -n "$CPU" ]; then taskset -c "$CPU" "$@"; else "$@"; fi; }
mkdir -p "$OUTDIR" || die "无法创建 $OUTDIR"

collect_arm() {   # $1=名字：跑 R 次采样，每次一个独立实验目录 + display 文本
    local name="$1" exe="$2" i
    for i in $(seq 1 "$R"); do
        local er="$OUTDIR/${name}_r$i.er"
        rm -rf "$er"
        # shellcheck disable=SC2086
        run_pin gprofng collect app -p on -s "$INTERVAL" -o "$er" "$exe" ${ARG:+"$ARG"} \
            >"$OUTDIR/${name}_r$i.collect.log" 2>&1 \
            || die "$name 第 $i 次采样失败，见 $OUTDIR/${name}_r$i.collect.log"
        gprofng display text -functions -limit "$LIMIT" "$er" \
            > "$OUTDIR/${name}_r$i.display" 2>&1 \
            || die "$name 第 $i 次 display 失败"
    done
}

# 把一臂的 R 份 display 聚成「excl_sec name」按秒降序（<Total> 除外）
aggregate() {   # $1=top|total，其余 = display 文件
    local want="$1"; shift
    awk -v want="$want" '
        $0 ~ /^[0-9.]+[0-9]/ {
            sec = $1 + 0; nm = $NF
            if (nm == "<Total>") { total += sec; next }
            if (want == "top") { sum[nm] += sec }
        }
        END {
            if (want == "total") { printf("%.4f\n", total + 0); exit }
            for (k in sum) printf("%.4f %s\n", sum[k], k)
        }' "$@" | sort -k1,1nr
}

sym_seconds() {   # $1=函数名，其余=display 文件 → 该函数的 exclusive 秒合计
    awk -v s="$1" '
        $0 ~ /^[0-9.]+[0-9]/ && $NF == s { acc += $1 + 0 }
        END { printf("%.4f\n", acc + 0) }' "${@:2}"
}

echo "=== gprofng 按函数归因：每臂 $R 次采样，间隔 ${INTERVAL}ms，绑核 ${CPU:-无} ==="
declare -a NAMES
i=0
for a in $ARMS; do
    n="${a%%=*}"; p="${a#*=}"
    [ -x "$p" ] || die "臂 $n 不可执行: $p"
    collect_arm "$n" "$p"
    NAMES[i]="$n"; i=$((i+1))
    echo ""
    echo "--- 臂 $n（$p）：exclusive 秒合计（$R 次） ---"
    disp=( "$OUTDIR"/${n}_r*.display )
    total="$(sym_seconds "<Total>" "${disp[@]}")"
    aggregate top "${disp[@]}" | head -"$LIMIT" | awk -v tot="$total" '
        { printf "  %8.3fs %6.2f%%  %s\n", $1, (tot + 0 > 0 ? $1 * 100 / tot : 0), $2 }'
    awk -v t="$total" -v r="$R" -v n="$n" \
        'BEGIN { printf "  （采样合计 %.3fs / %d 次 = 每次 %.3fs）\n", t, r, t / r }'
done

if [ -n "$SYM" ]; then
    echo ""
    echo "=== $SYM 的 exclusive CPU（每臂 $R 次采样合计） ==="
    printf "%-12s %10s %10s %8s   %s\n" 臂 excl_sec 总采样秒 pct
    base_sec=""; base_pct=""; j=0
    while [ "$j" -lt "$i" ]; do
        n="${NAMES[$j]}"
        disp=( "$OUTDIR"/${n}_r*.display )
        sec="$(sym_seconds "$SYM" "${disp[@]}")"
        tot="$(sym_seconds "<Total>" "${disp[@]}")"
        pct="$(awk -v s="$sec" -v t="$tot" 'BEGIN { printf "%.2f", (t + 0 > 0) ? s * 100 / t : 0 }')"
        printf "%-12s %10s %10s %8s   GPA|%s|%s|excl=%s|pct=%s\n" "$n" "$sec" "$tot" "$pct" "$n" "$SYM" "$sec" "$pct"
        if [ "$j" = 0 ]; then
            base_sec="$sec"; base_pct="$pct"
        else
            awk -v b="$base_sec" -v bp="$base_pct" -v v="$sec" -v vp="$pct" -v n="$n" -v s="$SYM" '
                BEGIN {
                    if (b + 0 > 0) printf("GPA|%s|%s|excl_ratio_vs_arm0=%.4f\n", n, s, v / b)
                    printf("GPA|%s|%s|pct_delta_vs_arm0=%+.2f\n", n, s, vp - bp)
                }'
        fi
        j=$((j+1))
    done
    echo ""
    echo "读法：excl_sec 受采样数影响，只能用于同参数下的臂间比值；pct 才是「改动动的到底"
    echo "      是哪个函数」的证据。两臂 SYM 占比接近而总时间差在别的函数上 ⇒ 收益（或退化）"
    echo "      不在 SYM 里 —— 那正是整程序配对比值看不出来的那一层。"
    echo "      另一条限制：gprofng 只能定位到**函数**，看不到函数内部哪一段，所以它补的是"
    echo "      「归因」而不是「分辨率」；判保留/回退仍看 ab_pair.sh。"
fi
exit 0
