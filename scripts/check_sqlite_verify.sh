#!/bin/bash
# ===========================================================================
# scripts/check_sqlite_verify.sh — SQLite 的「可完整跑完」子集验证 + speedtest1 内容哈希对拍
#
# 为什么单独有这一个脚本：`check_sqlite_suite.sh` 跑的是官方 quick.test 全量，
# 当前dcc 在套件中途会崩（docs §4.4），于是**全量跑不完**，能完整跑完的证据必须
# 单独取。本脚本做两件全量给不了的事：
#
#   A. select1~5.test 逐文件跑完（testfixture，TCL 格式），与 gcc 参照比
#      「用例结果行」逐字相同 + `0 errors out of N tests` + rc=0。
#   B. speedtest1 --verify 的 **Verification Hash**（对全部查询结果的内容哈希）
#      dcc 各档 vs gcc 参照必须同号；顺带 best-of-N 墙钟，供报告的性能一节引用。
#      负载由上游 speedtest1 提供，不再用自写的 20 万行 SQL。
#
# 两条口径上的老实话（别把读数读过头）：
#   * `sqllogictest` 不在 SQLite 发布包里（是独立项目），本地也没有；`select*.test`
#     是 **TCL** 格式，只有 testfixture 认它。所以上游那条路线这里用 testfixture 跑。
#   * 上游 3.54.0 的测试树里没有 `test/evidence/` 目录，也没有 `evid*.test`
#     （`find -iname '*evid*'` 为空）⇒ 那一项无从执行，不是被跳过。
#   * `-verify` 是 speedtest1 的 `--verify`；`main.mk` 里的 `src-verify` /
#     `verify-source` 是**源码完整性**检查，跟查询结果无关，不能混为一谈。
#
# 用法：
#   LEVELS="0 2" bash scripts/check_sqlite_verify.sh
# 前置（由 check_sqlite_suite.sh 产出，缺了会直接点名）：
#   $SQ_RUN/src            已 configure 的源码树（含生成的 sqlite3.c amalgamation）
#   $SQ_RUN/argv-ref.txt   gcc 参照那次抓到的真实编译参数（本脚本沿用以保持同配置）
#   $SQ_RUN/obj-dcc-O$L    各档的 testfixture 对象集
# 约束：限 CPU（逐 TU 串行编译、单条 timeout）、降权到 sqltest（SQLite 拒绝以 root 跑
# 测试）、绝不把产物写到 /dev/null。
# ===========================================================================
set -u
# EPOCHREALTIME 的小数点随 LC_NUMERIC 变，计时靠 `${EPOCHREALTIME/./}` 去掉它；
# 不固定 locale 的话在非 C 环境下整段计时都是错的。
LC_ALL=C
export LC_ALL

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO=$(cd "$HERE/.." && pwd)
DCC=${DCC:-$REPO/target/release/dcc}
SQ_RUN=${SQ_RUN:-/tmp/sqlite_suite/run}
WORK=${WORK:-/tmp/sqlite_verify}
# 补齐缺档对象集用的独立输出目录：与 SQ_RUN 必须分开（套件脚本开头会 rm -rf 自己
# 那个 $OUT/run，同目录嵌套会互相抹掉前置）。
PREREQ_OUT=${PREREQ_OUT:-/tmp/sqlite_objsbuild}
DCC_USER=${DCC_USER:-sqltest}
LEVELS=${LEVELS:-0 2}
SELECT_TESTS=${SELECT_TESTS:-"select1 select2 select3 select4 select5"}
# 有内容哈希且代价可接受的 testset。两条剔除理由都是量出来的，不是猜的：
#   * `json` / `star` 只出**空哈希**（两者同为 `0 0e12171d…`）⇒ 列进去等于没有判据；
#   * `rtree` 配 `--verify` 的第 111 步（复查 20000 次 intersect 切片查询）连 **gcc 参照
#     也跑不完**：实测两个二进制同过 150 s 未返回（rc=124）⇒ 那是上游负载本身的性
#     态，不是 dcc 特有。需要它时用 `TESTSETS=main,rtree RUN_TIMEOUT=1800` 显式拉开。
TESTSETS=${TESTSETS:-main,cte,orm,fp}
RUN_TIMEOUT=${RUN_TIMEOUT:-300}
ITER=${ITER:-3}
TU_TIMEOUT=${TU_TIMEOUT:-900}
SUITE_TIMEOUT=${SUITE_TIMEOUT:-600}

