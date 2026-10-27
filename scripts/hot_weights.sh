#!/bin/bash
# ===========================================================================
# scripts/hot_weights.sh — 「哪条 VM 指令被执行了多少次」的权重采集（阶段6 批0c）
#
# 为什么需要它：本机 PMU 硬件计数器全部不可用（`perf_event_paranoid=4`，
# `cycles`/`instructions`/`branch-misses` 都是 `<not supported>`，也没装 valgrind），
# 于是「这个改动在运行时上值多少」只剩两条可辩护的路：
#   1) 墙钟/task-clock 的配对 A/B —— 但同二进制跨轮 min 能漂 11%，分辨率约 13~14ms；
#   2) **零噪声的代理量** —— 每 VM 指令的动态发射量
#        E = Σ_op freq[op] · insns[handler_op]
#      其中 `freq` 与 dcc 的产物无关（只由 gcc 编的一副插桩臂取一次），
#      `insns` 是候选产物里该 opcode 处理体的静态条数。E 的相对变化就是
#      `perf stat -e instructions` 不可用时它的替身。
#      **但不要说它「对 nop/位移不敏感」**（方案 §5.1 的修正）：本脚本的计数管道
#      **不滤 nop**，所以 `DCC_HANDLER_PAD` 插进去的 nop 会抬高 E；而「滤掉 nop」也不是
#      出路 —— 那样「PAD=256 时 E 不变」这道防伪就结构上永不为伪（被滤掉的正是可能
#      真的被执行的那些字节）。两条合起来的口径是：**E 只能拿静态可达性证过的产物比**，
#      即 `scripts/aa_layout.sh` 的 2b) 断言（每段插入 nop 的前驱必须是 jmp/ret/jcc ⇒
#      只能被跳过，不可能被执行）；那个断言不成立的臂，E 只能当上界看。
# 同理，SQLite 跳表（批1）的收益取决于「比较链平均走多少臂」，也就是加权臂位
# P —— 这必须实测，不能假设「185 路 ⇒ 平均 92 次」：热点 opcode 在源码序里的
# 位置决定了真相（本轮实测 P=60.6，而不是 92）。
#
# 口径纪律（三件事都必须写进任何引用这些数字的结论）：
#   * 插桩只加在 **gcc 编的那一副**上：dcc 侧产物一律不插桩，否则权重与被测对象耦合；
#   * 插桩会改变 gcc 的优化决策，但**不改变执行到的 opcode 序列**（fib / speedtest1 的
#     都是确定性的），所以计数本身可用；
#   * 臂位 P 用的是「源码顺序的第几个具名 case」，与 dcc 实际发的比较链同序
#     （`emit/stmt.rs` 按源码序发链），但 dcc 会合并空体子句 ⇒ 真实臂数略少。
#
# 用法:
#   bash scripts/hot_weights.sh                    # 采 Lua + SQLite 两份权重
#   CAND=<dcc 产物 .o/.bin> SYM=luaV_execute \
#     bash scripts/hot_weights.sh                  # 额外算 E（需要 objdump）
#   LUA_SRC=/p/lua SQLITE_SRC=/p/sqlite HW=<前缀> WORKLOAD_LUA=/tmp/fib.lua \
#   S1_ARGS="--verify --testset main,cte,orm,fp --memdb :memory:" bash scripts/hot_weights.sh
#   （S1_ARGS 就是上游 speedtest1 的参数；换 testset 就是换被测负载。）
# 判据: 退出码 0 = 两份权重都拿到且总步数 > 0；缺依赖显式报错（不做静默 SKIP）。
#       本脚本不判绿任何性能结论 —— 它只产权重与 P。
# ===========================================================================
set -o pipefail
set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="${DCC_REPO:-$(cd "$HERE/.." && pwd)}"
LUA_SRC="${LUA_SRC:-/root/lua-5.5.1}"
SQLITE_SRC="${SQLITE_SRC:-/root/sqlite}"
HW="${HW:-/tmp/hw}"
CPU="${CPU:-3}"
WORKLOAD_LUA="${WORKLOAD_LUA:-/tmp/fib.lua}"
# SQLite 侧负载 = 上游 speedtest1 的参数（与 §4.5/§5 同一串 testset）
S1_ARGS="${S1_ARGS:---verify --testset main,cte,orm,fp --memdb :memory:}"
S1_TIMEOUT="${S1_TIMEOUT:-600}"

