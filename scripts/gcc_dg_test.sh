#!/usr/bin/env bash
# scripts/gcc_dg_test.sh — GCC dg Test Suite 测试驱动
#
# 用法:
#   scripts/gcc_dg_test.sh [compile|run|all]
#
# 环境变量:
#   DCC          — dcc 编译器路径 (默认: target/release/dcc)
#   DG_DIR       — gcc.dg suite 根目录 (默认: tests/gcc-torture/gcc/testsuite/gcc.dg)
#   JOBS         — 并行度 (默认: nproc)
#   VERBOSE      — 非空时输出每个测试的详细结果
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

DCC="${DCC:-$REPO_ROOT/target/release/dcc}"
DG_DIR="${DG_DIR:-$REPO_ROOT/tests/gcc-torture/gcc/testsuite/gcc.dg}"
JOBS="${JOBS:-$(nproc 2>/dev/null || echo 4)}"
VERBOSE="${VERBOSE:-}"
SKIP_FILE="$REPO_ROOT/tests/gcc_dg_skip.txt"

SUB="${1:-all}"

# ── 前置检查 ──
if [[ ! -x "$DCC" ]]; then
    echo "错误: dcc 不存在或不可执行: $DCC" >&2
    echo "请先运行: cargo build --release" >&2
    exit 1
fi

if [[ ! -d "$DG_DIR" ]]; then
    echo "错误: gcc.dg suite 未安装: $DG_DIR" >&2
    echo "请先运行 torture workflow (会自动 sparse-checkout gcc.dg)" >&2
    exit 1
fi

# ── 跳过列表 (gcc.dg 版本) ──
declare -a SKIP_PATTERNS=()
if [[ -f "$SKIP_FILE" ]]; then
    while IFS= read -r line; do
        line="${line%%#*}"           # 去注释
        line="${line//[[:space:]]/}" # 去空白
        [[ -n "$line" ]] && SKIP_PATTERNS+=("$line")
    done < "$SKIP_FILE"
fi

should_skip() {
    local rel_path="$1"
    local base_name
    base_name="$(basename "$rel_path")"
    for pat in "${SKIP_PATTERNS[@]+"${SKIP_PATTERNS[@]}"}"; do
        # 支持 prefix* glob、精确匹配、以及路径匹配
        if [[ "$pat" == *"*" ]]; then
            [[ "$base_name" == ${pat} || "$rel_path" == ${pat} ]] && return 0
        else
            [[ "$base_name" == "$pat" || "$rel_path" == "$pat" ]] && return 0
        fi
    done
    return 1
}

# ── 临时目录 ──
TMPDIR_DG=$(mktemp -d /tmp/dcc-gcc-dg.XXXXXX)
trap 'rm -rf "$TMPDIR_DG"' EXIT

# ── 提取 GCC dg 指令 ──
# 返回: do_type extra_opts skip_reason
# do_type: compile / run / link / preprocess / unknown / skip
parse_dg_directives() {
    local src="$1"
    local do_type=""
    local raw_opts=""
    local skip_reason=""

    # ── dg-do ──
    do_type=$(grep -oP '\{ dg-do \K[a-z]+' "$src" 2>/dev/null | head -1 || true)
    if [[ -z "$do_type" ]]; then
        # 没有 dg-do 指令，默认当作 compile
        do_type="compile"
    fi

    # ── dg-require-effective-target ──
    local req_target
    req_target=$(grep -oP '\{ dg-require-effective-target \K[^}]+' "$src" 2>/dev/null | head -1 || true)
    if [[ -n "$req_target" ]]; then
        # 只支持 x86_64 / x86 / * / any 相关
        case "$req_target" in
            x86_64|x86|i?86|*|any|always) ;;
            *)
                # 其他目标 (arm/powerpc/mips/riscv 等) 跳过
                skip_reason="target:$req_target"
                echo "skip $skip_reason"
                return
                ;;
        esac
    fi

    # ── dg-skip-if ── (简化处理: 跳过所有带 skip-if 的测试)
    if grep -qP '\{ dg-skip-if' "$src" 2>/dev/null; then
        skip_reason="dg-skip-if"
        echo "skip $skip_reason"
        return
    fi

    # ── dg-options ──
    local dg_opts
    dg_opts=$(grep -oP '\{ dg-options "\K[^"]+' "$src" 2>/dev/null | head -1 || true)
    if [[ -n "$dg_opts" ]]; then
        raw_opts="$dg_opts"
    fi

    # ── dg-additional-options ──
    local add_opts
    add_opts=$(grep -oP '\{ dg-additional-options "\K[^"]+' "$src" 2>/dev/null | head -1 || true)
    if [[ -n "$add_opts" ]]; then
        raw_opts="$raw_opts $add_opts"
    fi

    # 只保留 dcc 支持的选项
    local filtered=""
    local opt
    for opt in $raw_opts; do
        case "$opt" in
            -finstrument-functions|-fno-instrument-functions|-std=gnu89|-std=c89|\
            -std=c99|-std=gnu99|-std=c11|-std=gnu11|-O0|-O1|-O2|-O3|-Os|\
            -Wall|-Wextra|-w|-pedantic|-include\ *|\
            -mavx|-mavx2|-msse|-msse2|-msse3|-msse4.1|-msse4.2|\
            -no-pie|-fno-pie|-pie|\
            -fno-strict-aliasing|-fno-builtin|\
            -ffreestanding|--coverage|-fprofile-arcs|\
            -pg|-p|\
            -g|-g0|-g1|-g2|-g3|\
            -lm|-ldl|-lpthread|-lrt)
                filtered="$filtered $opt"
                ;;
            -ffunction-sections|-fdata-sections|-Wl,*)
                filtered="$filtered $opt"
                ;;
        esac
    done

    echo "$do_type $filtered"
}
export -f parse_dg_directives DCC TMPDIR_DG

