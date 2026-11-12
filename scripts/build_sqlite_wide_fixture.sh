#!/bin/bash
# ===========================================================================
# scripts/build_sqlite_wide_fixture.sh — 造一份「宽能力」testfixture（FTS3/4/5、STAT4…）
#
# 为什么需要它：
#   `check_sqlite_allfiles.sh` 用官方 make 的编译参数建 testfixture，那份参数里没有
#   -DSQLITE_ENABLE_FTS3 / STAT4 / SNAPSHOT 等。于是 test/fts3aa.test 这类文件在
#   `ifcapable !fts3 { finish_test; return }` 处直接收尾：汇总行仍写
#   `0 errors out of 1 tests`（finish_test 自己算一个伪用例），但**一个真实断言都没执行**。
#   逐文件对拍对它们等于空跑，只能记 NO-CAP。本脚本把这批文件所依赖的能力打开，让
#   它们真的跑起来，再交给 check_sqlite_allfiles.sh 同一套判据对拍。
#
# 怎么造（为什么不是整树重编 88 个 TU）：
#   能力开关只影响 sqlite3.c 这一个 TU（sqlite3_compileoption_get 也来自它）。所以
#   复用 check_sqlite_suite.sh 留下的对象集，只把 sqlite3.c 用额外 -D 重编一遍，
#   再重新链接。其余 87 个 TU（test1.c、tclsqlite.c、fts3_term.c…）与参照那轮逐字节
#   相同，既不扩大变量面，也把构建代价从「88 TU × 2」压到「1 TU × 2」。
#
# 产物目录与 $SQ_RUN 同构，可直接喂给逐文件对拍脚本：
#   $OUT/run/src -> $SQ_RUN/src（符号链接，不复制源码树）
#   $OUT/run/argv-ref.txt（副本，供脚本做前置校验）
#   $OUT/run/testfixture-ref、testfixture-dcc-O$L
#   $OUT/summary.txt（记录 dcc sha，逐文件脚本据此拒绝跨编译器复用）
#
# 用法：
#   bash scripts/build_sqlite_wide_fixture.sh
#   OUT=/tmp/sqlite_wide LEVELS="2" bash scripts/build_sqlite_wide_fixture.sh
#   EXTRA_DEFS="-DSQLITE_ENABLE_FTS3 -DSQLITE_ENABLE_FTS4" bash scripts/build_sqlite_wide_fixture.sh
#
# 之后：
#   SQ_RUN=/tmp/sqlite_wide/run OUT=/tmp/sqlite_allfiles_wide \
#     FILES="$(cat /tmp/sqlite_allfiles/wide-files.txt)" bash scripts/check_sqlite_allfiles.sh
# ===========================================================================
set -o pipefail
LC_ALL=C
export LC_ALL

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO=$(cd "$HERE/.." && pwd)
DCC=${DCC:-$REPO/target/release/dcc}
REFCC=${REFCC:-gcc}
SQ_RUN=${SQ_RUN:-/tmp/sqlite_suite/run}
OUT=${OUT:-/tmp/sqlite_wide}
LEVELS=${LEVELS:-2}
DCC_USER=${DCC_USER:-sqltest}
BUILD_TIMEOUT=${BUILD_TIMEOUT:-3600}
# 默认打开的能力，与 test/ 下 174 个 NO-CAP 文件的 ifcapable 依赖对齐（见
# docs/dcc-lua-sqlite-build-verify-report.md §7）：
#   fts3/fts4        → 94 个 fts*.test（唯一的整族空白）
#   stat4            → analyze*.test 一族
#   snapshot/rtree/scanstatus/unlock_notify → snapshot*/wherelimit/scanstatus 等
# 不列 ICU：需要额外链接 -licuuc，本机没装 ICU 头文件。
# 不列 UPDATE_DELETE_LIMIT：amalgamation 生成时就得带这个宏，事后 -D 只会让它静默
# 失效（解析器里没有那段语法）。
# 不列 SCANSTATUS：它的宏叫 SQLITE_ENABLE_STMT_SCANSTATUS，而 ifcapable 里写的
# 是 scanstatus（对应 PRAGMA compile_options 的 ENABLE_STMT_SCANSTATUS）；只涉及
# scanstatus1/2 两个文件，本轮不动它。
# 不列 FTS5：它把 sqlite3.c 再撑大一圈，dcc -O2 单 TU 实测 RSS 6.8G 被 OOM 杀掉
# （dmesg: Killed process dcc, anon-rss 6822920kB）；而 94 个 fts*.test 里没有任何
# 一个只靠 fts5 能力才能跑（它们要的是 fts3/fts4），把 FTS5 加进来只增加构建风险。
EXTRA_DEFS=${EXTRA_DEFS:-"-DSQLITE_ENABLE_FTS3 -DSQLITE_ENABLE_FTS3_PARENTHESIS \
-DSQLITE_ENABLE_FTS4 -DSQLITE_ENABLE_STAT4 -DSQLITE_ENABLE_SNAPSHOT \
-DSQLITE_ENABLE_RTREE -DSQLITE_ENABLE_UNLOCK_NOTIFY"}

