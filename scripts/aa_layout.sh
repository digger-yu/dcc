#!/bin/bash
# ===========================================================================
# scripts/aa_layout.sh — A/A 空臂：同一份语义、只有布局不同的产物族（阶段6 批0c）
#
# 要解决的问题：本机没有 PMU（`cycles`/`instructions`/`branch-misses` 全
# `<not supported>`，`perf_event_paranoid=4`，也没装 valgrind），于是「只减少取指体量」
# 那一类改动（冷路径外置、tail merging）**没有直接判据**。历史上因此一律搁置
# （方案 §13.7），但那等于永远不知道它值多少钱。出路是先量出本机的
# `d(runtime)/d(footprint)` 斜率：把同一份代码的热区整体位移若干字节，体量变了、
# 执行的指令一条没变 —— 这族差值就是「纯布局/前端」效应，也是任何体量改动的换算率。
# 顺带得到另一件必需的东西：**同二进制跨臂的噪声带 NA**。小于 NA 的差值一律不许
# 叫「改善」（同二进制跨轮 min 能漂 11%，本脚本给的是同一时刻交替测出的更严格版本）。
#
# 为什么必须自己造扰动源：现成可调的布局旋钮只有一个 —— `-falign-loops`
# （`src/cli.rs:2718-2722` → `emit/stmt.rs:808/835/865` → `instructions.rs:2922`，
# 填充发在 loop 标签**之前**，永不执行）；`-falign-jumps`/`-falign-functions`/
# `-falign-labels` 在 x86_64 是死旋钮（`maybe_align_jump` 零调用者），
# `-fpatchable-function-entry` 的 nop 在序言里**每次调用都执行**，只能当干扰对照组。
# 所以补了两个专用旋钮（`emit/stmt.rs`，`DCC_FN_SHIFT` 在分派链末尾那条无条件 jmp
# 之后、`DCC_HANDLER_PAD` 在每个子句标签之前，都只发单字节 `nop`）。
#
# 用法:
#   SRC=<含大 switch 的 .c> bash scripts/aa_layout.sh
#   SHIFTS="0 16 64 256" R=9 CPU=3 DCC=<路径> SYM=<被测函数名> \
#     EXTRA="<链接时附加的源文件/库>"  OUTDIR=/tmp/aa  bash scripts/aa_layout.sh
#     SRC 自己可编；要跑完整程序时用 EXTRA 补其余部分（只参与链接，不参与编译）。
#     SYM 只用于把 footprint 缩到被测函数（缺省用 .text 总量）。
# 判据（退出码）:
#   * 任一臂的汇编文本在**滤掉 nop 行之后**与基准不一致 ⇒ 立即失败。
#     这条是整套斜率换算的地基：它证明扰动族「语义全等、只差布局」，而不是
#     编译器顺手改了代码 —— 只看产物大小是看不出差别的。
#   * 其余都是报告：噪声带 NA、每臂 min/p50、斜率 slope(ms/KB)。
#     判「冷路径外置值不值得做」的阈值写在方案 §5：slope·ΔKB ≥ 2σ 才做。
# ===========================================================================
set -o pipefail
set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="${DCC_REPO:-$(cd "$HERE/.." && pwd)}"
DCC="${DCC:-$REPO/target/release/dcc}"
SRC="${SRC:-}"
SHIFTS="${SHIFTS:-0 16 64 256}"
R="${R:-9}"
CPU="${CPU:-3}"
SYM="${SYM:-}"
EXTRA="${EXTRA:-}"
MODE="${MODE:-lua}"
ARG="${ARG:-}"
OUTDIR="${OUTDIR:-/tmp/aa_layout}"

# 扰动族：默认用 DCC_FN_SHIFT；给 PADS 就改用 DCC_HANDLER_PAD（体量敏感的那一族）。
ARMSPEC_ARR=()
if [ -n "${PADS:-}" ]; then
  for k in $PADS; do ARMSPEC_ARR+=("pad$k:DCC_HANDLER_PAD=$k"); done
else
  for sh in ${SHIFTS:-0 16 64 256}; do ARMSPEC_ARR+=("sh$sh:DCC_FN_SHIFT=$sh"); done
fi

die() { echo "aa_layout: $*" >&2; exit 1; }
[ -x "$DCC" ] || die "DCC 不存在或不可执行: $DCC（先 cargo build --release）"
[ -n "$SRC" ] && [ -f "$SRC" ] || die "SRC=<含大 switch 的 .c> 必须给且要存在"
command -v objdump >/dev/null || die "objdump 不在 PATH"
mkdir -p "$OUTDIR" || die "无法创建 $OUTDIR"

