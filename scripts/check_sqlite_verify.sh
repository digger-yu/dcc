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
#   C. 家族抽样（mmap*/wal*/session* + where/join/index）与 A 同一判据：整文件跑
#      完，`.res` 结果行与 `.sum` 汇总行逐字节对照 gcc。回答的是“quick.test 只按
#      失败名集合判定、没有逐字节对照”这个缺口，不是“quick 排除了这些家族”
#      （见下面 FAMILY_TESTS 上方的账）。
#   D. `--verify` 给不出判据的三个 testset（rtree/json/star）：不带 --verify 跑
#      到结束，比**步骤序列**与**最终数据库 .dump 内容哈希**，再对 dcc 产物做
#      `PRAGMA integrity_check`。导出工具是现场用 gcc 从同一份 amalgamation 建的
#      sqlite3 shell —— 不能用 dcc 的产物去验证 dcc。
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
# Part C：家族抽样。先纠正两笔容易写错的账：
#   * quick.test **并不整体排除** mmap*/wal* —— 它只排除个别重文件（mmap4.test、
#     walcrash*/walslow/walprotocol/walthread 等）与 sessionbig/sessionB。
#   * ext/session/session1..9 虽然在 allquicktests 名单里，但官方 testfixture 没编
#     -DSQLITE_ENABLE_SESSION，运行时在 `ifcapable !session` 处直接收尾 ⇒ 一个用例
#     也没执行。所以 session 家族要用另编的 session 版 fixture（见 Part C 里的
#     session_fixture_for），不能拿 base fixture 的“逐字节相同”冒充覆盖。
# 真正的缺口是 **quick.test 只按失败名集合判定**，没有逐字节结果行对照；这里给
# 这些家族与 select*.test 同一判据。where/join/index 是“按功能分组抽样”，回答
# “不止 5 个文件”的覆盖问题。
FAMILY_TESTS=${FAMILY_TESTS:-"test/mmap1 test/mmap2 test/mmap3 test/wal2 test/wal3 test/wal9 \
    test/where2 test/where3 test/join test/join5 test/index test/index2 \
    ext/session/session1 ext/session/session3"}
# Part D：--verify 给不出判据的三个 testset。不使用 --verify 的原因见 Part D 注释。
EXTRA_TESTSETS=${EXTRA_TESTSETS:-"rtree json star"}
ST_SIZE=${ST_SIZE:-10}
# 导出数据库内容用的 sqlite3 shell。优先系统命令；没有就现场用 gcc 构建一个
# （导出工具必须与 dcc 无关，否则是拿 dcc 验证 dcc）。
SQLITE_SHELL=${SQLITE_SHELL:-}
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

# ---- Part C：家族抽样（mmap/wal/session + where/join/index）------------------
say ""
say "=== C. 家族抽样：mmap*/wal*/session* + where/join/index（逐字节结果行）==="
say "    文件：$(echo $FAMILY_TESTS)"

# session 扩展是编译期开关，而官方 testfixture 的 TESTFIXTURE_FLAGS **没有**
# -DSQLITE_ENABLE_SESSION/-DSQLITE_ENABLE_PREUPDATE_HOOK（main.mk 只把这两个给了
# sqlite3 shell 的 SHELL_OPT）。所以 quick.test 虽然把 ext/session/*.test 列进了
# allquicktests，运行时却在 `ifcapable !session` 处直接 `finish_test` —— 一个用例都
# 没跑（本脚本实测：base fixture 下 session1 只有 1 个收尾用例、0 行结果）。
# 要真跑，只能按同一份对象集另编一个 session 版 fixture：重编 sqlite3.c（核心）、
# src/test_config.c（ifcapable 的选项表）、src/test_tclsh.c（TestSession_Init 的
# 调用点）、ext/session/test_session.c（TCL 命令），其余对象原样复用；gcc 参照与
# dcc 各档吃同一套 define，保持可比。代价是每档多一次 sqlite3.c 编译。
objdir_for() { # $1=tag ⇒ stdout 对象目录；rc≠0 找不到
    local r
    for r in "$SQ_RUN" "$PREREQ_OUT/run"; do
        [ -d "$r/obj-$1" ] && { echo "$r/obj-$1"; return 0; }
    done
    return 1
}

