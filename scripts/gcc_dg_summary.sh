#!/usr/bin/env bash
# scripts/gcc_dg_summary.sh — 解析 gcc.dg 测试日志并输出 GitHub Actions Summary
#
# 用法:
#   scripts/gcc_dg_summary.sh <compile_log> [run_log]
#
# 输出 Markdown 格式的详细汇总：
#   - 总体表格
#   - 按 failure 类型分类的失败列表
#   - 按 skip 原因分类的跳过列表
set -euo pipefail

COMPILE_LOG="${1:-}"
RUN_LOG="${2:-}"

if [[ -z "$COMPILE_LOG" ]]; then
    echo "用法: $0 <compile_log> [run_log]" >&2
    exit 1
fi

# ── 从日志中提取统计数据 ──
parse_stats() {
    local log="$1"
    if [[ ! -f "$log" ]]; then
        echo "0 0 0 0"
        return
    fi
    local line
    line=$(grep -E '^[[:space:]]*total=' "$log" | tail -1)
    if [[ -z "$line" ]]; then
        echo "0 0 0 0"
        return
    fi
    local total pass fail skip
    total=$(echo "$line" | sed -n 's/.*total=\([0-9]*\).*/\1/p')
    pass=$(echo "$line" | sed -n 's/.*pass=\([0-9]*\).*/\1/p')
    fail=$(echo "$line" | sed -n 's/.*fail=\([0-9]*\).*/\1/p')
    skip=$(echo "$line" | sed -n 's/.*skip=\([0-9]*\).*/\1/p')
    echo "${total:-0} ${pass:-0} ${fail:-0} ${skip:-0}"
}

# ── 从日志中提取失败列表 ──
parse_failures() {
    local log="$1"
    if [[ ! -f "$log" ]]; then
        return
    fi
    grep -E '^\s+FAIL\s+' "$log" | sed 's/^\s*FAIL\s\+//' || true
}

# ── 从日志中提取跳过列表 ──
parse_skips() {
    local log="$1"
    if [[ ! -f "$log" ]]; then
        return
    fi
    grep -E '^\s+SKIP\s+' "$log" | sed 's/^\s*SKIP\s\+//' || true
}

# ── 计算通过率 ──
calc_rate() {
    local total="$1" pass="$2"
    if [[ "$total" -eq 0 ]]; then
        echo "N/A"
        return
    fi
    awk "BEGIN { printf \"%.2f%%\", ($pass / $total) * 100 }"
}

# ── 解析日志 ──
read -r c_total c_pass c_fail c_skip <<< "$(parse_stats "$COMPILE_LOG")"
if [[ -n "$RUN_LOG" && -f "$RUN_LOG" ]]; then
    read -r r_total r_pass r_fail r_skip <<< "$(parse_stats "$RUN_LOG")"
else
    r_total=0; r_pass=0; r_fail=0; r_skip=0
fi

g_total=$((c_total + r_total))
g_pass=$((c_pass + r_pass))
g_fail=$((c_fail + r_fail))
g_skip=$((c_skip + r_skip))

c_rate=$(calc_rate "$c_total" "$c_pass")
r_rate=$(calc_rate "$r_total" "$r_pass")
g_rate=$(calc_rate "$g_total" "$g_pass")

# ── 提取列表 ──
compile_failures=$(parse_failures "$COMPILE_LOG")
run_failures=""
if [[ -n "$RUN_LOG" && -f "$RUN_LOG" ]]; then
    run_failures=$(parse_failures "$RUN_LOG")
fi
compile_skips=$(parse_skips "$COMPILE_LOG")
run_skips=""
if [[ -n "$RUN_LOG" && -f "$RUN_LOG" ]]; then
    run_skips=$(parse_skips "$RUN_LOG")
fi

# ── 输出 Markdown ──
echo "# GCC dg Test Results"
echo ""
echo "## Summary"
echo ""
echo "| Category | Total | Pass | Fail | Skip | Pass Rate |"
echo "|----------|------:|-----:|-----:|-----:|----------:|"
echo "| Compile  | $c_total | $c_pass | $c_fail | $c_skip | $c_rate |"
if [[ "$r_total" -gt 0 ]]; then
    echo "| Run      | $r_total | $r_pass | $r_fail | $r_skip | $r_rate |"
