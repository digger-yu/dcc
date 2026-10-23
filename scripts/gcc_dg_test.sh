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
#   --shard N/M   将展开后的文件列表按文件序号**连续**切成 M 片, 只跑第 N 片 (1..M)。
#                 连续切分使字母序相邻的相关用例 (同一特性区域) 落在同一片。
#                配合 --dirs 可实现"某子目录再拆 N 份"。
#   本仓库约定切 **8 片** (M=8): 本地分批/调试用 `--shard N/8`, N=1..8;
#   例如 shard 3 = `--shard 3/8` (第 3/8 片)。CI 若要并行可自行改 M。
#   例:
#     ./gcc_dg_test.sh all --shard 3/8          # all 集第 3/8 片 (8 片约定)
#     ./gcc_dg_test.sh run --shard 1/8          # run 集第 1/8 片
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
        # 修剪 (grep 可能带出尾随空白)
        req_target="${req_target//[[:space:]]/}"
        # 仅 x86 家族 / 显式 "any"/"always" 视为本平台可用;
        # 其余 effective-target (float16_runtime / bfloat16_runtime / dfp /
        # fenv_exceptions / trapping / int128 / __float128 等) 一律视为非本
        # 平台, 自动跳过.
        # 注意: 原先列表含一个裸 '*' glob, 会匹配任意 effective-target 名,
        # 导致本该跳过的用例被当成"支持"而运行并虚假 FAIL. 这里移除该 '*'.
        #
        # 白名单里另外两个不是"平台三元组"而是"能力名", 它们对本目标恒真。
        # 放回的依据：把这两个名单门住的 71 个用例（另 4 个走 -fgimple）连
        # 同所在目录的兄弟头文件按原目录结构复制成夹具，走 harness 本身跑：
        #   compile 类 pass=33 fail=0；run 类 pass=8 fail=0（其余由别的
        #   名单 / dg-skip-if / 非本类别正常排除）。夹具必须保留目录结构，
        #   拍平会让 analyzer/ 的 `#include "analyzer-decls.h"` 找不到文件，
        #   制造出 4 个假 FAIL。
        # 其余同样看似"本平台应恒真"的名字暂不放回: 每个名单底下还门着一个
        # 真实缺陷, 放回要么让测试用例集变红, 要么得把缺陷写进 skip 清单变成假绿。
        #   size32plus     vec-cvt-1.c 运行错值
        #   stdint_types   未实现 __builtin_bswap128
        #   label_values   缺 analyzer 兄弟头文件; malloc-1.c
        #   nonlocal_goto  pr97986-1.c 要嵌套函数跳 `__label__`
        #   int32plus      多个 crc-builtin-* 与 C23 位域用例
        #   vect_* / lto / analyzer / tls / fpic 等本就与 dcc 无关或明确不支持
        # 注：多个 dg-require-effective-target 共存时本函数只看第一个（head -1），
        # 所以如 pr127166-2.c (int32 && vect_int) 仍靠 vect_int 这一条被排除。
        case "$req_target" in
            x86_64|x86|i?86|any|always|indirect_jumps|int32) ;;
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
        else
            # 选择器不含 '-'/'*' (如 aarch64_sve_hw / arm / powerpc), 属
            # effective-target 风格的非 triple 选择器; 本 harness 无法验证,
            # 凡命中已知非 x86 架构名即视为非本平台跳过 (避免误在 x86 上运行
            # 这些用例而虚假 FAIL). 含 x86 家族名则放行, 交由运行期定夺.
            if [[ "$pats" =~ (^|[^a-z0-9])(arm|aarch64|powerpc|ppc|pwr|mips|sparc|s390x?|riscv|alpha|ia64|sh|hppa|m68k|m32r|mn10300|frv|vax|cr16|epiphany|lm32|moxie|microblaze|nds32|arc|tile|visium|ft32|mcore|stormy16|iq2000|xstormy|msp430|rl78|avr|rx|pdp11)([^a-z0-9]|$) ]]; then
                echo "skip target:$pats"
                return
            fi
        fi
    fi

    local raw_opts=""
    local dg_opts add_opts
    # dg-options 既可能带引号 (`{ dg-options "-O2" }`) 也可能不带
    # (`{ dg-options -O2 }`, vrp13.c 等 55 个测试用此形式). 旧正则要求引号,
    # 漏掉无引号形式 ⇒ 该测试被按默认档编译 (如 vrp13 需 -O2 触发 VRP 才不链接失败).
    dg_opts=$(grep -oP '\{ dg-options\s+"?\K[^"}]+' "$src" 2>/dev/null | head -1 | sed -E 's/[[:space:]]+$//' || true)
    [[ -n "$dg_opts" ]] && raw_opts="$dg_opts"
    add_opts=$(grep -oP '\{ dg-additional-options\s+"?\K[^"}]+' "$src" 2>/dev/null | head -1 | sed -E 's/[[:space:]]+$//' || true)
    [[ -n "$add_opts" ]] && raw_opts="$raw_opts $add_opts"

    # dcc 不支持 -fgimple: 这些测试用 __GIMPLE(...) 书写 GCC 内部 IR,
    # C 前端无法解析, 只会在链接期报 undefined reference. GCC 自身也仅在
    # 支持 gimple 的目标上运行它们, 故这里一律跳过 (pr89595/pr93586/pr123175-2 等).
    if [[ "$raw_opts" == *-fgimple* ]]; then
        echo "skip fgimple"
        return
    fi

    local filtered=""
    local opt
    for opt in $raw_opts; do
        case "$opt" in
            -finstrument-functions|-fno-instrument-functions|-std=gnu89|-std=c89|\
            -std=c99|-std=gnu99|-std=c11|-std=gnu11|-std=c17|-std=gnu17|\
            -std=c23|-std=gnu23|-std=c2x|-std=gnu2x|-std=c2y|-std=gnu2y|\
            -O0|-O1|-O2|-O3|-Os|\
            -Wall|-Wextra|-w|-pedantic|-pedantic-errors|-include\ *|\
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

    # dg-additional-sources: 附加源与主源同目录, gcc 会把它们一起编译/链接。
    # harness 之前只编主源, 导致依赖附加源符号的 run/link 测试虚假 FAIL
    # (如 undefined reference to main / 其它符号)。这里解析出来供两个分支使用。
    local extra_srcs=""
    local as_line
    as_line=$(grep -oP 'dg-additional-sources "\K[^"]+' "$src" 2>/dev/null | head -1 || true)
    if [[ -n "$as_line" ]]; then
        local d_dir
        d_dir="$(dirname "$src")"
        local s
        for s in $as_line; do
            extra_srcs="$extra_srcs $d_dir/$s"
        done
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
        # 附加源也要能编译 (gcc 会一起编), 否则应判 FAIL
        if [[ $rc -eq 0 && -n "$extra_srcs" ]]; then
            local es eo erc
            for es in $extra_srcs; do
                eo="$TMPDIR_DG/$(echo "$es" | tr '/' '_').o"
                erc=0
                { timeout 30 "$DCC" -c -std=gnu11 $extra_opts $compat_inc -o "$eo" "$es" 2>/dev/null; } 2>/dev/null || erc=$?
                rm -f "$eo"
                if [[ $erc -ne 0 ]]; then
                    rc=$erc
                    break
                fi
            done
        fi
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
        if [[ -z "$extra_srcs" ]]; then
            if ! "$DCC" -std=gnu11 -lm $extra_opts -o "$exe" "$src" 2>/dev/null; then
                echo "$rel FAIL compile"
                return
            fi
        else
            # 多文件测试: dcc 不支持一次把多个 .c 编链成单个可执行,
            # 因此先各自 -c, 再用 .o 链接 (见上方 A 类失败的修复说明)。
            local oo objs=() ok=1 ss
            for ss in "$src" $extra_srcs; do
                oo="$TMPDIR_DG/$(echo "$ss" | tr '/' '_').o"
                if ! "$DCC" -c -std=gnu11 $extra_opts -o "$oo" "$ss" 2>/dev/null; then
                    ok=0
                    break
                fi
                objs+=("$oo")
            done
            if [[ $ok -eq 0 ]]; then
                echo "$rel FAIL compile"
                rm -f ${objs[@]+"${objs[@]}"}
                return
            fi
            if ! "$DCC" ${objs[@]+"${objs[@]}"} -lm -o "$exe" 2>/dev/null; then
                echo "$rel FAIL compile"
                rm -f ${objs[@]+"${objs[@]}"} "$exe"
                return
            fi
            rm -f ${objs[@]+"${objs[@]}"}
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
        # 分片: 按**连续块**切分 (而非轮询取模)。轮询 `((idx-1)%M)+1` 会把字母序
        # 相邻的相关用例 (如 float16/32/64/128-builtin、各 pr* 系列) 打散到不同片,
        # 修一个特性要跨多片验证, 与 "debuggable shards" 初衷相悖。连续块让同一
        # 特性区域的用例落在同一片, 单片即可覆盖一类特性的全部用例。
        # block(idx) = ((idx-1)*M)/total + 1, 把 1..total 连续映射到 1..M。
        if [[ -n "$SHARD_M" && $(( ((idx - 1) * SHARD_M) / total + 1 )) -ne "$SHARD_N" ]]; then
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
    # 实时透传: 通过 tee 同时落盘到结果文件, 并实时打印 FAIL 行 + 进度计数,
    # 避免「整批几万个用例跑完前一直静默」(单片调试时看不到中间状态)。
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
    local done=0
    < "$run_list" xargs -d '\n' -L1 -P "$JOBS" "$worker_cmd" 2>/dev/null \
        | tee "$tmpfiles_results" \
        | while IFS= read -r line; do
              # 失败实时可见, 方便单片调试时第一时间定位
              case "$line" in
                  *" FAIL "*) printf '  %s\n' "$line" >&2 ;;
              esac
              # 周期性进度计数 (无条件打印, 即便经 tee 管道到日志也可见),
              # 解决「整批几万个用例跑完前一直静默」的问题。
              done=$((done + 1))
              if (( done % 100 == 0 )); then
                  printf '[%s] 进度 %d/%d\n' "$category" "$done" "$run_count" >&2
              fi
          done
    printf '[%s] 完成 %d/%d\n' "$category" "$run_count" "$run_count" >&2

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
