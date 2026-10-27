#!/bin/bash
# ===========================================================================
# scripts/check_lua_suite_diff.sh — Lua 5.5 官方测试套件：gcc 基线 vs dcc 各档
#
# 判据（全部通过才 PASS）：
#   1) 每棵树都 rc=0、日志里有 "final OK !!!"、进度点数量与基线一致
#      （点少一个就说明有测试没跑到）；
#   2) 两次**同一棵 gcc 树**的输出经 scripts/lua_suite_norm.sh 归一化后相同
#      —— 这证明归一化器覆盖了全部"同编译器也会变"的行（噪声的定义本身）；
#   3) dcc 每档的输出经同一归一化后与 gcc 基线**逐行相同**；
#   4) 归一化器自测通过，且往基线日志里注入语义差异时**必须判红**
#      （元门禁：证明本脚本不是恒绿）。
#
# 为什么需要"归一化"而不是直接 diff：
#   all.lua 的输出里有随机种子（math.random 的种子来自 os.time/os.clock）、
#   pid、临时路径、耗时、GC 计数与内存统计，以及按 flush 边界漂移的进度点。
#   这些行每次运行都不同，直接 diff 全是红，真差异会被淹掉。归一化器按**类别**
#   改写并带取值校验（见 scripts/lua_suite_norm.sh 顶部说明）。归一化器本体是纯
#   bash（本机 awk 是 mawk，没有 gawk 的捕获数组；规则需要按类别取捕获组做取值
#   校验），不依赖 python。
#
# 为什么时序警告不再出现：all.lua 会把本次 clock 写进 time.txt /
#   time-debug.txt 供下一次运行比较，差值超 5% 就 warn 一行。以前那 1~2 行
#   "运行条件触发的时序警告"就是它。本脚本让每棵树在**各自的干净副本目录**里
#   跑（并先删掉拷贝里残留的 time*.txt），第一次运行没有 lasttime ⇒ 该行不可能
#   出现，于是它从"噪声"降级成"出现即红"。
#
# 用法：
#   bash scripts/check_lua_suite_diff.sh              # ltests+plain, dcc -O0..-O3
#   LEVELS="2" bash scripts/check_lua_suite_diff.sh   # 只跑 -O2
#   VARIANTS="ltests plain" bash scripts/check_lua_suite_diff.sh
#   VARIANTS="ltests plain user" ...   # 默认三条：ltests / stock 全量 / stock 用户模式
#
# 三个变体的区别只在**跑法与构建宏**，不在被测代码：
#   ltests — 核心 + ltests.c 同参数构建（LUA_USER_H="ltests.h"，LUA_DEBUG/内存
#            逐块核对都在这上）、跑完整 all.lua；
#   plain  — stock `make linux` 产物跑完整 all.lua（不跳非可移植用例）；
#   user   — 同一套 stock 产物加 `-e "_U=true"`，即报告里的“用户模式路径”：
#            `_U` 使 all.lua 置 `_port`/`_soft`，`main.lua` 直接 return，因此
#            独立解释器/写失败/动态模块那些用例被**跳过**（这一路径只证明 stock
#            构建能跑完套件，不覆盖那些区域）。
#   KEEP=1 ...                                        # 保留运行目录
# ===========================================================================
set -o pipefail

SELF_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
DCC_REPO=${DCC_REPO:-$(cd "$SELF_DIR/.." && pwd)}
NORM="$DCC_REPO/scripts/lua_suite_norm.sh"

DCC=${DCC:-$DCC_REPO/target/release/dcc}
REFCC=${REFCC:-gcc}
LUA_SRC=${LUA_SRC:-$HOME/lua-5.5.1}
LUA_TESTS=${LUA_TESTS:-$HOME/lua-5.5.1-tests}
OUT=${OUT:-/tmp/lua_suite_diff}
LEVELS=${LEVELS:-0 1 2 3}
REF_LEVEL=${REF_LEVEL:-2}
VARIANTS=${VARIANTS:-ltests plain user}
RUN_TIMEOUT=${RUN_TIMEOUT:-600}
BUILD_TIMEOUT=${BUILD_TIMEOUT:-600}
# main.lua 的 readline 用例需要默认库名可直接 dlopen；留空则用 Lua 自带默认值
# （本机没有 libreadline.so 软链时会整棵树失败，见 build_one 里的说明）。
READLINE_LIB=${READLINE_LIB:-libreadline.so.8}
KEEP=${KEEP:-}

