#!/usr/bin/env bash
# scripts/torture_summary.sh — 解析 torture 测试日志并输出 GitHub Actions Summary
#
# 用法:
#   scripts/torture_summary.sh <compile_log> <execute_log>
#
# 输出 Markdown 格式的测试汇总，包含测试总数、通过/失败/跳过数、
# 失败列表、跳过列表、通过率。
set -euo pipefail

COMPILE_LOG="${1:-}"
EXECUTE_LOG="${2:-}"

if [[ -z "$COMPILE_LOG" || -z "$EXECUTE_LOG" ]]; then
    echo "用法: $0 <compile_log> <execute_log>" >&2
    exit 1
fi

# ── 从日志中提取统计数据 ──
# 匹配格式: total=N  pass=N  fail=N  skip=N
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
    # 匹配 "  FAIL  filename" 行
    grep -E '^\s+FAIL\s+' "$log" | sed 's/^\s*FAIL\s\+//' || true
}

# ── 从日志中提取跳过列表 ──
parse_skips() {
    local log="$1"
    if [[ ! -f "$log" ]]; then
        return
    fi
    # 匹配 "  SKIP  filename" 行
    grep -E '^\s+SKIP\s+' "$log" | sed 's/^\s*SKIP\s\+//' || true
}

# ── 计算通过率 ──
calc_rate() {
    local total="$1" pass="$2"
    if [[ "$total" -eq 0 ]]; then
        echo "N/A"
        return
    fi
    # 用 awk 计算百分比，保留两位小数
    awk "BEGIN { printf \"%.2f%%\", ($pass / $total) * 100 }"
}

# ── 仓库内文件的可点击链接 ──
# Job Summary 里的相对链接按仓库根页面解析, `tests/skip/x.txt` 会被拼成
# https://github.com/<owner>/<repo>/tests/skip/x.txt (缺 /blob/<sha>/),
# 因此用 Actions 注入的环境变量拼绝对地址; 本地运行退回相对路径。
repo_blob_url() {
    local path="$1"
    if [[ -n "${GITHUB_REPOSITORY:-}" ]]; then
        echo "${GITHUB_SERVER_URL:-https://github.com}/${GITHUB_REPOSITORY}/blob/${GITHUB_SHA:-main}/${path}"
    else
        echo "$path"
    fi
}

# ── 解析两组日志 ──
read -r c_total c_pass c_fail c_skip <<< "$(parse_stats "$COMPILE_LOG")"
read -r e_total e_pass e_fail e_skip <<< "$(parse_stats "$EXECUTE_LOG")"

g_total=$((c_total + e_total))
g_pass=$((c_pass + e_pass))
g_fail=$((c_fail + e_fail))
g_skip=$((c_skip + e_skip))

c_rate=$(calc_rate "$c_total" "$c_pass")
e_rate=$(calc_rate "$e_total" "$e_pass")
g_rate=$(calc_rate "$g_total" "$g_pass")

# ── 提取失败和跳过列表 ──
compile_failures=$(parse_failures "$COMPILE_LOG")
execute_failures=$(parse_failures "$EXECUTE_LOG")
compile_skips=$(parse_skips "$COMPILE_LOG")
execute_skips=$(parse_skips "$EXECUTE_LOG")

# ── 输出 Markdown ──
echo "# GCC Torture Test Results"
echo ""
echo "## Summary"
echo ""
echo "| Category | Total | Pass | Fail | Skip | Pass Rate |"
echo "|----------|------:|-----:|-----:|-----:|----------:|"
echo "| Compile  | $c_total | $c_pass | $c_fail | $c_skip | $c_rate |"
echo "| Execute  | $e_total | $e_pass | $e_fail | $e_skip | $e_rate |"
echo "| **Total**| **$g_total** | **$g_pass** | **$g_fail** | **$g_skip** | **$g_rate** |"
echo ""

# ── 失败列表 ──
if [[ -n "$compile_failures" || -n "$execute_failures" ]]; then
    echo "## Failures"
    echo ""
    if [[ -n "$compile_failures" ]]; then
        echo "### Compile (${c_fail} failures)"
        echo ""
        echo '```'
        echo "$compile_failures"
        echo '```'
        echo ""
    fi
    if [[ -n "$execute_failures" ]]; then
        echo "### Execute (${e_fail} failures)"
        echo ""
        echo '```'
        echo "$execute_failures"
        echo '```'
        echo ""
    fi
else
    echo "All tests passed!"
    echo ""
fi

# ── 跳过列表 ──
if [[ -n "$compile_skips" || -n "$execute_skips" ]]; then
    echo "## Skipped Tests"
    echo ""
    echo "See [torture_skip.txt]($(repo_blob_url tests/skip/torture_skip.txt)) for skip reasons."
    echo ""
    if [[ -n "$compile_skips" ]]; then
        echo "### Compile (${c_skip} skipped)"
        echo ""
        echo '```'
        echo "$compile_skips"
        echo '```'
        echo ""
    fi
    if [[ -n "$execute_skips" ]]; then
        echo "### Execute (${e_skip} skipped)"
        echo ""
        echo '```'
        echo "$execute_skips"
        echo '```'
        echo ""
    fi
fi
