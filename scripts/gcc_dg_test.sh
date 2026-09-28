#!/usr/bin/env bash
# scripts/gcc_dg_test.sh — GCC dg Test Suite 测试驱动
#
# 用法:
#   scripts/gcc_dg_test.sh [compile|run|all] [--dirs subdir1[,subdir2,...]]
#
# 环境变量:
#   DCC          — dcc 编译器路径 (默认：target/release/dcc)
#   DG_DIR       — gcc.dg suite 根目录 (默认：tests/gcc-dg)
#   JOBS         — 并行度 (默认：nproc)
#   VERBOSE      — 非空时输出每个测试的详细结果
#   SUBDIRS      — 要测试的子目录 (可选，多个用逗号分隔；不指定则使用默认组合)
#
# 默认测试组合:
#   compile: compat,charset,noncompile,format (语言语义 + 负向诊断；cpp 需用 --dirs 手动指定)
#   run:     torture,tree-ssa (可执行正确性)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

DCC="${DCC:-$REPO_ROOT/target/release/dcc}"
DG_DIR="${DG_DIR:-$REPO_ROOT/tests/gcc-dg}"
# 兼容两种套件布局:
#   A) git sparse-checkout 布局 (CI): $DG_DIR/gcc/testsuite/gcc.dg/
#   B) 本地扁平布局:                 $DG_DIR/
if [[ -d "$DG_DIR/gcc/testsuite/gcc.dg" ]]; then
    DG_DIR="$DG_DIR/gcc/testsuite/gcc.dg"
fi
JOBS="${JOBS:-$(nproc 2>/dev/null || echo 4)}"
VERBOSE="${VERBOSE:-}"
SKIP_FILE="$REPO_ROOT/tests/skip/gcc_dg_skip.txt"
SUB="${1:-all}"

# ── 解析命令行参数 ──
case "$SUB" in
    compile|run|all)
        ;;
    --*)
        echo "用法：$0 [compile|run|all] [--dirs subdir1[,subdir2,...]]" >&2
        exit 1
        ;;
    *)
        echo "用法：$0 [compile|run|all] [--dirs subdir1[,subdir2,...]]" >&2
        exit 1
        ;;
esac

# 检查是否有 --dirs 参数
for arg in "$@"; do
    if [[ "$arg" == --dirs* ]]; then
        SUBDIRS="${arg#--dirs=}"
        break
    fi
    if [[ "$arg" == "--dirs" && "$2" != "compile" && "$2" != "run" && "$2" != "all" ]]; then
        SUBDIRS="$2"
        break
    fi
done