fi
echo "| **Total**| **$g_total** | **$g_pass** | **$g_fail** | **$g_skip** | **$g_rate** |"
echo ""

# ── 失败详情 (按类型分类) ──
all_failures="${compile_failures}
${run_failures}"
all_failures=$(echo "$all_failures" | sed '/^$/d')

if [[ -n "$all_failures" ]]; then
    echo "## Failures by Type"
    echo ""

    # compile failures
    compile_compile_fail=$(echo "$compile_failures" | grep -c 'compile' 2>/dev/null || echo 0)
    compile_timeout_fail=$(echo "$compile_failures" | grep -c 'timeout' 2>/dev/null || echo 0)
    if [[ "$c_fail" -gt 0 ]]; then
        echo "### Compile Test Failures (${c_fail})"
        echo ""
        echo "| Failure Type | Count |"
        echo "|-------------|------:|"
        echo "| Compile Error | $compile_compile_fail |"
        echo "| Timeout | $compile_timeout_fail |"
        echo ""
        echo '```'
        echo "$compile_failures"
        echo '```'
        echo ""
    fi

    # run failures
    if [[ "$r_total" -gt 0 && "$r_fail" -gt 0 ]]; then
        run_compile_fail=$(echo "$run_failures" | grep -c 'compile' 2>/dev/null || echo 0)
        run_runtime_fail=$(echo "$run_failures" | grep -c 'runtime' 2>/dev/null || echo 0)
        run_timeout_fail=$(echo "$run_failures" | grep -c 'timeout' 2>/dev/null || echo 0)
        echo "### Run Test Failures (${r_fail})"
        echo ""
        echo "| Failure Type | Count |"
        echo "|-------------|------:|"
        echo "| Compile Error | $run_compile_fail |"
        echo "| Runtime Error | $run_runtime_fail |"
        echo "| Timeout | $run_timeout_fail |"
        echo ""
        echo '```'
        echo "$run_failures"
        echo '```'
        echo ""
    fi
else
    echo "All tests passed!"
    echo ""
fi

# ── 跳过详情 (按原因分类) ──
all_skips="${compile_skips}
${run_skips}"
all_skips=$(echo "$all_skips" | sed '/^$/d')

if [[ -n "$all_skips" ]]; then
    echo "## Skipped Tests"
    echo ""
    echo "See [gcc_dg_skip.txt](tests/gcc_dg_skip.txt) for manually skipped items."
    echo ""

    # 按 skip 原因分类统计
    local_target_skips=$(echo "$all_skips" | grep -c 'target:' 2>/dev/null || echo 0)
    dg_skip_if=$(echo "$all_skips" | grep -c 'dg-skip-if' 2>/dev/null || echo 0)
    dg_do_skip=$(echo "$all_skips" | grep -c 'dg-do:' 2>/dev/null || echo 0)
    manual_skips=$((g_skip - local_target_skips - dg_skip_if - dg_do_skip))
    if [[ $manual_skips -lt 0 ]]; then manual_skips=0; fi

    echo "| Skip Reason | Count |"
    echo "|-------------|------:|"
    echo "| Manual (skip list) | $manual_skips |"
    echo "| Target not x86_64 | $local_target_skips |"
    echo "| dg-skip-if directive | $dg_skip_if |"
    echo "| dg-do type unsupported | $dg_do_skip |"
    echo "| **Total** | **$g_skip** |"
    echo ""

    # compile skips
    if [[ -n "$compile_skips" ]]; then
        echo "### Compile Test Skips (${c_skip})"
        echo ""
        echo '```'
        echo "$compile_skips"
        echo '```'
        echo ""
    fi

    # run skips
    if [[ -n "$RUN_LOG" && -f "$RUN_LOG" && -n "$run_skips" ]]; then
        echo "### Run Test Skips (${r_skip})"
        echo ""
        echo '```'
        echo "$run_skips"
        echo '```'
        echo ""
    fi
fi
