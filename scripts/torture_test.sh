#!/usr/bin/env bash
# scripts/torture_test.sh — GCC C Torture Suite 测试驱动
#
# 用法:
#   scripts/torture_test.sh [compile|execute|all]
#
# 环境变量:
#   DCC          — dcc 编译器路径 (默认: target/release/dcc)
#   TORTURE_DIR  — torture suite 根目录 (默认: tests/gcc-torture)
#   JOBS         — 并行度 (默认: nproc)
#   VERBOSE      — 非空时输出每个测试的详细结果
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

DCC="${DCC:-$REPO_ROOT/target/release/dcc}"
TORTURE_DIR="${TORTURE_DIR:-$REPO_ROOT/tests/gcc-torture}"
JOBS="${JOBS:-$(nproc 2>/dev/null || echo 4)}"
VERBOSE="${VERBOSE:-}"
SKIP_FILE="$REPO_ROOT/tests/torture_skip.txt"

SUB="${1:-all}"
SUITE_DIR="$TORTURE_DIR/gcc/testsuite/gcc.c-torture"

# ── 前置检查 ──
if [[ ! -x "$DCC" ]]; then
    echo "错误: dcc 不存在或不可执行: $DCC" >&2
    echo "请先运行: cargo build --release" >&2
    exit 1
fi

if [[ ! -d "$SUITE_DIR" ]]; then
    echo "错误: torture suite 未安装: $SUITE_DIR" >&2
    echo "请先运行: bash scripts/setup_torture.sh" >&2
    exit 1
fi

# ── 跳过列表 ──
declare -a SKIP_PATTERNS=()
if [[ -f "$SKIP_FILE" ]]; then
    while IFS= read -r line; do
        line="${line%%#*}"           # 去注释
        line="${line//[[:space:]]/}" # 去空白
        [[ -n "$line" ]] && SKIP_PATTERNS+=("$line")
    done < "$SKIP_FILE"
fi

should_skip() {
    local name="$1"
    for pat in "${SKIP_PATTERNS[@]+"${SKIP_PATTERNS[@]}"}"; do
        # 支持 prefix* glob 和精确匹配
        if [[ "$pat" == *"*" ]]; then
            [[ "$name" == ${pat} ]] && return 0
        else
            [[ "$name" == "$pat" ]] && return 0
        fi
    done
    return 1
}

# ── 临时目录 ──
TMPDIR_TORTURE=$(mktemp -d /tmp/dcc-torture.XXXXXX)
trap 'rm -rf "$TMPDIR_TORTURE"' EXIT

# ── 结果收集 ──
RESULT_DIR="$TMPDIR_TORTURE/results"
mkdir -p "$RESULT_DIR"

# ── compile 测试 ──
run_compile_test() {
    local src="$1"
    local name
    name="$(basename "$src")"
    local out="$TMPDIR_TORTURE/${name}.o"

    if "$DCC" -c -std=gnu11 -o "$out" "$src" 2>/dev/null; then
        rm -f "$out"
        echo "PASS" 
    else
        rm -f "$out"
        echo "FAIL"
    fi
}
export -f run_compile_test
export DCC TMPDIR_TORTURE

# ── execute 测试 ──
run_execute_test() {
    local src="$1"
    local name
    name="$(basename "$src" .c)"
    local exe="$TMPDIR_TORTURE/${name}.exe"

    if ! "$DCC" -std=gnu11 -o "$exe" "$src" 2>/dev/null; then
        echo "COMPILE_FAIL"
        return
    fi

    # 运行，超时 10 秒
    local rc=0
    timeout 10 "$exe" >/dev/null 2>&1 || rc=$?
    rm -f "$exe"

    if [[ $rc -eq 0 ]]; then
        echo "PASS"
    elif [[ $rc -eq 124 ]]; then
        echo "TIMEOUT"
    else
        echo "RUNTIME_FAIL"
    fi
}
export -f run_execute_test
export DCC TMPDIR_TORTURE