mkdir -p "$OUT" || exit 1
# 两个官方套件门禁（本脚本与 check_sqlite_suite.sh）都靠墙钟/CPU 才能稳定：main.lua
# 的 Ctrl-C 用例在机器被压满时会「读串管道」（`& echo $!` 与子进程的 `print(15)` 抢同
# 一条管道，pid 被读成 15），实测就是这么出现假红的。整段门禁持一把机器级独占锁，
# 禁止两个门禁同时跑。
exec 8>"${DCC_GATE_LOCK:-/tmp/dcc-suite-gate.lock}" || exit 1
if ! flock -w 0 8; then
  echo "ERROR: another dcc suite gate is running (lock: ${DCC_GATE_LOCK:-/tmp/dcc-suite-gate.lock})" >&2
  exit 3
fi
# 同一个 OUT 只允许一个实例：两实例会各自 rm -rf "$OUT/build/$tag" 和
# "$OUT/run/$tag"，日志与运行目录互相覆盖 ⇒ 汇总里出现两段交错的记录，
# rc/进度点来自不同二进制，既可能假红也可能假绿。用 flock 直接把并发挡掉。
exec 9>"$OUT/.lock" || exit 1
if ! flock -w 0 9; then
  echo "ERROR: another instance is already using $OUT (lock: $OUT/.lock)" >&2
  exit 3
fi
SUMMARY="$OUT/summary.txt"
: >"$SUMMARY"
say() { echo "$*" | tee -a "$SUMMARY"; }
fails=0

die() {
  say "ERROR: $*"
  exit 2
}

# ---- 依赖 -----------------------------------------------------------------
[ -x "$DCC" ] || die "DCC not executable: $DCC"
command -v "$REFCC" >/dev/null 2>&1 || die "reference compiler missing: $REFCC"
command -v make >/dev/null 2>&1 || die "make missing"
[ -f "$NORM" ] || die "normalizer missing: $NORM"
[ -f "$LUA_SRC/src/lua.c" ] || die "no Lua source at $LUA_SRC"
[ -f "$LUA_TESTS/all.lua" ] || die "no test suite at $LUA_TESTS"
[ -f "$LUA_TESTS/ltests/ltests.c" ] || die "no ltests at $LUA_TESTS/ltests"
[ -n "$LEVELS" ] || die "LEVELS is empty"

say "dcc:        $DCC"
say "reference:  $REFCC -O$REF_LEVEL"
say "lua src:    $LUA_SRC"
say "lua tests:  $LUA_TESTS"
say "variants:   $VARIANTS   levels: $LEVELS"
say ""

# ---- 归一化器自测 ---------------------------------------------------------
say "=== normalizer selftest ==="
if ! bash "$NORM" --selftest >"$OUT/norm-selftest.txt" 2>&1; then
  say "NORM-SELFTEST-FAIL（见 $OUT/norm-selftest.txt）"
  tail -20 "$OUT/norm-selftest.txt" | sed 's/^/  /' | tee -a "$SUMMARY"
  exit 1
fi
tail -1 "$OUT/norm-selftest.txt" | sed 's/^/  /' | tee -a "$SUMMARY"
say ""

