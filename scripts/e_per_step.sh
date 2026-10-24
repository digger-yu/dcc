#!/bin/bash
# ===========================================================================
# e_per_step.sh — 按 opcode 执行权重折算的「每 VM 步机器指令数」E/步
#
# 为什么必须有它：本机 PMU 全不可用（`perf_event_paranoid=4`，cycles/instructions
# 都是 `<not supported>`），而**静态条数已被实测证明不能当墙钟的代理**：同一个函数里
# 冷尾与热臂的条数完全混在一起按整臂计费，而解释器每步只走其中一段。要用一个能辩护的
# 量把「这个改动在运行时值多少」说清楚，唯一可用的组合是：
#
#     E/步 = Σ_op freq[op] · insns[arm(op)] / Σ_op freq[op]
#
# `freq` 只由 **gcc 编的插桩臂**取一次（`scripts/hot_weights.sh`），与被测产物无关，
# 所以候选与基线共用同一份权重，改动带来的 E 变化就是「动态发射量」的变化。
#
# 三条使用纪律（写结论时必须带上）：
#   * **E 是上界，不是墙钟。** 它按整臂计费，臂内的冷分支（内联的 float 比较格、错误
#     处理）每步并不执行，却被全额计入 ⇒ E 系统性高估这些臂。E 只能用于「同结构前提下的
#     相对变化」，不得拿去换算 ms（本仓曾按 0.011 ms/条 换算并据此判错过一次主轴）。
#   * 不滤 nop：加了 nop 的臂 E 会变差，这是对的（nop 也在指令流里）。防伪靠
#     `scripts/aa_layout.sh` 的 2b) 可达性断言，而不是把 nop 剔掉。
#   * 臂的切分取「跳表指向地址之间的区间」（与 `hot_cold_census.sh` 同一口径）；
#     `≥max(label)` 的冷尾不计入任何臂，单列 OUT，否则会串味。
#
# 为什么不只报条数、还要拆出载入与存：本机标定过的价格是**分开的**（同一个“每步多一次”
# 在三个轴上差一个量级）：纯载入 ≈5 ms/(次·步)，内存读改写(RMW) ≈33 ms/(次·步)，
# 廉价指令 ≈3.2，taken 分支 ≈13.6，热区体量 ≈11 ms/KB（标定方法见 scripts/meta_gate.sh）。
# 拿混在一起的条数去预期收益，会把“消载入”高估 6 倍、把“消存”低估。
#
# 用法:
#   bash scripts/e_per_step.sh <binary> [sym] [freq 文件] [表符号正则]
#     freq 文件默认 /tmp/hw/lua_op_freq.txt（每行「opcode 编号 次数」，
#     由 scripts/hot_weights.sh 产出；SQLite 用 sq_op_freq.txt + 自己的名单）
#     NAMES=<文件>  每行一个 opcode 名，行号-1 == opcode 编号（默认 lopcodes.h 抽出的表）
#     OPCODE_HEADER=<lopcodes.h>
#     FB=<寄存器名正则> 帧基址集；dcc 默认 r13，量 gcc 要传 FB='r(sp|bp)'
#   例: bash scripts/e_per_step.sh /tmp/mg7/base luaV_execute
#       bash scripts/e_per_step.sh /tmp/.../lua_gcc_O2
#       SQLite: NAMES=/tmp/sqnames bash scripts/e_per_step.sh <bin> sqlite3VdbeExec /tmp/hw/sq_op_freq.txt jumpTable
# ===========================================================================
set -o pipefail
set -u

BIN=${1:?用法: e_per_step.sh <binary> [sym] [freq] [表符号正则]}
SYM=${2:-luaV_execute}
FREQ=${3:-/tmp/hw/lua_op_freq.txt}
PAT=${4:-disptab|jumpTable}
TMP=${TMP:-/tmp/eps}

AWKF='function h2d(s,  i,c,v){v=0;s=tolower(s);sub(/^0x/,"",s);if(s=="")return 0;for(i=1;i<=length(s);i++){c=substr(s,i,1);v=v*16+index("0123456789abcdef",c)-1}return v}'

[ -f "$BIN" ] || { echo "e_per_step: 产物不存在: $BIN" >&2; exit 1; }
[ -s "$FREQ" ] || { echo "e_per_step: freq 表不存在或为空: $FREQ（先跑 scripts/hot_weights.sh）" >&2; exit 1; }

# ---- opcode 名单（行号-1 == opcode 编号）----
if [ -n "${NAMES:-}" ] && [ -f "$NAMES" ]; then
  cat "$NAMES" > "$TMP.names"
else
  awk '/^typedef enum \{/{f=1;next} f&&/^} OpCode;/{exit} f' \
    "${OPCODE_HEADER:-/root/lua-5.5.1/src/lopcodes.h}" | grep -oE 'OP_[A-Z0-9_]+' > "$TMP.names"
fi
[ -s "$TMP.names" ] || { echo "e_per_step: 拿不到 opcode 名单" >&2; exit 1; }