# ── 运行一组测试 ──
run_suite() {
    local category="$1"
    local dir="$SUITE_DIR/$category"
    local runner="$2"

    if [[ ! -d "$dir" ]]; then
        echo "  [$category] 目录不存在，跳过"
        return
    fi

    local pass=0 fail=0 skip=0 total=0
    local -a failures=()
    local tmpfiles_list="$TMPDIR_TORTURE/${category}_list.txt"
    local tmpfiles_results="$TMPDIR_TORTURE/${category}_results.txt"

    # 收集 .c 文件 (仅顶层)
    find "$dir" -maxdepth 1 -name '*.c' | sort > "$tmpfiles_list"
    total=$(wc -l < "$tmpfiles_list")

    if [[ $total -eq 0 ]]; then
        echo "  [$category] 无测试文件"
        return
    fi

    # 过滤跳过列表
    local run_list="$TMPDIR_TORTURE/${category}_run.txt"
    while IFS= read -r src; do
        local name
        name="$(basename "$src")"
        if should_skip "$name"; then
            skip=$((skip + 1))
        else
            echo "$src" >> "$run_list"
        fi
    done < "$tmpfiles_list"

    local run_count=0
    [[ -f "$run_list" ]] && run_count=$(wc -l < "$run_list")

    if [[ $run_count -eq 0 ]]; then
        echo "  [$category] total=$total  skip=$skip  (全部跳过)"
        return
    fi

    # 并行执行
    : > "$tmpfiles_results"
    # 构建 "src\tindex" 列表用于跟踪
    local idx=0
    local idx_file="$TMPDIR_TORTURE/${category}_indexed.txt"
    while IFS= read -r src; do
        echo "$idx $src" >> "$idx_file"
        idx=$((idx + 1))
    done < "$run_list"

    # 使用 xargs 并行
    cat "$run_list" | xargs -P "$JOBS" -I{} bash -c '
        src="$1"
        runner="$2"
        name="$(basename "$src")"
        result=$($runner "$src")
        echo "$name $result"
    ' _ {} "$runner" > "$tmpfiles_results"

    # 统计结果
    while IFS=' ' read -r name result; do
        case "$result" in
            PASS)
                pass=$((pass + 1))
                [[ -n "$VERBOSE" ]] && echo "  PASS  $name"
                ;;
            FAIL|COMPILE_FAIL)
                fail=$((fail + 1))
                failures+=("$name")
                [[ -n "$VERBOSE" ]] && echo "  FAIL  $name"
                ;;
            RUNTIME_FAIL)
                fail=$((fail + 1))
                failures+=("$name (runtime)")
                [[ -n "$VERBOSE" ]] && echo "  FAIL  $name (non-zero exit)"
                ;;
            TIMEOUT)
                fail=$((fail + 1))
                failures+=("$name (timeout)")
                [[ -n "$VERBOSE" ]] && echo "  FAIL  $name (timeout)"
                ;;
        esac
    done < "$tmpfiles_results"

    # 汇总
    echo ""
    echo "  === Torture [$category] ==="
    echo "  total=$total  pass=$pass  fail=$fail  skip=$skip"

    if [[ ${#failures[@]} -gt 0 ]]; then
        echo "  Failures:"
        for f in "${failures[@]}"; do
            echo "    FAIL  $f"
        done
    fi

    # 写入全局结果
    echo "$category pass=$pass fail=$fail skip=$skip" >> "$RESULT_DIR/summary.txt"
    if [[ ${#failures[@]} -gt 0 ]]; then
        echo "1" >> "$RESULT_DIR/has_failures"
    fi
}

# ── 主流程 ──
echo "== GCC C Torture Suite =="
echo "   dcc: $DCC"
echo "   suite: $SUITE_DIR"
echo "   jobs: $JOBS"
echo ""

case "$SUB" in
    compile)
        run_suite "compile" run_compile_test
        ;;
    execute)
        run_suite "execute" run_execute_test
        ;;
    all)
        run_suite "compile" run_compile_test
        echo ""
        run_suite "execute" run_execute_test
        ;;
    *)
        echo "用法: $0 [compile|execute|all]" >&2
        exit 1
        ;;
esac

echo ""

# ── 总汇总 ──
if [[ -f "$RESULT_DIR/summary.txt" ]]; then
    total_pass=0 total_fail=0 total_skip=0
    while read -r _ parts; do
        eval "$parts"
        total_pass=$((total_pass + pass))
        total_fail=$((total_fail + fail))
        total_skip=$((total_skip + skip))
    done < "$RESULT_DIR/summary.txt"
    echo "=== Total: pass=$total_pass  fail=$total_fail  skip=$total_skip ==="
fi

# 有失败则退出非零
if [[ -f "$RESULT_DIR/has_failures" ]]; then
    exit 1
fi
exit 0