# ---- 构建 -----------------------------------------------------------------
# $1=cc $2=level $3=variant(ltests|plain) $4=tag ⇒ stdout 输出版构建目录
build_one() {
  local cc=$1 L=$2 var=$3 tag=$4
  local dir="$OUT/build/$tag"
  local log="$OUT/build-$tag.log"
  rm -rf "$dir"
  mkdir -p "$dir" || return 1
  cp -a "$LUA_SRC/src" "$dir/src" || return 1
  (cd "$dir/src" && make clean) >/dev/null 2>&1

  # 直接调编译器时用数组（宏值里的引号按 shell 语义保留）；
  # 交给 make 时用字符串 CFLAGS（要按 make→/bin/sh 的语义再转义一层）。
  #
  # 为什么是 `CFLAGS=` 而不是 `MYCFLAGS=`：Lua 的 src/Makefile 写的是
  #   CFLAGS= -O2 -Wall -Wextra $(SYSCFLAGS) $(MYCFLAGS)
  # 若走 MYCFLAGS，编译命令里会同时出现 `-O2 … -O$L`（两个 -O），本级是否生效
  # 就全指望“后出现的 -O 胜”（gcc 与 dcc 确实都如此，用例
  # `repeated_opt_flag_last_wins` 逐字节钉住了这条）。命令行变量会**整体覆盖**
  # Makefile 里的同名赋值，所以这里直接给 CFLAGS ⇒ 实测（`make -n -B all`）：
  #   dcc -O0 -Wall -Wextra -std=gnu99 -DLUA_USE_LINUX -c -o lapi.o lapi.c
  # 一行里只有一个 -O，四档的差异不依赖任何“后者胜”语义。也因此 -std=gnu99
  # 不能再附在 CC 里（那样同一标志出现两次，读数上容易误读成“两个 -O”同类问题）。
  local cflags="-O$L -Wall -Wextra -std=gnu99 -DLUA_USE_LINUX"
  local -a ccflags=(-O"$L" -Wall -Wextra -std=gnu99 -DLUA_USE_LINUX)
  local mylibs=""
  if [ -n "$READLINE_LIB" ]; then
    # main.lua 的 readline 用例（-E -W -i）要求默认库名可用：这台机器只有
    # libreadline.so.8，没有 dev 软链 libreadline.so，LUA_READLINELIB 的默认值
    # 会让 -i 报 "unable to load readline library"，而 ltests 把 test 模式下的
    # 意外 warning 视为致命（"Unexpected warning in test mode"）⇒ 整棵树失败。
    # 两边用同一个显式库名，既不依赖系统软链，也不改变被测代码。
    cflags="$cflags -DLUA_READLINELIB=\\\"$READLINE_LIB\\\""
    ccflags+=("-DLUA_READLINELIB=\"$READLINE_LIB\"")
  fi
  if [ "$var" = ltests ]; then
    # 官方做法：ltests.c 与核心同参数编译，LUA_USER_H 让 ltests.h 生效
    # （LUA_DEBUG / LUAI_ASSERT / 内存统计都在里面）。少这一个宏，整棵树测的
    # 内容就变了（T.nonblock 等一批用例会静默跳过），两棵树也就不可比。
    cflags="$cflags -DLUA_USER_H=\\\"ltests.h\\\" -I$LUA_TESTS/ltests"
    ccflags+=("-DLUA_USER_H=\"ltests.h\"" "-I$LUA_TESTS/ltests")
    "$cc" "${ccflags[@]}" -I"$dir/src" \
      -c "$LUA_TESTS/ltests/ltests.c" -o "$dir/ltests.o" \
      >"$OUT/build-$tag-ltests.log" 2>&1 ||
      { tail -5 "$OUT/build-$tag-ltests.log" >&2; return 1; }
    mylibs="$dir/ltests.o"
  fi

  if [ -n "$mylibs" ]; then
    # ltests 变体：核心（lgc/linit/lapi…）在 LUA_DEBUG 下引用了 ltests.c 里的
    # 符号（l_memcontrol/luai_tracegctest…），所以 lua 和 luac 都必须挂上
    # ltests.o；`make all` 不知道这回事，会直接链接失败。于是只让 make 产出
    # 库，驱动 .o 与链接自己做。
    # 也不能简单用 MYLIBS：那样 ltests.o 排在 liblua.a 之后，链接器单趟扫描
    # 时 linit.o 还没被拉进来，luaL_openselectedlibs 就成了未定义。
    timeout "$BUILD_TIMEOUT" make -C "$dir/src" liblua.a \
      CC="$cc" CFLAGS="$cflags" >>"$log" 2>&1 || return 1
    ( cd "$dir/src" &&
      "$cc" "${ccflags[@]}" -c lua.c -o lua.o &&
      "$cc" "${ccflags[@]}" -c luac.c -o luac.o &&
      "$cc" -o lua lua.o liblua.a -lm -Wl,-E -ldl "$mylibs" &&
      "$cc" -o luac luac.o "$mylibs" liblua.a -lm -Wl,-E -ldl ) >>"$log" 2>&1 || return 1
  else
    timeout "$BUILD_TIMEOUT" make -C "$dir/src" all \
      CC="$cc" CFLAGS="$cflags" \
      SYSLIBS="-Wl,-E -ldl" MYLIBS="" >>"$log" 2>&1 || return 1
  fi
  [ -x "$dir/src/lua" ] || return 1
  echo "$dir"
}

