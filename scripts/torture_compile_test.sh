#!/usr/bin/env bash
# scripts/torture_compile_test.sh — GCC C Torture Suite 编译测试
#
# 用法:
#   scripts/torture_compile_test.sh
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
SKIP_FILE="$REPO_ROOT/tests/skip/torture_skip.txt"

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

# 测试可能 #include ../../gcc.dg/... 或 ../../gcc.target/... 这类
# gcc.c-torture 之外的兄弟文件。文件缺失时 (套件子集/稀疏检出没带)
# 自动跳过, 避免把「套件缺文件」算成编译器失败; 文件齐备时返回 1
# (不跳过), 测试正常参与编译。
sibling_include_missing() {
    local src="$1" dir resolved inc
    dir="$(dirname "$src")"
    while IFS= read -r inc; do
        [[ "$inc" == ../* ]] || continue
        resolved="$dir/$inc"
        [[ -f "$resolved" ]] || return 0
    done < <(grep -oP '#\s*include\s+"\K[^"]+' "$src" 2>/dev/null)
    return 1
}

# ── 临时目录 ──
TMPDIR_TORTURE=$(mktemp -d /tmp/dcc-torture-compile.XXXXXX)
trap 'rm -rf "$TMPDIR_TORTURE"' EXIT

# ── 提取 GCC dg-options 指令 (仅保留 dcc 支持的选项) ──
extract_dg_options() {
    local src="$1"
    local raw_opts=""
    local dg_opts
    dg_opts=$(grep -oP '\{ dg-options "\K[^"]+' "$src" 2>/dev/null | head -1)
    if [[ -n "$dg_opts" ]]; then
        raw_opts="$dg_opts"
    fi
    local add_opts
    add_opts=$(grep -oP '\{ dg-additional-options "\K[^"]+' "$src" 2>/dev/null | head -1)
    if [[ -n "$add_opts" ]]; then
        raw_opts="$raw_opts $add_opts"
    fi
    local filtered=""
    local opt
    for opt in $raw_opts; do
        case "$opt" in
            -finstrument-functions|-fno-instrument-functions|-std=gnu89)
                filtered="$filtered $opt"
                ;;
        esac
    done
    echo "$filtered"
}
export -f extract_dg_options

# ── compile 测试函数 ──
run_compile_test() {
    local src="$1"
    local name
    name="${src##*/}"
    local out="$TMPDIR_TORTURE/${name}.o"

    local extra_opts
    extra_opts=$(extract_dg_options "$src")

    local rc=0
    timeout 30 "$DCC" -c -std=gnu11 $extra_opts -o "$out" "$src" 2>/dev/null || rc=$?
    if [[ $rc -eq 0 ]]; then
        rm -f "$out"
        echo "PASS" 
    elif [[ $rc -eq 124 ]]; then
        rm -f "$out"
        echo "TIMEOUT"
    else
        rm -f "$out"
        echo "FAIL"
    fi
}
export -f run_compile_test
export DCC TMPDIR_TORTURE

# ── 主流程 ──
echo "== GCC C Torture Suite - Compile Tests =="
echo "   dcc: $DCC"
echo "   suite: $SUITE_DIR/compile"
echo "   jobs: $JOBS"
echo ""

category="compile"
dir="$SUITE_DIR/$category"

if [[ ! -d "$dir" ]]; then
    echo "错误: 编译测试目录不存在: $dir" >&2
    exit 1
fi

pass=0 fail=0 skip=0 total=0
declare -a failures=()

# 收集 .c 文件 (仅顶层)
find "$dir" -maxdepth 1 -name '*.c' | sort > "$TMPDIR_TORTURE/file_list.txt"
total=$(wc -l < "$TMPDIR_TORTURE/file_list.txt")

if [[ $total -eq 0 ]]; then
    echo "无测试文件"
    exit 0
fi

# 过滤跳过列表
# 只有极少数用例 #include 了 gcc.c-torture 之外的兄弟文件。先一次性
# 挑出来, 逐例过滤时不再为每个 .c 启动一次 grep。
declare -A ext_include_tests=()
while IFS= read -r src; do
    ext_include_tests["$src"]=1
done < <(grep -rlP '#\s*include\s+"\.\./' "$dir" --include='*.c' 2>/dev/null)

declare -a skipped_names=()
while IFS= read -r src; do
    name="${src##*/}"
    if should_skip "$name" \
        || { [[ -n "${ext_include_tests[$src]:-}" ]] && sibling_include_missing "$src"; }; then
        skip=$((skip + 1))
        skipped_names+=("$name")
    else
        echo "$src" >> "$TMPDIR_TORTURE/run_list.txt"
    fi
done < "$TMPDIR_TORTURE/file_list.txt"

run_count=0
[[ -f "$TMPDIR_TORTURE/run_list.txt" ]] && run_count=$(wc -l < "$TMPDIR_TORTURE/run_list.txt")

if [[ $run_count -eq 0 ]]; then
    echo "total=$total  skip=$skip  (全部跳过)"
    exit 0
fi

# 并行执行测试
: > "$TMPDIR_TORTURE/results.txt"
cat "$TMPDIR_TORTURE/run_list.txt" | xargs -P "$JOBS" -I{} bash -c '
    src="$1"
    name="${src##*/}"
    result=$(run_compile_test "$src")
    echo "$name $result"
' _ {} > "$TMPDIR_TORTURE/results.txt"

# 统计结果
while IFS=' ' read -r name result; do
    case "$result" in
        PASS)
            pass=$((pass + 1))
            [[ -n "$VERBOSE" ]] && echo "  PASS  $name"
            ;;
        FAIL)
            fail=$((fail + 1))
            failures+=("$name")
            [[ -n "$VERBOSE" ]] && echo "  FAIL  $name"
            ;;
        TIMEOUT)
            fail=$((fail + 1))
            failures+=("$name (timeout)")
            [[ -n "$VERBOSE" ]] && echo "  FAIL  $name (timeout)"
            ;;
    esac
done < "$TMPDIR_TORTURE/results.txt"

# 汇总
echo ""
echo "=== Torture [compile] ==="
echo "total=$total  pass=$pass  fail=$fail  skip=$skip"

if [[ ${#failures[@]} -gt 0 ]]; then
    echo "Failures:"
    for f in "${failures[@]}"; do
        echo "  FAIL  $f"
    done
fi

if [[ ${#skipped_names[@]} -gt 0 ]]; then
    echo "Skipped:"
    for s in "${skipped_names[@]}"; do
        echo "  SKIP  $s"
    done
fi

echo ""
echo "=== Total: pass=$pass  fail=$fail  skip=$skip ==="

# 有失败则退出非零
if [[ ${#failures[@]} -gt 0 ]]; then
    exit 1
fi
exit 0
