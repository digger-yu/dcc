#!/bin/bash
# scripts/cargo_test_summary.sh —— 跑 cargo test 并给出**跨全部 target 的总计**。
#
# 为什么需要它：cargo 只按 target 各打一行 `test result: ok. N passed; ...`，
# 一次 `cargo test` 跑完看不见总量 —— 到底 10 个 target 合计多少项、失败几条、
# 忽略哪几条，都得自己翻日志。Tier 0 的第 2 条正是「全部 target 通过」，
# 而它原先**没有可核对的汇总**，看绿只能逐行数。
#
# 输出四件事：
#   1) 每个 target 一行（passed / failed / ignored / measured）
#   2) 合计：测试项总数与三类计数
#   3) 失败列表与忽略列表（带所属 target，便于直接 --exact 重跑）
#   4) 原始日志**最上面**的元信息头：开始/结束时间（北京时间 UTC+8）、命令行、git
#      版本、耗时、cargo 退出码。为什么非要加它：cargo 的原话紧跟在头后面，而它第一
#      行必然是编译期的 `warning:` —— 只看日志开头会误判成「这文件被截断了」。汇总
#      表里也把这批元信息复述一遍，`--from-log` 复述旧日志时同样能带出时间。
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
#
# 元信息头的行首一律是 `# [dcc-test-meta] `：这个前缀与 cargo / libtest 的任何输出
# 形状都不重合，所以汇总器能照它归集，既不会把这几行算进任何计数，也不会因为日志
# 里混进测试打印（如 `--nocapture`）而误读。
set -u

# 北京时间（UTC+8）时间戳。有 zoneinfo 就走 IANA 名字；极简容器里 zoneinfo 缺失时
# 退回 POSIX 偏移量写法 `UTC-8`（POSIX 的符号与直觉相反，`UTC-8` 即 UTC+8），两条
# 路径打印出的是同一个数 —— 不让元信息因为环境差异塌成空白或错位成别的时区。
ts_cn() {
    if [ -r /usr/share/zoneinfo/Asia/Shanghai ]; then
        TZ=Asia/Shanghai date -d "@$1" '+%Y-%m-%d %H:%M:%S 北京时间 (UTC+8)'
    else
        TZ=UTC-8 date -d "@$1" '+%Y-%m-%d %H:%M:%S 北京时间 (UTC+8)'
    fi
}

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
    # 命令行组装成数组，一是让元信息头能原样复述这一轮跑了什么参数（含 LOG= 覆盖外的
    # 全部 JOBS/TEST_THREADS/选择器），二是消除下面那条 `# shellcheck disable=SC2086`：
    # 未加引号的 ${CARGO_ARGS[@]} 靠 word splitting 传参，带空格的参数会被拆坏。
    CARGO_CMD=(cargo test "${PROFILE_ARGS[@]}" "-j$JOBS")
    [ ${#CARGO_ARGS[@]} -gt 0 ] && CARGO_CMD+=("${CARGO_ARGS[@]}")
    CARGO_CMD+=(-- "--test-threads=$TEST_THREADS")
    [ ${#TEST_ARGS[@]} -gt 0 ] && CARGO_CMD+=("${TEST_ARGS[@]}")
    T_START=$(date +%s)
    GIT_INFO="git $(git rev-parse --short HEAD 2>/dev/null || echo '未知')"
    [ -n "$(git status --porcelain 2>/dev/null)" ] && GIT_INFO="$GIT_INFO (工作区有未提交改动)"
    {
        printf '# [dcc-test-meta] ----- 元信息（本脚本写入；cargo 的原话紧跟其后）-----\n'
        printf '# [dcc-test-meta] 开始时间 : %s\n' "$(ts_cn "$T_START")"
        printf '# [dcc-test-meta] 命令     : %s\n' "${CARGO_CMD[*]}"
        printf '# [dcc-test-meta] 仓库     : %s  %s\n' "$REPO" "$GIT_INFO"
        printf '# [dcc-test-meta] 说明     : 下面紧跟着的是 cargo 的原话，开头那批 warning: 是编译期告警（正常，不是日志被截断）\n'
    } >> "$LOG"
    # 必须是追加：cargo 那侧若用 `> "$LOG"`，它启动的瞬间就把刚写好的头 truncate 掉。
    "${CARGO_CMD[@]}" >> "$LOG" 2>&1
    RC=$?
    T_END=$(date +%s)
    DUR=$((T_END - T_START))
    {
        printf '# [dcc-test-meta] 结束时间 : %s\n' "$(ts_cn "$T_END")"
        printf '# [dcc-test-meta] 耗时     : %ds (%dm%02ds)\n' "$DUR" "$((DUR / 60))" "$((DUR % 60))"
        printf '# [dcc-test-meta] cargo 退出码 : %s\n' "$RC"
    } >> "$LOG"
fi

# ---------------------------------------------------------------- 汇总
# 一次 awk 扫完：按 target 归集计数，并记下 FAILED / ignored 的用例名与其 target。
awk -v rc="$RC" -v reran="$RERAN" -v logname="$LOG" '
function flush_row() {
    if (cur != "") { NT++; tgt_name[NT] = cur; cnt_p[NT] = P; cnt_f[NT] = F; cnt_i[NT] = I; cnt_m[NT] = M }
    cur = ""; P = F = I = M = 0
}
# 元信息头：收下来原样复述，不参与任何计数（`next` 保证不落到下面的归集规则）。
/^# \[dcc-test-meta\] / {
    m = $0
    sub(/^# \[dcc-test-meta\] +/, "", m)
    if (m ~ /^-----/) next      # 日志里的分隔横幅不复述进汇总表：它那半句「cargo 的原话紧跟其后」只在日志里有意义
    nmeta++
    meta_all[nmeta] = m
    next
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
    # 元信息头复述。缺头只有两种可能：改动前跑的老日志，或日志不是本脚本写的 ——
    # 两种都得点名，否则「几点跑的」这个问题就重新变成只能靠猜。
    if (nmeta + 0 > 0) {
        printf "---- 日志元信息 ----\n"
        for (mi = 1; mi <= nmeta; mi++) printf "%s\n", meta_all[mi]
    } else {
        printf "!! 日志里没有元信息头 —— 拿不到开始时间（改动前的老日志，或不是本脚本写的）\n"
    }
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
