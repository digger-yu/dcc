#!/bin/bash
# ===========================================================================
# scripts/check_sqlite_allfiles.sh — SQLite 测试树逐文件全量对拍（gcc 参照 vs dcc）
#
# 为什么需要这个脚本：
#   `check_sqlite_suite.sh` 跑官方 quick.test，判据是**失败名集合**（dcc 的失败
#   集合里不得出现 gcc 没有的名字），并不逐文件对照完整结果行；`check_sqlite_verify.sh`
#   只对 19 个代表文件（select1~5、mmap/wal/where/join/index 抽样、session1/3）
#   做逐字节对照。测试树里其余 1 177 个 `.test` 文件因此只有“失败名”级覆盖。
#   本脚本把 `test/*.test` 里的文件**逐个独立进程**跑完，给每个文件同一判据：
#   归一化结果行逐字相同 + `0 errors out of N tests` + N 相同 + rc=0。
#
# 判据与口径：
#   * 基线 = gcc -O2 的 testfixture（$SQ_RUN/testfixture-ref），dcc 档 =
#     testfixture-dcc-O$L。两边的 fixture 必须由同一份 argv 构建（由
#     check_sqlite_suite.sh 保证），且 fixture 的编译器 sha 要与当前 $DCC 一致：
#     否则就是拿旧编译器的产物当本轮证据，开跑前直接拒绝。
#   * 只判「gcc 参照自己干净跑完」的文件：ref rc=0、`0 errors out of N tests`、
#     N>=1。gcc 自己就失败、崩溃、超时或一个用例都没跑（能力关闭）的文件不构成
#     dcc 判据 —— 这与 quick.test 的失败名集合口径一致，避免把环境缺陷记成 dcc。
#   * gcc 有失败的文件（如 zipfile.test 的已知 1 error）不整文件丢弃：仍比较
#     失败名集合，dcc 出现**参照里没有的**失败名、或参照能跑完而 dcc 崩溃/超时，
#     照样记候选缺陷。只是「dcc 也失败同一批名字」不判红。
#   * 结果行必须归一化后再比：testfixture 的通过行是 `name... Ok`，失败行是
#     `name...`（没有 Ok，详情在 `! expected/got` 行）；少数性能用例把实测毫秒
#     写进行内，如 `name... (9 ms - want less than 1000.0)  Ok`。两档二进制的
#     毫秒必然不同，逐字比原始行会假红。这里统一成 `name Ok` / `name FAIL` /
#     `name FAILED` / `name ERROR` 再逐字节比；失败详情由 errors 计数约束。
#   * 可疑文件先重跑一次 gcc 与 dcc 复核（RETRY=1）：两遍都复现才记 CONFIRMED，
#     只复现一遍记 FLAKY。测试里存在时间/随机相关的用例，不能拿单次差异定性。
#   * 每文件一个独立进程 + 独立超时（FILE_TIMEOUT，默认 180 s）。测试若 fork
#     子 testfixture（lock*/wal*/crash* 那族从 `./testfixture` 启动），每轮先把
#     `$SRC/testfixture` 指向本轮 fixture；超时后按 fixture 路径清理残留子进程。
#   * 运行身份：SQLite 拒绝以 root 跑部分权限用例（attach.test 直接 exit 1），
#     所以与另外两个套件脚本一样降权到 $DCC_USER，并显式检查 /dev/null 可写。
#
# 被排除的“runner”文件（它们自己不定义用例，而是再跑一遍别的文件）：
#   all quick veryquick full extraquick permutations memleak soak fts3 mallocAll
#   rtree session rbu —— 与 SQLite 自己的 alltests 排除表一致。
#
# 用法：
#   bash scripts/check_sqlite_allfiles.sh                    # -O2 全树
#   LEVELS="0 2" bash scripts/check_sqlite_allfiles.sh
#   FILES="test/fts5.test test/btree01.test" bash scripts/check_sqlite_allfiles.sh
#   FILE_TIMEOUT=900 RETRY=0 bash scripts/check_sqlite_allfiles.sh
#
# 前置（由 check_sqlite_suite.sh 产出）：
#   $SQ_RUN/src                 已 configure 的源码树
#   $SQ_RUN/argv-ref.txt        gcc 参照那次抓到的真实编译参数
#   $SQ_RUN/testfixture-ref     gcc -O2 参照 fixture
#   $SQ_RUN/testfixture-dcc-O$L 各档 dcc fixture
#
# 输出：默认 $OUT=/tmp/sqlite_allfiles
#   summary.txt                 汇总（含逐项缺陷候选与读数）
#   results.tsv                 每文件一行：档位/文件/ref 与 dcc 的 rc·errors·tests/耗时/结论
#   log/<tag>-<file>.log        原始日志
#   res/<tag>-<file>.res        归一化结果行
#   sum/<tag>-<file>.sum        `rc errors out of N tests`（缺汇总行记 NO-SUMMARY）
#
# 退出码：0 无 dcc 新增失败；1 有确认的 dcc 失败；2 前置不满足；3 抢不到锁
# ===========================================================================
set -o pipefail
# EPOCHREALTIME 的小数点随 LC_NUMERIC 变，计时靠 `${EPOCHREALTIME/./}` 去掉它；
# 不固定 locale 的话在非 C 环境下整段计时都是错的。
LC_ALL=C
export LC_ALL

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO=$(cd "$HERE/.." && pwd)
DCC=${DCC:-$REPO/target/release/dcc}
SQ_RUN=${SQ_RUN:-/tmp/sqlite_suite/run}
OUT=${OUT:-/tmp/sqlite_allfiles}
DCC_USER=${DCC_USER:-sqltest}
# ext/session/*.test 需要另编的 session 版 fixture（官方 testfixture 没编
# -DSQLITE_ENABLE_SESSION，会在 `ifcapable !session` 处直接收尾）。两份都
# 由 check_sqlite_verify.sh 的 Part C 产出，这里只在 FILES 含 ext/session 时用。
SESSION_DIR=${SESSION_DIR:-/tmp/sqlite_verify}
LEVELS=${LEVELS:-2}
FILE_TIMEOUT=${FILE_TIMEOUT:-180}
RETRY=${RETRY:-1}
FILES=${FILES:-}
RUNNERS="all quick veryquick full extraquick permutations memleak soak fts3 mallocAll rtree session rbu"

