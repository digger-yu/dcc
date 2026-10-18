#!/usr/bin/env bash
# scripts/gcc_dg_test.sh — GCC dg Test Suite 测试驱动
#
# 用法:
#   scripts/gcc_dg_test.sh [compile|run|all] [--dirs sub1[,sub2,...]] [--shard N/M]
#
# 环境变量:
#   DCC          — dcc 编译器路径 (默认：target/release/dcc)
#   DG_DIR       — gcc.dg suite 根目录 (默认：tests/gcc-dg)
#   JOBS         — 并行编译/执行度 (默认：nproc)
#   VERBOSE      — 非空时输出每个测试的详细结果
#   SUBDIRS      — 要测试的子目录 (可选；不指定则使用默认组合)
#
# 分片:
#   --shard N/M   将展开后的文件列表按文件号均匀分成 M 片, 只跑第 N 片 (1..M)。
#                配合 --dirs 可实现"某子目录再拆 N 份"。CI 可用 M=4 并行 4 个 job。
#   例:
#     ./gcc_dg_test.sh run --shard 1/4     # run 集第 1/4 片
#     ./gcc_dg_test.sh compile --dirs tree-ssa --shard 2/4
#
# 默认测试组合:
#   compile: compat,charset,noncompile,format (语言语义 + 负向诊断)
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

# ── 解析命令行参数 ──
SUB="all"
SUBDIRS=""
SHARD_N=""
SHARD_M=""
TOPICS=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        compile|run|all)
            SUB="$1"; shift
            ;;
        --dirs=*)
            SUBDIRS="${1#--dirs=}"; shift
            ;;
        --shard=*)
            shard_val="${1#--shard=}"
            SHARD_N="${shard_val%%/*}"
            SHARD_M="${shard_val#*/}"
            shift
            ;;
        --dirs)
            SUBDIRS="${2:-}"; shift 2
            ;;
        --shard)
            shard_val="${2:-}"
            SHARD_N="${shard_val%%/*}"
            SHARD_M="${shard_val#*/}"
            shift 2
            ;;
        --topics=*)
            TOPICS="${1#--topics=}"; shift
            ;;
        --topics)
            TOPICS="${2:-}"; shift 2
            ;;
        *)
            echo "用法: $0 [compile|run|all] [--dirs sub1[,sub2,...]] [--shard N/M] [--topics a,b,c]" >&2
            exit 1
            ;;
    esac
done

# 校验 shard 参数
if [[ -n "$SHARD_N" || -n "$SHARD_M" ]]; then
    if [[ -z "$SHARD_N" || -z "$SHARD_M" || \
          ! "$SHARD_N" =~ ^[0-9]+$ || ! "$SHARD_M" =~ ^[0-9]+$ || \
          "$SHARD_N" -lt 1 || "$SHARD_N" -gt "$SHARD_M" || "$SHARD_M" -lt 1 ]]; then
        echo "错误: --shard 需要 N/M 格式且 1 <= N <= M" >&2
        exit 1
    fi
fi

# 默认测试子目录组合（如果未指定 --dirs）
DEFAULT_COMPILE_SUBDIRS="compat,charset,noncompile,format"
DEFAULT_RUN_SUBDIRS="torture,tree-ssa"

# ── 前置检查 ──
if [[ ! -x "$DCC" ]]; then
    echo "错误: dcc 不存在或不可执行: $DCC" >&2
    echo "请先运行: cargo build --release" >&2
    exit 1
fi

if [[ ! -d "$DG_DIR" ]]; then
    echo "错误: gcc.dg suite 未安装: $DG_DIR" >&2
    exit 1
fi

# ── 跳过列表 (gcc.dg 版本) ──
declare -a SKIP_PATTERNS=()
if [[ -f "$SKIP_FILE" ]]; then
    while IFS= read -r line; do
        line="${line%%#*}"
        line="${line//[[:space:]]/}"
        [[ -n "$line" ]] && SKIP_PATTERNS+=("$line")
    done < "$SKIP_FILE"
fi

should_skip() {
    local rel_path="$1"
    local base_name
    base_name="$(basename "$rel_path")"
    for pat in "${SKIP_PATTERNS[@]+"${SKIP_PATTERNS[@]}"}"; do
        if [[ "$pat" == *"*" ]]; then
            [[ "$base_name" == ${pat} || "$rel_path" == ${pat} ]] && return 0
        else
            [[ "$base_name" == "$pat" || "$rel_path" == "$pat" ]] && return 0
        fi
    done
    return 1
}
export -f should_skip