say() { echo "$@"; }

text_bytes() { # $1=.o → .text 的字节数（mawk 没有 strtonum，十六进制自己推）
  objdump -h "$1" 2>/dev/null | awk '$2==".text"{
    h=$3; v=0; for(i=1;i<=length(h);i++){ c=substr(h,i,1);
      d=index("0123456789abcdef", tolower(c))-1; v=v*16+d } print v }'
}

fn_bytes() { # $1=.o $2=sym → 该符号的字节数（拿不到符号就退回 .text）
  # nm -S 的行是「地址 大小 类型 名字」（4 列），名字在第 4 列；static 函数可能
  # 被内联掉而根本没有符号，所以拿不到时必须退回 .text，不能报空。
  if [ -n "$2" ]; then
    local v
    v=$(nm -S "$1" 2>/dev/null | awk -v s="$2" 'NF>=4 && $4==s {
          h=$2; n=0; for(i=1;i<=length(h);i++){ c=substr(h,i,1);
            d=index("0123456789abcdef", tolower(c))-1; n=n*16+d } print n }')
    if [ -n "$v" ]; then echo "$v"; return; fi
  fi
  text_bytes "$1"
}

# ---- 1) 造臂：同一 SRC，只换扰动旋钮 ----
# 两个旋钮各自测不同的东西，不要混为一谈：
#   DCC_FN_SHIFT=n     把**整个子句区**位移 n 字节（只加 n 字节，区域体量不变）
#                      ⇒ 量的是「位置/对齐」敏感度；
#   DCC_HANDLER_PAD=k  每个子句标签前插 k 个 nop ⇒ 热区体量长 k×子句数
#                      （185 路的 switch 上 k=64 就是 +11.6KB）⇒ 量的是「体量/前端」敏感度，
#                      斜率换算要用这一族。
#
# 载体必须是**真的走到发射器 Switch 臂**的那一个：两个旋钮都长在
# `emit/stmt.rs` 的 `Stmt::Switch` 臂上，而 -O2 起 `ssa::lower_switches_in_func` 会把
# 能忠实降级的 switch 改成 if 链（实测：一个稠密无 fallthrough 的微基准在 -O2 下
# 根本不发 `Stmt::Switch`，两旋钮一位没动，.text 完全同大）。所以：要么用
# `MYCFLAGS=-O0/-O1`，要么拿真正的目标（sqlite3VdbeExec：它含 goto 与非空 fallthrough，
# 降级被拒 ⇒ 留在发射链上）。本脚本默认的 MYCFLAGS 是 -O2，跑前先确认体量真的动了。
declare -a OBJS TBYTES ANAMES
i=0
for spec in "${ARMSPEC_ARR[@]}"; do
  nm_="${spec%%:*}"; env_="${spec#*:}"
  o="$OUTDIR/$nm_.o"; s="$OUTDIR/$nm_.s"
  if [ -s "$o" ] && [ -s "$s" ] && [ "$o" -nt "$SRC" ] && [ "$o" -nt "$DCC" ] && [ "$s" -nt "$DCC" ]; then
    # 复用条件：比源码与**编译器本身**都新。漏掉后者会拿旧 dcc 编的臂与新 dcc 的臂
    # 比，结论全是假的（sqlite3.c 一轮要两分钟，不能靠「我记得它是新的」）。
    say "  臂 $nm_ 复用已有产物（比 $SRC 与 $DCC 都新）"
  else
  # shellcheck disable=SC2086
  env $env_ "$DCC" ${MYCFLAGS:--O2} -std=gnu99 -S "$SRC" -o "$s" 2>/dev/null || die "-S 编译失败 ($nm_: $env_)"
  # shellcheck disable=SC2086
  env $env_ "$DCC" ${MYCFLAGS:--O2} -std=gnu99 -c "$SRC" -o "$o" 2>/dev/null || die "-c 编译失败 ($nm_)"
  fi
  sz="$(fn_bytes "$o" "$SYM")"
  OBJS[i]="$o"; TBYTES[i]="$sz"; ANAMES[i]="$nm_"; i=$((i+1))
  # 每臂一行进度：sqlite3.c 这种载体单臂要编两次（-S 与 -c）、共两三分钟，
  # 全部编完才输出会让外层的轮询看不出是「在跑」还是「挂住」。
  say "  臂 $nm_ 就绪（$env_）⇒ $o ${sz:-?} 字节"