mkdir -p "$OUT/log" "$OUT/res" "$OUT/sum" || exit 1
SUMMARY="$OUT/summary.txt"
: >"$SUMMARY"
say() { echo "$*" | tee -a "$SUMMARY"; }
fails=0

die() {
    say "ERROR: $*" >&2
    exit 2
}

# ---- 机器级互斥 -----------------------------------------------------------
# 与 check_sqlite_suite.sh / check_sqlite_verify.sh 同一把锁：套件里 recoverfault/
# timeout 一类用例对机器负载敏感，两边同时跑会把对方的基线污染成假红。
if [ -z "${DCC_GATE_LOCK_HELD:-}" ]; then
    exec 8>"${DCC_GATE_LOCK:-/tmp/dcc-suite-gate.lock}" || exit 1
    if ! flock -w 0 8; then
        echo "ERROR: another dcc suite gate is running (${DCC_GATE_LOCK:-/tmp/dcc-suite-gate.lock})" >&2
        exit 3
    fi
fi
# 同一个 OUT 只允许一个实例：两实例会互相覆盖 log/res，汇总里出现交错读数。
exec 9>"$OUT/.lock" || exit 1
if ! flock -w 0 9; then
    echo "ERROR: another instance is already using $OUT (lock: $OUT/.lock)" >&2
    exit 3
fi