say() { echo "$@"; }
die() { echo "hot_weights: $*" >&2; exit 1; }

command -v gcc >/dev/null || die "gcc 不在 PATH"
command -v objdump >/dev/null || die "objdump 不在 PATH"
[ -f "$LUA_SRC/src/Makefile" ] || die "缺 Lua 源码树: $LUA_SRC/src/Makefile"
[ -f "$SQLITE_SRC/sqlite3.c" ] || die "缺 SQLite amalgamation: $SQLITE_SRC/sqlite3.c"
[ -f "$LUA_SRC/src/lopcodes.h" ] || die "缺 lopcodes.h: $LUA_SRC/src/lopcodes.h"

mkdir -p "$HW" || die "无法创建 $HW"

# ---- 工作负载：与 scripts/bench_lua_sqlite.sh / check_sqlite_verify.sh 同一口径 ----
# SQLite 侧用上游 speedtest1（`--verify --testset …`），不再用自写的 20 万行 SQL：
# 自写负载只能量到「插入+排序」那几条链，而权重必须对应被计时的那段程序（阶段 6
# 的 E/P 两个量都是按这份频率算的）。可用 S1_ARGS 改 testset。
if [ ! -f "$WORKLOAD_LUA" ]; then
  cat > "$WORKLOAD_LUA" <<'LUA'
local function fib(n) if n<2 then return n end return fib(n-1)+fib(n-2) end
print(fib(32))
LUA
  say "已生成 Lua 负载 $WORKLOAD_LUA"
fi

# ---------------------------------------------------------------------------
# 插桩模板：数组 + destructor 落盘。放在**函数定义之前**（此时 stdio 已被本 TU 的
# 头文件拉进来），不碰文件顶部的 feature-test 宏 —— 实测把 `#include <stdio.h>`
# 插在 sqlite3.c 第 1 行会让后面的 `mremap` 因为 _GNU_SOURCE 被冻结而编不过。
# ---------------------------------------------------------------------------
emit_probe_block() { # $1=落盘路径 $2=数组名
  printf '%s\n' \
    "#include <stdio.h>" \
    "static unsigned long long $2[512];" \
    "static void ${2}_dump(void) __attribute__((destructor));" \
    "static void ${2}_dump(void) {" \
    "  int i; FILE *f = fopen(\"$1\", \"w\");" \
    "  if (!f) return;" \
    "  for (i = 0; i < 512; i++) if ($2[i]) fprintf(f, \"%d %llu\\n\", i, $2[i]);" \
    "  fclose(f);" \
    "}"
}

# ============================= Lua =============================
say ""
say "=== Lua：按 GET_OPCODE(i) 计权 ==="
LHW="$HW/lua_hw"
rm -rf "$LHW"; mkdir -p "$LHW" || die "mkdir $LHW"
cp -r "$LUA_SRC/." "$LHW/" >/dev/null 2>&1
LV="$LHW/src/lvm.c"
LJ="$LHW/src/ljumptab.h"
[ -f "$LV" ] || die "复制后找不到 $LV"
# 计数点必须在**每一次**分派上。这里有个坑：Lua 的 `vmbreak` 会展开成
# 「vmfetch(); vmdispatch(GET_OPCODE(i))」，也就是**每个 handler 尾部各有一个分派点**；
# 只插在主循环头部那一处 ⇒ 只数到「从函数入口/返回处进循环」的那一路（实测
# fib(32) 只得 1400 万次、OP_CALL 仅 1 次，而真实调用数就是 7 049 155）。
# 所以：跳表模式（`LUA_USE_JUMPTABLE`，gcc 下默认开）直接改 `vmdispatch` 宏本体；
# 退回 `switch` 模式时叒只有一个分派点，那就插在调用点前。
LDEF=$(grep -n '^void luaV_execute' "$LV" | head -1 | cut -d: -f1)
[ -n "$LDEF" ] || LDEF=$(grep -n 'luaV_execute *(lua_State' "$LV" | head -1 | cut -d: -f1)
[ -n "$LDEF" ] || die "在 lvm.c 里找不到 luaV_execute 的定义行"
emit_probe_block "$HW/lua_op_freq.txt" hw_lua > "$HW/lua_probe.txt"
if [ -f "$LJ" ] && grep -q '^#define vmdispatch' "$LJ"; then
  sed -i 's|^#define vmdispatch(x).*|#define vmdispatch(x)     { hw_lua[(int)(x) \& 255] += 1; goto *disptab[x]; }|' "$LJ"
  grep -q 'hw_lua\[(int)(x)' "$LJ" || die "ljumptab.h 的 vmdispatch 宏形式变了，改不动了"
  say "Lua 计数点：ljumptab.h 的 vmdispatch 宏（跳表模式，每个 handler 尾部都过）"