done
NARM=$i
base_s="$OUTDIR/${ANAMES[0]}.s"

# ---- 2) 地基断言：滤掉 nop 之后**指令流**逐字相同 ----
# 不能直接比文本：插 nop 会把 `.L1301:` 这类标签相对指令行挤歪一位，整行 diff 会把它
# 报成「非 nop 差异」（实测就这样假失败了一次）。所以：
#   a) 指令流 = 去掉空 nop 行、也去掉「纯标签行」（它们的位置本来就是布局的一部分）；
#   b) 标签多重集单独比（排序后逐字相同）⇒ 保证没多标签也没丢标签，
#      否则「标签少了一个」会在 a) 里隐身（那行本来就是标签）。
filt() { grep -vE '^[[:space:]]+nop([[:space:]]|$)' "$1" | grep -vE '^[._[:alnum:]]+:[[:space:]]*$'; }
labels() { grep -oE '^[._[:alnum:]]+:' "$1" | sort; }
for nm_ in "${ANAMES[@]}"; do
  o="$OUTDIR/$nm_.s"
  if ! diff -q <(filt "$base_s") <(filt "$o") >/dev/null; then
    echo "---- 与基准臂的指令流差异（前 12 行）----" >&2
    diff <(filt "$base_s") <(filt "$o") | head -12 >&2
    die "$o 与基准在**指令流**上不一致 ⇒ 扰动族不是纯布局，斜率与噪声带全部作废"
  fi
  if ! diff -q <(labels "$base_s") <(labels "$o") >/dev/null; then
    echo "---- 标签集合差异（前 12 行）----" >&2
    diff <(labels "$base_s") <(labels "$o") | head -12 >&2
    die "$o 的标签集合与基准不同 ⇒ 扰动改变了控制流，斜率与噪声带作废"
  fi
done
say "布局族地基成立：${ANAMES[*]} 各臂的指令流与标签集合与 ${ANAMES[0]} 逐字相同（只有 nop 数不同）"

# ---- 2b) 静态可达性断言（方案 §5.1 的那条修正）----
# 上面那道「滤掉 nop」不够：`hot_weights.sh` 算 E 的管道同样 `grep -v nop`，于是
# 「PAD=256 时 E 必须不变」这道防伪**结构上永不为伪** —— 被过滤掉的字节恰好是
# 可能真的被执行的那些。真立得住的判据是静态的：**每段 pad 的前驱落点必须以
# jmp / ret / jcc 结尾**（那种位置只能被跳过，不可能被落入）。
#   * FN_SHIFT 臂（插在分派链末尾那条无条件 jmp 之后）必须全段可跳过 ⇒ 不成立就 die：
#     那意味着旋钮不再是「纯位移」，整个扰动族与它换出的斜率全部作废；
#   * HANDLER_PAD 臂插在子句标签之前，上一个臂若是 fall-through 进来的会**真的执行**
#     这些 nop ⇒ 那一族只报告可落入段数，并据此把它从**斜率拟合**里排除（方案 §5.1
#     的口径本来就写「斜率以 DCC_FN_SHIFT 为准」，本旋钮只用来区分对齐与体量两种效应）。
#
# 判的是**本脚本插进去的那些 nop 段**，不是产物里所有的 nop：对齐填充（`.p2align`、
# 循环标签前的填充）本来就在，它们的前驱当然不是跳转（实测：基准臂自带 4 段这种 nop）。
# 所以先按「(前驱助记符, 后随标号)」把每段 nop 做成一个键，再与基准臂比：只有**长度变了
# 或多出来的**那些段才是旋钮插的，只对这些段要求前驱是 jmp/ret/jcc。
pad_runs() {   # $1=汇编文本 → 每行「前驱|后随标号|段长」
    awk '
        { raw = $0; s = $0; sub(/^[ \t]+/, "", s);
          if (s == "" || s ~ /^[.#]/ && raw !~ /^[ \t]+/) {          # 纯标号/指示符行
              if (inrun > 0) { lab = raw; sub(/:.*/, "", lab); print last "|" lab "|" inrun; inrun = 0 }
              next
          }
          if (raw !~ /^[ \t]+/) { next }                             # 其余非指令行（节、.size 等）
          split(s, a, /[ \t]+/); mn = a[1]
          if (mn == "nop") { inrun++; last_non_instr = 1; next }
          if (inrun > 0) { print last "|<no-label>|" inrun; inrun = 0 }
          last = mn
        }
        END { if (inrun > 0) print last "|<eof>|" inrun }' "$1"
}
pad_diff_report() {   # $1=基准臂汇编 $2=待测臂汇编 → 「新增或变长的段数 其中可被落入的段数」
    awk -F'|' '
        FNR == 1 { part++ }
        part == 1 { base[$1 "|" $2] = $3 + 0; next }
        { k = $1 "|" $2; b = (k in base) ? base[k] : 0
          if ($3 + 0 > b) {
              ins++
              p = $1
              if (!(p == "jmp" || p == "ret" || (p ~ /^j/ && p != "jmp"))) {
                  fall++
                  printf("  插入段可被落入：前驱=%s 标号=%s（基准 %d → 本臂 %d）\n", p, $2, b, $3 + 0) > "/dev/stderr"
              }
          } }
        END { printf("%d %d\n", ins + 0, fall + 0) }' "$1" "$2"
}
SLOPE_ARMS=()
for nm_ in "${ANAMES[@]}"; do
    set -- $(pad_diff_report <(pad_runs "$base_s") <(pad_runs "$OUTDIR/$nm_.s"))
    ins="$1"; fall="$2"
    say "  臂 $nm_：旋钮插入的 nop 段 $ins 段，其中前驱不是 jmp/ret/jcc 的 $fall 段"
    case "$nm_" in
      pad*)
        # PAD 族允许有可落入段（那是它设计上的混合扰动性质），但不进斜率拟合。
        if [ "${fall:-0}" -gt 0 ]; then
            say "    ⇒ 该臂不参与斜率拟合（有 $fall 段会被执行，不是纯体量）"
        elif [ "${ins:-0}" -gt 0 ]; then
            SLOPE_ARMS+=("$nm_")
        fi
        ;;
      *)
        # FN_SHIFT 族：任何一段能被落入就说明旋钮不是纯位移，地基直接破。
        [ "${fall:-0}" -eq 0 ] \
            || die "$nm_ 臂有 $fall 段插入 nop 的前驱不是 jmp/ret/jcc ⇒ 该臂会被执行，不是纯位移；扰动族作废"
        # 一位没动（ins=0，就是基准臂本身）不能作为斜率臂：它没有位移量，参不进拟合。
        [ "${ins:-0}" -gt 0 ] && SLOPE_ARMS+=("$nm_")
        ;;
    esac