# ---- 前置检查 -------------------------------------------------------------
[ -x "$DCC" ] || die "DCC not executable: $DCC"
command -v runuser >/dev/null 2>&1 || die "runuser missing (util-linux); SQLite must not run as root"
id "$DCC_USER" >/dev/null 2>&1 || die "user $DCC_USER missing (create it or set DCC_USER)"
SRC=$SQ_RUN/src
[ -d "$SRC/test" ] || die "no $SRC/test — run scripts/check_sqlite_suite.sh first"
[ -f "$SQ_RUN/argv-ref.txt" ] || die "no $SQ_RUN/argv-ref.txt — run scripts/check_sqlite_suite.sh first"
[ -x "$SQ_RUN/testfixture-ref" ] || die "no $SQ_RUN/testfixture-ref — run scripts/check_sqlite_suite.sh first"

# 编译器指纹：fixture 是在 check_sqlite_suite.sh 那轮用某个 dcc 二进制编出来的，
# 之后若 cargo 重编过 release，本轮读到的 dcc sha 就与对象集/产物不一致。用套件
# 汇总里记录的 sha 对一次，不一致直接拒绝（与 verify 脚本同一纪律）。
DCC_SHA=$(sha256sum "$DCC" | awk '{print $1}')
suite_sum=$SQ_RUN/../summary.txt
suite_sha=$(sed -n 's/.*compiler:.*sha256=\([0-9a-f]\{64\}\).*/\1/p' "$suite_sum" 2>/dev/null | head -1)
if [ -n "$suite_sha" ] && [ "$suite_sha" != "$DCC_SHA" ]; then
    die "fixtures were built by dcc sha256=$suite_sha, current $DCC is $DCC_SHA; rebuild fixtures (check_sqlite_suite.sh BUILD_ONLY=1) before this sweep"
fi

runuser -u "$DCC_USER" -- test -w /dev/null ||
    die "/dev/null is not writable by $DCC_USER (a tool used '-o /dev/null' and recreated the node); chmod 666 /dev/null"

# ---- 列表归一 + fixture 校验 ---------------------------------------------
split_list() { # $1 = 逗号/分号/空白混合的列表 → stdout = 空格分隔、保序去重
    local raw=$1 x out="" seen=""
    set -f
    raw=${raw//,/ }
    raw=${raw//;/ }
    for x in $raw; do
        case " $seen " in *" $x ") continue ;; esac
        seen="$seen $x"
        out="$out $x"
    done
    set +f
    echo "${out# }"
}

LEVELS=$(split_list "$LEVELS")
[ -n "$LEVELS" ] || die "LEVELS is empty"
for L in $LEVELS; do
    case $L in '' | *[!0-9]*) die "bad level '$L' (want 0-9)" ;; esac
done

REF_TF=$SQ_RUN/testfixture-ref
for L in $LEVELS; do
    [ -x "$SQ_RUN/testfixture-dcc-O$L" ] ||
        die "missing $SQ_RUN/testfixture-dcc-O$L — run check_sqlite_suite.sh LEVELS='$LEVELS'"
done
# session 文件用另一套 fixture；缺了就在真正跑到 ext/session 时点名，
# 不因为默认 test/*.test 清单里没有它而强制要求前置。
NEED_SESSION=0
case " $FILES " in *"ext/session/"*) NEED_SESSION=1 ;; esac
if [ "$NEED_SESSION" = 1 ]; then
    [ -x "$SESSION_DIR/testfixture-session-ref" ] ||
        die "missing $SESSION_DIR/testfixture-session-ref — run check_sqlite_verify.sh (Part C) first"
    for L in $LEVELS; do
        [ -x "$SESSION_DIR/testfixture-session-dcc-O$L" ] ||
            die "missing $SESSION_DIR/testfixture-session-dcc-O$L — run check_sqlite_verify.sh LEVELS='$LEVELS' first"
    done
fi