# 当前平台的目标三元组 (driver 运行其上的机器). 用于判定 dg 行内 target 选择器.
DCC_TARGET="${DCC_TARGET:-$(uname -m)-pc-linux-gnu}"

# 判断单个 triple 通配是否命中当前目标 (bash glob 语义, '-' 为字面量).
target_matches_one() {
    [[ "$DCC_TARGET" == $1 ]]
}

# 判定一个 dg `target` 选择器是否包含当前目标.
# 语义 (与 gcc dg 一致): 列表里的 pattern 可带 '!' 表示取反.'!' 既可能
# 紧贴 pattern (`!x86_64-*-*`), 也可能是独立词法单元、作用于紧随其后的
# pattern (`{ ! *-*-mingw* }`).因此必须两种写法都认, 否则会把「除 mingw 外
# 都跑」的测试误判成「只在 mingw 跑」而错误跳过.
#   - 存在正 pattern 时: 必须命中至少一个正 pattern, 且不能命中任何负 pattern;
#   - 只有负 pattern 时: 不能命中任何负 pattern 即纳入.
target_included() {
    local any_pos=0 pos_ok=0 neg_ok=0 pending=0
    local tok pat neg
    for tok in $1; do
        if [[ "$tok" == "!" ]]; then
            pending=1
            continue
        fi
        neg=0
        pat="$tok"
        if [[ "$tok" == '!'* ]]; then
            neg=1
            pat="${tok#!}"
        elif (( pending )); then
            neg=1
        fi
        pending=0
        if target_matches_one "$pat"; then
            if (( neg )); then
                neg_ok=1
            else
                any_pos=1
                pos_ok=1
            fi
        else
            (( neg )) || any_pos=1
        fi
    done
    if (( any_pos )); then
        (( pos_ok && ! neg_ok ))
    else
        (( ! neg_ok ))
    fi
}
export -f target_included

# ── 临时目录 ──
TMPDIR_DG=$(mktemp -d /tmp/dcc-gcc-dg.XXXXXX)
trap 'rm -rf "$TMPDIR_DG"' EXIT

# ── 提取 GCC dg 指令 ──
# 输出: "do_type extra_opts" 或 "skip <reason>"
parse_dg_directives() {
    local src="$1"
    local do_type
    do_type=$(grep -oP '\{ dg-do \K[a-z]+' "$src" 2>/dev/null | head -1 || true)
    if [[ -z "$do_type" ]]; then
        do_type="compile"
    fi

    local req_target
    req_target=$(grep -oP '\{ dg-require-effective-target \K[^}]+' "$src" 2>/dev/null | head -1 || true)
    if [[ -n "$req_target" ]]; then
        case "$req_target" in
            x86_64|x86|i?86|*|any|always) ;;
            *)
                echo "skip target:$req_target"
                return
                ;;
        esac
    fi

    if grep -qP '\{ dg-skip-if' "$src" 2>/dev/null; then
        echo "skip dg-skip-if"
        return
    fi

    # 注意: dg-require-iconv 不在此处一刀切跳过. 多数 iconv 测试只是依赖
    # 执行字符集翻译, dcc 仍能正常编译它们 (只有少数使用了虚构汇编助记符
    # 的才编译不了), 那些应逐个列入本文件末尾的手动跳过清单, 避免误伤.

    # 行内 target 选择器, 例如 `dg-do compile { target { *-*-mingw* } }` 或
    # `dg-do compile { target x86_64-*-* }`. 任一 triple 通配命中当前目标则运行,
    # 否则跳过 (这些测试本就不针对本平台). 只处理像 triple 的选择器 (含 '-' 或 '*'),
    # 以免误伤 effective-target 名 (如 nonpic).
    local do_full
    do_full=$(grep -oP '\{ dg-do [^}]*\}' "$src" 2>/dev/null | head -1)
    local pats=""
    if [[ -n "$do_full" && "$do_full" == *target* ]]; then
        pats=$(echo "$do_full" | grep -oP '\btarget\b\s*(\{[^}]*\}|[^{} "]+)' | head -1 \
               | sed -E 's/^target\s*//; s/^[{}]//; s/[}]*$//')
    fi
    if [[ -n "$pats" ]]; then
        if [[ "$pats" == *-* || "$pats" == *'*'* ]]; then
            if ! target_included "$pats"; then
                echo "skip target:$pats"
                return
            fi
        fi
    fi

    local raw_opts=""
    local dg_opts add_opts
    dg_opts=$(grep -oP '\{ dg-options "\K[^"]+' "$src" 2>/dev/null | head -1 || true)
    [[ -n "$dg_opts" ]] && raw_opts="$dg_opts"
    add_opts=$(grep -oP '\{ dg-additional-options "\K[^"]+' "$src" 2>/dev/null | head -1 || true)
    [[ -n "$add_opts" ]] && raw_opts="$raw_opts $add_opts"

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
export -f parse_dg_directives

