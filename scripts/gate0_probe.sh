#!/bin/bash
# ===========================================================================
# gate0_probe.sh — 方案 §2 的 Gate 0 三探针（0a 影子槽表 / 0b raw 归因 / 0c 分桶）
#
# 为什么必须有它而不是一次性手敲：Gate 0 的输出**决定后面写哪段代码**（方案 §2 原文），
# 所以它必须能在任何一次改动之后重跑，否则「S1 还是 S2 优先」这个决定就变成一次性结论，
# 之后没人能复核。0b 的「两列」也是同一个道理：同一套计数在补全解码器前后各跑一次。
#
# 三个探针都不改产物（0a/0b 的影子层与仪表只读），所以它们在同一次构建里就能全出。
#
# 用法:
#   bash scripts/gate0_probe.sh [lua|sqlite|both]
# 环境:
#   DCC=<路径>        dcc 二进制（默认 target/release/dcc，缺失则退回 target/debug/dcc）
#   LUA_SRC=<路径>    Lua 源码目录（默认 /root/lua-5.5.1/src）
#   SQLITE_C=<路径>   sqlite3.c（默认 /root/sqlite/sqlite3.c）
#   FUNCS=<名单>      要读的函数（空格分隔；默认按负载取 luaV_execute / sqlite3VdbeExec）
#   GCC=<命令>        gcc（默认 gcc）
#   KEEP=<目录>       中间 .o 的落点（默认 /tmp/gate0）
#
# 判据落点（方案 §8.2 修订后的式子，不是 §2 的旧式子）：
#   0a：`hit_call+hit_label+hit_pressure+hit_capacity < 260` ⇒ S2 停，只保 S1。
#       旧稿写的 `shadow_total < 260` 是**死门**（影子表只被真存槽清，必然 ≥ 真实重复数）。
#   0b：夹着 raw-Unknown 的比例 ≥60% ⇒ L1 是 L3 的主因，S1 第一优先；
#       <30% ⇒ 真寄存器压力，区域家（S4）第一优先。两列 = 同一计数器在
#       `DCC_BLOCKOPT_RULES` 默认档与含 R_RAW 档下各读一次。
#   0c：gcc 对照本体按同一口径分桶 ⇒ 桶表直接给「这个负载上还有多少可消」。
# ===========================================================================
set -u

TARGET="${1:-both}"
DCC="${DCC:-}"
if [ -z "$DCC" ]; then
  if [ -x target/release/dcc ]; then DCC=target/release/dcc
  elif [ -x target/debug/dcc ]; then DCC=target/debug/dcc
  else echo "gate0: 找不到 dcc 二进制，先 cargo build" >&2; exit 1; fi
fi
LUA_SRC="${LUA_SRC:-/root/lua-5.5.1/src}"
SQLITE_C="${SQLITE_C:-/root/sqlite/sqlite3.c}"
GCC="${GCC:-gcc}"
KEEP="${KEEP:-/tmp/gate0}"
mkdir -p "$KEEP"

# 影子表与仪表只在 stderr 上说话，所以每条臂都要把 stderr 收进文件再筛。
shadow_of() { # <file> <func>
  awk -v f="$2" '$1=="slotshadow" && $2=="fn="f {
    for (i=3; i<=NF; i++) { split($i, kv, "="); printf "%s=%s ", kv[1], kv[2] }
    print ""
  }' "$1"
}

# 0a + 0b 的残余变体表：把仪表输出聚合成「哪个 raw 变体杀得最多」。
killvar_top() { # <err-file> [top-n]
  local top="${2:-10}"
  awk '$1=="killvar" { split($3, a, "="); v[a[2]] += substr($4,6)+0 }
       END { for (k in v) print v[k], k }' "$1" | sort -rn | head -"$top"
}