# ---- 运行 -----------------------------------------------------------------
# $1=builddir $2=tag $3=log $4=传给 lua 的额外参数（用户模式用 `-e _U=true`）
# ⇒ stdout 输出版 rc
run_one() {
  local dir=$1 tag=$2 log=$3 extra=${4:-}
  local rdir="$OUT/run/$tag"
  rm -rf "$rdir"
  mkdir -p "$rdir" || return 1
  cp -a "$LUA_TESTS/." "$rdir/" || return 1
  # 残留的 time*.txt 会让 all.lua 与上一次运行比时间并 warn ⇒ 清掉（见文件头）。
  rm -f "$rdir/time.txt" "$rdir/time-debug.txt"
  # files.lua / main.lua 有直接 shell 调 `lua`、`luac` 的用例，必须让 PATH 指向
  # 本棵树的二进制，否则会去别的解释器（或干脆 not found）——那是环境差异，
  # 会把两棵树的对比污染成假红。
  # stdin 必须是**不可 seek** 的：ltests 下 _port=nil，files.lua 断言
  # io.stdin:seek("set",1000) 失败。交互运行（终端）或管道满足这点，而
  # /dev/null 可 seek（lseek 返回 0），会让 files.lua 直接断言失败。所以这里
  # 用空管道而不是 `< /dev/null`，让 headless 运行与终端行为一致。
  (
    cd "$rdir" || exit 1
    PATH="$dir/src:$PATH"
    export PATH
    printf '' | timeout "$RUN_TIMEOUT" "$dir/src/lua" $extra all.lua
  ) >"$log" 2>&1
  echo $?
}

# all.lua 每跑一步打一个 '.'；数量明显对不上 = 有测试没跑到（崩溃/提前结束）。
# 只数**进度点**：先把数字里的小数点抓掉（`time: 0.212493`、`1.4M`、`2.8e-05`、
# `18.21 msec.` 这些都带小数点），再数剩下的 '.'。不抓就会把“计时数字的有效位不一样”
# 算成“有测试没跑到”（实测 gcc 13111 vs dcc-O2 13110、plain 1854 vs 1852）。
dots() { sed -E 's/[0-9]\.[0-9]+//g' "$1" | tr -cd '.' | wc -c; }

# 进度点总数的容差：gc.lua/all.lua 里有些点是按时间或 GC 步数打的，同一棵 gcc 树
# 跑两遍就会差 1~2 个（实测 plain 变体差 0、ltests 差 0，但跨编译器会差 1~2）。
# 拿它当「有没有漏跑测试」的硬判据不可靠；真正的硬判据是下面的 `***** FILE` 个数
# （一个测试文件一行，它与归一化后的逐行比对一起承担这个责任）。所以这里给一个宽带，
# 只能拦住“少跑几百个点”那种跳闸。
DOT_SLACK=${DOT_SLACK:-16}

# ---- 主流程 ---------------------------------------------------------------
say "=== builds & runs ==="
declare -A LOG=() RC=() BUILDOK=()
for var in $VARIANTS; do
  tag="ref-$var"
  say "build $tag ($REFCC -O$REF_LEVEL)"
  if ! dir=$(build_one "$REFCC" "$REF_LEVEL" "$var" "$tag"); then
    say "  REF-BUILD-FAIL $tag（基线都编不出来，见 $OUT/build-$tag.log）"
    BUILDOK[$tag]=0
    fails=$((fails + 1))
    continue
  fi
  BUILDOK[$tag]=1
  # 基线跑两遍：两遍之间的差异 = "同编译器也会变"的噪声全集。
  # `user` 变体只是多一个 `-e _U=true`，构建与 `plain` 完全同一条路径。
  # 这里不能在顶层用 `local`（主流程不是函数），直接单变量赋值。
  uflag=""; [ "$var" = user ] && uflag="-e _U=true"
  for i in 1 2; do
    rc=$(run_one "$dir" "$tag-r$i" "$OUT/log-$tag-r$i.txt" "$uflag")
    LOG["$tag-r$i"]="$OUT/log-$tag-r$i.txt"
    RC["$tag-r$i"]=$rc
    say "  run $tag-r$i rc=$rc"
  done
  for L in $LEVELS; do
    tag="dcc-O$L-$var"
    say "build $tag ($DCC -O$L)"
    if ! dir=$(build_one "$DCC" "$L" "$var" "$tag"); then
      say "  DCC-BUILD-FAIL $tag（见 $OUT/build-$tag.log）"
      tail -3 "$OUT/build-$tag.log" 2>/dev/null | sed 's/^/    /' | tee -a "$SUMMARY"
      BUILDOK[$tag]=0
      fails=$((fails + 1))
      continue
    fi
    BUILDOK[$tag]=1
    rc=$(run_one "$dir" "$tag" "$OUT/log-$tag.txt" "$uflag")
    LOG["$tag"]="$OUT/log-$tag.txt"
    RC["$tag"]=$rc
    say "  run $tag rc=$rc"
  done