# 每个文件用哪套 fixture：ext/session 用 session 版，其余用普通版。
pick_fixture() { # $1=文件 $2=ref|dcc-O$L
    case $1 in
        ext/session/*) echo "$SESSION_DIR/testfixture-session-$2" ;;
        *) echo "$SQ_RUN/testfixture-$2" ;;
    esac
}

# ---- 文件清单 -------------------------------------------------------------
# 默认 test/*.test 去掉 runner。FILES 显式给出时原样使用（可带 test/ 前缀）。
if [ -n "$FILES" ]; then
    FILES=$(split_list "$FILES")
else
    FILES=""
    for p in "$SRC"/test/*.test; do
        b=$(basename "$p" .test)
        case " $RUNNERS " in *" $b "*) continue ;; esac
        FILES="$FILES test/$b.test"
    done
fi
[ -n "$FILES" ] || die "no test files selected"
NFILES=0
for f in $FILES; do NFILES=$((NFILES + 1)); done

say "=== SQLite 全树逐文件对拍（testfixture，TCL 格式）==="
say "    dcc: $DCC"
say "    sha256: $DCC_SHA"
say "    ref: $REF_TF"
say "    档位: $LEVELS   文件数: $NFILES   单文件超时: ${FILE_TIMEOUT}s   复核: $RETRY"
say "    gcc 参照口径: rc=0 且 '0 errors out of N tests'（N>=1）才算该文件的判据"

# ---- 运行原语 -------------------------------------------------------------
as_user() { runuser -u "$DCC_USER" -- env -u BASH_ENV -u ENV sh -c "$1"; }

base_of() { echo "${1//\//_}"; }

# 归一化结果行。原始形态（testfixture 的 tester.tcl）：
#   name... Ok
#   name...                                  ← 失败，详情在后续 `! expected/got` 行
#   name... (9 ms - want less than 1000.0)  Ok
# 归一为 `name Ok|FAILED|ERROR|FAIL`。失败详情不进 .res：同一个失败在两侧的
# expected/got 文本可能带时间/指针，而“谁失败了”由名字+状态即可判定，errors
# 计数与 rc 另外约束“是否干净”。
#
# 两处必须做的修正（都是实测踩出来的）：
#   * 名字里的 pid：walsetlk* 那族把 `$pid` 拼进用例名（`...-1.3.(504266) Ok`），
#     两个二进制必然不同号 ⇒ 不抹掉就会把“同一批用例全过”记成 DCC-DIFF。
#   * 行序：thread002 这类多线程用例的完成顺序不确定，gcc 自己两遍也不同。
#     这里按名字排序后落盘，比较的是“同一批名字+状态”，不是输出顺序。
norm_res() {
    awk '
        /^[^[:space:]]+\.\.\./ {
            name = $0; sub(/\.\.\..*$/, "", name)
            gsub(/\.\([0-9]+\)/, "", name)
            rest = $0; sub(/^[^[:space:]]+\.\.\./, "", rest)
            if (rest ~ /Ok[[:space:]]*$/)       st = "Ok"
            else if (rest ~ /FAILED/)           st = "FAILED"
            else if (rest ~ /ERROR/)            st = "ERROR"
            else                                st = "FAIL"
            print name " " st
        }
    ' "$1" | LC_ALL=C sort
}

# $1=fixture $2=tag $3=文件（相对 $SRC）$4=超时 ⇒ 落 log/res/sum；stdout=rc
run_file() {
    local bin=$1 tag=$2 f=$3 tmo=$4 base log rc sumline
    base=$(base_of "$f")
    log=$OUT/log/$tag-$base.log
    # 测试若从 `./testfixture` fork 子进程（lock2/lock6/walblock/crash* 等），
    # 子进程必须与本轮主进程同一个二进制，否则是拿另一个编译器在跑被测逻辑。
    ln -sfn "$bin" "$SRC/testfixture"
    as_user "cd '$SRC' && rm -f test.db test.db-journal test.db-wal test.db-shm && timeout -k 5 $tmo '$bin' '$f'" >"$log" 2>&1
    rc=$?
    # 超时/崩溃时 fork 出来的子 testfixture 可能还挂着（crash.tcl 那一族），
    # 子进程 argv[0] 可能是 `./testfixture`（路径里没有 $bin），所以按名字清。
    # 只限 $DCC_USER，且本进程已持机器级锁 ⇒ 不会误杀别的验证任务。
    pkill -u "$DCC_USER" -f 'testfixture' 2>/dev/null
    norm_res "$log" >"$OUT/res/$tag-$base.res"
    sumline=$(grep -oE '[0-9]+ errors out of [0-9]+ tests' "$log" | tail -1)
    echo "$rc ${sumline:-NO-SUMMARY}" >"$OUT/sum/$tag-$base.sum"
    echo "$rc"
}

# 读 .sum ⇒ 设置全局 RC ERRS NTESTS（缺汇总行时 ERRS/NTESTS 为空）。
# 汇总行有 7 个词：`0 0 errors out of 199 tests`，N 在第 6 个词（不是第 5 个）。
read_sum() {
    local rc a b c d e f
    read -r rc a b c d e f <"$1"
    RC=$rc
    ERRS=$a
    NTESTS=$e
    [ "$a" = "NO-SUMMARY" ] && { ERRS=""; NTESTS=""; }
}

fail_names() { awk '$2 != "Ok" { print $1 }' "$1" | sort -u; }

# 判定一对（ref, dcc）结果。$1=ref前缀 $2=dcc前缀 $3=文件基名 ⇒ stdout=结论
verdict_pair() {
    local rp=$1 dp=$2 base=$3 rr re rn dr de dn
    read_sum "$OUT/sum/$rp-$base.sum"; rr=$RC; re=$ERRS; rn=$NTESTS
    read_sum "$OUT/sum/$dp-$base.sum"; dr=$RC; de=$ERRS; dn=$NTESTS

    # 参照自己不可用：只有 dcc 相对参照“更差”时才可能有问题。
    if [ -z "$re" ] || [ -z "$rn" ]; then
        if [ "$dr" = 124 ] || [ "$dr" = 137 ]; then
            echo "REF-TIMEOUT-DCC-TIMEOUT"
        elif [ -z "$de" ]; then
            echo "REF-INCOMPLETE-DCC-INCOMPLETE"
        else
            echo "REF-INCOMPLETE-DCC-OK"
        fi
        return
    fi
    if [ "$re" != 0 ]; then
        # 参照有已知失败：仍比失败名集合，dcc 多出来的名字才算新问题。
        fail_names "$OUT/res/$rp-$base.res" >"$OUT/.ref.fails"
        fail_names "$OUT/res/$dp-$base.res" >"$OUT/.dcc.fails"
        if [ -z "$de" ]; then
            echo "REF-BAD-DCC-INCOMPLETE"
        elif comm -13 "$OUT/.ref.fails" "$OUT/.dcc.fails" | grep -q .; then
            echo "REF-BAD-DCC-EXTRA"
        else
            echo "REF-BAD"
        fi
        return
    fi
    if [ "$rn" = 0 ]; then
        echo "NO-CAP"
        return
    fi
    # 空结果行 = 该文件没有执行任何真实用例。`ifcapable !fts3` 一族在能力关闭时
    # 只调 finish_test，汇总行会写 `0 errors out of 1 tests`（finish_test 自己算
    # 一个伪用例），只看 N>=1 会把“能力未启用”误记成逐文件通过。fts3/fts5、
    # shell*（树里没建 sqlite3 shell）、平台限定文件都属于这一类。
    if [ ! -s "$OUT/res/$rp-$base.res" ]; then
        echo "NO-CAP"
        return
    fi
    if [ -z "$de" ]; then
        echo "DCC-INCOMPLETE"
        return
    fi
    if cmp -s "$OUT/res/$rp-$base.res" "$OUT/res/$dp-$base.res" && [ "$de" = 0 ] && [ "$dn" = "$rn" ] && [ "$dr" = 0 ]; then
        echo "PASS"
        return
    fi
    if [ "$de" != 0 ] && [ "$de" -gt "$re" ] 2>/dev/null; then
        echo "DCC-FAIL"
    elif ! cmp -s "$OUT/res/$rp-$base.res" "$OUT/res/$dp-$base.res"; then
        echo "DCC-DIFF"
    else
        echo "DCC-COUNT"
    fi
}

# 可疑结论的复核：同一文件再跑一对 ref/dcc，仍可疑才算确认。
rerun_pair() { # $1=file $2=level
    local f=$1 L=$2 base rp dp ref_bin dcc_bin
    base=$(base_of "$f")
    rp="ref-r2"; dp="dcc-O$L-r2"
    ref_bin=$(pick_fixture "$f" ref)
    dcc_bin=$(pick_fixture "$f" "dcc-O$L")
    run_file "$ref_bin" "$rp" "$f" "$FILE_TIMEOUT" >/dev/null
    run_file "$dcc_bin" "$dp" "$f" "$FILE_TIMEOUT" >/dev/null
    verdict_pair "$rp" "$dp" "$base"
}

# ---- 主循环 ---------------------------------------------------------------
: >"$OUT/results.tsv"
say ""
say "$(printf '%-8s %-34s %-28s %-28s %s' level file 'ref rc/e/n/ms' 'dcc rc/e/n/ms' verdict)"

t_start=$EPOCHREALTIME
for L in $LEVELS; do
    dcc_tf=$SQ_RUN/testfixture-dcc-O$L
    for f in $FILES; do
        [ -f "$SRC/$f" ] || { say "  MISSING $f（$SQ_RUN/src 里没有这个文件）"; fails=$((fails + 1)); continue; }
        base=$(base_of "$f")
        ref_bin=$(pick_fixture "$f" ref)
        dcc_bin=$(pick_fixture "$f" "dcc-O$L")
        s=$EPOCHREALTIME
        rrc=$(run_file "$ref_bin" ref "$f" "$FILE_TIMEOUT")
        read_sum "$OUT/sum/ref-$base.sum"; ref_ms=$(( (${EPOCHREALTIME/./} - ${s/./}) / 1000 ))
        ref_e=${ERRS:-X}; ref_n=${NTESTS:-X}

        # 参照超时/崩溃：dcc 结果没有比较意义，省掉一次同长度等待；
        # 但仍落一行，汇总里点名这是“没判”的文件，不是“通过”。
        if [ "$rrc" = 124 ] || [ "$rrc" = 137 ] || [ -z "$ERRS" ]; then
            # 空日志 + rc=0 是上游“禁用文件”（如 alias.test 在 source tester.tcl 后
            # 直接 return、win32*.test 在非 Windows 上什么都不跑），记 NO-TESTS，
            # 与“跑到一半崩了/没汇总行”的 REF-INCOMPLETE 分开。
            if [ "$rrc" = 0 ] && [ ! -s "$OUT/res/ref-$base.res" ]; then
                verdict=NO-TESTS
            elif [ "$rrc" = 124 ] || [ "$rrc" = 137 ]; then
                verdict=REF-TIMEOUT
            else
                verdict=REF-INCOMPLETE
            fi
            printf '%s\t%s\t%s/%s/%s/%s\t-\t%s\n' "O$L" "$f" "$rrc" "$ref_e" "$ref_n" "$ref_ms" "$verdict" >>"$OUT/results.tsv"
            say "$(printf '%-8s %-34s %-28s %-28s %s' "O$L" "$f" "$rrc/$ref_e/$ref_n/$ref_ms" - "$verdict")"
            continue
        fi

        s=$EPOCHREALTIME
        drc=$(run_file "$dcc_bin" "dcc-O$L" "$f" "$FILE_TIMEOUT")
        read_sum "$OUT/sum/dcc-O$L-$base.sum"; dcc_ms=$(( (${EPOCHREALTIME/./} - ${s/./}) / 1000 ))
        dcc_e=${ERRS:-X}; dcc_n=${NTESTS:-X}

        verdict=$(verdict_pair ref "dcc-O$L" "$base")
        # 只在“参照干净、dcc 不干净/不同”或“参照有失败、dcc 多出失败名/崩溃”时复核。
        case $verdict in
            PASS | NO-CAP | REF-BAD) ;;
            *)
                if [ "$RETRY" = 1 ]; then
                    v2=$(rerun_pair "$f" "$L")
                    # 先查两边各自是否确定：crash.test 用 rand() 选崩溃点，
                    # 同一 gcc 二进制连跑四次得到 900/927/900/885 条子用例
                    # （实测），dcc 也同理。这种名字差异不能当缺陷；
                    # 但参照两遍都干净地跑完、而 dcc 崩溃/报错，仍算真问题。
                    if ! cmp -s "$OUT/res/ref-$base.res" "$OUT/res/ref-r2-$base.res" ||
                       ! cmp -s "$OUT/res/dcc-O$L-$base.res" "$OUT/res/dcc-O$L-r2-$base.res"; then
                        case $verdict in
                            DCC-INCOMPLETE | DCC-FAIL | REF-BAD-DCC-INCOMPLETE | REF-BAD-DCC-EXTRA)
                                verdict="CONFIRMED($verdict)" ;;
                            *)
                                verdict="UNSTABLE($verdict)" ;;
                        esac
                    elif [ "$v2" = PASS ] || [ "$v2" = "REF-BAD" ]; then
                        verdict="FLAKY($verdict)"
                    else
                        verdict="CONFIRMED($verdict)"
                    fi
                fi
                ;;
        esac

        printf '%s\t%s\t%s/%s/%s/%s\t%s/%s/%s/%s\t%s\n' "O$L" "$f" \
            "$rrc" "$ref_e" "$ref_n" "$ref_ms" "$drc" "$dcc_e" "$dcc_n" "$dcc_ms" "$verdict" >>"$OUT/results.tsv"
        case $verdict in
            PASS | NO-CAP | REF-BAD) ;;
            *) say "$(printf '%-8s %-34s %-28s %-28s %s' "O$L" "$f" \
                   "$rrc/$ref_e/$ref_n/$ref_ms" "$drc/$dcc_e/$dcc_n/$dcc_ms" "$verdict")" ;;
        esac
        case $verdict in CONFIRMED*) fails=$((fails + 1)) ;; esac
    done
done
t_end=$EPOCHREALTIME

# ---- 汇总 ---------------------------------------------------------------
say ""
say "=== summary ==="
awk -F'\t' '
    { c[$5]++ }
    END { for (k in c) printf "    %-28s %d\n", k, c[k] }
' "$OUT/results.tsv" | sort
say "    总耗时: $(( (${t_end/./} - ${t_start/./}) / 1000000 )) s"
say "    dcc sha256（开跑前）= $DCC_SHA"
now_sha=$(sha256sum "$DCC" | awk '{print $1}')
say "    dcc sha256（收尾时）= $now_sha"
if [ "$now_sha" != "$DCC_SHA" ]; then
    say "  COMPILER-CHANGED: 跑的过程中 $DCC 被重写了，本轮证据不来自同一个编译器 ⇒ 必须重跑"
    fails=$((fails + 1))
fi
say "    明细: $OUT/results.tsv   日志: $OUT/log/   归一化结果: $OUT/res/"

if [ "$fails" = 0 ]; then
    say "RESULT: PASS（无 dcc 新增失败；未判文件与能力关闭项见上表）"
    exit 0
fi
say "RESULT: FAIL ($fails confirmed dcc issue(s))"
exit 1
