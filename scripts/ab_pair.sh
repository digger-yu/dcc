#!/bin/bash
# ===========================================================================
# scripts/ab_pair.sh — 轮内**配对**A/B 计时与判定（P13 阶段6 批0c）
#
# 为什么不是「各臂自己跑 best-of-N 再比 min」：本仓两份历史脚本
# （.work_tmp/blockopt_ab.sh、.work_tmp/s2b_ab.sh）都是那样写的，那个口径已经证伪：
#   * 同一二进制跨轮 min 能漂 11%（Lua 实测 291.6~324ms），而臂与臂之间的差往往
#     就在这带里 —— 各自取 min 等于专挑「本机那一刻对这一臂最幸运的那一轮」，
#     §2.2 那次「删 135 条 jmp 反而慢 3.8%」正是被这种比较掩盖方向的；
#   * 机器状态（温度/频率/邻居负载）随时间走，先跑的臂和后跑的臂不在同一条件下。
# 这里改成：**同一轮内**依次跑完所有臂，比较只能发生在轮内
#   r_i = cand_i / base_i
# 报 median(r) + 自助法 95% CI + 符号检验；`min`/`p50` 只作为附报，不作判据。
# gcc 臂（若有）不是对手，而是**漂移协变量**：机器变慢时所有臂一起变慢，
# 比值不敏感，绝对值敏感。
#
# 用法:
#   ARMS="gcc=/root/lua-5.5.1/src/lua base=... cand=..." R=15 \
#     BASE=0 bash scripts/ab_pair.sh [lua|sqlite]
#     BASE=0   基准臂的下标（默认 0；其余臂都与它配对比值）
#     ARG      lua 模式传 .lua 文件；sqlite 模式是 SQL 文件经 stdin 喂给 shell
#     CPU      绑核（默认 3，与 bench_lua_sqlite.sh 的历史数据同口径）
#     R        轮数（默认 15；判 3% 以下的效应要 25 轮以上）
#   例: ARMS="gcc=/tmp/lua_gcc base=/tmp/lua_def cand=/tmp/lua_new" \
#       ARG=/tmp/fib.lua R=15 bash scripts/ab_pair.sh lua
#
# 判据口径（与 §5 的三态一致，脚本只报数，是否「保留/回退」由人按表定）:
#   保留   median(r) ≤ 0.985 且 CI 上界 < 1 且 符号检验 ≥ 7/8 同向
#   回退   median(r) ≥ 1.02  或 主代理量 E 上升（E 由 hot_weights.sh 另算）
#   其余   不可判 ⇒ 新臂默认关，代码可留，不计入收益，登记负结果
# 输出: $RAW（逐轮原始值，任何统计都能事后重算）+ stdout 汇总表。
# ===========================================================================
set -o pipefail
set -u

MODE="${1:-lua}"
RAW="${RAW:-/tmp/ab_pair.raw}"
R="${R:-15}"
CPU="${CPU:-3}"
BASE="${BASE:-0}"
ARG="${ARG:-}"
: "${ARMS:?ARMS=\"name=path [name=path ...]\"，第一个臂之前的约定：下标 0 通常是 gcc 参考臂}"

# shellcheck disable=SC2206
arms=($ARMS)
if [ "${#arms[@]}" -lt 2 ]; then
  echo "ab_pair: 至少要有两个臂（ARMS=\"name=path ...\"）" >&2
  exit 1
fi

names=(); paths=()
for a in "${arms[@]}"; do
  n="${a%%=*}"; p="${a#*=}"
  [ -x "$p" ] || { echo "ab_pair: 臂 '$n' 的可执行文件不存在或不可执行: $p" >&2; exit 1; }
  names+=("$n"); paths+=("$p")
done

one() { # $1=path  → 打印 task-clock 毫秒数（perf 不可用时退回 /usr/bin/time）
  case "$MODE" in
    sqlite) taskset -c "$CPU" perf stat -e task-clock -x, "$1" :memory: < "$ARG" 2>&1 ;;
    *)      taskset -c "$CPU" perf stat -e task-clock -x, "$1" ${ARG:+"$ARG"} 2>&1 ;;
  esac | awk -F, '/task-clock/{print $1}'
}

: > "$RAW"
say() { echo "$@"; }
say "=== 配对 A/B：mode=$MODE 轮数=$R 基准臂下标=$BASE 绑核=$CPU ==="
i=0
while [ "$i" -lt "$R" ]; do
  # 臂序按轮号旋转：消除「总先跑 gcc」这类系统性偏置
  j=0
  while [ "$j" -lt "${#paths[@]}" ]; do
    k=$(( (j + i) % ${#paths[@]} ))
    v="$(one "${paths[$k]}")"
    [ -n "$v" ] || { echo "第 $((i+1)) 轮臂 ${names[$k]} 没取到读数" >&2; exit 1; }
    printf "%s %s %s\n" "$i" "$k" "$v" >> "$RAW"
    j=$((j+1))
  done
  i=$((i+1))
  echo "  round $((i+1))/$R done" >&2
done

awk -v R="$R" -v A="${#names[@]}" -v BASE="$BASE" -v seed=20261022 -v RAWF="$RAW" \
    -v NAMES="$(IFS=,; echo "${names[*]}")" '
  BEGIN { n = split(NAMES, arname, ",") }
  function sortv(v, m,   i, j, t) { for (i=1;i<=m;i++) for (j=i+1;j<=m;j++) if (v[j]<v[i]) { t=v[i]; v[i]=v[j]; v[j]=t } }
  function rnd() { seed = (seed * 1103515145 + 12345) % 2147483648; return seed / 2147483648 }
  { d[$1 "," $2] = $3 }
  END {
    for (a = 0; a < A; a++) {
      # 各臂绝对值（附报，不作判据）
      for (i = 0; i < R; i++) { v[i+1] = d[i "," a]; s += v[i+1] }
      sortv(v, R)
      printf "%-10s n=%d min=%.1f p50=%.1f max=%.1f mean=%.1f\n", arname[a+1], R, v[1], v[int((R+1)/2)], v[R], s/R
      s = 0
    }
    print "--- 轮内配对比值（判据用 median 与 CI，不用 min）---"
    for (a = 0; a < A; a++) {
      if (a == BASE) continue
      m = 0
      for (i = 0; i < R; i++) {
        if (d[i "," BASE] + 0 == 0) continue
        r[++m] = d[i "," a] / d[i "," BASE]
      }
      sortv(r, m)
      med = r[int((m+1)/2)]
      lo = hi = 0
      for (b = 1; b <= 2000; b++) {
        for (t = 1; t <= m; t++) q[t] = r[int(rnd() * m) + 1]
        sortv(q, m)
        bm[b] = q[int((m+1)/2)]
      }
      sortv(bm, 2000)
      lo = bm[int(2000 * 0.025) + 1]; hi = bm[int(2000 * 0.975) + 1]
      neg = 0; for (t = 1; t <= m; t++) if (r[t] < 1) neg++
      verdict = "不可判（默认关，不计收益）"
      if (med <= 0.985 && hi < 1 && neg * 8 >= m * 7) verdict = "保留候选"
      else if (med >= 1.02) verdict = "回退候选"
      printf "%-10s / %-10s median=%.4f  95%%CI=[%.4f, %.4f]  快于基准 %d/%d 轮  => %s\n",
             arname[a+1], arname[BASE+1], med, lo, hi, neg, m, verdict
    }
    printf "原始逐轮值: %s（换判据可事后重算，不必重跑）\n", RAWF
  }' "$RAW"