# 从 gcc 参照那次抓到 argv 里取 testfixture 的编译参数（`-o testfixture` 之前 + 尾部
# 的 -I/-D）。不能直接用 Part B 的 BASE_FLAGS：那份剔掉了 testfixture 钩子。
FIX_PRE=()
FIX_TAIL=()
parse_fixture_argv() {
    local -a A
    local i n oidx=-1
    mapfile -t A <"$SQ_RUN/argv-ref.txt" || return 1
    n=${#A[@]}
    for ((i = 0; i < n; i++)); do
        if [ "${A[$i]}" = "-o" ] && [ "${A[$((i + 1))]}" = "testfixture" ]; then
            oidx=$i
            break
        fi
    done
    [ "$oidx" -gt 0 ] || return 1
    for ((i = 0; i < oidx; i++)); do
        case ${A[$i]} in -O*) ;; *) FIX_PRE+=("${A[$i]}") ;; esac
    done
    for ((i = oidx + 2; i < n; i++)); do
        case ${A[$i]} in -I* | -D*) FIX_TAIL+=("${A[$i]}") ;; esac
    done
    return 0
}

SESSION_DEFS=(-DSQLITE_ENABLE_SESSION -DSQLITE_ENABLE_PREUPDATE_HOOK)

session_fixture_for() { # $1=tag $2=cc $3=优化级别 ⇒ stdout session 版 fixture
    local tag=$1 cc=$2 lvl=$3 base od bin log ccref cur src ccexec basereal stamp
    base=$(objdir_for "$tag") || return 1
    basereal=$(realpath "$base")
    od=$SQ_RUN/obj-session-$tag
    bin=$OUT/testfixture-session-$tag
    log=$OUT/build-session-$tag.log
    ccref=$cc
    [ -x "$cc" ] || ccref=$(command -v "$cc" 2>/dev/null)
    [ -n "$ccref" ] && cur=$(sha256sum "$ccref" | awk '{print $1}')
    # 指纹里带上对象目录：同一个 od 目录先后从 /tmp/sqlite_suite/run 与
    # /tmp/sqlite_objsbuild/run 取 base 时，平铺后的对象名不同，残留的旧 .o 会与
    # 新拷入的同类对象“同名不同路径”地在链接时撞成 multiple definition。
    stamp="$basereal ${cur:-} O$lvl v2"
    # 按编译器指纹失效（同 build_st1）：mtime 在“刚重编过 dcc”时恰好相反。
    if [ -x "$bin" ] && [ -f "$od/.stamp" ] && [ "$(cat "$od/.stamp")" = "$stamp" ]; then
        echo "$bin"
        return 0
    fi
    # 降权后编译器必须对 $DCC_USER 可执行：dcc 通常住在 /root 下（目录 0700），
    # 直接执行就是 Permission denied。与 check_sqlite_suite.sh 同策略：复制一份。
    ccexec=$ccref
    if [ "$(id -u)" -eq 0 ] && ! as_user "test -x '$ccref'" 2>/dev/null; then
        ccexec=$SQ_RUN/cc-session-$tag
        if ! cp -f "$ccref" "$ccexec" || ! chmod 755 "$ccexec"; then
            return 1
        fi
    fi
    # 先清掉旧 .o：换过 base 目录后平铺文件名会变，不清就是上一轮的残留。
    if ! as_user "rm -f '$od'/*.o; mkdir -p '$od' && cp -f '$base'/*.o '$od/' &&
        rm -f '$od'/*test_config.c.o '$od'/*test_tclsh.c.o '$od'/*test_session.c.o '$od'/sqlite3.c.o"; then
        return 1
    fi
    {
        echo '#!/bin/bash'
        echo 'set -e'
        echo "cd '$SQ_RUN/src'"
        for src in sqlite3.c src/test_config.c src/test_tclsh.c ext/session/test_session.c; do
            echo "timeout $TU_TIMEOUT '$ccexec' $(printf '%q ' "${FIX_PRE[@]}") -O$lvl -g $(printf '%q ' "${FIX_TAIL[@]}") ${SESSION_DEFS[*]} -c $src -o '$od/session-$(basename "$src").o'"
        done
    } >"$OUT/session-build-$tag.sh"
    chmod 755 "$OUT/session-build-$tag.sh"
    if ! as_user "bash '$OUT/session-build-$tag.sh'" >>"$log" 2>&1; then
        return 1
    fi
    if ! gcc -no-pie -o "$bin" "$od"/*.o -L/usr/lib/x86_64-linux-gnu \
        -ltcl8.6 -ldl -lz -lpthread -lm >>"$log" 2>&1; then
        return 1
    fi
    echo "$stamp" >"$od/.stamp"
    echo "$bin"
}

run_family() { # $1=fixture $2=tag ⇒ 每文件 .res（结果行）与 .sum（rc+汇总行）
    local bin=$1 tag=$2 f base log rc
    for f in $FAMILY_RUN; do
        base=${f//\//_}
        log=$OUT/$tag-$base.log
        as_user "cd '$SQ_RUN/src' && rm -f test.db* && timeout $SUITE_TIMEOUT '$bin' $f.test" \
            >"$log" 2>&1
        rc=$?
        grep -E '\.\.\. (Ok|FAILED|ERROR)' "$log" >"$OUT/$tag-$base.res"
        echo "$rc $(grep -oE '[0-9]+ errors out of [0-9]+ tests' "$log" | tail -1)" \
            >"$OUT/$tag-$base.sum"
    done
}

# 汇总行解析：`<rc> <n> errors out of <N> tests`。N 必须 >0，否则空结果行也会
# “逐字相同”；errors 必须为 0，否则两侧同错也照样同字节。
family_ok() { # $1=.sum ⇒ stdout N；rc≠0 = 该文件不构成判据
    local rc sum n
    read -r rc sum <"$1"
    n=$(sed -E 's/.*out of ([0-9]+) tests.*/\1/' <<<"$sum")
    [ "$rc" = 0 ] && [[ $sum == 0* ]] && [ "${n:-0}" -ge 1 ] || return 1
    echo "$n"
}

