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
# 必须 export: worker 是 xargs 拉起的新 bash 进程，只 export 函数不带值 ——
# DCC_TARGET 在子进程里为空会让 `[[ "" == $1 ]]` 恒假，所有
# `dg-do run { target *-*-linux* ... }` 都被误跳（pr110799/pr112736 等）。
export DCC_TARGET

# 判断单个 triple 通配是否命中当前目标 (bash glob 语义, '-' 为字面量).
target_matches_one() {
    [[ "$DCC_TARGET" == $1 ]]
}

# 提取 `{ dg-do <type> { target <expr> } }` 里 target 后面的表达式（可能嵌套
# 花括号）。返回去掉 `target` 前缀后的文本（含外层花括号），供 `dg_skip_expr`
# 按完整 dejagnu 语义求值 —— 旧实现只抓第一层 `{...}` 的 token 列表，
# `{ { i?86-*-* x86_64-*-* } && ia32 }` 里的 `&& ia32` 被丢掉，
# x86_64 上的 alloca-5/6（仅 ia32）被误放行。
extract_do_target_expr() {
    local block g
    # grep -z 用 NUL 分隔记录, 直接塞进 $(...) 会被 "ignored null byte" 截断;
    # 先 tr 成换行再拼接 (每条记录自身花括号平衡, 拼接不影响配对).
    # 注意结尾的 `\}`: 不带上它,dg-do 的 body 就不闭合, 无法按组切分.
    block=$(grep -ozP '\{\s*dg-do\s+[a-z]+\s*(\{(?:[^{}]|(?1))*\}\s*)*\}' -- "$1" 2>/dev/null \
            | tr '\0' '\n' | head -1)
    [[ -z "$block" ]] && return 0
    # dg-do 的 body: `{ dg-do run { target EXPR } }`。多组时 target 只可能出现
    # 在第一层, 直接按关键字切; EXPR 自身花括号平衡, 去掉末尾 dg-do 闭合的
    # `}` 即可。旧写法用 split_brace_groups 取组, 但它只输出顶层内容
    # (整条指令), 在 `target` 前缀匹配上必然落空。
    [[ "$block" != *target* ]] && return 0
    g="${block#*target}"
    # 依次去掉尾空白 + 两个 `}`：target 选择器自身的闭合与 dg-do 的闭合。
    g="${g%"${g##*[![:space:]]}"}"
    g="${g%\}}"
    g="${g%"${g##*[![:space:]]}"}"
    g="${g%\}}"
    printf '%s\n' "$g"
}
export -f extract_do_target_expr

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
# 注意: `target_included` 内部调用 `target_matches_one`, 而 xargs 拉起的 worker 是
# **新 bash 进程**: 没 export 的函数在子进程里根本不存在 (实测报
# `environment: line N: target_matches_one: command not found`, 返回 127 ⇒
# `target_included` 在 worker 里恒为假), 于是所有带行内 target 选择器的用例都被
# 误跳。两者必须一起导出。
export -f target_matches_one

# ── effective-target / dg-skip-if 求值 ──────────────────────────────
# 旧实现把「文件里出现 dg-skip-if」一律当作跳过。dejagnu 的真实语义是**条件命中
# 才跳过**，而 gcc.dg 里 966 条 dg-skip-if 中约 700 条只针对别的平台
# (avr/pdp11/hppa/powerpc/nvptx/...)，在 x86_64 上本来就该跑——一刀切把它们全
# 吞掉了。下面按 dejagnu 的表达式语法求值: `!`、`{ }` 嵌套、`&&`、`||`;
# 原子既可以是目标三元组 pattern，也可以是 effective-target 能力名。
#
# 未知名一律按「不成立」处理，方向是仔细选过的:
#   - `{ ! unknown }`（「非该能力才跳过」）仍会跳过，保守;
#   - `{ unknown }`（「该能力才跳过」）会真跑，由实际结果说话，不制造假绿。
#
# 能力名名单与 dg-require-effective-target 共用: x86_64 + Linux + glibc 上恒真、
# 且 dcc 确有实现的那一批。每个名字都经「放入后真跑一遍」验证
# (torture/tree-ssa 全量 0 FAIL；期间修掉 TLS tentative definition 合并、
# `-fno-fp-int-builtin-inexact` 两族缺陷)。
eff_target_true() {
    case "$1" in
        # 目标三元组 pattern（含 glob）
        *'*'*) target_matches_one "$1" ;;
        # 32/64 位数据模型与基线 ISA
        x86_64|x86|i\?86|any|always|indirect_jumps|int32|tls|pthread) return 0 ;;
        int32plus|size32plus|lp64|ptr32plus|sse2_runtime) return 0 ;;
        # C 运行时 / 语言与 ABI 能力（glibc + dcc 已实现）
        c99_runtime|size20plus|wchar|mmap|signal|hard_float|int128) return 0 ;;
        label_values|trampolines|nonlocal_goto|untyped_assembly) return 0 ;;
        sync_char_short|sync_int_long|stdint_types|tls_runtime) return 0 ;;
        # 浮点: _FloatN / __float128 与 fenv
        float16|float32|float64|float32x|float64x|float128|inf) return 0 ;;
        # __float128 是「目标有该类型」的判据 (与 float128 的「有库支持」分开);
        # dcc 的 Float128 已走 libgcc soft-fp, 真跑验证。
        __float128) return 0 ;;
        fenv_exceptions|fenv_exceptions_double|fenv_exceptions_long_double) return 0 ;;
        # `fenv` = 有 <fenv.h> 与浮点环境 builtin (x86_64 恒真);
        # dcc 侧 feclearexcept/fetestexcept 已实现 (fenv_exceptions 一档)。
        fenv) return 0 ;;
        float16_runtime|float32_runtime|float64_runtime|float128_runtime) return 0 ;;
        # _FloatNx 家族：dcc 的类型映射 (_Float32x→double、_Float64x→x87
        # long double、_Float128x→binary128) 与运行期库 (libgcc soft-fp) 均已
        # 就位；42 个 float{32,64,128}x-*/fp-int-convert-*x-* 用例实测全过
        # (期间修掉 fetestexcept 的 x87 DE 位误报，float128-div-underflow.c)。
        float32x_runtime|float64x_runtime|float128x_runtime|floatn_nx_runtime) return 0 ;;
        # x86_64 的 __float128 就是 IEEE binary128，有 libgcc soft-fp。
        base_quadfloat_support) return 0 ;;
        # x86_64 的 double 恰为 64 位、long double 为 80 位 (x87)。
        # double64/large_double/large_long_double 是「宽度不低于」类判据,
        # double64plus 是 GCC 的 "double is at least 64 bits" 名称。
        double64|double64plus|large_double|large_long_double) return 0 ;;
        # 小端；sizeof 相关下限 (x86_64: 指针 8、long 8) 均成立。
        le|size24plus) return 0 ;;
        # 数据模型：int 32 位、long long 64 位。
        longlong64) return 0 ;;
        # x86_64 基线带 SSE2（含 sse/sse2/sse_runtime 三个方言）。
        sse|sse2|sse_runtime) return 0 ;;
        # dcc 的 cli 接受 -fschedule-insns/-fschedule-insns2（当前不做调度，
        # 按无操作处理）；GCC 的 `scheduling` 能力名就是「支持该选项」，
        # 3 个用例只需该选项能传下去，运行期检查与调度无关，实测全过。
        scheduling) return 0 ;;
        # glibc 提供 mempcpy；x86_64 Linux 的 wchar_t 是 4 字节。
        mempcpy|4byte_wchar_t) return 0 ;;
        # x86 允许非对齐访问；x86_64 有 .init_array 全局构造器
        # (dcc 的 constructor 属性走同一机制)。
        non_strict_align|global_constructor) return 0 ;;
        # dcc 默认非 PIC 链接（host-link 传 -no-pie）⇒ nonpic 成立；
        # pie_enabled 不成立（未开 PIE）。
        nonpic) return 0 ;;
        # 位操作内建: popcount/clz/ctz 的 SImode/DImode 变体 dcc 全部实现
        # (含 clzg/ctzg 与 libgcc 回退)。名字带 l/ll/si 后缀的是 GCC
        # effective-target 的按宽度方言 (l=long, ll=long long, si=int)。
        popcount|popcountl|popcountll) return 0 ;;
        clz|clzl|clzll|clzsi) return 0 ;;
        ctz|ctzl|ctzll|ctzsi) return 0 ;;
        fpic|exceptions|bitint) return 0 ;;
        # dcc 不做「删除空指针检查」这类优化 ⇒ GCC 的这两条判据在 dcc 上成立，
        # 依赖相反行为的测试要跳过。
        keeps_null_pointer_checks|natural_alignment_32) return 0 ;;
        # 没有 AddressSanitizer: `{ no_fsanitize_address }` 跳过。
        no_fsanitize_address) return 0 ;;
        *) return 1 ;;
    esac
}
export -f eff_target_true