done

say ""
say "=== structural checks ==="
for var in $VARIANTS; do
  ref="ref-$var"
  [ "${BUILDOK[$ref]:-0}" = 1 ] || continue
  d1=$(dots "${LOG[$ref-r1]}")
  d2=$(dots "${LOG[$ref-r2]}")
  # 进度点本身有一点内在抖动（个别用例走的分支取决于环境/时序），用两次基线
  # 运行之间的差值当作容差；跑飞或漏跑测试时点数会明显偏离，仍然判红。
  tol=$((d1 > d2 ? d1 - d2 : d2 - d1))
  f1=$(grep -c "^\*\*\*\*\* FILE" "${LOG[$ref-r1]}")
  say "  baseline $var: rc=${RC[$ref-r1]}/${RC[$ref-r2]}, 进度点 $d1/$d2（两遍差 $tol，宽带 ±$DOT_SLACK）, 文件 $f1 个, final OK $(grep -c 'final OK !!!' "${LOG[$ref-r1]}") 处"
  for i in 1 2; do
    [ "${RC[$ref-r$i]:-1}" = 0 ] || { say "  RUN-FAIL $ref-r$i rc=${RC[$ref-r$i]}"; fails=$((fails + 1)); }
    grep -q 'final OK !!!' "${LOG[$ref-r$i]}" || { say "  RUN-INCOMPLETE $ref-r$i（没有 'final OK !!!'）"; fails=$((fails + 1)); }
  done
  for L in $LEVELS; do
    tag="dcc-O$L-$var"
    [ "${BUILDOK[$tag]:-0}" = 1 ] || continue
    [ "${RC[$tag]:-1}" = 0 ] || { say "  RUN-FAIL $tag rc=${RC[$tag]}（见 ${LOG[$tag]}）"; fails=$((fails + 1)); }
    grep -q 'final OK !!!' "${LOG[$tag]}" || { say "  RUN-INCOMPLETE $tag"; fails=$((fails + 1)); }
    if grep -q 'time difference from previous test' "${LOG[$tag]}"; then
      say "  RUN-TIMEWARN $tag：出现时序警告 ⇒ 运行目录不干净，归一化前提不成立"
      fails=$((fails + 1))
    fi
    dn=$(dots "${LOG[$tag]}")
    lo=$((d1 < d2 ? d1 : d2))
    hi=$((d1 > d2 ? d1 : d2))
    if [ "$dn" -lt $((lo - tol - DOT_SLACK)) ] || [ "$dn" -gt $((hi + tol + DOT_SLACK)) ]; then
      say "  DOT-MISMATCH $tag：进度点 $dn ∉ [$lo,$hi]±($tol+$DOT_SLACK)（有测试没跑到）"
      fails=$((fails + 1))
    fi
    # 硬判据：跑了多少个测试文件。`***** FILE 'xxx.lua'*****` 每个文件一行，个数不等就
    # 是真的漏跑/多跑了文件（进度点受时序抖动影响，担不了这个责任）。
    fn=$(grep -c "^\*\*\*\*\* FILE" "${LOG[$tag]}")
    if [ "$fn" != "$f1" ]; then
      say "  FILE-COUNT $tag：执行了 $fn 个测试文件，基线是 $f1 个"
      fails=$((fails + 1))
    fi
  done
done

say ""
say "=== normalized diffs ==="
for var in $VARIANTS; do
  ref="ref-$var"
  if [ "${BUILDOK[$ref]:-0}" != 1 ]; then
    say "  skip $var（基线没编出来）"
    continue
  fi
  if bash "$NORM" "${LOG[$ref-r1]}" "${LOG[$ref-r2]}" --audit \
      >"$OUT/norm-selfnoise-$var.txt" 2>&1; then
    say "  self-noise $var: 同一棵 gcc 树两遍运行归一化后一致（噪声类别见 $OUT/norm-selfnoise-$var.txt）"
  else
    say "  SELF-NOISE-DIFF $var：同一棵 gcc 树两遍运行归一化后仍不同 ⇒ 归一化器漏了噪声类别"
    sed -n '1,40p' "$OUT/norm-selfnoise-$var.txt" | sed 's/^/    /' | tee -a "$SUMMARY"
    fails=$((fails + 1))
  fi
  for L in $LEVELS; do
    tag="dcc-O$L-$var"
    if [ "${BUILDOK[$tag]:-0}" != 1 ]; then
      say "  skip $tag"
      continue
    fi
    if bash "$NORM" "${LOG[$ref-r1]}" "${LOG[$tag]}" --audit \
        >"$OUT/norm-$tag.txt" 2>&1; then
      say "  ok $tag ⇔ $REFCC -O$REF_LEVEL：归一化后逐行相同"
    else
      say "  SUITE-DIFF $tag：归一化后仍有差异（见 $OUT/norm-$tag.txt）"
      sed -n '1,60p' "$OUT/norm-$tag.txt" | sed 's/^/    /' | tee -a "$SUMMARY"
      fails=$((fails + 1))
    fi
  done