else
  LSEL=$(grep -n '^ *vmdispatch *(GET_OPCODE(i))' "$LV" | head -1 | cut -d: -f1)
  [ -n "$LSEL" ] || die "既没有 ljumptab.h 也没找到 vmdispatch (GET_OPCODE(i)) 调用点"
  say "Lua 计数点：lvm.c:$LSEL 的 vmdispatch 调用点（switch 模式）"
  sed -i "${LSEL}i\\    hw_lua[GET_OPCODE(i)] += 1;" "$LV"
fi
sed -i "$((LDEF-1))r $HW/lua_probe.txt" "$LV"
( cd "$LHW/src" && make clean >/dev/null 2>&1 && \
  make -j2 posix CC=gcc MYCFLAGS="-std=gnu99 -O2" AR="ar rcu" RANLIB=ranlib \
    > "$HW/lua_build.log" 2>&1 ) || die "gcc 构建插桩 Lua 失败，见 $HW/lua_build.log"
rm -f "$HW/lua_op_freq.txt"
taskset -c "$CPU" "$LHW/src/lua" "$WORKLOAD_LUA" > "$HW/lua_run.out" 2>&1 || die "插桩 Lua 运行失败"
[ -s "$HW/lua_op_freq.txt" ] || die "没拿到 Lua 权重（$HW/lua_op_freq.txt 空 ⇒ destructor 没跑或计数点没命中）"
LUA_STEPS=$(awk '{s+=$2}END{print s+0}' "$HW/lua_op_freq.txt")
say "Lua VM 指令总数（=Σfreq）= $LUA_STEPS，被执行到的 opcode 种类 = $(wc -l < "$HW/lua_op_freq.txt")"