done
[ "${#SLOPE_ARMS[@]}" -gt 0 ] \
    || die "没有任何位移臂能参与斜率拟合（FN_SHIFT 没插进 nop？先确认载体真的走到发射器 Switch 臂）"
say "静态可达性断言成立：参与斜率拟合的臂 ${SLOPE_ARMS[*]} 里，插入的每一段 nop 都只能被跳过"

# ---- 3) 体量表 ----
say ""
say "=== 每臂体量与相对变化（SYM=${SYM:-<.text>}；基准臂=${ANAMES[0]}） ==="
for j in "${!ANAMES[@]}"; do
  printf "  %-8s 字节=%-9s Δ=%+d\n" "${ANAMES[$j]}" "${TBYTES[$j]}" "$(( ${TBYTES[$j]} - TBYTES[0] ))"
done

# ---- 4) 链接成可执行（需要跑起来才有 runtime） ----
if [ -n "${EXTRA:-}" ] || grep -q '\bmain\b' "$SRC"; then
  j=0
  for nm_ in "${ANAMES[@]}"; do
    # shellcheck disable=SC2086
    gcc -O2 -o "$OUTDIR/exe_$nm_" "${OBJS[$j]}" $EXTRA >"$OUTDIR/link_$nm_.log" 2>&1 \
      || die "链接可执行失败 ($nm_)：见 $OUTDIR/link_$nm_.log"
    j=$((j+1))
  done
  ARMS=""; j=0
  for nm_ in "${ANAMES[@]}"; do ARMS="$ARMS $nm_=$OUTDIR/exe_$nm_"; j=$((j+1)); done
  # 再加一个**与基准臂同一二进制**的臂：各臂 min 极差混的是“布局敏感度”（pad64 就是
  # 真慢），拿它当噪声带会把敏感度误判成分辨率；只有同一二进制跑两遍的差才是真噪声。
  cp -f "$OUTDIR/exe_${ANAMES[0]}" "$OUTDIR/exe_dup0"
  ARMS="$ARMS dup0=$OUTDIR/exe_dup0"
  say ""
  say "=== 交替配对 $R 轮（臂序按轮旋转；比值以第一臂 ${ANAMES[0]} 为基准） ==="
  # shellcheck disable=SC2086
  R="$R" CPU="$CPU" RAW="$OUTDIR/raw.txt" BASE=0 ARMS="${ARMS# }" ARG="$ARG" \
    bash "$HERE/ab_pair.sh" "$MODE" || die "ab_pair 失败"
  say ""
  say "=== 噪声带 NA 与前端斜率（把上面两个数直接拿去当后续判据用） ==="
  # 体量序列：相对基准臂的字节增量（与 ARMS 同序，dup0 与基准同体积记 0）
  FPD="0"; ALIST="${ANAMES[0]}"; j=1
  while [ "$j" -lt "$NARM" ]; do
    FPD="$FPD,$(( ${TBYTES[$j]} - TBYTES[0] ))"; ALIST="$ALIST,${ANAMES[$j]}"
    j=$((j+1))
  done
  FPD="$FPD,0"; ALIST="$ALIST,dup0"
  awk -v R="$R" -v A="$((NARM + 1))" -v LAST="$((NARM - 1))" -v FPD="$FPD" -v NAMES="$ALIST" '
    BEGIN { n = split(NAMES, arname, ","); m = split(FPD, fp, ",") }
    { d[$1 "," $2] = $3 }
    END {
      for (a = 0; a < A && a < n; a++) {
        for (i = 0; i < R; i++) v[i+1] = d[i "," a]
        for (i = 1; i <= R; i++) for (j = i+1; j <= R; j++) if (v[j] < v[i]) { t = v[i]; v[i] = v[j]; v[j] = t }
        mn[a] = v[1]; mx[a] = v[R]; md[a] = v[int((R+1)/2)]
      }
      gmin = mn[0]; gmax = mn[0]
      for (a = 1; a < A && a < n; a++) { if (mn[a] < gmin) gmin = mn[a]; if (mn[a] > gmax) gmax = mn[a] }
      NA = gmax - gmin
      printf "布局臂 min 极差 = %.1f ms（%.2f%%）——这是**布局敏感度**，不是噪声带\n",
             NA, 100 * NA / gmin
      # dup0 臂与基准臂是同一个二进制 ⇒ 它的 min 与基准的 min 之差才是真噪声
      for (a = 0; a < A && a < n; a++) if (arname[a+1] == "dup0") dup = a
      if (dup > 0) printf "真噪声带（同一二进制两进两出）= %.1f ms：Δ 小于它就是不可判\n",
                          (mn[dup] > mn[0] ? mn[dup] - mn[0] : mn[0] - mn[dup])
      for (a = 0; a < A && a < n; a++) {
        # fp 是 split() 出的 **1 基**数组，臂下标是 0 基：写 fp[a] 会把整列体量
        # 往左移一个臂（实测 pad32 报了 0 KB、真值 5.8 KB，斜率也就跟着错）。
        kb = fp[a+1] / 1024
        sl = (kb == 0 || a == 0) ? 0 : (md[a] - md[0]) / kb
        printf "  %-10s Δfootprint=%+7.2f KB  median=%.1fms  相对=%+.3fms/KB\n", arname[a+1], kb, md[a], sl
      }
      hi = LAST; if (hi >= n) hi = n - 1
      kb = fp[hi+1] / 1024
      noise = -1
      for (a = 0; a < A && a < n; a++) if (arname[a+1] == "dup0")
        noise = (mn[a] > mn[0] ? mn[a] - mn[0] : mn[0] - mn[a])
      if (kb > 0) {
        slope = (md[hi] - md[0]) / kb
        printf "斜率 slope = %.3f ms/KB（用最大位移臂 %.2f KB）\n", slope, kb
        printf "换算：冷路径外置若省 4 KB ⇒ 预期 %.1f ms（真噪声带 %s ms；布局敏感带 %.1f ms）\n",
               slope * 4, (noise < 0 ? "未测" : sprintf("%.1f", noise)), NA
        printf "判据（方案 §5）：slope ≥ 1.5 ms/KB 且省下的体量折算收益 > 真噪声带 ⇒ 值得做；"
        printf "  |slope|<0.3 ms/KB ⇒ 前端不是瓶颈，(a)/(b) 从方案里划掉\n"
      }
    }' "$OUTDIR/raw.txt"
else
  say ""
  say "SRC 里没有 main() 且未给 EXTRA ⇒ 只出体量与文本断言。要拿斜率请补齐："
  say "  EXTRA=\"<其余 .o/源文件 -lpthread -ldl -lm>\" MODE=sqlite ARG=/tmp/q.sh bash $0"
fi

say ""
say "aa_layout: OK（布局族语义等价已证）"