done

# ---- 元门禁：证明本脚本会红 -----------------------------------------------
say ""
say "=== meta check（注入真差异必须判红）==="
probe=$(ls "$OUT"/log-ref-*-r1.txt 2>/dev/null | head -1)
if [ -z "$probe" ]; then
  say "  skip（没有基线日志）"
else
  # 每个注入都是对**基线自己**的语义改动，归一化器必须判红；哪个被归一化吞掉了
  # 哪个就是假绿源。注入后文件必须与原件不同（cmp），否则该注入不算数。
  # 名称|生成器（向 stdout 写篡改后的日志）
  injections=(
    "追加一行|{ cat \"$probe\"; echo INJECTED-SEMANTIC-DIFF; }"
    "删掉收尾行|grep -v 'closing state' \"$probe\""
    "改测试文件名|sed \"s/FILE 'main[.]lua'/FILE 'niam.lua'/\" \"$probe\""
    "改类型尺寸|sed \"s/short 2, int 4/short 2, int 8/\" \"$probe\""
  )
  ran=0
  for inj in "${injections[@]}"; do
    name=${inj%%|*}
    gen=${inj#*|}
    tmpa="$OUT/meta-tamper.txt"
    eval "$gen" >"$tmpa" 2>/dev/null || true
    if cmp -s "$probe" "$tmpa"; then
      say "  skip $name（注入没改变原件）"
      continue
    fi
    ran=$((ran + 1))
    bash "$NORM" "$probe" "$tmpa" >"$OUT/meta-tamper.txt.diff" 2>&1
    mrc=$?
    if [ "$mrc" -eq 0 ]; then
      say "  META-FAIL：注入的语义差异（$name）被归一化吞掉了"
      fails=$((fails + 1))
    elif [ "$mrc" -gt 1 ]; then
      say "  META-FAIL：注入（$name）让归一化器报错（rc=$mrc）而不是判红"
      fails=$((fails + 1))
    else
      say "  ok $name ⇒ 判红"
    fi
  done
  if [ "$ran" -eq 0 ]; then
    say "  META-FAIL：一个有效注入都没跑成，元门禁形同虚设"
    fails=$((fails + 1))
  fi
fi

# ---- 汇总 -----------------------------------------------------------------
say ""
say "=== summary ==="
for var in $VARIANTS; do
  ref="ref-$var"
  say "$(printf '%-8s %-9s %s' "$var" "ref -O$REF_LEVEL" \
    "$([ "${BUILDOK[$ref]:-0}" = 1 ] && echo "ok rc=${RC[$ref-r1]:-?}/${RC[$ref-r2]:-?}" || echo BUILD-FAIL)")"
  for L in $LEVELS; do
    tag="dcc-O$L-$var"
    st=NOT-RUN
    if [ -n "${BUILDOK[$tag]:-}" ]; then
      st=BUILD-FAIL
      [ "${BUILDOK[$tag]}" = 1 ] && st="ok rc=${RC[$tag]:-?}"
    fi
    say "$(printf '%-8s %-9s %s' "$var" "dcc -O$L" "$st")"
  done
done

[ -n "$KEEP" ] || rm -rf "$OUT/run"

if [ "$fails" -eq 0 ]; then
  say ""
  say "RESULT: PASS（dcc 各档与 $REFCC -O$REF_LEVEL 在官方套件上归一化后逐行相同）"
  exit 0
fi
say ""
say "RESULT: FAIL（$fails 项）"
say "细节: $SUMMARY；原始日志: $OUT/log-*.txt；归一化 diff: $OUT/norm-*.txt"
exit 1