# ============================ SQLite ============================
say ""
say "=== SQLite：按 pOp->opcode 计权 + 源码序臂位 ==="
SHW="$HW/sq_hw"
rm -rf "$SHW"; mkdir -p "$SHW" || die "mkdir $SHW"
cp "$SQLITE_SRC/sqlite3.c" "$SQLITE_SRC"/*.h "$SHW/" >/dev/null 2>&1
# 负载程序用上游 test/speedtest1.c（与 bench/verify 两处同一个），不再用 shell.c + 自写 SQL。
S1_SRC="${S1_SRC:-$SQLITE_SRC/test/speedtest1.c}"
[ -f "$S1_SRC" ] || die "找不到 speedtest1 源：$S1_SRC（设 S1_SRC=… 或把 SQLITE_SRC 指向解包源码树）"
cp "$S1_SRC" "$SHW/speedtest1.c" || die "拷 speedtest1.c 失败"
SQ="$SHW/sqlite3.c"
# 主分派锚点：全文里只有 VdbeExec 的 case 标号是**顶格**的（生成器约定），所以
# 「第一个顶格 case 之前的最后一个 `switch( pOp->opcode )`」才是主分派点 —— 不能拿
# 「第一个匹配」（实测会撞上 88766 行那一处无关的同类写法，导致只数到 63 步）。
CASE1=$(grep -n '^case OP_' "$SQ" | head -1 | cut -d: -f1)
[ -n "$CASE1" ] || die "找不到顶格的 case OP_（SQLite 版本的分派写法变了）"
SSEL=$(awk -v c="$CASE1" 'NR<c && /^ *switch\( pOp->opcode \)\{/ {n=NR} END{print n}' "$SQ")
[ -n "$SSEL" ] || die "在第一个顶格 case（第 $CASE1 行）之前找不到 switch( pOp->opcode )"
say "SQLite 主分派点：$SQ:$SSEL（第一个顶格 case 在第 $CASE1 行）"
emit_probe_block "$HW/sq_op_freq.txt" hw_sq > "$HW/sq_probe.txt"
sed -i "${SSEL}i\\    hw_sq[pOp->opcode] += 1;" "$SQ"
# 数组与 destructor 要放在**文件作用域**：switch 体内插入声明不合法，文件头插入会
# 冻结 feature-test 宏（实测会把后面的 `mremap` 编挂）。取主分派点之前最后一个
# `#include <stdio.h>` 之后落下，两个问题都避开。
INC=$(awk -v s="$SSEL" 'NR<s && $0=="#include <stdio.h>" {n=NR} END{print n}' "$SQ")
[ -n "$INC" ] || die "在主分派点之前找不到 `#include <stdio.h>`，插桩块无处安放"
sed -i "${INC}r $HW/sq_probe.txt" "$SQ"
gcc -O2 -o "$SHW/st_hw" "$SHW/speedtest1.c" "$SHW/sqlite3.c" -I"$SHW" \
    -DSQLITE_ENABLE_RTREE -DSQLITE_ENABLE_JSON1 -lpthread -ldl -lm -lz \
    > "$HW/sq_build.log" 2>&1 || die "gcc 构建插桩 SQLite+speedtest1 失败，见 $HW/sq_build.log"
rm -f "$HW/sq_op_freq.txt" "$SHW"/speedtest1.db*
# speedtest1 会在 cwd 落库文件，所以进到 $SHW 里跑；权重与结果哈希都走它自己的输出。
(
  cd "$SHW" || exit 1
  taskset -c "$CPU" timeout "$S1_TIMEOUT" ./st_hw $S1_ARGS
) > "$HW/sq_run.out" 2>&1 || die "插桩 speedtest1 运行失败/超时，见 $HW/sq_run.out"
[ -s "$HW/sq_op_freq.txt" ] || die "没拿到 SQLite 权重（$HW/sq_op_freq.txt 空）"
SQ_STEPS=$(awk '{s+=$2}END{print s+0}' "$HW/sq_op_freq.txt")
say "SQLite VM 步数（=Σfreq，与编译器无关）= $SQ_STEPS，被执行到的 opcode 种类 = $(wc -l < "$HW/sq_op_freq.txt")"

# ---- 臂位：VdbeExec 主 switch 里**顶格**的 `case OP_xxx:` 才是链上的一环；缩进更深的
# ---- 是臂体内的嵌套 switch，不能混进来。全文只有 VdbeExec 用顶格 case（生成器约定，
# ---- 实测 190 个），所以拿「主分派点之后的全部顶格 case」就是链序；不要拿「下一个
# 顶格 `}`」当结束（注释栏里就有能把窗口切到 1 个臂的 `}`）。SBODY 那一套已撤。
awk -v s="$SSEL" '
  NR>s && /^case OP_/ { line=$0; sub(/^case[ \t]*/,"",line); sub(/[^A-Za-z0-9_].*/,"",line);
                        arm++; print arm, line }
' "$SQ" > "$HW/sq_arm_order.txt"
ARM_N=$(wc -l < "$HW/sq_arm_order.txt")
[ "$ARM_N" -gt 0 ] || die "没抽出 VdbeExec 的具名 case 顺序"
# opcode 名 → 数值（`#define OP_X n`），再用数值把 freq 对到臂位
grep -oE '^#define OP_[A-Za-z0-9_]+ +[0-9]+' "$SQ" | awk '{print $2, $3}' > "$HW/sq_op_val.txt"
awk 'NR==FNR { val[$1] = $2; next } { print $1, val[$2], $2 }' \
    "$HW/sq_op_val.txt" "$HW/sq_arm_order.txt" > "$HW/sq_arm_value.txt"