# 硬保护：套件脚本第一件事是 `rm -rf $OUT/run`，若 PREREQ_OUT 指到 $SQ_RUN 的父目录
# 就会把父脚本自己依赖的前置一并删掉（注释约束不算约束，这里让它不可能发生）。
if [ "$(realpath -m "$PREREQ_OUT/run")" = "$(realpath -m "$SQ_RUN")" ]; then
    echo "PREREQ_OUT=$PREREQ_OUT 与 SQ_RUN=$SQ_RUN 重叠：补齐会 rm -rf 掉本轮要用的前置，拒绝运行" >&2
    exit 2
fi

say() { echo "$@" | tee -a "$SUMMARY"; }

[ -n "${BASH_VERSINFO:-}" ] && [ "${BASH_VERSINFO[0]}" -ge 4 ] || {
    echo "需要 bash >= 4" >&2
    exit 1
}
command -v runuser >/dev/null || { echo "缺 runuser（需要以 $DCC_USER 身份跑测试）" >&2; exit 1; }

# ---- 机器级互斥：与 check_sqlite_suite.sh 同一把锁，避免同时压 CPU -----------
# 变量名必须与套件脚本一致（DCC_GATE_LOCK）：两边各用一个名字，谁覆盖了其中一个就
# 不再互斥，而两边的注释都写着“同一把锁”。
LOCKF=${DCC_GATE_LOCK:-${DCC_VERIFY_LOCK:-/tmp/dcc-suite-gate.lock}}
exec 8>"$LOCKF" || exit 1
if ! flock -w 0 8; then
    echo "另一个 SQLite 验证/套件任务正持锁（$LOCKF），先停一个再跑" >&2
    exit 3
fi

OUT=$WORK
SUMMARY=$WORK/summary.txt
mkdir -p "$OUT"
: >"$SUMMARY"

fails=0
[ -x "$DCC" ] || { echo "找不到 dcc：$DCC（先 cargo build --release）" >&2; exit 1; }
# 编译器指纹：本轮所有对象集/哈希/计时只能对应这一个二进制。全跑完再对一次，
# 中途被 cargo 重写（release 构建会原地替换 target/release/dcc）就当场判红。
DCC_SHA=$(sha256sum "$DCC" | awk '{print $1}')
[ -d "$SQ_RUN/src" ] || { echo "缺源码树 $SQ_RUN/src，请先跑 scripts/check_sqlite_suite.sh" >&2; exit 1; }
[ -f "$SQ_RUN/argv-ref.txt" ] || { echo "缺 $SQ_RUN/argv-ref.txt（gcc 参照的真实编译参数），请先跑 check_sqlite_suite.sh" >&2; exit 1; }
AMALG=$SQ_RUN/src/sqlite3.c
[ -f "$AMALG" ] || { echo "缺 amalgamation $AMALG（configure 树里应已生成）" >&2; exit 1; }
S1_SRC=$(ls "$SQ_RUN"/../src-orig/test/speedtest1.c "$SQ_RUN"/src/../test/speedtest1.c \
    /root/sqlite-src-3540000/test/speedtest1.c 2>/dev/null | head -1)
[ -n "$S1_SRC" ] && [ -f "$S1_SRC" ] || { echo "找不到 test/speedtest1.c" >&2; exit 1; }

