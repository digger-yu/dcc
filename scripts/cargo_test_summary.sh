#!/bin/bash
# scripts/cargo_test_summary.sh —— 跑 cargo test 并给出**跨全部 target 的总计**。
#
# 为什么需要它：cargo 只按 target 各打一行 `test result: ok. N passed; ...`，
# 一次 `cargo test` 跑完看不见总量 —— 到底 10 个 target 合计多少项、失败几条、
# 忽略哪几条，都得自己翻日志。Tier 0 的第 2 条正是「全部 target 通过」，
# 而它原先**没有可核对的汇总**，看绿只能逐行数。
#
# 输出三件事：
#   1) 每个 target 一行（passed / failed / ignored / measured）
#   2) 合计：测试项总数与三类计数
#   3) 失败列表与忽略列表（带所属 target，便于直接 --exact 重跑）
#
# 硬约束：**退出码一律透传 cargo test 的**。汇总器只报告，不判绿 —— 否则就成了
# 新的假绿来源（本仓 §7.5 一族教训正是「pass 接上了但产物没变」式的自欺）。
#
# 用法:
#   bash scripts/cargo_test_summary.sh                  # 跑并汇总
#   bash scripts/cargo_test_summary.sh --lib             # cargo 侧参数（选 target/名字过滤）
#   bash scripts/cargo_test_summary.sh -- --exact foo::bar   # 测试二进制侧参数
#   LOG=... bash scripts/cargo_test_summary.sh            # 指定并**保留**原始日志
#   bash scripts/cargo_test_summary.sh --from-log PATH     # 只汇总已有日志（不重跑）
#   JOBS=4 TEST_THREADS=4 ...                              # 显式提高并发（默认各 2）
set -u

REPO=${DCC_REPO:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}
JOBS=${JOBS:-2}
TEST_THREADS=${TEST_THREADS:-2}
PROFILE_ARGS=(${CARGO_TEST_ARGS:---release})

FROM_LOG=""
CARGO_ARGS=()   # 送给 cargo 的（选 target / 改名过滤，必须排在 `--` 之前）
TEST_ARGS=()    # 送给测试二进制的（排在 `--` 之后）
SEEN_DD=0
while [ $# -gt 0 ]; do
    case "$1" in
        --from-log) FROM_LOG=${2:-}; shift 2 ;;
        --) SEEN_DD=1; shift ;;
        *)
            if [ $SEEN_DD -eq 0 ]; then CARGO_ARGS+=("$1"); else TEST_ARGS+=("$1"); fi
            shift ;;
    esac
done

if [ -n "$FROM_LOG" ]; then
    [ -r "$FROM_LOG" ] || { echo "错误: 日志不可读: $FROM_LOG" >&2; exit 2; }
    LOG="$FROM_LOG"
    RC=0                      # 复述模式不重跑, 退出码由解析结果决定
    RERAN=0
else
    if [ -z "${LOG:-}" ]; then
        LOG="/tmp/cargo_test_summary.log"
    fi
    KEEP_LOG=1                # 日志始终保留（默认 /tmp/cargo_test_summary.log，或用户指定的路径）
    : > "$LOG"
    RERAN=1
    cd "$REPO" || exit 2
    # shellcheck disable=SC2086
    cargo test "${PROFILE_ARGS[@]}" -j"$JOBS" ${CARGO_ARGS[@]+"${CARGO_ARGS[@]}"} -- \
        --test-threads="$TEST_THREADS" ${TEST_ARGS[@]+"${TEST_ARGS[@]}"} > "$LOG" 2>&1
    RC=$?
fi