# 求值一个 dg-skip-if 的 selector: 命中（该跳过）⇒ 0。
dg_skip_expr() {
    local e="$1" tok rest pending=0 hit=0
    e="${e#"${e%%[![:space:]]*}"}"
    e="${e%"${e##*[![:space:]]}"}"
    [[ -z "$e" ]] && return 1
    # 剥一层花括号（dejagnu 用 `{ ... }` 包住整个表达式，可能嵌套）
    if [[ "$e" == \{*\} ]]; then
        e="${e#\{}"
        e="${e%\}}"
        dg_skip_expr "$e"
        return $?
    fi
    # 整体取反: `! X`，X 可以是 `{...}` 块或单个名字
    if [[ "$e" == '!'* ]]; then
        rest="${e#!}"
        dg_skip_expr "$rest" && return 1
        return 0
    fi
    # 逻辑或 / 与（dejagnu 允许出现在 selector 里, 如 `sparc*-*-solaris2* && lp64`）
    if [[ "$e" == *'||'* ]]; then
        while IFS= read -r tok; do
            dg_skip_expr "$tok" && return 0
        done < <(printf '%s\n' "$e" | sed 's/||/\n/g')
        return 1
    fi
    if [[ "$e" == *'&&'* ]]; then
        while IFS= read -r tok; do
            dg_skip_expr "$tok" || return 1
        done < <(printf '%s\n' "$e" | sed 's/&&/\n/g')
        return 0
    fi
    # 空格分隔的原子列表: 任一成立即成立; `!` 可以紧贴原子也可以是独立词
    for tok in $e; do
        if [[ "$tok" == "!" ]]; then
            pending=1
            continue
        fi
        hit=0
        if [[ "$tok" == '!'* ]]; then
            rest="${tok#!}"
            dg_skip_expr "$rest" || hit=1
        else
            eff_target_true "$tok" && hit=1
        fi
        if (( pending )); then
            (( hit )) || return 0
            pending=0
            continue
        fi
        (( hit )) && return 0
    done
    return 1
}
export -f dg_skip_expr

# 提取一个源文件里所有 dg-skip-if 的 selector（花括号配对），每行一个。
# PCRE 的递归引用 (?1) 处理 `{ ! { i?86-*-* x86_64-*-* } }` 这类嵌套;
# -z 让整文件成为一个记录, selector 跨行也能配对。
extract_skip_if_selectors() {
    grep -ozP '\{ dg-skip-if\s+("(?:[^"\\]|\\.)*"|[^{}]*)\s*\K(\{(?:[^{}]|(?1))*\})' \
        -- "$1" 2>/dev/null | tr '\0' '\n' || true
}
export -f extract_skip_if_selectors

# 提取一条 dg-skip-if 从 selector 起的所有花括号组 (NUL 分隔记录)。
# 完整语法: `{ dg-skip-if comment {selector} {target} {options} {exclude_options} }`,
# 后三组可省略。旧的 `extract_skip_if_selectors` 只取 selector, 丢掉了
# target/options 判据 —— 于是 `{ ! run_expensive_tests } { "*" } { "-O2" }`
# 这种「只在 -O2 下才跳过」的指令被当成无条件跳过 (torture/bitint-*.c 共 118 个)。
extract_skip_if_groups() {
    grep -ozP '\{ dg-skip-if\s+("(?:[^"\\]|\\.)*"|[^{}]*)\s*\K\{(?:[^{}]|(?1))*\}(?:\s*\{(?:[^{}]|(?1))*\})*' \
        -- "$1" 2>/dev/null || true
}
export -f extract_skip_if_groups

# 把一段 `{a} {b} {c}` 文本拆成每组内容一行 (不含外层花括号)。
# selector 里没有转义花括号或字符串, 逐字符配对即可。
split_brace_groups() {
    local s="$1" depth=0 cur="" i c
    for ((i = 0; i < ${#s}; i++)); do
        c="${s:i:1}"
        case "$c" in
            '{')
                depth=$((depth + 1))
                if ((depth == 1)); then cur=""; else cur+="$c"; fi
                ;;
            '}')
                depth=$((depth - 1))
                if ((depth == 0)); then printf '%s\n' "$cur"; else cur+="$c"; fi
                ;;
            *) ((depth >= 1)) && cur+="$c" ;;
        esac
    done
}
export -f split_brace_groups

# 按 dejagnu 语义求值一条 dg-skip-if 的完整参数。命中（该跳过）⇒ 0。
#   1) selector 必须成立;
#   2) target 列表（空/`*` 表示所有目标）必须包含当前目标;
#   3) options 列表为空 ⇒ 不看选项; 非空 ⇒ 当前编译选项须命中其中一项
#      (第 4 参数是「仅在这些选项下跳过」);
#   4) exclude_options 命中当前选项 ⇒ 不跳过 (第 5 参数是「除这些选项外」)。
dg_skip_if_match() {
    local sel="$1" tgt="$2" opts="$3" keep="$4" cur="$5" o hit
    dg_skip_expr "$sel" || return 1
    if [[ -n "$tgt" ]]; then
        target_included "$tgt" || return 1
    fi
    if [[ -n "$opts" ]]; then
        hit=0
        for o in $opts; do
            [[ "$o" == "*" ]] && { hit=1; break; }
            case " $cur " in *" $o "*) hit=1; break ;; esac
        done
        ((hit)) || return 1
    fi
    if [[ -n "$keep" ]]; then
        for o in $keep; do
            [[ "$o" == "*" ]] && return 1
            case " $cur " in *" $o "*) return 1 ;; esac
        done
    fi
    return 0
}
export -f dg_skip_if_match

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

    local req_targets
    # 提取「能力名 + 可选 { target ... } 门槛」。旧写法 `[^}]+` 遇到带门槛的
    # 写法会截成 `sse_runtime { target { i?86-*-* x86_64-*-* ` 这种残缺串，
    # eff_target_true 不识别 ⇒ va-arg-25/vector-1/2 被误跳。
    # 注意 `(\{...\})` 必须是**捕获组**：递归引用 `(?1)` 指向它，
    # 写成 `(?:...)` 会直接报 "reference to non-existent subpattern"。
    req_targets=$(grep -ozP '\{\s*dg-require-effective-target\s+\K[^{}\s]+(?:\s*(\{(?:[^{}]|(?1))*\})\s*)*' \
        -- "$src" 2>/dev/null | tr '\0' '\n' || true)
    if [[ -n "$req_targets" ]]; then
        # 多个 `dg-require-effective-target` 是 AND 语义：任一不满足就跳过。
        # 旧实现只取第一行（head -1），于是 `int32plus && dfp` 这类用例会因为
        # 第一个名字通过而被误跑（pr105231 需要 dfp，pr57584 需要 label_values）。
        while IFS= read -r req_target; do
            [[ -z "$req_target" ]] && continue
            # 名字后的第一个 token；余下可能是 `{ target EXPR }` 门槛。
            local req_name="${req_target%%[[:space:]]*}"
            local req_rest="${req_target#"$req_name"}"
            if [[ "$req_rest" == *target* ]]; then
                # 门槛不命中 ⇒ 本条 require 不适用于本目标（不是失败）。
                # 例：`sse_runtime { target { i?86-*-* x86_64-*-* } }` 在
                # powerpc 上不要求 sse_runtime。
                local gate="${req_rest#*target}"
                gate="${gate%"${gate##*[![:space:]]}"}"
                gate="${gate%\}}"
                if ! dg_skip_expr "$gate"; then
                    continue
                fi
            fi
            # 判据统一走 `eff_target_true`（与 dg-skip-if 求值共用一份名单），
            # 名单外的名字按「不成立」处理 ⇒ 自动跳过（保守）。
            #
            # 历史上这里只有 x86 家族 + int32plus/size32plus/lp64/ptr32plus/
            # sse2_runtime 五个名字是对的，其余看似「恒真」的能力名一个都没放：
            # 每个名字底下还门着真实缺陷，放回要么让用例集变红，要么得把缺陷写进
            # skip 清单变成假绿。本轮把 31 个名字逐个放回并真跑（torture/tree-ssa
            # 全量 0 FAIL），期间修掉两族缺陷：TLS/static 同名 tentative
            # definition 被重复分配存储（thr-init-2.c），以及
            # `-fno-fp-int-builtin-inexact`/C23 下 ceil/floor/round/trunc
            # 仍置 FE_INEXACT（builtin-fp-int-inexact{,-c23}.c）。
            # 仍然**不放**的名字（与 dcc 无关或明确不支持）：
            #   vect_* / store_merge / scheduling / freorder  没有自动向量化与调度
            #   lto / analyzer / split_stack / dfp / trapping 特性不存在
            #   bfloat16 / 各类 *_hw 变体（需特定硬件）
            if ! eff_target_true "$req_name"; then
                echo "skip target:$req_name"
                return
            fi
        done <<< "$req_targets"
    fi

    # dg-skip-if 是**条件**跳过：selector 命中**且** target 列表包含当前目标
    # **且**当前编译选项命中 options 参数（若给出）时才 skip。旧实现只看
    # selector，把「仅在 -O2 下跳过」当成了无条件跳过（详见 `dg_skip_if_match`）。
    #
    # 当前选项在 skip 判定里只用于 dg-skip-if 的第 4/5 参数；取 dg-options /
    # dg-additional-options 的原始文本即可（与下方 extra_opts 的白名单过滤无关：
    # 这些参数列的本来就是 -O2/-flto 这类原样选项）。
    local cur_opts="" skip_ao
    cur_opts=$(grep -oP '\{ dg-options\s+"?\K[^"}]+' "$src" 2>/dev/null | head -1 | sed -E 's/[[:space:]]+$//' || true)
    skip_ao=$(grep -oP '\{ dg-additional-options\s+"?\K[^"}]+' "$src" 2>/dev/null | head -1 | sed -E 's/[[:space:]]+$//' || true)
    [[ -n "$skip_ao" ]] && cur_opts="$cur_opts $skip_ao"
    local rec
    while IFS= read -r -d '' rec; do
        [[ -z "$rec" ]] && continue
        local -a grp=()
        local g
        while IFS= read -r g; do grp+=("$g"); done < <(split_brace_groups "$rec")
        if dg_skip_if_match "${grp[0]:-}" "${grp[1]:-}" "${grp[2]:-}" "${grp[3]:-}" "$cur_opts"; then
            echo "skip dg-skip-if"
            return
        fi
    done < <(extract_skip_if_groups "$src")

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
    # 含逻辑运算符/嵌套花括号的表达式（如 `{ { i?86-*-* x86_64-*-* } && ia32 }`）
    # 要用完整求值器，不能只做一次 token 包含判定：alloca-5/6 在 x86_64
    # 上因 `&& ia32` 为假本应跳过，旧写法把该子句丢了就误跑了。
    local tgt_expr
    tgt_expr=$(extract_do_target_expr "$src")
    if [[ -n "$tgt_expr" && ( "$tgt_expr" == *'&&'* || "$tgt_expr" == *'||'* || "$tgt_expr" == *'!'* ) ]]; then
        if ! dg_skip_expr "$tgt_expr"; then
            echo "skip target:${tgt_expr# }"
            return
        fi
    elif [[ -n "$pats" ]]; then
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
            # int16/int8 是「sizeof(int)==2/1」类能力名, 在 x86_64 (int=32b)
            # 上恒假: vrp66-int16-sw.c 这类用例连本机 gcc 都过不了, 放行只会
            # 制造虚假 FAIL (gcc 自己的 dejagnu 也因 target int16 跳过它).
            if [[ "$pats" =~ (^|[^a-z0-9])(arm|aarch64|powerpc|ppc|pwr|mips|sparc|s390x?|riscv|alpha|ia64|sh|hppa|m68k|m32r|mn10300|frv|vax|cr16|epiphany|lm32|moxie|microblaze|nds32|arc|tile|visium|ft32|mcore|stormy16|iq2000|xstormy|msp430|rl78|avr|rx|pdp11|int16|int8)([^a-z0-9]|$) ]]; then
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
            -fno-fp-int-builtin-inexact|-ffp-int-builtin-inexact|\
            -ffreestanding|--coverage|-fprofile-arcs|\
            -pg|-p|\
            -g|-g0|-g1|-g2|-g3|\
            -lm|-ldl|-lpthread|-pthread|-lrt)
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
    if [[ "$do_type" == "preprocess" ]]; then
        echo "$rel SKIP dg-do:$do_type"
        return
    fi

    # 每个用例在自己的工作目录里跑：dcc 会把伴生文件落在**当前目录**
    # （-fprofile-arcs 的 `<stem>.gcovrt.c` / `.gcno` / `.gcda`、PGO 的
    # `<stem>.pgort.c` / `.profraw`、MemProf 的 `.memprofc.c`）。直接在调用者
    # 的 CWD（通常是仓库根）跑，这些文件会一个个堆进 git status；本函数的
    # 源文件与输出路径都是绝对路径，切换 CWD 不影响任何一次调用。
    # （套件里没有 `-I.` 形式的 dg-options，已核查 torture/tree-ssa/compat/
    #   charset/noncompile/format 六个目录；出现时需另行处理。）
    local workdir="$TMPDIR_DG/work/$(echo "$rel" | tr '/' '_')"
    mkdir -p "$workdir"
    cd "$workdir" || return

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
        # assemble / link 也是本类别该管的：前者验 -S 文本能被 gas 接受，
        # 后者验完整链接（GCC 的 dg-do link 不执行程序）。旧写法把两者
        # 一律跳过，87 个 link 用例与 5 个 assemble 用例等于零覆盖。
        if [[ "$do_type" != "compile" && "$do_type" != "run" &&
              "$do_type" != "assemble" && "$do_type" != "link" ]]; then
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
        if [[ "$do_type" == "assemble" ]]; then
            # `-S` 只是文本镜像；真正能暴露 -S 格式缺陷（`%%rax`、
            # `shr %rax, 16` 这类 GAS 语法错）的是把文本交给 as。
            local asm_out="$TMPDIR_DG/$(echo "$rel" | tr '/' '_').s"
            local obj_out="$TMPDIR_DG/$(echo "$rel" | tr '/' '_').as.o"
            { timeout 60 "$DCC" -S -std=gnu11 $extra_opts $compat_inc -o "$asm_out" "$src" 2>"$errf"; } 2>/dev/null || rc=$?
            if [[ $rc -eq 0 ]]; then
                { timeout 60 as -o "$obj_out" "$asm_out" 2>>"$errf"; } 2>/dev/null || rc=$?
            fi
            rm -f "$asm_out" "$obj_out"
        elif [[ "$do_type" == "link" ]]; then
            local exe_out="$TMPDIR_DG/$(echo "$rel" | tr '/' '_').exe"
            { timeout 60 "$DCC" -std=gnu11 -lm $extra_opts $compat_inc -o "$exe_out" "$src" 2>"$errf"; } 2>/dev/null || rc=$?
            rm -f "$exe_out"
        else
            { timeout 30 "$DCC" -c -std=gnu11 $extra_opts $compat_inc -o "$out" "$src" 2>"$errf"; } 2>/dev/null || rc=$?
            rm -f "$out"
        fi
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
        if [[ "$do_type" != "run" && "$do_type" != "link" ]]; then
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
        if [[ "$do_type" == "link" ]]; then
            # dg-do link 的判据是**链接成功**（不执行）。回写写零的错不能靠
            # 执行期用例发现的，就在这一步暴露（如 weak/hidden 符号、
            # inline 语义、codegen 缺定义）。
            rm -f "$exe"
            echo "$rel PASS"
            return
        fi
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