# ── 并行 worker (由 xargs 调用): 收到一个 "src" 参数和一个 "rel" 参数 ──
# 输出 "rel RESULT detail"
dg_worker() {
    local src="$1"
    local rel="$2"

    local parsed do_type extra_opts
    parsed=$(parse_dg_directives "$src")
    do_type="${parsed%% *}"
    extra_opts="${parsed#* }"

    if [[ "$do_type" == "skip" ]]; then
        echo "$rel SKIP $extra_opts"
        return
    fi
    if [[ "$do_type" == "preprocess" || "$do_type" == "link" ]]; then
        echo "$rel SKIP dg-do:$do_type"
        return
    fi

    if [[ "$category" == "compile" ]]; then
        if [[ "$do_type" != "compile" && "$do_type" != "run" ]]; then
            echo "$rel SKIP dg-do:$do_type"
            return
        fi
        # compat.exp 习惯: scalar-by-value-N.c (无 _x/_y/_main 后缀) 由 harness
        # 自动 -include scalar-by-value-x.h 编译; 这里复刻该行为.
        local compat_inc=""
        if [[ "$rel" == compat/* ]]; then
            local bn
            bn="$(basename "$src")"
            if [[ "$bn" =~ ^scalar-by-value-[0-9]+\.c$ ]]; then
                compat_inc="-I$(dirname "$src") -include scalar-by-value-x.h"
            fi
        fi
        # 负向测试 (dg-error / dg-warning): 代码本应被编译器拒绝或给出诊断,
        # 编译失败 (或非零退出 + 产生诊断信息) 即视为通过.
        local neg=""
        if grep -qE 'dg-(error|warning)' "$src" 2>/dev/null; then
            neg="1"
        fi
        local out="$TMPDIR_DG/$(echo "$rel" | tr '/' '_').o"
        local errf="$TMPDIR_DG/$(echo "$rel" | tr '/' '_').err"
        : > "$errf"
        local rc=0
        { timeout 30 "$DCC" -c -std=gnu11 $extra_opts $compat_inc -o "$out" "$src" 2>"$errf"; } 2>/dev/null || rc=$?
        rm -f "$out"
        if [[ $rc -eq 0 ]]; then
            echo "$rel PASS"
        elif [[ $rc -eq 124 ]]; then
            echo "$rel FAIL timeout"
        elif [[ -n "$neg" ]] && grep -qE 'error|warning' "$errf" 2>/dev/null; then
            echo "$rel PASS"
        else
            echo "$rel FAIL compile"
        fi
    else  # run
        if [[ "$do_type" != "run" ]]; then
            echo "$rel SKIP not-dg-do-run"
            return
        fi
        local exe="$TMPDIR_DG/$(echo "$rel" | tr '/' '_').exe"
        if ! "$DCC" -std=gnu11 -lm $extra_opts -o "$exe" "$src" 2>/dev/null; then
            echo "$rel FAIL compile"
            return
        fi
        local rc=0
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
}
export -f dg_worker

# ── 运行一组测试 ──
run_suite() {
    local category="$1"

    export DCC TMPDIR_DG DG_DIR category

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
    : > "$tmpfiles_list"
    : > "$run_list"

    # 递归收集 .c 文件
    if [[ -n "$dirs" ]]; then
        IFS=',' read -ra DIR_ARRAY <<< "$dirs"
        local find_args=()
        local d
        for d in "${DIR_ARRAY[@]}"; do
            d="$(echo "$d" | xargs)"
            [[ -d "$DG_DIR/$d" ]] && find_args+=("$DG_DIR/$d")
        done
        if [[ ${#find_args[@]} -eq 0 ]]; then
            echo "错误：指定的子目录不存在：$dirs" >&2
            exit 1
        fi
        for d in "${find_args[@]}"; do
            find "$d" -name '*.c' -type f >> "$tmpfiles_list"
        done
        sort -o "$tmpfiles_list" "$tmpfiles_list"
    else
        find "$DG_DIR" -name '*.c' -type f | sort > "$tmpfiles_list"
    fi
    local total
    total=$(wc -l < "$tmpfiles_list")

    if [[ $total -eq 0 ]]; then
        echo "  [$category] 无测试文件"
        return
    fi

    # 计算相对路径 + 过滤跳过列表 + 可选 shard 分片
    local idx=0
    local src rel base
    while IFS= read -r src; do
        idx=$((idx + 1))
        rel="${src#$DG_DIR/}"
        base="$(basename "$src")"
        if should_skip "$rel" || should_skip "$base"; then
            skip=$((skip + 1))
            skipped+=("$rel")
            continue
        fi
        # 主题过滤: --topics 给一组文件名子串 (逗号分隔, 大小写不敏感),
        # 仅保留 basename 命中任一子串的用例。用于把扁平的根目录大杂烩
        # (7549 个 .c) 拆成可独立跑/分析的小块, 例如只跑 C23 特性子集:
        #   --topics bitint,floatn,stdfloat,Float,imaginary,c23,c2y
        if [[ -n "$TOPICS" ]]; then
            local matched=0 tp
            IFS=',' read -ra TOPIC_ARRAY <<< "$TOPICS"
            for tp in "${TOPIC_ARRAY[@]}"; do
                tp="$(echo "$tp" | xargs)"
                [[ -z "$tp" ]] && continue
                if [[ "$base" == *"$tp"* ]]; then
                    matched=1
                    break
                fi
            done
            if [[ $matched -eq 0 ]]; then
                continue
            fi
        fi
        if [[ -n "$SHARD_M" && $(( ((idx - 1) % SHARD_M) + 1 )) -ne "$SHARD_N" ]]; then
            continue
        fi
        printf '%s\t%s\n' "$src" "$rel" >> "$run_list"
    done < "$tmpfiles_list"

    local run_count
    run_count=$(wc -l < "$run_list")

    local shard_note=""
    if [[ -n "$SHARD_M" ]]; then
        shard_note="  shard=$SHARD_N/$SHARD_M"
    fi

    if [[ $run_count -eq 0 ]]; then
        echo "  [$category] total=$total  skip=$skip$shard_note  (本片无测试)"
        return
    fi

    echo "  [$category] total=$total  run=$run_count  skip=$skip  jobs=$JOBS$shard_note"

    # 并行执行: 把 "src TAB rel" 行通过 stdin 喂给 xargs; 每个 worker 收到一行,
    # 在 bash -c 里用 awk 切 tab。用 awk 避免内嵌 tab 字面量的引号问题。
    : > "$tmpfiles_results"
    local worker_cmd="$TMPDIR_DG/${category}_parallel.sh"
    cat > "$worker_cmd" <<'EOF'
#!/usr/bin/env bash
line="$1"
src="${line%%	*}"
rel="${line##*	}"
dg_worker "$src" "$rel"
EOF
    chmod +x "$worker_cmd"
    < "$run_list" xargs -d '\n' -L1 -P "$JOBS" "$worker_cmd" \
        > "$tmpfiles_results" 2>/dev/null || true

    # 统计结果
    local line rel_str result detail
    while IFS= read -r line; do
        rel_str="${line%% *}"
        local rest="${line#* }"
        result="${rest%% *}"
        detail="${rest#* }"

        case "$result" in
            PASS)
                pass=$((pass + 1))
                [[ -n "$VERBOSE" ]] && echo "  PASS  $rel_str"
                ;;
            FAIL)
                fail=$((fail + 1))
                failures+=("$rel_str ($detail)")
                [[ -n "$VERBOSE" ]] && echo "  FAIL  $rel_str ($detail)"
                ;;
            SKIP)
                skip=$((skip + 1))
                skipped+=("$rel_str ($detail)")
                [[ -n "$VERBOSE" ]] && echo "  SKIP  $rel_str ($detail)"
                ;;
        esac
    done < "$tmpfiles_results"

    echo ""
    echo "  === gcc.dg [$category] ==="
    echo "  total=$total  pass=$pass  fail=$fail  skip=$skip"

    if [[ ${#failures[@]} -gt 0 ]]; then
        echo "  Failures:"
        for f in "${failures[@]}"; do
            echo "    FAIL  $f"
        done
    fi

    if [[ ${#skipped[@]} -gt 0 && -z "$VERBOSE" ]]; then
        echo "  Skipped (${#skipped[@]}):"
        for s in "${skipped[@]:0:20}"; do
            echo "    SKIP  $s"
        done
        if [[ ${#skipped[@]} -gt 20 ]]; then
            echo "    ... (还有 $(( ${#skipped[@]} - 20 )) 条)"
        fi
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
[[ -n "$SHARD_M" ]] && echo "   shard: $SHARD_N/$SHARD_M"
echo ""

case "$SUB" in
    compile)
        run_suite "compile"
        ;;
    run)
        run_suite "run"
        ;;
    all)
        run_suite "compile"
        echo ""
        run_suite "run"
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

if [[ -f "$TMPDIR_DG/has_failures" ]]; then
    exit 1
fi
exit 0