P=$(awk 'NR==FNR { f[$1] = $2; next }
        { if ($2 in f) { s += f[$2] * $1; t += f[$2] } }
        END { if (t > 0) printf "%.1f", s/t; else print "NA" }' \
     "$HW/sq_op_freq.txt" "$HW/sq_arm_value.txt")
[ "$P" != "NA" ] || die "加权臂位算不出来（臂序的 opcode 名与 #define 的数值对不上）"
say "主分派链的步内覆盖：$(awk 'NR==FNR{f[$1]=$2;next}{if($2 in f)t+=f[$2]}END{print t}' "$HW/sq_op_freq.txt" "$HW/sq_arm_value.txt") / $SQ_STEPS 步"
say "SQLite 主分派：源码序具名 case $ARM_N 个，加权臂位 P = $P（均匀假设会给 $((ARM_N/2))）"
say "  ⇒ 比较链每步约 $(awk -v p="$P" 'BEGIN{printf "%.0f", p*4}') 条（每臂 4 条：mov imm / load slot / cmp / je）"

# ======================= E（可选：给定候选产物） =======================
if [ -n "${CAND:-}" ]; then
  say ""
  say "=== 主代理量 E：Σ freq[op]·insns[handler] ==="
  SYM="${SYM:-luaV_execute}"
  # lopcodes.h 的枚举序就是 GET_OPCODE 的取值（`OP_MOVE,` 顶格，一行一个）。
  awk '/^OP_[A-Z0-9_]+,/ { n++; name=$0; sub(/,.*/,"",name); print n-1, name }' \
      "$LUA_SRC/src/lopcodes.h" > "$HW/lua_op_val.txt"
  [ -s "$HW/lua_op_val.txt" ] || die "lopcodes.h 里没抽出 opcode 枚举序（形式变了）"
  # fn_opcode_heat.sh 的列：区间名 / 地址 / dcc 条数 / gcc 条数 / 倍数 / 样本 / 占比
  bash "$HERE/fn_opcode_heat.sh" "$CAND" "$SYM" 2>/dev/null \
    | awk 'NR>1 && $1 ~ /^OP_/ { print $1, $3 }' > "$HW/heat_raw.txt"
  [ -s "$HW/heat_raw.txt" ] || die "从 $CAND 没切出任何区间（需要带 disptab 的产物）"
  awk 'FILENAME ~ /lua_op_val/  { idx[$2] = $1; next }
       FILENAME ~ /lua_op_freq/ { freq[$1] = $2; next }
       FILENAME ~ /heat_raw/ {
         n = $1; c = $2 + 0;
         if (n in idx) { k = idx[n]; if (k in freq) { e += freq[k] * c; t += freq[k]; cov++ } }
       }
       END {
         if (t == 0) { print "E: 区间名与枚举序完全对不上（覆盖 0）"; exit 1 }
         printf "E = %.2f 条/VM 指令（加权总步数 %d，覆盖 %d 个区间）\n", e/t, t, cov
       }' \
     "$HW/lua_op_val.txt" "$HW/lua_op_freq.txt" "$HW/heat_raw.txt" || exit 1
  say "  （上一行与候选产物同档；与基线比较时只比这一行的相对差值，不比绝对值）"
fi

say ""
say "产物（供其它脚本复用）："
say "  $HW/lua_op_freq.txt      Lua 每 opcode 执行次数（opcode 序号 = lopcodes.h 的枚举序）"
say "  $HW/sq_op_freq.txt       SQLite 每 opcode 执行次数（opcode 数值）"
say "  $HW/sq_arm_order.txt     VdbeExec 主 switch 的臂序（臂号 OP_名）"
say "  $HW/sq_arm_value.txt     臂号 / 数值 / 名字"
say "hot_weights: OK"