say() { echo "$*"; }
die() { echo "ERROR: $*" >&2; exit 2; }

# ---- 机器级互斥：与三个套件脚本同一把锁 -------------------------------------
# 本脚本要长时间压 CPU（sqlite3.c 单 TU 峰值 RSS ~2.7G），与其他验证任务并发会把
# recoverfault/timeout 一类用例的墙钟判据污染成假红。
if [ -z "${DCC_GATE_LOCK_HELD:-}" ]; then
    exec 8>"${DCC_GATE_LOCK:-/tmp/dcc-suite-gate.lock}" || exit 1
    if ! flock -w 0 8; then
        echo "ERROR: another dcc suite gate is running（${DCC_GATE_LOCK:-/tmp/dcc-suite-gate.lock}）" >&2
        exit 3
    fi
fi

[ -x "$DCC" ] || die "DCC not executable: $DCC"
for L in $LEVELS; do
    case $L in '' | *[!0-9]*) die "bad level '$L' (want 0-9)" ;; esac
done

# ---- 前置 ----------------------------------------------------------------
SRC=$SQ_RUN/src
[ -d "$SRC" ] || die "no $SQ_RUN/src — run scripts/check_sqlite_suite.sh first"
[ -f "$SQ_RUN/argv-ref.txt" ] || die "no $SQ_RUN/argv-ref.txt — run scripts/check_sqlite_suite.sh first"
[ -f "$SRC/sqlite3.c" ] || die "no $SRC/sqlite3.c (configure 生成的 amalgamation)"
DCC_SHA=$(sha256sum "$DCC" | awk '{print $1}')
suite_sha=$(sed -n 's/.*compiler:.*sha256=\([0-9a-f]\{64\}\).*/\1/p' "$SQ_RUN/../summary.txt" 2>/dev/null | head -1)
if [ -n "$suite_sha" ] && [ "$suite_sha" != "$DCC_SHA" ]; then
    die "对象集来自 dcc sha256=$suite_sha，当前 $DCC 是 $DCC_SHA；先用同一编译器重跑 check_sqlite_suite.sh"
fi

# ---- 从 argv-ref.txt 还原编译参数 -----------------------------------------
# 与 check_sqlite_suite.sh 里的 build-tu.sh 同一套解析：`-o testfixture` 之前是编译
# 主参数，之后是链接尾部；尾部的 -I/-D 也要参与编译（TCL_INCLUDE_SPEC 就在那里）。
mapfile -t A <"$SQ_RUN/argv-ref.txt" || die "cannot read argv-ref.txt"
oidx=-1
for i in "${!A[@]}"; do
    if [ "${A[$i]}" = "-o" ] && [ "${A[$((i + 1))]}" = "testfixture" ]; then oidx=$i; break; fi
done
[ "$oidx" -ge 0 ] || die "argv-ref.txt 里找不到 '-o testfixture'"
FLAGS=("${A[@]:0:oidx}")
INCDEFS=()
for ((i = oidx + 2; i < ${#A[@]}; i++)); do
    case ${A[$i]} in -I* | -D*) INCDEFS+=("${A[$i]}") ;; esac
done
# 链接库与 check_sqlite_verify.sh 的 fixture_for 逐字一致（同一台机器上只有这一种链法）。
LINKLIBS=(-L/usr/lib/x86_64-linux-gnu -ltcl8.6 -ldl -lz -lpthread -lm)

RUN=$OUT/run
mkdir -p "$RUN" || die "cannot create $RUN"
ln -sfn "$SRC" "$RUN/src"
cp -f "$SQ_RUN/argv-ref.txt" "$RUN/argv-ref.txt"
say "=== 宽能力 testfixture：$EXTRA_DEFS"

# 能力可见性检查：只认 `PRAGMA compile_options`（ifcapable 自己也是从这查的）。
# 不能用 sqlite3_compileoption_used —— 它是 C API，testfixture 里没有同名 TCL 命令
# （实测 "invalid command name"，四个能力全报缺的假红）。
check_caps() { # $1=fixture
    local bin=$1 opts miss="" cap
    opts=$(printf 'set d [sqlite3 db_mem :memory:]\nputs [join [db_mem eval {PRAGMA compile_options}] " "]\n' |
        "$bin" 2>/dev/null)
    for cap in FTS3 FTS4 STAT4 SNAPSHOT RTREE UNLOCK_NOTIFY; do
        case " $opts " in *" ENABLE_$cap "*) ;; *) miss="$miss $cap" ;; esac
    done
    [ -z "$miss" ] || { say "  缺能力:$miss"; return 1; }
    say "  能力检查: FTS3/FTS4/STAT4/SNAPSHOT/RTREE/UNLOCK_NOTIFY 均可用"
}

