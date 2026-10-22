#!/bin/bash
# fn_opcode_heat.sh — computed-goto 函数的「按 opcode 区间」静态体量 + 采样热度
#
# 报告 §6.6 / 方案 .trae/documents/x86_64-p13-hot-path-emission-optimization-plan.md §8 的口径来源。
# 为什么按区间：`luaV_execute` / `sqlite3VdbeExec` 这类函数只有一张标签表（disptab），
# 相邻标签地址之间就是「一条 VM 指令的发射体」。把 objdump 的线性指令流按标签切段，才能
# 回答「哪个 opcode 膨胀」；整函数计数只给总量。
#
# 用法:
#   bash scripts/fn_opcode_heat.sh <binary> <sym> [baseline-binary]
#     HEAT=1 HEAT_ARGS="<传给 binary 的参数>"   追加 perf 采样热度列
#     NAMES=<文件>            每行一个区间名（与表项同序）；缺省时从 OPCODE_HEADER 抽 OP_*
#     OPCODE_HEADER=<lopcodes.h>
#     TABLE_SYM=<符号名片段>  跳转表符号正则（默认 'disptab|jumpTable'，覆盖 Lua/SQLite）
#     BASE_TABLE_SYM=<片段>   基线侧跳转表符号（默认同 TABLE_SYM）
#     HEAT_CPU=<n> HEAT_FREQ=<hz> TMP=<前缀>
#   例: HEAT=1 HEAT_ARGS=/tmp/fib34.lua \
#       bash scripts/fn_opcode_heat.sh /tmp/lua_def_final luaV_execute /tmp/lua_gcc_O2
#   SQLite 侧: TABLE_SYM 默认已含 jumpTable；区间名可用 NAMES 供一份 opcode 名单
#
# 口径限制（写结论时必须带上）：
#   * 区间是「相邻标签地址之间」，上一条 opcode 的冷尾巴会落进下一个区间，只作相对排序；
#   * 本机 PMU 硬件计数器不可用（cycles/instructions/branch-misses 全 `<not supported>`），
#     热度来自软件事件 cpu-clock 的采样数，不是执行指令数；
#   * 采样 IP 有 skid（常落在下一条指令），只用于区间之间的相对高低。
set -u

BIN=${1:?用法: fn_opcode_heat.sh <binary> <sym> [baseline]}
SYM=${2:?需要函数符号名}
BASE=${3:-}
TABLE_SYM=${TABLE_SYM:-disptab|jumpTable}
BASE_TABLE_SYM=${BASE_TABLE_SYM:-$TABLE_SYM}
OPCODE_HEADER=${OPCODE_HEADER:-/root/lua-5.5.1/src/lopcodes.h}
HEAT=${HEAT:-0}
HEAT_ARGS=${HEAT_ARGS:-}
HEAT_FREQ=${HEAT_FREQ:-4000}
HEAT_CPU=${HEAT_CPU:-3}
TMP=${TMP:-/tmp/fnoh}

# mawk 没有 strtonum：十六进制转换与 4 字节序反转（objdump -s 按内存序打字节）都自己写
AWKF='function h2d(s,  i,c,v){v=0;s=tolower(s);for(i=1;i<=length(s);i++){c=substr(s,i,1);v=v*16+index("0123456789abcdef",c)-1}return v}
function bswap4(w,  b0,b1,b2,b3){b0=w%256;b1=int(w/256)%256;b2=int(w/65536)%256;b3=int(w/16777216)%256;return b0*16777216+b1*65536+b2*256+b3}'

# ---------- 区间名 ----------
if [ -n "${NAMES:-}" ] && [ -f "$NAMES" ]; then
  cp "$NAMES" "$TMP.names"
elif [ -f "$OPCODE_HEADER" ]; then
  awk '/^typedef enum \{/{f=1;next} f&&/^} OpCode;/{exit} f' "$OPCODE_HEADER" |
    grep -oE 'OP_[A-Z0-9_]+' > "$TMP.names"
else
  : > "$TMP.names"
fi