# 0c：按方案 §0 的桶口径给两侧分桶。口径全部写在同一个 awk 里，所以「dcc 的帧载入」
# 与「gcc 的帧载入」的定义不可能漂移（历史上这两套口径漂移过一次，导致 §8.2 记的
# 那三个互不相容的数字）。
#
# 帧基址寄存器**两侧不同**是事实而不是口径：dcc 的分析-only 通道把帧基地址常驻
# `%r13`，gcc -O2 用 `%rbp`（Lua）或 `%rsp`+红区（SQLite）。所以下一个参数必传。
#
# 方向判据（Intel 语法，目的在前）：第一个操作数带 `[<基址>` ⇒ 存出；否则 ⇒ 载入。
# 拿「谁不带括号」反推会把每一条存出数成载入（本函数第一版就这么错过）。
bucket() { # <obj> <func> <帧基址正则>
  objdump -d -M intel --no-show-raw-insn "$1" 2>/dev/null | awk -v sym="$2" -v fb="$3" '
    $0 ~ ("<" sym ">:") { inside=1; next }
    inside && /^[0-9a-f]+ <.*>:/ { exit }
    inside && /^[[:space:]]+[0-9a-f]+:/ {
      line=$0
      sub(/^[[:space:]]*[0-9a-f]+:[[:space:]]*/, "", line)
      n++
      mn = line; sub(/[[:space:]].*/, "", mn)
      ops = line; sub(/[^[:space:]]+[[:space:]]+/, "", ops)
      first = ops; sub(/,.*/, "", first)
      rest = ops; sub(/[^,]*,/, "", rest)
      if (first ~ ("\\[" fb)) out["store"]++
      else if (ops ~ ("\\[" fb)) out["load"]++
      # `mov 号,号`：只算 64 位寄存器对（以 r 开头的两侧同号；e 开头的 32 位形不计，
      # 它与方案 §0 表里那 442 条 gcc 读数的口径一致）。
      if (mn ~ /^mov[a-z]*$/ && ops ~ /^r[a-z0-9]+,r[a-z0-9]+$/) out["movrr"]++
      if (mn == "movsxd") out["movsxd"]++
      if (mn == "movzx" || mn == "movsx" || mn == "movsxb" || mn == "movsxl" || mn == "movsxw") out["movzx"]++
      if (mn ~ /^mov/ && ops ~ /^r[a-z0-9]+,\$/) out["movimm"]++
      if (mn == "cmp" || mn == "test" || mn ~ /^j/) out["br"]++
      if (mn == "call") out["call"]++
      if (mn == "lea") out["lea"]++
      next
    }
    END {
      printf "insn=%d", n
      key[1] = "load";   lbl[1] = "帧载入"
      key[2] = "store";  lbl[2] = "帧存出"
      key[3] = "movrr";  lbl[3] = "mov r64,r64"
      key[4] = "movsxd"; lbl[4] = "movsxd"
      key[5] = "movimm"; lbl[5] = "立即数 mov"
      key[6] = "br";     lbl[6] = "比较/分支"
      key[7] = "call";   lbl[7] = "调用"
      key[8] = "movzx";  lbl[8] = "movzx"
      key[9] = "lea";    lbl[9] = "lea"
      for (i = 1; i <= 9; i++) printf "\n    %-13s %d", lbl[i], out[key[i]]
      print ""
    }'
}

lua_cmd() { "$DCC" -O2 -I"$LUA_SRC" -DLUA_USE_LINUX -o "$1" -c "$LUA_SRC/lvm.c"; }
lua_gcc() { "$GCC" -O2 -I"$LUA_SRC" -DLUA_USE_LINUX -o "$1" -c "$LUA_SRC/lvm.c"; }
sql_cmd() { "$DCC" -O2 -DSQLITE_THREADSAFE=0 -o "$1" -c "$SQLITE_C"; }
sql_gcc() { "$GCC" -O2 -DSQLITE_THREADSAFE=0 -o "$1" -c "$SQLITE_C"; }