# ── compile 测试函数 ──
run_dg_compile_test() {
    local src="$1"
    local rel_path="$2"
    local out="$TMPDIR_DG/$(echo "$rel_path" | tr '/' '_').o"

    local parsed
    parsed=$(parse_dg_directives "$src")
    local do_type="${parsed%% *}"
    local extra_opts="${parsed#* }"

    if [[ "$do_type" == "skip" ]]; then
        local reason="$extra_opts"
        echo "SKIP $reason"
        return
    fi

    if [[ "$do_type" == "preprocess" || "$do_type" == "link" ]]; then
        echo "SKIP dg-do:$do_type"
        return
    fi

    if [[ "$do_type" != "compile" && "$do_type" != "run" ]]; then
        echo "SKIP dg-do:$do_type"
        return
    fi

    local rc=0
    timeout 30 "$DCC" -c -std=gnu11 $extra_opts -o "$out" "$src" 2>/dev/null || rc=$?
    if [[ $rc -eq 0 ]]; then
        rm -f "$out"
        echo "PASS"
    elif [[ $rc -eq 124 ]]; then
        rm -f "$out"
        echo "FAIL timeout"
    else
        rm -f "$out"
        echo "FAIL compile"
    fi
}
export -f run_dg_compile_test

# ── run 测试函数 ──
run_dg_run_test() {
    local src="$1"
    local rel_path="$2"
    local exe="$TMPDIR_DG/$(echo "$rel_path" | tr '/' '_').exe"

    local parsed
    parsed=$(parse_dg_directives "$src")
    local do_type="${parsed%% *}"
    local extra_opts="${parsed#* }"

    if [[ "$do_type" == "skip" ]]; then
        local reason="$extra_opts"
        echo "SKIP $reason"
        return
    fi

    if [[ "$do_type" == "preprocess" || "$do_type" == "link" ]]; then
        echo "SKIP dg-do:$do_type"
        return
    fi

    if [[ "$do_type" != "run" ]]; then
        echo "SKIP not-dg-do-run"
        return
    fi

    if ! "$DCC" -std=gnu11 -lm $extra_opts -o "$exe" "$src" 2>/dev/null; then
        echo "FAIL compile"
        return
    fi

    # 运行，超时 10 秒
    local rc=0
    timeout 10 "$exe" >/dev/null 2>&1 || rc=$?
    rm -f "$exe"

    if [[ $rc -eq 0 ]]; then
        echo "PASS"
    elif [[ $rc -eq 124 ]]; then
        echo "FAIL timeout"
    else
        echo "FAIL runtime"
    fi
}
export -f run_dg_run_test