# ---------- 单侧统计：产出 $TMP.<tag>.out = 每行「表序 表项地址 区间指令数」 ----------
analyze() {
  local bin=$1 sym=$2 pat=$3 tag=$4
  local info addr size end fs

  info=$(readelf -sW "$bin" | awk -v p="$pat" '$8 ~ p && $3>0 {print $2, $3; exit}')
  [ -z "$info" ] && { echo "fn_opcode_heat: $bin 无匹配 '$pat' 的跳转表符号" >&2; return 1; }
  addr=$((16#${info%% *})); size=${info##* }; end=$((addr + size))

  objdump -s --start-address=$addr --stop-address=$end "$bin" |
    awk 'NF>=2 && $1 ~ /^[0-9a-f]+$/ {for(i=2;i<=5;i++) if($i ~ /^[0-9a-f]+$/) print $i}' > "$TMP.$tag.words"
  awk "$AWKF"'{w[NR]=h2d($1)} END{for(i=1;i+1<=NR;i+=2) print bswap4(w[i]) + bswap4(w[i+1])*4294967296}' \
    "$TMP.$tag.words" > "$TMP.$tag.tab"
  rm -f "$TMP.$tag.words"

  objdump -d -M intel --no-show-raw-insn "$bin" |
    awk -v sym="$sym" '$0 ~ ("<" sym ">:"){f=1;next} f&&/^[0-9a-f]+ <.*>:/{exit}
      f&&/^[[:space:]]+[0-9a-f]+:/{gsub(":","",$1); print $1}' > "$TMP.$tag.ins"
  [ -s "$TMP.$tag.ins" ] || { echo "fn_opcode_heat: $bin 里反汇编不出 $sym" >&2; return 1; }
  fs=$(readelf -sW "$bin" | awk -v s="$sym" '$8==s {print $2; exit}')
  fs=$(awk "$AWKF"'BEGIN{print h2d(ARGV[1])}' "$fs")

  sort -n "$TMP.$tag.tab" | uniq > "$TMP.$tag.labels"
  awk "$AWKF"'
    NR==FNR {l[++n]=$1+0; next}
    {a=h2d($1)+FS0; while(k<n && l[k+1]<=a) k++; c[k]++}
    END{for(i=1;i<=n;i++) print l[i], c[i]+0}' "$TMP.$tag.labels" "$TMP.$tag.ins" FS0=$fs \
    > "$TMP.$tag.counts"
  awk 'NR==FNR{c[$1]=$2; next} {print FNR, $1, c[$1]+0}' "$TMP.$tag.counts" "$TMP.$tag.tab" \
    > "$TMP.$tag.out"
}

# ---------- 热度：采样 IP 落到哪个区间就记哪个区间 ----------
heat() {
  local data="$TMP.$SYM.data" script="$TMP.$SYM.script" fs
  taskset -c "$HEAT_CPU" perf record -F "$HEAT_FREQ" -e cpu-clock -o "$data" -- "$BIN" $HEAT_ARGS \
    >/dev/null 2>&1
  [ -s "$data" ] || { echo "fn_opcode_heat: perf record 无样本，热度列留空" >&2; return 0; }
  perf script -i "$data" 2>/dev/null > "$script"
  fs=$(readelf -sW "$BIN" | awk -v s="$SYM" '$8==s {print $2; exit}')
  fs=$(awk "$AWKF"'BEGIN{print h2d(ARGV[1])}' "$fs")
  awk "$AWKF"'
    NR==FNR {lab[++n]=$1+0; next}
    { if (index($0, SY"+0x")>0) {
        a=h2d(substr($0, index($0, SY"+0x")+length(SY)+3))+FS0;
        k=0; for(i=1;i<=n;i++) if(lab[i]<=a) k=i; else break; s[k]++; tot++ } }
    END{for(i=1;i<=n;i++) print lab[i], s[i]+0; printf "采样命中 %s 区间: %d 条\n", SY, tot > "/dev/stderr"}' \
    "$TMP.a.labels" "$script" SY="$SYM" FS0=$fs > "$TMP.bylabel"
  awk 'NR==FNR{c[$1]=$2; next} {print c[$2]+0}' "$TMP.bylabel" "$TMP.a.out" > "$TMP.a.samp"
}

analyze "$BIN" "$SYM" "$TABLE_SYM" a || exit 1
rm -f "$TMP.a.samp"
[ -n "$BASE" ] && { analyze "$BASE" "$SYM" "$BASE_TABLE_SYM" b || BASE=""; }
[ "$HEAT" = 1 ] && { [ -n "$HEAT_ARGS" ] || { echo "HEAT=1 需要 HEAT_ARGS" >&2; exit 1; }; heat; }

# ---------- 汇总 ----------
SAMPF=""
[ -f "$TMP.a.samp" ] && SAMPF="$TMP.a.samp"
awk -v namef="$TMP.names" -v basef="${BASE:+$TMP.b.out}" -v sampf="$SAMPF" '
  BEGIN{
    while((getline l < namef)>0) nm[++m]=l
    if (basef!="") {i=0; while((getline l < basef)>0){i++; split(l,p," "); bg[i]=p[3]+0}}
    if (sampf!="") {i=0; while((getline l < sampf)>0){i++; ss[i]=l+0; st+=l+0}}
    printf "%-16s %-10s %7s %7s %6s %8s %7s\n", "区间", "地址", "dcc", "gcc", "倍数", "样本", "占比"
  }
  { i=$1+0; d=$3+0; g=bg[i]+0; s=ss[i]+0; td+=d; tg+=g
    printf "%-16s %-10d %7d %7d %6.1f %8d %6.1f%%\n",
      (nm[i]!=""?nm[i]:("region"i)), $2, d, g, (g>0?d/g:0), s, (st>0?100*s/st:0) }
  END{ printf "--- 合计: dcc=%d gcc=%d 区间样本=%d\n", td, tg, st }' "$TMP.a.out"

if [ -n "$SAMPF" ]; then
  echo "--- 热度 Top（样本降序，非零区间）:"
  awk 'NR==FNR{nm[++m]=$1; next} {print $0, FNR}' "$TMP.names" "$TMP.a.samp" |
    sort -k1,1nr | awk '$1>0' | head -12 |
    awk -v namef="$TMP.names" 'BEGIN{while((getline l<namef)>0) nm[++m]=l}
      {printf "%-16s samples=%-6d\n", (nm[$2]!=""?nm[$2]:"region"$2), $1}'
fi