# ---------------------------------------------------------------- 汇总
# 一次 awk 扫完：按 target 归集计数，并记下 FAILED / ignored 的用例名与其 target。
awk -v rc="$RC" -v reran="$RERAN" -v logname="$LOG" '
function flush_row() {
    if (cur != "") { NT++; tgt_name[NT] = cur; cnt_p[NT] = P; cnt_f[NT] = F; cnt_i[NT] = I; cnt_m[NT] = M }
    cur = ""; P = F = I = M = 0
}
/^ +Running |^ +Doc-tests/ {
    t = $0
    sub(/^ +/, "", t)
    sub(/ *\(.*/, "", t)              # 去掉 " (target/release/deps/...)"
    sub(/^Running unittests /, "", t)  # 表里只留 "src/lib.rs"
    sub(/^Running /, "", t)            # 只留 "tests/integration.rs"
    if (t != "") { flush_row(); cur = t }
    next
}
# 注意：cargo 还会打小写的 "running N tests" 分隔行，它不是 target 头，
# 上面两条只匹配 cargo 真正的头（缩进 + 大写 Running / Doc-tests）。
/^test result:/ {
    head = $0
    sub(/^test result: /, "", head)
    verdict = (head ~ /^ok\./) ? "ok" : "FAILED"
    n = split(head, fld, ";")
    for (k = 1; k <= n; k++) {
        c = fld[k]; gsub(/^ +| +$/, "", c)
        # 必须用正则取字段里第一个数字：首字段长得很可能
        # "ok. 2059 passed" / "FAILED. 0 passed"，直接 c+0 会归成 0，把所有计数抹掉。
        v = 0
        if (match(c, /[0-9]+/)) v = substr(c, RSTART, RLENGTH) + 0
        if      (c ~ /passed/)   P += v
        else if (c ~ /failed/)   F += v
        else if (c ~ /ignored/)  I += v
        else if (c ~ /measured/) M += v
    }
    if (verdict == "FAILED") bad_targets++
    TOT_P += P; TOT_F += F; TOT_I += I; TOT_M += M
    flush_row()
    next
}
/^test .* \.\.\. / {
    p = index($0, " ... ")
    if (p > 6) {
        tname = substr($0, 6, p - 6)
        st = substr($0, p + 5)
        why = ""
        if (index(st, ",") > 0) { why = st; sub(/^[^,]*, */, "", why) }
        sub(/,.*/, "", st); gsub(/^ +| +$/, "", st)
        tgt = (cur == "") ? "?" : cur
        if (st == "FAILED") { nfail++; fail_line[nfail] = tgt "|" tname }
        else if (st == "ignored") {
            nign++; ign_line[nign] = tgt "|" tname (why == "" ? "" : "  ← " why)
        }
    }
    next
}
/^error(\[|:) / {
    if (nerr == 0) first_err = $0
    nerr++
}
END {
    flush_row()
    printf "\n"
    printf "══════════════════ cargo test 汇总 ══════════════════\n"
    if (reran == 1) printf "日志: %s\n", logname
    else            printf "来源: %s (仅汇总, 未重跑)\n", logname
    if (NT == 0) {
        printf "!! 没有任何 test result 行 —— 测试很可能没有跑起来\n"
    } else {
        printf "%-46s %8s %7s %8s %6s\n", "target", "passed", "failed", "ignored", "meas"
        for (r = 1; r <= NT; r++)
            printf "%-46s %8d %7d %8d %6d\n", substr(tgt_name[r], 1, 46), cnt_p[r], cnt_f[r], cnt_i[r], cnt_m[r]
        # 表格行标签一律用 ASCII：%-46s 按**字节**填充，中文标签会让列歪掉。
        printf "%-46s %8s %7s %8s %6s\n", "---- " NT " targets above ----", "", "", "", ""
        printf "%-46s %8d %7d %8d %6d\n", "TOTAL", TOT_P, TOT_F, TOT_I, TOT_M
        printf "\n"
        printf "测试项总数 : %d\n", TOT_P + TOT_F + TOT_I + TOT_M
        printf "  通过     : %d\n", TOT_P
        printf "  失败     : %d\n", TOT_F
        printf "  忽略     : %d\n", TOT_I
        printf "  仅计时   : %d\n", TOT_M
        printf "失败 target: %d\n", bad_targets + 0
    }
    printf "编译/运行错误行: %d\n", nerr + 0
    if (nerr + 0 > 0) printf "  首条: %s\n", substr(first_err, 1, 120)

    # 自核对：逐行之和必须等于总计。不相等就说明有 test result 行没归属到
    # target（输出形状变了），宁可报出来也不给一张对不上的表。
    if (NT > 0) {
        sp = sf = si = sm = 0
        for (r = 1; r <= NT; r++) { sp += cnt_p[r]; sf += cnt_f[r]; si += cnt_i[r]; sm += cnt_m[r] }
        if (sp == TOT_P && sf == TOT_F && si == TOT_I && sm == TOT_M)
            printf "行合计校对 : ✓ (逐行之和 == 总计)\n"
        else
            printf "!! 行合计校对 不一致: 行(%d,%d,%d,%d) vs 总计(%d,%d,%d,%d) —— 有 result 行未归属\n", sp, sf, si, sm, TOT_P, TOT_F, TOT_I, TOT_M
    }

    printf "\n──── 失败列表 (%d) ────\n", nfail + 0
    for (i = 1; i <= nfail + 0; i++) printf "  ✗ %s\n", fail_line[i]
    if (nfail + 0 == 0) print "  (无)\n"

    printf "\n──── 忽略列表 (%d) ────\n", nign + 0
    for (i = 1; i <= nign + 0; i++) printf "  ○ %s\n", ign_line[i]
    if (nign + 0 == 0) print "  (无)\n"
    printf "═════════════════════════════════════════════════════\n"

    # 退出码: 复述模式按解析结果推断; 真跑一律透传 cargo 的 rc。
    # （mawk 不支持 `exit (a)?1:0` 这种写法, 先算进变量再 exit）
    if (reran == 1) { code = rc + 0 } else {
        code = ((TOT_F + 0) > 0 || (nerr + 0) > 0 || NT == 0) ? 1 : 0
    }
    exit code
}
' "$LOG"
exit $?