# ── 运行一组测试 ──
run_suite() {
    local category="$1"   # compile / run
    local runner="$2"

    local pass=0 fail=0 skip=0 total=0
    local -a failures=()
    local -a skipped=()

    local tmpfiles_list="$TMPDIR_DG/${category}_list.txt"
    local tmpfiles_results="$TMPDIR_DG/${category}_results.txt"
    local run_list="$TMPDIR_DG/${category}_run.txt"

    # 递归收集 .c 文件 (排除子目录中的其他目录)
    find "$DG_DIR" -name '*.c' -type f | sort > "$tmpfiles_list"
    total=$(wc -l < "$tmpfiles_list")

    if [[ $total -eq 0 ]]; then
        echo "  [$category] 无测试文件"
        return
    fi

    # 计算相对路径 + 过滤跳过列表
    while IFS= read -r src; do
        local rel="${src#$DG_DIR/}"
        local base
        base="$(basename "$src")"
        if should_skip "$rel" || should_skip "$base"; then
            skip=$((skip + 1))
            skipped+=("$rel")
        else
            echo "$src $rel" >> "$run_list"
        fi
    done < "$tmpfiles_list"

    local run_count=0
    [[ -f "$run_list" ]] && run_count=$(wc -l < "$run_list")

    if [[ $run_count -eq 0 ]]; then
        echo "  [$category] total=$total  skip=$skip  (全部跳过)"
        return
    fi

    echo "  [$category] total=$total  run=$run_count  skip=$skip  jobs=$JOBS"

    # 并行执行
    : > "$tmpfiles_results"
    cat "$run_list" | xargs -P "$JOBS" -I{} bash -c '
        entry="$1"
        src="${entry% *}"
        rel="${entry#* }"
        runner="$2"
        result=$($runner "$src" "$rel")
        echo "$rel $result"
    ' _ {} "$runner" > "$tmpfiles_results"

    # 统计结果
    local skip_reason_count=0
    while IFS= read -r line; do
        local rel="${line%% *}"
        local rest="${line#* }"
        local result="${rest%% *}"
        local detail="${rest#* }"

        case "$result" in
            PASS)
                pass=$((pass + 1))
                [[ -n "$VERBOSE" ]] && echo "  PASS  $rel"
                ;;
            FAIL)
                fail=$((fail + 1))
                failures+=("$rel ($detail)")
                [[ -n "$VERBOSE" ]] && echo "  FAIL  $rel ($detail)"
                ;;
            SKIP)
                skip=$((skip + 1))
                skipped+=("$rel ($detail)")
                [[ -n "$VERBOSE" ]] && echo "  SKIP  $rel ($detail)"
                ;;
        esac
    done < "$tmpfiles_results"

    # 汇总输出
    echo ""
    echo "  === gcc.dg [$category] ==="
    echo "  total=$total  pass=$pass  fail=$fail  skip=$skip"

    if [[ ${#failures[@]} -gt 0 ]]; then
        echo "  Failures:"
        for f in "${failures[@]}"; do
            echo "    FAIL  $f"
        done
    fi

    if [[ ${#skipped[@]} -gt 0 ]]; then
        echo "  Skipped:"
        for s in "${skipped[@]}"; do
            echo "    SKIP  $s"
        done
    fi

    # 写入全局结果
    echo "$category pass=$pass fail=$fail skip=$skip" >> "$TMPDIR_DG/summary.txt"
    if [[ ${#failures[@]} -gt 0 ]]; then
        echo "1" >> "$TMPDIR_DG/has_failures"
    fi
}

# ── 主流程 ──
echo "== GCC dg Test Suite =="
echo "   dcc: $DCC"
echo "   suite: $DG_DIR"
echo "   jobs: $JOBS"
echo ""

case "$SUB" in
    compile)
        run_suite "compile" run_dg_compile_test
        ;;
    run)
        run_suite "run" run_dg_run_test
        ;;
    all)
        run_suite "compile" run_dg_compile_test
        echo ""
        run_suite "run" run_dg_run_test
        ;;
    *)
        echo "用法: $0 [compile|run|all]" >&2
        exit 1
        ;;
esac

echo ""

# ── 总汇总 ──
if [[ -f "$TMPDIR_DG/summary.txt" ]]; then
    total_pass=0 total_fail=0 total_skip=0
    while read -r _ parts; do
        eval "$parts"
        total_pass=$((total_pass + pass))
        total_fail=$((total_fail + fail))
        total_skip=$((total_skip + skip))
    done < "$TMPDIR_DG/summary.txt"
    echo "=== Total: pass=$total_pass  fail=$total_fail  skip=$total_skip ==="
fi

# 有失败则退出非零
if [[ -f "$TMPDIR_DG/has_failures" ]]; then
    exit 1
fi
exit 0