# 默认测试子目录组合（如果未指定 --dirs）
DEFAULT_COMPILE_SUBDIRS="compat,charset,noncompile,format"
DEFAULT_RUN_SUBDIRS="torture,tree-ssa"
SUBDIRS="${SUBDIRS:-}"

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
    { timeout 30 "$DCC" -c -std=gnu11 $extra_opts -o "$out" "$src" 2>/dev/null; } 2>/dev/null || rc=$?
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
    { timeout 10 "$exe" >/dev/null 2>&1; } 2>/dev/null || rc=$?
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

    # 导出给子进程使用
    export DCC TMPDIR_DG
    
    # 根据测试类型决定默认的子目录（用户未通过 --dirs 指定时）
    local dirs="$SUBDIRS"
    if [[ -z "$dirs" ]]; then
        if [[ "$category" == "compile" ]]; then
            dirs="$DEFAULT_COMPILE_SUBDIRS"
        else
            dirs="$DEFAULT_RUN_SUBDIRS"
        fi
    fi
    local -a failures=()
    local -a skipped=()
    local pass=0 fail=0 skip=0

    local tmpfiles_list="$TMPDIR_DG/${category}_list.txt"
    local tmpfiles_results="$TMPDIR_DG/${category}_results.txt"
    local run_list="$TMPDIR_DG/${category}_run.txt"

    # 递归收集 .c 文件
    # 如果指定了 dirs，则只搜索这些子目录
    if [[ -n "$dirs" ]]; then
        # 将逗号分隔的子目录转换为空格分隔的数组
        IFS=',' read -ra DIR_ARRAY <<< "$dirs"
        find_args=()
        for dir in "${DIR_ARRAY[@]}"; do
            dir="$(echo "$dir" | xargs)"  # 去除首尾空格
            [[ -d "$DG_DIR/$dir" ]] && find_args+=("$DG_DIR/$dir")
        done
        
        if [[ ${#find_args[@]} -eq 0 ]]; then
            echo "错误：指定的子目录不存在：$dirs"
            exit 1
        fi
        
        # 在这些子目录中递归查找 .c 文件
        : > "$tmpfiles_list"
        for dir in "${find_args[@]}"; do
            find "$dir" -name '*.c' -type f >> "$tmpfiles_list"
        done
        sort -o "$tmpfiles_list" "$tmpfiles_list"
    else
        # 默认：搜索整个 DG_DIR
        find "$DG_DIR" -name '*.c' -type f | sort > "$tmpfiles_list"
    fi
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

    # 并行执行 - 直接嵌入函数定义而不是使用 export -f
    : > "$tmpfiles_results"
    cat "$run_list" | while IFS= read -r line; do
        src="${line% *}"
        rel="${line#* }"
        
        # 编译测试
        if [[ "$category" == "compile" ]]; then
            out="$TMPDIR_DG/$(echo "$rel" | tr '/' '_').o"
            parsed=$(parse_dg_directives "$src")
            do_type="${parsed%% *}"
            extra_opts="${parsed#* }"
            
            if [[ "$do_type" == "skip" ]]; then
                echo "$rel SKIP $extra_opts"
            elif [[ "$do_type" == "preprocess" || "$do_type" == "link" ]]; then
                echo "$rel SKIP dg-do:$do_type"
            elif [[ "$do_type" != "compile" && "$do_type" != "run" ]]; then
                echo "$rel SKIP dg-do:$do_type"
            else
                rc=0
                { timeout 30 "$DCC" -c -std=gnu11 $extra_opts -o "$out" "$src" 2>/dev/null; } 2>/dev/null || rc=$?
                rm -f "$out"
                if [[ $rc -eq 0 ]]; then
                    echo "$rel PASS"
                elif [[ $rc -eq 124 ]]; then
                    echo "$rel FAIL timeout"
                else
                    echo "$rel FAIL compile"
                fi
            fi
        # 运行测试
        elif [[ "$category" == "run" ]]; then
            exe="$TMPDIR_DG/$(echo "$rel" | tr '/' '_').exe"
            parsed=$(parse_dg_directives "$src")
            do_type="${parsed%% *}"
            extra_opts="${parsed#* }"
            
            if [[ "$do_type" == "skip" ]]; then
                echo "$rel SKIP $extra_opts"
            elif [[ "$do_type" == "preprocess" || "$do_type" == "link" ]]; then
                echo "$rel SKIP dg-do:$do_type"
            elif [[ "$do_type" != "run" ]]; then
                echo "$rel SKIP not-dg-do-run"
            else
                if ! "$DCC" -std=gnu11 -lm $extra_opts -o "$exe" "$src" 2>/dev/null; then
                    echo "$rel FAIL compile"
                    continue
                fi
                
                rc=0
                { timeout 10 "$exe" >/dev/null 2>&1; } 2>/dev/null || rc=$?
                rm -f "$exe"
                
                if [[ $rc -eq 0 ]]; then
                    echo "$rel PASS"
                elif [[ $rc -eq 124 ]]; then
                    echo "$rel FAIL timeout"
                else
                    echo "$rel FAIL runtime"
                fi
            fi
        fi
    done > "$tmpfiles_results"

    # 如果启用了并行，可以用 GNU parallel 或其他工具替代

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