# ---- 臂入口标签（与 hot_cold_census.sh 同一套取法）----
info=$(readelf -sW "$BIN" | awk -v s="$SYM" '$8==s && $3>0 {print $2, $3; exit}')
[ -n "$info" ] || { echo "e_per_step: $BIN 里没有带大小的符号 $SYM" >&2; exit 1; }
START=$(awk "$AWKF"'BEGIN{print h2d(ARGV[1])}' "${info%% *}")
SIZE=${info##* }
END=$((START + SIZE))
: > "$TMP.labels"
tinfo=$(readelf -sW "$BIN" | awk -v p="$PAT" '$8 ~ p && $3>0 {print $2, $3; exit}')
if [ -n "$tinfo" ]; then
  ta=$((16#${tinfo%% *})); te=$((ta + ${tinfo##* }))
  objdump -s --start-address=$ta --stop-address=$te "$BIN" 2>/dev/null |
    awk 'NF>=2 && $1 ~ /^[0-9a-f]+$/ {for(i=2;i<=NF;i++) if($i ~ /^[0-9a-f]+$/) print $i}' > "$TMP.w"
  awk "$AWKF"'{w[NR]=h2d($1)}
    END{for(i=1;i+1<=NR;i+=2){b0=w[i]%256;b1=int(w[i]/256)%256;b2=int(w[i]/65536)%256;b3=int(w[i]/16777216)%256;
        v=(b0*16777216+b1*65536+b2*256+b3) + (bswap(w[i+1]))*4294967296;
        if(v>='"$START"' && v<'"$END"') print v}}
    function bswap(w,  b0,b1,b2,b3){b0=w%256;b1=int(w/256)%256;b2=int(w/65536)%256;b3=int(w/16777216)%256;return b0*16777216+b1*65536+b2*256+b3}' \
    "$TMP.w" | sort -n | uniq > "$TMP.labels"
  rm -f "$TMP.w"
fi
if [ ! -s "$TMP.labels" ]; then
  LC_ALL=C readelf -rW "$BIN" 2>/dev/null |
    awk "$AWKF"'$3=="R_X86_64_64" && $5==".text" && $6=="+" { v=h2d($7); if (v>='"$START"' && v<'"$END"') print v }' |
    sort -n | uniq > "$TMP.labels"
fi
[ -s "$TMP.labels" ] || { echo "e_per_step: 取不到臂入口标签" >&2; exit 1; }

objdump -d -M intel --no-show-raw-insn --start-address=$START --stop-address=$END "$BIN" > "$TMP.dis"

# ---- 每臂条数（按跳表指向的地址切；≥max(label) 归 OUT 不计入任何臂）----
# 帧基址集由 FB 传入（dcc 用 r13，gcc 用 r(sp|bp)），否则两边不同口径不可比。
# 注意：`-v` 会吃掉反斜杠，所以 FB 只给寄存器名，括号由 `~ /\[/` 那一步单独判。
awk -v fb="${FB:-r13}" "$AWKF"'
  function trim(x){sub(/^ +/,"",x);sub(/ +$/,"",x);return x}
  NR==FNR { lab[++n]=$1+0; next }
  /^[ \t]+[0-9a-f]+:/ {
    a=$1; gsub(":","",a); addr=h2d(a)
    if (addr < '"$START"' || addr >= '"$END"') next
    if (addr < lab[1]) { E++ ; next }
    if (addr >= lab[n]) { O++; next }
    while(k<n && lab[k+1]<=addr) k++
    reg[lab[k]]++; T++
    # 拆载入/存：第一操作数带内存操作数就是存，第二个就是载。
    # 帧基址集由 FB 传入（dcc 用 r13，gcc 用 rsp/rbp），否则两边不同口径不可比。
    txt=""; for(i=2;i<=NF;i++) txt=txt (i>2?" ":"") $i; txt=trim(txt)
    body=txt; sub(/^[^ ]+ +/,"",body); nf=split(body,op,/,/)
    for(q=1;q<=nf;q++) op[q]=trim(op[q])
    if (nf==2) {
      if (op[1] ~ /\[/ && op[1] ~ fb) ST[lab[k]]++
      else if (op[2] ~ /\[/ && op[2] ~ fb) LD[lab[k]]++
    }
  }
  END {
    printf "# entry=%d hot=%d out=%d arms=%d\n", E+0, T+0, O+0, n
    # 每个跳表项一行：序号(=opcode 编号) 条数 载入 存
    for (i=1;i<=n;i++) print i-1, reg[lab[i]]+0, LD[lab[i]]+0, ST[lab[i]]+0
  }' "$TMP.labels" "$TMP.dis" > "$TMP.perarm"

# ---- 帧槽归因（按执行权重排序）----
# 为什么需要它：按价格折算后「存」是每步最贵的一项（载入 ≈5 ms/(次·步)，存 ≈33），
# 但「一次存」落在哪个槽上完全决定该先动哪个机制：落在临时落脚槽上是 P1/P7（分配层），
# 落在家寄存器的跨调用 persist/restore 上是 `abi/call.rs`，落在真语义写（Lua 的栈槽）上
# 则谁也不该动。没有这张表就只能按静态计数猜。槽标识只取「基址+位移」：带变址的是地址链，
# 不属于落脚槽。
awk -v fb="${FB:-r13}" -v labf="$TMP.labels" "$AWKF"'
  function trim(x){sub(/^ +/,"",x);sub(/ +$/,"",x);return x}
  FILENAME == labf { lab[++n] = $1+0; next }
  FILENAME !~ /freq/ && /^[ \t]+[0-9a-f]+:/ {
    a=$1; gsub(":","",a); addr=h2d(a)
    while(k<n && lab[k+1]<=addr) k++
    if (k == 0 || k >= n) next
    w = fr[k-1] + 0
    if (w == 0) next                          # 本负载下根本没被执行到的臂不计
    txt=""; for(i=2;i<=NF;i++) txt=txt (i>2?" ":"") $i; txt=trim(txt)
    body=txt; sub(/^[^ ]+ +/,"",body); nf=split(body,op,/,/)
    for(q=1;q<=nf;q++) op[q]=trim(op[q])
    if (nf != 2) next
    st = (op[1] ~ /\[/ && op[1] ~ fb && op[2] !~ /\[/)
    ld = (op[2] ~ /\[/ && op[2] ~ fb && op[1] !~ /\[/)
    if (!st && !ld) next
    mem = st ? op[1] : op[2]
    slot = mem; gsub(/.*\[/, "", slot); gsub(/\].*/, "", slot); gsub(/[ \t]/, "", slot)
    if (slot == "") slot = "+0"
    if (index(slot, "*") || index(slot, ",")) next     # 带变址的是地址链，不算落脚槽
    if (st) { Sv[slot] += w; ts += w } else { Lv[slot] += w; tl += w }
    next
  }
  FILENAME ~ /freq/ { fr[$1+0] = $2+0; sq += $2+0; next }
  END {
    if (sq == 0) { print "freq 表为空"; exit 1 }
    printf "\n帧槽归因（只算本负载被执行到的臂；单位=次/步）\n"
    printf "%-16s %-9s %-9s %-9s\n", "槽", "存/步", "载/步", "合计"
    m = 0; for (s in Sv) kk[++m] = s; for (s in Lv) if (!(s in Sv)) kk[++m] = s
    for (i=1;i<=m;i++) v[i] = (Sv[kk[i]]+0 + Lv[kk[i]]+0)
    for (i=1;i<=m;i++) for (j=i+1;j<=m;j++) if (v[j] > v[i]) {
      t=v[i]; v[i]=v[j]; v[j]=t; s=kk[i]; kk[i]=kk[j]; kk[j]=s }
    for (i=1; i<=m && i<=14; i++) {
      s = kk[i]
      printf "%-16s %-9.2f %-9.2f %-9.2f\n", s, Sv[s]/sq, Lv[s]/sq, v[i]/sq
    }
    # 除数必须是 Σfreq（每步一次），不得按指令行累加：那样会把分母乘上臂长，
    # 数值被压小一个量级，看起来就像「没有落脚槽」。
    shown = 0; for (i=1;i<=m && i<=14;i++) shown += v[i]
    printf "本负载帧访问合计：存 %.2f/步，载 %.2f/步；前 14 个槽占 %.2f/步（%.0f%%）\n", \
           ts/sq, tl/sq, shown/sq, 100.0*shown/(ts+tl)
  }' "$TMP.labels" "$FREQ" "$TMP.dis"

# ---- 按 freq 加权：E/步、载入/步、存/步 ----
awk '
  BEGIN { N = 0; while ((getline l < "'"$TMP.names"'") > 0) { nm[N] = l; N++ } }
  /^#/ { print; next }
  NR == FNR { ins[$1+0] = $2+0; ld[$1+0] = $3+0; st[$1+0] = $4+0; next }
  { c = $2 + 0; f[$1+0] = c; steps += c }
  END {
    if (steps == 0) { print "freq 表为空"; exit 1 }
    ED = 0; EL = 0; ES = 0; rows = 0
    for (i = 0; i < N; i++) {
      c = f[i] + 0; if (c == 0) continue
      ED += c * ins[i]; EL += c * ld[i]; ES += c * st[i]; rows++
      printf "op#%-4d %-16s freq=%-10d 条=%-5d 载=%-4d 存=%-4d 条/步=%-7.2f 载/步=%-6.2f 存/步=%-6.2f\n", \
             i, nm[i], c, ins[i], ld[i], st[i], c*ins[i]/steps, c*ld[i]/steps, c*st[i]/steps
    }
    print "-----------------------------------------------------------"
    printf "步数 Σfreq=%d  被执行到的 opcode 种类=%d\n", steps, rows
    printf "E/步=%.2f 条｜载入/步=%.2f｜存/步=%.2f\n", ED/steps, EL/steps, ES/steps
    printf "按已标定价格折算上界(ms/(次·步)): 载入5.0、存(写)33.0、指令3.2 ⇒ 载入价 %.0f ms，存价 %.0f ms，指令价 %.0f ms\n", \
           5.0*EL/steps, 33.0*ES/steps, 3.2*ED/steps
  }' "$TMP.perarm" "$FREQ"