# 沿用套件那次的定义，另加 RTREE（否则 speedtest1 的 rtree testset 直接报错）；
# 去掉 testfixture 专有的 -DTCLSH_INIT_PROC / -DSQLITE_TEST / -DSQLITE_CRASH_TEST，
# 因为 speedtest1 是独立程序，链接时没有那些测试钩子。
#
# **必须剔除 -O 与链接器选项**：本级优化号由调用方的 `-O$L` 唯一决定。留着参照那
# 次的 `-O2` 拼在后面，就成了「两个 -O 后者胜」⇒ 四档实际全部编成 -O2（本仓的 Lua
# 侧刚因为同类问题补了 §2 的说明与 `repeated_opt_flag_last_wins` 用例）。
# 也要剔除 `SQLITE_CKSUMVFS_STATIC` / `SQLITE_STATIC_RANDOMJSON`：那两个把注册入口
# 改成「由调用方显式调 `sqlite3_register_*`」，而 speedtest1 不链 `ext/misc/cksumvfs.c`
# 与 `ext/misc/random.c` ⇒ 直接 undefined reference（实测 ld 报的就是这一条）。
mapfile -t BASE_FLAGS < <(grep -E '^-' "$SQ_RUN/argv-ref.txt" |
    grep -vE '^-c$|^-o$|^-O|^-Wl,' |
    grep -vE 'TCLSH_INIT_PROC|SQLITE_TEST=|SQLITE_CRASH_TEST' |
    grep -vE 'SQLITE_CKSUMVFS_STATIC|SQLITE_STATIC_RANDOMJSON')
LIBDEFS=(-DSQLITE_ENABLE_RTREE -DSQLITE_ENABLE_JSON1)
INCLUDES=(-I"$SQ_RUN/src" -I"$SQ_RUN/src/src")
LINKLIBS=(-lm -lpthread -ldl -lz)

as_user() {
    # 只掉 `BASH_ENV`/`ENV`（root 的 bashrc 钩子会污染降权后的行为），保留 PATH；
    # 不能用 `env -i`：那会连 PATH 一起清掉，降权后的 `cd`/`rm` 直接找不到。
    runuser -u "$DCC_USER" -- env -u BASH_ENV -u ENV sh -c "$1"
}

run_st1() { # $1=exe $2=落盘路径  ⇒ 带 timeout 地跑一次 --verify（超时算失败，不无限等）
    as_user "cd '$SQ_RUN/src' && rm -f speedtest1.db* && timeout $RUN_TIMEOUT '$1' --verify --testset $TESTSETS --memdb :memory:" \
        >"$2" 2>&1
    return $?
}