if [ "$TARGET" = lua ] || [ "$TARGET" = both ]; then
  FUNCS="${FUNCS:-luaV_execute}"
  if [ -f "$LUA_SRC/lvm.c" ]; then
    # 三档：默认位图（不含 R_RAW）/ 含 R_RAW 的当前解码器 / 再加两个形状位。
    # 前两档就是 0b 要的「两列」；第三档只用来确认「形状位不改变 0a 的读数」。
    for rules in 3 0x27 0xE7; do
      DCC_SLOT_SHADOW=1 DCC_KILL_ALL_STAT=1 DCC_BLOCKOPT_RULES=$rules lua_cmd "$KEEP/lua.r$rules.o" \
        2>"$KEEP/lua.r$rules.err" \
        || echo "  lua 编译失败 rules=$rules" >&2
      for f in $FUNCS; do
        printf '  rules=%-5s fn=%-16s %s\n' "$rules" "$f" "$(shadow_of "$KEEP/lua.r$rules.err" "$f")"
      done
    done
    echo "=== [lua] 0b 残余 raw 变体 Top10（默认档 rules=3）==="
    killvar_top "$KEEP/lua.r3.err"
    echo "=== [lua] 0b 残余 raw 变体 Top10（R_RAW 档 rules=0x27）==="
    killvar_top "$KEEP/lua.r0x27.err"
    echo "=== [lua] 0c 分桶：dcc(r13) vs gcc(rbp) ==="
    lua_gcc "$KEEP/lua.gcc.o" 2>/dev/null || echo "  gcc 编 lvm.c 失败" >&2
    for pair in "dcc $KEEP/lua.r3.o r13" "gcc $KEEP/lua.gcc.o rbp"; do
      set -- $pair
      for f in $FUNCS; do echo "  [$1/$f] $(bucket "$2" "$f" "$3")"; done
    done
    # 判据算术：屏障挡住的转发数 = 四个 hit_* 之和（§8.2 的修订式）。
    for rules in 3 0x27; do
      awk -v thr=260 -v R="$rules" '
        /slotshadow fn=luaV_execute/ {
          for (i=1; i<=NF; i++) {
            split($i, kv, "=")
            if (kv[1] ~ /^hit_/ && kv[1] != "hit_raw") s += kv[2]
            if (kv[1] == "total") t = kv[2]
            if (kv[1] == "hit_raw") raw = kv[2]
          }
          printf "  0a/0b 判据（rules=%s）：total=%d 屏障合计=%d ⇒ %s；hit_raw=%d（占 %.0f%%）\n", \
            R, t, s, (s < thr ? "S2 停、只保 S1" : "S2 继续"), raw, (t ? 100.0*raw/t : 0)
        }' R="$rules" "$KEEP/lua.r$rules.err"
    done
  else
    echo "gate0: 没有 $LUA_SRC/lvm.c，跳过 lua" >&2
  fi
fi

if [ "$TARGET" = sqlite ] || [ "$TARGET" = both ]; then
  FUNCS="${FUNCS:-sqlite3VdbeExec}"
  if [ -f "$SQLITE_C" ]; then
    for rules in 3 0x27; do
      DCC_SLOT_SHADOW=1 DCC_KILL_ALL_STAT=1 DCC_BLOCKOPT_RULES=$rules sql_cmd "$KEEP/sql.r$rules.o" \
        2>"$KEEP/sql.r$rules.err" \
        || echo "  sqlite 编译失败 rules=$rules" >&2
      for f in $FUNCS; do
        printf '  rules=%-5s fn=%-16s %s\n' "$rules" "$f" "$(shadow_of "$KEEP/sql.r$rules.err" "$f")"
      done
    done
    echo "=== [sqlite] 0b 残余 raw 变体 Top10（默认档）==="
    killvar_top "$KEEP/sql.r3.err"
    echo "=== [sqlite] 0c 分桶：dcc(r13) vs gcc(rsp) ==="
    sql_gcc "$KEEP/sql.gcc.o" 2>/dev/null || echo "  gcc 编 sqlite3.c 失败" >&2
    for pair in "dcc $KEEP/sql.r3.o r13" "gcc $KEEP/sql.gcc.o rsp"; do
      set -- $pair
      for f in $FUNCS; do echo "  [$1/$f] $(bucket "$2" "$f" "$3")"; done
    done
  else
    echo "gate0: 没有 $SQLITE_C，跳过 sqlite" >&2
  fi
fi