# 链接用的源文件清单（与 build-tu.sh 同一解析：`-o testfixture` 之后的 *.c）。
SRCS=()
for ((i = oidx + 2; i < ${#A[@]}; i++)); do
    case ${A[$i]} in *.c) SRCS+=("${A[$i]}") ;; esac
done
[ "${#SRCS[@]}" -gt 0 ] || die "argv-ref.txt 里没有源文件"

# 受能力宏影响的 TU：源码文本里出现任一宏的。必须现算而不是只重编 sqlite3.c ——
# ext/fts3/fts3_term.c 这类文件在能力关闭时编成**空对象**，只重编 sqlite3.c 就会在
# 链接期报 undefined reference to `sqlite3Fts3InitTerm`（已踩过）。
cap_re=""
for d in $EXTRA_DEFS; do
    m=${d#-D}
    m=${m%%=*}
    cap_re=${cap_re:+$cap_re|}$m
done
AFFECTED=()
for src in "${SRCS[@]}"; do
    # argv 里的源文件两种形态都有：相对 $SRC 的（sqlite3.c）与绝对路径的（test1.c）。
    # 直接拼 "$SRC/$src" 会把绝对路径拼成不存在的位置，整个 AFFECTED 只剩相对那几个，
    # 漏编的文件只能在链接期以 undefined reference 报警。
    p=$src
    case $p in /*) ;; *) p=$SRC/$src ;; esac
    [ -f "$p" ] || continue
    grep -qE "$cap_re" "$p" && AFFECTED+=("$src")
done
aff_names=""
for src in "${AFFECTED[@]}"; do aff_names="$aff_names ${src##*/}"; done
say "  受能力宏影响的 TU: ${#AFFECTED[@]} / ${#SRCS[@]}（${aff_names# }）"

# 建一档：$1=ref|dcc-O$L  $2=cc  $3=额外 -O
build_one() {
    local tag=$1 cc=$2 opt=$3 src_obj objdir src o
    src_obj=$SQ_RUN/obj-$tag
    [ -d "$src_obj" ] || { say "  缺对象集 $src_obj（先跑 check_sqlite_suite.sh BUILD_ONLY=1）"; return 1; }
    objdir=$RUN/obj-$tag
    # 必须是整份复制而不是硬链接：编译器 -o 会原地截断目标，硬链接会把参照那套
    # 对象集一并改写（那正是后面要做对比的基线）。
    rm -rf "$objdir"
    cp -a "$src_obj" "$objdir" || return 1
    : >"$OUT/build-$tag.log"
    for src in "${AFFECTED[@]}"; do
        o="$objdir/${src//\//_}.o"
        say "  [$tag] 重编 ${src##*/}（$cc $opt）..."
        # $opt 必须排在 FLAGS 之后：argv-ref.txt 里已经带了 `-O2 -g`，同一命令行上
        # 后出现的 -O 才生效（本仓 Lua 侧踩过“两个 -O 后者胜”这个坑）。
        # /usr/bin/time -v 把峰值 RSS 记进日志：dcc 的内存随 TU 规模涨得很快，
        # 下一轮要调能力集合时，这一行是唯一的硬读数。
        if ! ( cd "$SRC" && /usr/bin/time -v timeout "$BUILD_TIMEOUT" "$cc" \
                "${FLAGS[@]}" "${INCDEFS[@]}" $EXTRA_DEFS $opt -c "$src" -o "$o" ) \
                >>"$OUT/build-$tag.log" 2>&1; then
            say "  [$tag] $src 编译失败，见 $OUT/build-$tag.log"
            tail -5 "$OUT/build-$tag.log" | sed 's/^/    /'
            return 1
        fi
    done
    if ! gcc -no-pie -o "$RUN/testfixture-$tag" "$objdir"/*.o "${LINKLIBS[@]}" \
            >"$OUT/link-$tag.log" 2>&1; then
        say "  [$tag] 链接失败，见 $OUT/link-$tag.log"
        tail -5 "$OUT/link-$tag.log" | sed 's/^/    /'
        return 1
    fi
    say "  [$tag] $RUN/testfixture-$tag $(stat -c%s "$RUN/testfixture-$tag") bytes，峰值 RSS: $(sed -n 's/.*Maximum resident set size (kbytes): //p' "$OUT/build-$tag.log" | sort -n | tail -1) kB"
}

fails=0
build_one ref "$REFCC" "" || fails=$((fails + 1))
for L in $LEVELS; do
    build_one "dcc-O$L" "$DCC" "-O$L" || fails=$((fails + 1))
done

# summary.txt 供 check_sqlite_allfiles.sh 做编译器指纹（格式与套件脚本一致）。
{
    echo "wide fixture build: $EXTRA_DEFS"
    echo "compiler: $DCC sha256=$DCC_SHA"
    echo "source:   $SQ_RUN"
} >"$OUT/summary.txt"

if [ "$fails" -ne 0 ]; then
    say "RESULT: FAIL ($fails 档构建失败)"
    exit 1
fi
if [ -x "$RUN/testfixture-ref" ]; then
    check_caps "$RUN/testfixture-ref" || fails=$((fails + 1))
fi
say ""
say "用法：SQ_RUN=$RUN OUT=/tmp/sqlite_allfiles_wide FILES=\"...\" bash scripts/check_sqlite_allfiles.sh"
[ "$fails" -eq 0 ] && { say "RESULT: PASS"; exit 0; }
say "RESULT: FAIL"
exit 1
