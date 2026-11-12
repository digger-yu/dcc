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
SKIP_FILE="$REPO_ROOT/tests/skip/torture_skip.txt"

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

# 该用例的 dg-do target 条件在 x86_64 宿主上是否成立。
#
# GCC testsuite 只运行 target 条件成立的用例：`{ target mips*-*-* }` 在 x86_64
# 上根本不参与，而 `{ target { { i?86-*-* x86_64-*-* } && { ! ia32 } } }` 参与。
# dcc 的 torture 驱动直接遍历整个目录，必须自己补这一步，否则会把「目标专属
# 语料」误记成跳过（mipscop-1..4）。判据只认三元组族与几个宿主特性名；认不出
# 的条件按「适用」处理（保持原有覆盖），所以排除只会来自明确写出的非宿主目标。
dg_target_applicable() {
    local src="$1"
    local expr
    expr=$(grep -m1 -oP '\{ dg-do\s+\w+[^}]*\{ target\s+\K[^}]*' "$src" 2>/dev/null || true)
    [[ -n "$expr" ]] || return 0
    case "$expr" in
        *x86_64*|*i\?86*|*ia32*|*int128*|*int32plus*|*ptr32plus*|*llp64*|*asm_goto_with_outputs*)
            return 0 ;;
    esac
    case "$expr" in
        *mips*|*powerpc*|*arm*|*aarch64*|*avr*|*s390*|*sparc*|*riscv*|*sh*-*-*|*alpha*|*hppa*|*ia64*|*loongarch*|*m68k*)
            return 1 ;;
    esac
    return 0
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
TMPDIR_TORTURE=$(mktemp -d /tmp/dcc-torture.XXXXXX)
trap 'rm -rf "$TMPDIR_TORTURE"' EXIT

# ── 结果收集 ──
RESULT_DIR="$TMPDIR_TORTURE/results"
mkdir -p "$RESULT_DIR"

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
    # 只保留 dcc 支持的选项: -finstrument-functions, -fno-instrument-functions, -std=gnu89
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

# ── compile 测试 ──
run_compile_test() {
    local src="$1"
    local name
    name="${src##*/}"
    local out="$TMPDIR_TORTURE/${name}.o"

    local extra_opts
    extra_opts=$(extract_dg_options "$src")

    local rc=0
    local errf="$TMPDIR_TORTURE/${name}.err"
    timeout 30 "$DCC" -c -std=gnu11 $extra_opts -o "$out" "$src" 2>"$errf" || rc=$?
    if [[ $rc -eq 0 ]]; then
        rm -f "$out"
        echo "PASS" 
    elif [[ $rc -eq 124 ]]; then
        rm -f "$out"
        echo "TIMEOUT"
    elif grep -q 'error\[E0311\]' "$errf" \
        && timeout 30 "$DCC" -S -std=gnu11 $extra_opts -o "${out%.o}.s" "$src" 2>/dev/null; then
        # GCC 的 `dg-do compile` 判据是「生成汇编」, 不要求汇编器接受模板
        # （920521-1 的 `asm("f")` 连 gas 也拒绝）。dcc 的 -c 要自编码, E0311
        # 说明指令表里没有这条; 而 -S 能把原文写进 GAS 文本 —— 与 GCC 的
        # compile 判据一致, 不放宽其它任何失败。
        rm -f "$out" "${out%.o}.s"
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
    name="${src##*/}"
    name="${name%.c}"
    local exe="$TMPDIR_TORTURE/${name}.exe"

    local extra_opts
    extra_opts=$(extract_dg_options "$src")

    if ! "$DCC" -std=gnu11 -lm $extra_opts -o "$exe" "$src" 2>/dev/null; then
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

    local pass=0 fail=0 skip=0 na=0 total=0
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
    # 只有极少数用例 #include 了 gcc.c-torture 之外的兄弟文件。先一次性
    # 挑出来, 逐例过滤时不再为每个 .c 启动一次 grep。
    local -A ext_include_tests=()
    while IFS= read -r src; do
        ext_include_tests["$src"]=1
    done < <(grep -rlP '#\s*include\s+"\.\./' "$dir" --include='*.c' 2>/dev/null)

    local run_list="$TMPDIR_TORTURE/${category}_run.txt"
    while IFS= read -r src; do
        local name
        name="${src##*/}"
        if should_skip "$name" \
            || { [[ -n "${ext_include_tests[$src]:-}" ]] && sibling_include_missing "$src"; }; then
            skip=$((skip + 1))
        elif ! dg_target_applicable "$src"; then
            # 目标条件不成立：GCC testsuite 在 x86_64 上同样不运行它，
            # 从适用总数里扣掉，但不记成「跳过」（不是 dcc 的缺口）。
            na=$((na + 1))
        else
            echo "$src" >> "$run_list"
        fi
    done < "$tmpfiles_list"
    total=$((total - na))

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
        name="${src##*/}"
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
    echo "  total=$total  pass=$pass  fail=$fail  skip=$skip  na=$na"

    if [[ ${#failures[@]} -gt 0 ]]; then
        echo "  Failures:"
        for f in "${failures[@]}"; do
            echo "    FAIL  $f"
        done
    fi

    # 写入全局结果
    echo "$category pass=$pass fail=$fail skip=$skip na=$na" >> "$RESULT_DIR/summary.txt"
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
    total_pass=0 total_fail=0 total_skip=0 total_na=0
    while read -r _ parts; do
        eval "$parts"
        total_pass=$((total_pass + pass))
        total_fail=$((total_fail + fail))
        total_skip=$((total_skip + skip))
        total_na=$((total_na + ${na:-0}))
    done < "$RESULT_DIR/summary.txt"
    echo "=== Total: pass=$total_pass  fail=$total_fail  skip=$total_skip  na=$total_na ==="
fi

# 有失败则退出非零
if [[ -f "$RESULT_DIR/has_failures" ]]; then
    exit 1
fi
exit 0