fixture_for() { # $1=ref|dcc-O0… ⇒ stdout 该档 testfixture 路径；rc≠0 = 真失败
    local tag=$1 r objd bin
    bin=$OUT/testfixture-$tag
    # 先看两个根目录：上一轮套件留下的 $SQ_RUN，以及本脚本为补缺档另建的
    # $PREREQ_OUT/run。（必须分开：check_sqlite_suite.sh 开头会 `rm -rf $OUT/run`，
    # 往同一个 OUT 里嵌套调用会把别的档刚建好的对象集一并抹掉。）
    for r in "$SQ_RUN" "$PREREQ_OUT/run"; do
        if [ -x "$r/testfixture-$tag" ]; then
            echo "$r/testfixture-$tag"
            return 0
        fi
        objd=$r/obj-$tag
        [ -d "$objd" ] || continue
        # 每次都重新链：缓存链接产物（$OUT/testfixture-$tag）跨轮复用 = 可能拿旧对象集
        # 链出来的二进制冒充本轮证据；链接本身只要一两秒，不值得为它留一个陈旧面。
        rm -f "$bin"
        # 链接一律用 host gcc：测试钩子与 libtcl 都是宿主库，dcc 只需负责被检验的 .o
        if ! gcc -no-pie -o "$bin" "$objd"/*.o -L/usr/lib/x86_64-linux-gnu \
            -ltcl8.6 -ldl -lz -lpthread -lm >"$OUT/link-$tag.log" 2>&1; then
            echo ""
            return 1
        fi
        echo "$bin"
        return 0
    done
    echo ""
    return 1
}

ensure_prereqs() {
    # 缺哪几档就在**独立 OUT** 里一次补齐（BUILD_ONLY 只建不跑套件）。
    # 不在这里把缺档当成 SKIP：“四档”是正文里的承诺，缺了就得自己补出来，
    # 而不是把覆盖度降成两档还在总结里写一行“本轮未跑”。
    #
    # “已满足”只能按 $SQ_RUN 判：被调用的 check_sqlite_suite.sh 第一件事就是
    # `rm -rf $PREREQ_OUT/run`，把落在那里的档抹掉。若把 $PREREQ_OUT 也算作
    # “已满足”，那一档既不会被重建、又会被删掉 ⇒ 回到 Part A 反而找不到。
    local L missing="" need_ref=0
    for L in $LEVELS; do
        if [ ! -d "$SQ_RUN/obj-dcc-O$L" ] && [ ! -x "$SQ_RUN/testfixture-dcc-O$L" ]; then
            missing="$missing $L"
        fi
    done
    if [ ! -d "$SQ_RUN/obj-ref" ] && [ ! -x "$SQ_RUN/testfixture-ref" ]; then
        need_ref=1
    fi
    if [ -n "$missing" ] || [ "$need_ref" = 1 ]; then
        say "  前置补齐：dcc 缺档 =${missing:-无} 参照缺=$need_ref ⇒ 现场建对象集（OUT=$PREREQ_OUT，不动 $SQ_RUN）"
        # DCC_GATE_LOCK_HELD=1：本进程已持机器级锁，子进程不得重复抢（同文件 flock
        # 会死等，父又在等子 ⇒ 自锁）。BUILD_REF=0 时子进程不重建参照档（省一次整树
        # 复制 + configure + 88 TU），LEVELS 为空只补参照这条路也放行了。
        DCC_GATE_LOCK_HELD=1 BUILD_ONLY=1 BUILD_REF="$need_ref" OUT="$PREREQ_OUT" LEVELS="${missing# }" \
            bash "$HERE/check_sqlite_suite.sh" >>"$OUT/prereq.log" 2>&1 ||
            say "  注意：补齐返回非 0（详见 $OUT/prereq.log），下面缺什么会逐项点名"
    fi
}

# ---- Part A：select1~5.test 跑完并与 gcc 参照逐行比 -------------------------
say "=== A. select*.test（testfixture，TCL 格式）==="
say "    dcc: $DCC"
say "    sha256: $DCC_SHA"
say "    文件：$SELECT_TESTS"
ensure_prereqs

run_tests() { # $1=fixture $2=tag  ⇒ 每文件一份结果行/汇总行落盘
    local bin=$1 tag=$2 f log rc
    for f in $SELECT_TESTS; do
        log=$OUT/$tag-$f.log
        as_user "cd '$SQ_RUN/src' && rm -f test.db* && timeout $SUITE_TIMEOUT '$bin' test/$f.test" \
            >"$log" 2>&1
        rc=$?
        grep -E '\.\.\. (Ok|FAILED|ERROR)' "$log" >"$OUT/$tag-$f.res"
        echo "$rc $(grep -oE '[0-9]+ errors out of [0-9]+ tests' "$log" | tail -1)" \
            >"$OUT/$tag-$f.sum"
    done
}

REF_TF=$(fixture_for ref) || true
if [ -z "$REF_TF" ]; then
    say "  A-PREREQ-FAIL: gcc 参照 testfixture 连 BUILD_ONLY 也没能产出 ⇒ A 段无参照可用"
    fails=$((fails + 1))
else
    run_tests "$REF_TF" ref
    for L in $LEVELS; do
        tf=$(fixture_for "dcc-O$L")
        if [ $? -ne 0 ] || [ -z "$tf" ]; then
            say "  dcc -O$L  A-PREREQ-FAIL: 补不出 obj-dcc-O$L / testfixture（详见 $OUT/prereq.log）"
            fails=$((fails + 1))
            continue
        fi
        run_tests "$tf" "dcc-O$L"
        for f in $SELECT_TESTS; do
            if ! cmp -s "$OUT/ref-$f.res" "$OUT/dcc-O$L-$f.res"; then
                say "  dcc -O$L  $f  结果行与 gcc 不一致："
                diff "$OUT/ref-$f.res" "$OUT/dcc-O$L-$f.res" | head -3 | sed 's/^/      /' | tee -a "$SUMMARY"
                fails=$((fails + 1))
                continue
            fi
            read -r rc sum <"$OUT/dcc-O$L-$f.sum"
            # “0 errors out of N tests” 里的 N 必须 >0：两侧都没跑到任何用例时，
            # 空结果行也会逐字相同（0 errors out of 0 tests 同样以 0 开头）。
            n=$(sed -E 's/.*out of ([0-9]+) tests.*/\1/' <<<"$sum")
            if [ "$rc" != 0 ] || [[ $sum != 0* ]] || [ "${n:-0}" -lt 1 ]; then
                say "  dcc -O$L  $f  rc=$rc  $sum（参照 $(cut -d' ' -f2 "$OUT/ref-$f.sum")）"
                fails=$((fails + 1))
                continue
            fi
            say "$(printf '  %-9s %-14s %s 行结果逐字相同 ✓  %s' "dcc -O$L" "$f.test" \
                "$(wc -l <"$OUT/ref-$f.res")" "$sum")"
        done
    done
fi

# ---- Part B：speedtest1 --verify 内容哈希对拍 + best-of-N 墙钟 --------------
say ""
say "=== B. speedtest1（上游负载，--verify 出内容哈希）==="
say "    testset=$TESTSETS  迭代=best-of-$ITER  负载源=$S1_SRC"

build_st1() { # $1=cc $2=level $3=name
    local cc=$1 L=$2 name=$3 odir=$OUT/obj-$3 stamp=$OUT/.st1-$3.cc ccref cur
    mkdir -p "$odir"
    # 缓存按**编译器指纹**失效，不按 mtime：刚重编过 dcc 时，旧产物的 mtime 一定比
    # 新编译器“新”，拿 mtime 当判据会直接复用旧二进制（=拿旧证据下结论）。
    ccref=$cc
    [ -x "$cc" ] || ccref=$(command -v "$cc" 2>/dev/null)
    [ -n "$ccref" ] && cur=$(sha256sum "$ccref" | awk '{print $1}')
    if [ -x "$OUT/st1-$3" ] && [ -f "$stamp" ] && [ "$(cat "$stamp")" = "${cur:-}" ]; then
        return 0
    fi
    rm -f "$OUT/st1-$3" "$stamp"
    ( cd "$SQ_RUN/src" &&
        timeout "$TU_TIMEOUT" "$cc" -O$L -g "${BASE_FLAGS[@]}" "${LIBDEFS[@]}" "${INCLUDES[@]}" \
            -c sqlite3.c -o "$odir/sqlite3.o" &&
        timeout "$TU_TIMEOUT" "$cc" -O$L -g "${BASE_FLAGS[@]}" "${LIBDEFS[@]}" "${INCLUDES[@]}" \
            -c "$S1_SRC" -o "$odir/speedtest1.o" &&
        gcc -no-pie -o "$OUT/st1-$3" "$odir"/*.o "${LINKLIBS[@]}" ) \
        >"$OUT/build-$3.log" 2>&1 || return 1
    echo "${cur:-unknown}" >"$stamp"
}

# 哈希抽取：`Verification Hash: <n> <sha>`
st1_hash() { grep -oE 'Verification Hash: [0-9]+ [0-9a-f]+' "$1" | tail -1 | awk '{print $3, $4}'; }
st1_errs() { grep -cE 'SQL error|MISMATCH' "$1"; }

best_ms_st1() { # $1=exe $2=tag  ⇒ best-of-N 墙钟 ms（整段进程时间，不读它自己的 TOTAL）
    local best=-1 i s e ms rc
    for ((i = 1; i <= ITER; i++)); do
        s=$((10#${EPOCHREALTIME/./}))
        run_st1 "$1" "$OUT/timing-$2-$i.out"
        # 先取 rc 再做任何赋值：`e=$((...))` 会把 $? 改成 0，超时就再也不会被认出来。
        rc=$?
        e=$((10#${EPOCHREALTIME/./}))
        if [ "$rc" -ne 0 ]; then
            # 失败/超时样本不是有效计时：失败样本往往更快，混进 best 会把数字压低（假快）
            echo "RUN-FAIL(rc=$rc)"
            return 1
        fi
        ms=$(((e - s) / 1000))
        ((best < 0 || ms < best)) && best=$ms
    done
    echo "$best"
}

if ! build_st1 gcc 2 ref; then
    # 不是 SKIP：参照都建不出来就没有 B 段判据，直接计失败，不降级成“本轮未跑”
    say "  B-REF-BUILD-FAIL: gcc 参照 speedtest1 构建失败（见 $OUT/build-ref.log）"
    tail -3 "$OUT/build-ref.log" | sed 's/^/      /' | tee -a "$SUMMARY"
    fails=$((fails + 1))
else
    reflog=$OUT/st1-ref-run.out
    run_st1 "$OUT/st1-ref" "$reflog"
    ref_rc=$?
    refhash=$(st1_hash "$reflog")
    say "$(printf '  %-14s rc=%s  SQL错误=%s  %s' "gcc -O2 参照" "$ref_rc" "$(st1_errs "$reflog")" "${refhash:-无哈希}")"
    [ "$ref_rc" = 0 ] && [ -n "$refhash" ] || { say "  参照本身不可用 ⇒ B 无判据"; fails=$((fails + 1)); }
    for L in $LEVELS; do
        if ! build_st1 "$DCC" "$L" "dcc-O$L"; then
            say "  dcc -O$L  B-BUILD-FAIL: speedtest1 构建失败（见 $OUT/build-dcc-O$L.log）"
            tail -3 "$OUT/build-dcc-O$L.log" | sed 's/^/      /' | tee -a "$SUMMARY"
            fails=$((fails + 1))
            continue
        fi
        lg=$OUT/st1-dcc-O$L-run.out
        run_st1 "$OUT/st1-dcc-O$L" "$lg"
        rc=$?
        h=$(st1_hash "$lg")
        if [ "$rc" != 0 ]; then
            say "  dcc -O$L  rc=$rc 非零，哈希=$( [ -n "$h" ] && echo "$h" || echo 无 )"
            grep -E 'SQL error|MISMATCH' "$lg" | head -2 | sed 's/^/      /' | tee -a "$SUMMARY"
            fails=$((fails + 1))
            continue
        fi
        if [ "$h" != "$refhash" ]; then
            say "  dcc -O$L  内容哈希与参照不同：$h ≠ $refhash"
            fails=$((fails + 1))
            continue
        fi
        ms=$(best_ms_st1 "$OUT/st1-dcc-O$L" "dcc-O$L") || true
        if ! [[ $ms =~ ^[0-9]+$ ]]; then
            say "  dcc -O$L  哈希已对拍通过，但计时不可用（$ms）"
            fails=$((fails + 1))
            continue
        fi
        say "$(printf '  %-9s -O%-2s  步骤行 %s 条与参照同源 ✓  哈希=%s  best-of-%s=%s ms' "dcc" "$L" \
            "$(grep -cE '^ ?[0-9]+ - ' "$lg")" "$h" "$ITER" "$ms")"
    done
    ms=$(best_ms_st1 "$OUT/st1-ref" ref) || true
    say "$(printf '  %-9s -O%-2s  参照计时 best-of-%s=%s ms' "gcc" 2 "$ITER" "$ms")"
fi

say ""
say "=== summary ==="
say "    dcc sha256（开跑前）= $DCC_SHA"
now_sha=$(sha256sum "$DCC" | awk '{print $1}')
say "    dcc sha256（收尾时）= $now_sha"
if [ "$now_sha" != "$DCC_SHA" ]; then
    say "  COMPILER-CHANGED: 跑的过程中 $DCC 被重写了，本轮证据不来自同一个编译器 ⇒ 必须重跑"
    fails=$((fails + 1))
fi
if [ "$fails" = 0 ]; then
    say "RESULT: PASS"
else
    say "RESULT: FAIL ($fails issue(s))"
fi
exit "$fails"
