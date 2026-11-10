#!/bin/bash
# 中端管线体检：逐个 pass 量两件事实 —— **命中率**（跳过它，产物字节变不变）与
# **边际耗时**（跳过它，编译快多少）。
#
# 为什么要这两个数而不是只看总耗时：`-O1` 的产物与 `-O0` 几乎同尺寸，而编译时间是
# `-O0` 的一个量级（docs/dcc-kernel-build-verify-report.md §5.13）。要么某些 pass 在
# 很大比例的代码上**什么都没改**（那就该删/该加准入判据），要么它们改了但改不动
# 代码尺寸（那是 cost model 的事）。光看火焰图分不出这两种，只有「产物字节是否变化」
# 能分。
#
# 判据是**产物字节**，不是 stdout 摘要：命中 = 至少有一个 TU 的 `.o` 与不跳过时不同。
# 耗时取同一 TU 交替 N 次的中位数（与 `scripts/ab_pair.sh` 同一条「同轮配对」纪律：
# 先后各跑一轮会把机器状态的漂移记到 pass 头上）。
#
# 用法:
#   bash scripts/opt_pass_audit.sh <文件...>            # 默认 -O2、默认 dcc release
#   LEVEL=-O1 bash scripts/opt_pass_audit.sh a.i b.i
#   DCC=target/debug/dcc REPS=3 bash scripts/opt_pass_audit.sh $(cat list.txt)
#
# 输入建议是**已预处理**的 `.i`（`make O=<build> fs/ext4/inode.i`）：那样量到的才是
# 中端 + 后端，不掺预处理与头文件搜索。`-E` 之后 `__FILE__` 之类仍指向原始源文件。
set -uo pipefail
DCC=${DCC:-/root/dcc/target/release/dcc}
LEVEL=${LEVEL:--O2}
# REPS=1 是「只量命中率」的快速一档（一个内核 `.i` 在 -O2 下就 12 s，19 pass × 5 文件
# × 2 臂 × 5 次是三个小时的事）。要拿可信的耗时差再调大 REPS，并只跑几个小文件。
REPS=${REPS:-1}
# TIMING=0 跳过末尾那组档位对照（它在 REPS=1 下本来就没有意义）。
TIMING=${TIMING:-1}
EXTRA=${EXTRA:-}

files=("$@")
[ ${#files[@]} -gt 0 ] || {
    echo "用法: bash scripts/opt_pass_audit.sh <.i/.c 文件...>   (LEVEL=-O1|.. REPS=n DCC=..)" >&2
    exit 2
}
[ -x "$DCC" ] || { echo "DCC 不可执行: $DCC" >&2; exit 2; }

# pass 名必须与 `src/opt.rs` 里 `opt_skipped("…")` 的调用一一对应；这里不硬编码清单，
# 直接从源码抽，免得两处漂移（抽不出来就是名字改了/换了出处，宁可不跑也不要量错对象）。
mapfile -t passes < <(grep -rhoE 'opt_skipped\("[a-z0-9_]+"\)' "$(dirname "$0")/../src/opt.rs" \
    | sed 's/opt_skipped("//;s/")//' | sort -u)
[ ${#passes[@]} -gt 0 ] || { echo "从 src/opt.rs 没抽到 pass 名" >&2; exit 2; }

work=$(mktemp -d /tmp/optaudit.XXXXXX)
trap 'rm -rf "$work"' EXIT

# 中位数：把每档的多次读数排序取中间那个（偶数个取中间两数的均值），单位 ns。
median_ns() {
    local -a v=("$@")
    local n=${#v[@]}
    [ "$n" -gt 0 ] || { echo 0; return; }
    mapfile -t s < <(printf '%s\n' "${v[@]}" | sort -n)
    if (( n % 2 )); then echo "${s[n / 2]}"; else echo $(( (${s[n / 2 - 1]} + ${s[n / 2]}) / 2 )); fi
}

# 一次编译：产 `.o` 的 sha256 + 本次 wall ns。编译失败记成 FAIL（失败本身也是一条事实：
# 某个 pass 被跳过就编不出来，说明后面的 pass 依赖它建立的前提）。
one_run() { # $1=out-tag $2=env-assign-or-empty $3=file
    local tag=$1 skipvar=$2 src=$3
    local o="$work/$tag.o"
    local t0 t1
    t0=$(date +%s%N)
    if [ -n "$skipvar" ]; then
        DCC_OPT_SKIP="$skipvar" "$DCC" $LEVEL $EXTRA -c -o "$o" "$src" >/dev/null 2>&1 || { echo "FAIL -"; return; }
    else
        "$DCC" $LEVEL $EXTRA -c -o "$o" "$src" >/dev/null 2>&1 || { echo "FAIL -"; return; }
    fi
    t1=$(date +%s%N)
    echo "$(sha256sum "$o" | cut -c1-16) $(( t1 - t0 ))"
}

echo "audit: level=$LEVEL reps=$REPS dcc=$DCC files=${#files[@]} passes=${#passes[@]}"
printf '%-38s %8s %8s %10s %s\n' "pass" "命中TU" "样本" "Δ中位(ms)" "每 TU 的 Δ(ms)"

for p in "${passes[@]}"; do
    hits=0
    n=0
    total_delta_ms=0
    per_file=""
    for f in "${files[@]}"; do
        base=()
        skp=()
        # 交替配对：每一轮里 baseline 与 skip 紧挨着跑，机器状态漂移对两边同等作用。
        for _ in $(seq 1 "$REPS"); do
            read -r hb tb <<<"$(one_run b "" "$f")"
            read -r hs ts <<<"$(one_run s "$p" "$f")"
            [ "$hb" = FAIL ] && { base=(); break; }
            base+=("$tb")
            [ "$hs" != FAIL ] && skp+=("$ts")
            last_base=$hb
            last_skip=$hs
        done
        [ ${#base[@]} -eq 0 ] && continue
        n=$(( n + 1 ))
        mb=$(median_ns "${base[@]}")
        if [ ${#skp[@]} -gt 0 ]; then ms=$(median_ns "${skp[@]}"); else ms=$mb; fi
        d_ms=$(( (ms - mb) / 1000000 ))
        # 命中 = skip 后的产物与 baseline 不同（两边都编成功才比得了）。
        if [ "$last_skip" != FAIL ] && [ "$last_base" != "$last_skip" ]; then
            hits=$(( hits + 1 ))
        fi
        total_delta_ms=$(( total_delta_ms + d_ms ))
        per_file+="$d_ms;"
    done
    mean_ms=0
    [ "$n" -gt 0 ] && mean_ms=$(( total_delta_ms / n ))
    printf '%-38s %8s %8s %10s %s\n' "$p" "$hits" "$n" "$mean_ms" "$per_file"
done

if [ "$TIMING" = 0 ]; then
    exit 0
fi
# 总账：整条中端关掉（`-O0`）当参照，看 LEVEL 相对它到底贵多少。
echo
echo "== 档位对照（同一批文件，median of $REPS）=="
for lv in -O0 "$LEVEL"; do
    tot=0
    for f in "${files[@]}"; do
        v=()
        for _ in $(seq 1 "$REPS"); do
            t0=$(date +%s%N)
            "$DCC" "$lv" $EXTRA -c -o "$work/lv.o" "$f" >/dev/null 2>&1 || continue
            t1=$(date +%s%N)
            v+=("$(( t1 - t0 ))")
        done
        [ ${#v[@]} -gt 0 ] && tot=$(( tot + $(median_ns "${v[@]}") ))
    done
    printf '  %-6s 合计 %6d ms\n' "$lv" "$(( tot / 1000000 ))"
done