FAMILY_PLAIN=$(echo $FAMILY_TESTS | tr ' ' '\n' | grep -v '^ext/session/' | tr '\n' ' ')
FAMILY_SESSION=$(echo $FAMILY_TESTS | tr ' ' '\n' | grep '^ext/session/' | tr '\n' ' ')
declare -A REF_N=()

run_family_ref() { # $1=fixture $2=结果前缀
    local bin=$1 refp=$2 f base n
    run_family "$bin" "$refp"
    for f in $FAMILY_RUN; do
        base=${f//\//_}
        if ! n=$(family_ok "$OUT/$refp-$base.sum"); then
            read -r rr rsum <"$OUT/$refp-$base.sum"
            say "  gcc 参照 $f  rc=$rr $rsum ⇒ 参照自己没跑完，该文件不构成判据"
            fails=$((fails + 1))
            continue
        fi
        REF_N[$base]=$n
        say "$(printf '  %-12s %-26s %s' 'gcc' "$f.test" "$n tests, 0 errors")"
    done
}

check_family_level() { # $1=显示标签 $2=fixture $3=结果前缀 $4=参照前缀
    local label=$1 bin=$2 tag=$3 refp=$4 f base n rc sum
    run_family "$bin" "$tag"
    for f in $FAMILY_RUN; do
        base=${f//\//_}
        [ -n "${REF_N[$base]:-}" ] || continue
        if ! cmp -s "$OUT/$refp-$base.res" "$OUT/$tag-$base.res"; then
            say "  $label  $f  结果行与 gcc 不一致："
            diff "$OUT/$refp-$base.res" "$OUT/$tag-$base.res" | head -3 | sed 's/^/      /' | tee -a "$SUMMARY"
            fails=$((fails + 1))
            continue
        fi
        if ! n=$(family_ok "$OUT/$tag-$base.sum"); then
            read -r rc sum <"$OUT/$tag-$base.sum"
            say "  $label  $f  rc=$rc  $sum（参照 ${REF_N[$base]} tests）"
            fails=$((fails + 1))
            continue
        fi
        if [ "$n" != "${REF_N[$base]}" ]; then
            say "  $label  $f  用例数 $n ≠ 参照 ${REF_N[$base]}"
            fails=$((fails + 1))
            continue
        fi
        say "$(printf '  %-12s %-26s %s 行结果逐字相同 ✓  %s tests, 0 errors' "$label" \
            "$f.test" "$(wc -l <"$OUT/$refp-$base.res")" "$n")"
    done
}

if [ -z "$REF_TF" ]; then
    say "  C-PREREQ-FAIL: 没有 gcc 参照 testfixture ⇒ C 段无参照可用"
    fails=$((fails + 1))
else
    FAMILY_RUN=$FAMILY_PLAIN
    run_family_ref "$REF_TF" ref
    for L in $LEVELS; do
        tf=$(fixture_for "dcc-O$L") || true
        if [ -z "$tf" ]; then
            say "  dcc -O$L  C-PREREQ-FAIL: 补不出 obj-dcc-O$L / testfixture（详见 $OUT/prereq.log）"
            fails=$((fails + 1))
            continue
        fi
        check_family_level "dcc -O$L" "$tf" "dcc-O$L" ref
    done

    if [ -n "$FAMILY_SESSION" ]; then
        say "  -- ext/session：官方 fixture 未编入 session（ifcapable 直接跳过），另编 session 版 --"
        if ! parse_fixture_argv; then
            say "  C-SESSION-FAIL: 解析 $SQ_RUN/argv-ref.txt 失败，建不出 session 版 fixture"
            fails=$((fails + 1))
        else
            rsf=$(session_fixture_for ref gcc 2) || true
            if [ -z "$rsf" ]; then
                say "  C-SESSION-FAIL: 参照 session fixture 建不出来（见 $OUT/build-session-ref.log）"
                tail -3 "$OUT/build-session-ref.log" 2>/dev/null | sed 's/^/      /' | tee -a "$SUMMARY"
                fails=$((fails + 1))
            else
                FAMILY_RUN=$FAMILY_SESSION
                REF_N=()
                run_family_ref "$rsf" ref-session
                for L in $LEVELS; do
                    sf=$(session_fixture_for "dcc-O$L" "$DCC" "$L") || true
                    if [ -z "$sf" ]; then
                        say "  dcc -O$L  C-SESSION-FAIL: session 版 fixture 建不出来（见 $OUT/build-session-dcc-O$L.log）"
                        fails=$((fails + 1))
                        continue
                    fi
                    check_family_level "dcc -O$L" "$sf" "dcc-O$L-session" ref-session
                done
            fi
        fi
    fi
fi

# ---- Part D：rtree/json/star：步骤序列 + 数据库内容哈希 ---------------------
say ""
say "=== D. speedtest1：rtree/json/star（不用 --verify：步骤序列 + .dump 内容哈希）==="
say "    testsets=$EXTRA_TESTSETS  size=$ST_SIZE"

# 导出工具与 dcc 无关：优先调用方指定的 shell，否则用 gcc 从**同一份 amalgamation**
# 建一个。不能用 dcc 编的 shell 去导出 dcc 的库，那样哈希对得上也不说明问题。
ensure_shell() {
    local bin=$OUT/sqlite3-dump
    if [ -n "$SQLITE_SHELL" ]; then
        [ -x "$SQLITE_SHELL" ] && { echo "$SQLITE_SHELL"; return 0; }
        return 1
    fi
    if [ -x "$bin" ] && [ -f "$bin.stamp" ] && [ "$(cat "$bin.stamp")" = "$(sha256sum "$AMALG" | awk '{print $1}')" ]; then
        echo "$bin"; return 0
    fi
    if [ -f "$SQ_RUN/src/shell.c" ]; then
        if ! gcc -O1 -o "$bin" "$SQ_RUN/src/shell.c" "$AMALG" "${LIBDEFS[@]}" "${INCLUDES[@]}" \
            -lpthread -ldl -lm -lz >"$OUT/build-sqlite3-dump.log" 2>&1; then
            tail -3 "$OUT/build-sqlite3-dump.log" | sed 's/^/      /' | tee -a "$SUMMARY"
            return 1
        fi
        sha256sum "$AMALG" | awk '{print $1}' >"$bin.stamp"
        echo "$bin"; return 0
    fi
    command -v sqlite3 >/dev/null && { command -v sqlite3; return 0; }
    return 1
}

st1_plain() { # $1=exe $2=testset $3=库名（相对 $SQ_RUN/src） $4=日志 ⇒ 不带 --verify 跑完
    local exe=$1 ts=$2 db=$3 log=$4
    as_user "cd '$SQ_RUN/src' && rm -f '$db' '$db-journal' '$db-wal' '$db-shm' && timeout $RUN_TIMEOUT '$exe' '$db' --testset $ts --size $ST_SIZE" \
        >"$log" 2>&1
    return $?
}

# 步骤签名：保留 `%4d - 描述....`，去掉行尾耗时 `%4d.%03ds`。步数本身就是覆盖度，
# 序列相同才是“跑了同一批操作”。
st1_steps() { grep -E '^ *[0-9]+ - ' "$1" | sed -E 's/ *[0-9]+\.[0-9]{3}s$//'; }

dump_hash() { # $1=db $2=tag ⇒ stdout sha256；rc≠0 = 导出失败
    local db=$1 tag=$2
    local d=$OUT/dump-$tag.sql
    "$SHELL_BIN" "$db" .dump >"$d" 2>"$d.err" || return 1
    [ -s "$d" ] || return 1
    sha256sum "$d" | awk '{print $1}'
}

db_integrity() { # $1=db ⇒ stdout 首行（期望 ok）
    "$SHELL_BIN" "$1" 'PRAGMA integrity_check;' 2>/dev/null | head -1
}

SHELL_BIN=$(ensure_shell) || SHELL_BIN=""
if [ -z "$SHELL_BIN" ]; then
    say "  D-PREREQ-FAIL: 没有可用的 sqlite3 shell（SQLITE_SHELL / \$SQ_RUN/src/shell.c / 系统 sqlite3）⇒ 导不出内容哈希"
    fails=$((fails + 1))
else
    say "    导出工具：$SHELL_BIN（与 dcc 无关，只读库）"
    if ! build_st1 gcc 2 ref; then
        say "  D-REF-BUILD-FAIL: gcc 参照 speedtest1 构建失败（见 $OUT/build-ref.log）"
        fails=$((fails + 1))
    else
        for ts in $EXTRA_TESTSETS; do
            rdb="st1-$ts-ref.db"
            st1_plain "$OUT/st1-ref" "$ts" "$rdb" "$OUT/st1-$ts-ref.log"
            rrc=$?
            st1_steps "$OUT/st1-$ts-ref.log" >"$OUT/st1-$ts-ref.steps"
            rsteps=$(wc -l <"$OUT/st1-$ts-ref.steps")
            rhash=$(dump_hash "$SQ_RUN/src/$rdb" "$ts-ref") || rhash=""
            rint=$(db_integrity "$SQ_RUN/src/$rdb")
            if [ "$rrc" != 0 ] || [ "$rsteps" -lt 1 ] || [ -z "$rhash" ] || [ "$rint" != ok ]; then
                say "  gcc 参照 $ts: rc=$rrc 步数=$rsteps 哈希=${rhash:-无} integrity=${rint:-无} ⇒ 参照不可用，该 testset 不构成判据"
                fails=$((fails + 1))
                continue
            fi
            say "$(printf '  %-9s %-6s 步数=%-3s 内容哈希=%s  integrity=ok' 'gcc' "$ts" "$rsteps" "$rhash")"
            for L in $LEVELS; do
                if ! build_st1 "$DCC" "$L" "dcc-O$L"; then
                    say "  dcc -O$L  D-BUILD-FAIL: speedtest1 构建失败（见 $OUT/build-dcc-O$L.log）"
                    fails=$((fails + 1))
                    continue
                fi
                ddb="st1-$ts-dcc-O$L.db"
                st1_plain "$OUT/st1-dcc-O$L" "$ts" "$ddb" "$OUT/st1-$ts-dcc-O$L.log"
                rc=$?
                st1_steps "$OUT/st1-$ts-dcc-O$L.log" >"$OUT/st1-$ts-dcc-O$L.steps"
                steps=$(wc -l <"$OUT/st1-$ts-dcc-O$L.steps")
                if [ "$rc" != 0 ]; then
                    say "  dcc -O$L  $ts rc=$rc 非零（步数=$steps）"
                    fails=$((fails + 1))
                    continue
                fi
                if ! cmp -s "$OUT/st1-$ts-ref.steps" "$OUT/st1-$ts-dcc-O$L.steps"; then
                    say "  dcc -O$L  $ts 步骤序列与参照不同（$steps vs $rsteps）："
                    diff "$OUT/st1-$ts-ref.steps" "$OUT/st1-$ts-dcc-O$L.steps" | head -4 | sed 's/^/      /' | tee -a "$SUMMARY"
                    fails=$((fails + 1))
                    continue
                fi
                h=$(dump_hash "$SQ_RUN/src/$ddb" "$ts-dcc-O$L") || h=""
                int=$(db_integrity "$SQ_RUN/src/$ddb")
                if [ -z "$h" ] || [ "$h" != "$rhash" ]; then
                    say "  dcc -O$L  $ts 内容哈希不同：${h:-无} ≠ $rhash"
                    fails=$((fails + 1))
                    continue
                fi
                if [ "$int" != ok ]; then
                    say "  dcc -O$L  $ts integrity_check=$int（期望 ok）"
                    fails=$((fails + 1))
                    continue
                fi
                say "$(printf '  %-9s %-6s 步数=%-3s 与参照同序 ✓  内容哈希=%s ✓  integrity=ok' "dcc -O$L" "$ts" "$steps" "$h")"
            done
        done
    fi
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
