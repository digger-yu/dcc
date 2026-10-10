#!/bin/bash
# ===========================================================================
# scripts/check_fixtures.sh — fixture 引用完整性 / 命名 / 目录一致性门禁
#
# 权威规格: docs/tests_quality_system_plan.md §7 (Fixture 规则) + §8.2 (重写规格)
# 依赖: 仅 bash + coreutils(grep/sed/awk/find/sort/comm/cut/tr) + git —— 无 python/perl/jq
# v2.0: 旧版用于豁免未引用检查的环境变量开关已彻底删除 —— 本门禁不存在任何
#       放宽规则的过渡开关，未引用即红；存量债由 §7.4 逐个判定清零。
#
# 5 项检查（全部 exit 1 级；前缀标签纯 ASCII，便于 grep 断言）:
#   ① MISSING:                被引用的 fixture 文件不存在
#   ② UNREFERENCED:           非白名单目录下 .c/.s/.cpp/.h 未被任何引用命中
#   ③ FORBIDDEN-ARCHIVE-REF:  任何 .rs/.sh/.yml 引用 tests/fixtures/_archive/ 下文件
#   ④ FORBIDDEN-NAME:         相对 git ls-tree -r HEAD 新增的 m<N>_ 前缀 fixture
#   ⑤ DIR-MISMATCH:           目录-扩展名一致性（c/ cpp/ cpp_abi_*/ asm/）
#
# 输出顺序固定为 ③ → ① → ④ → ⑤ → ②：注入件常同时命中多条规则，把 ② 放最后
# 可保证 §8.3 反向自测的「首个命中标签」断言具有确定性。
#
# 退出码: 0 = 全绿；1 = 存在违规 或 前置 FATAL（FATAL 路径绝不打印成功文案）
# 用法:   bash scripts/check_fixtures.sh
# ===========================================================================
set -o pipefail
export LC_ALL=C

errors=0
scanned=0

fail_fatal() {
    echo "FATAL: $*"
    echo "gate did not complete all checks -> FAIL (不打印成功文案)"
    exit 1
}

# --- REPO_ROOT 由 BASH_SOURCE 解析；解析失败立即 FATAL ----------------------
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." 2>/dev/null && pwd)"
if [[ -z "${REPO_ROOT:-}" || ! -d "$REPO_ROOT/tests/fixtures" ]]; then
    fail_fatal "repo root 解析失败: '${REPO_ROOT:-<empty>}' 或 tests/fixtures 不存在"
fi
cd "$REPO_ROOT" || fail_fatal "无法进入仓库根目录: $REPO_ROOT"
command -v git >/dev/null 2>&1 || fail_fatal "git 不可用，无法执行 ④ 增量命名检查"

WORK="$(mktemp -d)" || fail_fatal "mktemp 失败"
trap 'rm -rf "$WORK"' EXIT

# ---------------------------------------------------------------------------
# 白名单（整目录豁免 ②）。理由逐条注明；变更须同步 tests/fixtures/README.md。
#   corpus     : 真实 .o/二进制样本 + 外部语料（§7.1 明示「整目录豁免」），非夹具源码
#   gcc_tools  : libiberty 探测组合，由 scripts/ 探测脚本按目录枚举，不逐个引用
#   probe      : 后端能力探针（归 backend 案），由 backend 矩阵按目录发现，不主题化
#   cpp_abi_*  : GCC 差分 ABI 夹具族，由 tests/cpp_abi_gcc_cross_check.rs 按 *目录名*
#                动态发现（cpp_abi_groups_are_all_covered），磁盘上逐个 .cpp 不出现在
#                任何字面量中 —— 等价于「整目录被 check_group() 引用」（§10.2 明示豁免）
#   errors     : eNNNN.c/wNNNN.c + *.stderr.snap，路径由 tests/errors.rs 的
#                ErrorCase{name, fixture_ext} 动态拼接；动态名匹配（Pattern-D）已把
#                这些件标进 REFERENCED，白名单只是双保险，避免快照件与负向件被误报
# 注：c/ cpp/ asm/ opt/ c_xfail/ 不豁免 —— 它们必须逐个被引用。
# ---------------------------------------------------------------------------
WHITELIST_DIRS=(
    tests/fixtures/corpus
    tests/fixtures/gcc_tools
    tests/fixtures/probe
    tests/fixtures/errors
)

is_whitelisted() { # $1 = 仓库相对路径
    local p="$1" w first
    for w in "${WHITELIST_DIRS[@]}"; do
        [[ "$p" == "$w"/* ]] && return 0
    done
    first="${p#tests/fixtures/}"
    first="${first%%/*}"
    [[ "$first" == cpp_abi_* ]] && return 0
    return 1
}

# 合法「不存在」的被引用件（负向用例，非悬挂）：errors.rs e0100 故意读取不存在的
# 源文件以验证诊断路径。表项若已失效应一并删除（本表只在 §8.2 骨架中允许存在）。
declare -A INTENTIONALLY_MISSING=(
    ["tests/fixtures/errors/does_not_exist.c"]="e0100 负向用例: 故意引用不存在的源文件"
)

is_intentionally_missing() { # $1 = 完整相对路径 或 basename
    local want="$1" k b
    b="${want##*/}"
    for k in "${!INTENTIONALLY_MISSING[@]}"; do
        [[ "$k" == "$want" || "${k##*/}" == "$b" ]] && return 0
    done
    return 1
}

declare -A REFERENCED    # fixture path -> 首个引用定位 file:line
declare -A NEED_EXIST    # path -> 首个引用定位（需做存在性检查）
declare -A BASE_PATHS    # basename -> 该 basename 对应的全部夹具路径（换行分隔）

note_ref() { [[ -v REFERENCED["$1"] ]] || REFERENCED["$1"]="$2"; }
note_need() { [[ -v NEED_EXIST["$1"] ]] || NEED_EXIST["$1"]="$2"; }

# --- 引用形式正则（ERE，作为字符串传入使用处，不加引号展开）----------------
# (1)(2) include_str!("<任意个 ../>fixtures/…")   (3) "tests/fixtures/…" 字面量
RE_FX='(\.\./)*(tests/)?fixtures/[A-Za-z0-9_./+-]+\.(c|s|cpp|h)'
# (4) .join("x.c"/"x.s"/"x.cpp"/"x.h")
RE_JOIN='\.join\("([^"]+\.(c|s|cpp|h))"'
# (5) 含 fixture 字样的行里的裸引号字面量（覆盖 offload_fixture("x.c") 这类助手函数）
RE_QSTR='"([A-Za-z0-9_./+-]+\.(c|s|cpp|h))"'
# (6) ErrorCase 动态构造（保留旧 scripts/check_fixtures.sh:67-76 语义）
RE_NAME='name:[[:space:]]*"([^"]+)"'
RE_EXT='fixture_ext:[[:space:]]*"([^"]+)"'

handle_path_ref() { # $1=loc(file:line) $2=引用 token $3=匹配前一个字符
    local loc="$1" tok="$2" pc="${3:-}"
    RESOLVED=""
    resolve_scope "$tok" "$pc" || return 0   # 1=域外其它夹具树 2=无目录标记（由名字候选解析处理）
    local path="$RESOLVED"
    if [[ "$path" == tests/fixtures/_archive/* ]]; then
        # ③ _archive 禁止被任何引用命中（不做存在性检查，保证标签唯一）
        printf 'FORBIDDEN-ARCHIVE-REF: %s (referenced at %s)\n' "$path" "$loc" >>"$WORK/archive.txt"
        note_ref "$path" "$loc"
        return 0
    fi
    note_ref "$path" "$loc"
    note_need "$path" "$loc"
    return 0
}

# 只接受本测试用例脚本管辖的唯一树: tests/fixtures/**（含 smoke/ 子树）。
# 前缀合法形式: `fixtures/…`、`../fixtures/…`（任意个 ../）、`tests/fixtures/…`。
# 若 `fixtures/` 前还有其它目录成分（如 tests/diff_test/fixtures/x.c —— 另一个测试
# 树自己的运行时夹具），必须当作「域外」丢弃，否则会把它强行当成 tests/fixtures/x.c
# 而报假 MISSING。判断依据：token 裸起头于 fixtures/ 且匹配前一个字符是 '/'。
resolve_scope() { # $1=tok $2=prevc → 设置 RESOLVED; 返回 0/1/2
    local t="$1" pc="$2" pre tail
    if [[ "$t" =~ ^((\.\./)*(tests/)?)fixtures/(.+)$ ]]; then
        pre="${BASH_REMATCH[1]}"; tail="${BASH_REMATCH[4]}"
        if [[ -z "$pre" && "$pc" == "/" ]]; then return 1; fi
        RESOLVED="tests/fixtures/$tail"
        return 0
    fi
    if [[ "$t" == *"/fixtures/"* ]]; then return 1; fi
    return 2
}

extract_line_refs() { # $1=loc $2=整行 $3=ERE 字符串
    local loc="$1" line="$2" re="$3" rest="$line" m tok pre pc guard=0
    while [[ -n "$rest" && "$rest" =~ $re ]]; do
        m="${BASH_REMATCH[0]}"
        tok="$m"
        pre="${rest%%"$m"*}"
        pc="${pre: -1}"
        handle_path_ref "$loc" "$tok" "$pc"
        rest="${rest#*"$m"}"
        guard=$((guard + 1))
        [[ $guard -gt 64 ]] && break
    done
    return 0
}

# ---------------------------------------------------------------------------
# 扫描面（§7.3）: tests/*.rs、tests/integration/*.rs、tests/common/*.rs、
#   scripts/*.sh、.github/workflows/*.yml|yaml
# 门禁脚本自身不入面：check_fixtures*.sh 内含白名单字面量 "tests/fixtures/…"，
# 若当作引用命中会造成假绿（自引用豁免，不是规则放宽）。
# ---------------------------------------------------------------------------
add_surface() { # $1=dir $2=maxdepth(0=无限) $3..=-name patterns
    local dir="$1" depth="$2"; shift 2
    local -a pat=() q=()
    [[ -d "$dir" ]] || return 0
    for q in "$@"; do pat+=(-o -name "$q"); done
    [[ ${#pat[@]} -gt 0 ]] && pat=("${pat[@]:1}")
    if [[ "$depth" == "0" ]]; then
        find "$dir" -type f \( "${pat[@]}" \) -print >>"$WORK/surface" || fail_fatal "find 失败: $dir"
    else
        find "$dir" -maxdepth "$depth" -type f \( "${pat[@]}" \) -print >>"$WORK/surface" || fail_fatal "find 失败: $dir"
    fi
    return 0
}

: >"$WORK/surface"
add_surface tests 1 '*.rs'
add_surface tests/integration 1 '*.rs'
add_surface tests/common 1 '*.rs'
add_surface scripts 1 '*.sh'
add_surface .github/workflows 1 '*.yml'
add_surface .github/workflows 1 '*.yaml'

sort -u "$WORK/surface" 2>/dev/null | grep -vE '^scripts/check_fixtures(_selftest)?\.sh$' >"$WORK/srcs"
if [[ ! -s "$WORK/srcs" ]]; then
    fail_fatal "扫描面为 0 个源文件（tests/ scripts/ .github/ 全部未命中）—— 拒绝假绿"
fi

# ===========================================================================
# PASS 1 — 引用提取（4 种形式 + ErrorCase 动态构造）
# ===========================================================================
while IFS= read -r src; do
    [[ -f "$src" ]] || continue
    scanned=$((scanned + 1))

    # (a) fixtures 字面量路径
    : >>"$WORK/names.txt"
    while IFS= read -r hit; do
        ln="${hit%%:*}"
        line="${hit#*:}"
        [[ "$line" == *fixtures/* ]] || continue
        extract_line_refs "$src:$ln" "$line" "$RE_FX"
    done < <(grep -nE 'fixtures/' "$src" 2>/dev/null || true)

    # (b) 显式路径引用（.join("tests/fixtures/…") / .join("../fixtures/…")）——
    #     只有带 fixtures/ 标记的参数才按路径解析；不带标记的带斜杠参数
    #     （如 tmp.join("src/main.c")）是临时文件，不参与存在性判定。
    while IFS= read -r hit; do
        ln="${hit%%:*}"
        line="${hit#*:}"
        rest="$line"
        while [[ -n "$rest" && "$rest" =~ $RE_JOIN ]]; do
            fname="${BASH_REMATCH[1]}"
            m="${BASH_REMATCH[0]}"
            if [[ "$fname" == *fixtures/* ]]; then
                handle_path_ref "$src:$ln" "$fname"
            fi
            rest="${rest#*"$m"}"
        done
    done < <(grep -nE '\.join\("' "$src" 2>/dev/null || true)

    # (c) 「同行含 fixture 字样」的引号字面量 → 名字候选（basename 或相对夹具根的子路径）
    #     覆盖 .join("c/profile.c")、offload_fixture("offload_cuda.c")、Path::new("x.c") 等
    #     无法静态确定根目录的写法；歧义在解析阶段用磁盘真值收敛。
    #     反向保护：不带 fixture 字样的行（dir.join("main.c") / tmp.join("test.c") 这类
    #     临时文件写入）一律不入候选，否则会制造假 MISSING。
    while IFS= read -r hit; do
        ln="${hit%%:*}"
        line="${hit#*:}"
        [[ "$line" == *fixture* || "$line" == *Fixture* ]] || continue
        rest="$line"
        while [[ -n "$rest" && "$rest" =~ $RE_QSTR ]]; do
            printf '%s\t%s:%s\n' "${BASH_REMATCH[1]}" "$src" "$ln" >>"$WORK/names.txt"
            rest="${rest#*"${BASH_REMATCH[0]}"}"
        done
    done < <(grep -nE '"[^"]*\.(c|s|cpp|h)"' "$src" 2>/dev/null || true)

    # (d) ErrorCase{name, fixture_ext} → tests/fixtures/errors/<name>.<ext>
    pend=""
    while IFS= read -r hit; do
        ln="${hit%%:*}"
        line="${hit#*:}"
        if [[ "$line" =~ $RE_EXT ]]; then
            ext="${BASH_REMATCH[1]}"
            if [[ -n "$pend" ]]; then
                note_ref "tests/fixtures/errors/${pend}.${ext}" "$src:$ln"
                note_need "tests/fixtures/errors/${pend}.${ext}" "$src:$ln"
                pend=""
            fi
        elif [[ "$line" =~ $RE_NAME ]]; then
            pend="${BASH_REMATCH[1]}"
        fi
    done < <(grep -nE '(name|fixture_ext):[[:space:]]*"' "$src" 2>/dev/null || true)
done <"$WORK/srcs"

# --- 夹具磁盘真值 ----------------------------------------------------------
find tests/fixtures -type f \( -name '*.c' -o -name '*.s' -o -name '*.cpp' -o -name '*.h' \) \
    -print | sort >"$WORK/fixtures.txt" || fail_fatal "find tests/fixtures 失败"
[[ -s "$WORK/fixtures.txt" ]] || fail_fatal "tests/fixtures 下未发现任何夹具源文件"
grep -v '^tests/fixtures/_archive/' "$WORK/fixtures.txt" >"$WORK/live.txt"

# basename 索引（供纯 basename 的 .join 引用解析）
while IFS= read -r p; do
    b="${p##*/}"
    BASE_PATHS["$b"]+="$p"$'\n'
done <"$WORK/live.txt"

# (e) 名字候选解析（来自上面 (c) 的「fixture 同引号字面量」）
#     优先序: 含 fixtures/ 标记 → 按路径解析；其次相对夹具根存在；
#     再次按 basename 在夹具树（排除 _archive）内匹配；都不中 → MISSING
while IFS=$'\t' read -r key loc; do
    [[ -n "$key" ]] || continue
    if [[ "$key" == *fixtures/* ]]; then
        handle_path_ref "$loc" "$key" ""
        continue
    fi
    if [[ -f "tests/fixtures/$key" ]]; then
        note_ref "tests/fixtures/$key" "$loc"
        note_need "tests/fixtures/$key" "$loc"
        continue
    fi
    if [[ -n "${BASE_PATHS[$key]:-}" ]]; then
        while IFS= read -r p; do
            [[ -n "$p" ]] && note_ref "$p" "$loc"
        done <<<"${BASE_PATHS[$key]}"
    elif ! is_intentionally_missing "$key"; then
        printf 'MISSING: tests/fixtures/**/%s (referenced at %s; name-only fixture reference, no such fixture on disk)\n' \
            "$key" "$loc" >>"$WORK/missing.txt"
    fi
done < <(sort -u "$WORK/names.txt" 2>/dev/null)

emit_and_count() { # $1=文件：逐行打印并累加 errors
    while IFS= read -r l; do
        [[ -n "$l" ]] || continue
        echo "$l"
        errors=$((errors + 1))
    done <"$1"
}

# ===========================================================================
# ③ FORBIDDEN-ARCHIVE-REF
# ===========================================================================
if [[ -s "$WORK/archive.txt" ]]; then
    sort -u "$WORK/archive.txt" >"$WORK/archive_u.txt"
    emit_and_count "$WORK/archive_u.txt"
fi

# ===========================================================================
# ① MISSING — 被引用的 fixture 必须存在
# ===========================================================================
: >>"$WORK/missing.txt"
while IFS= read -r p; do
    [[ -n "$p" ]] || continue
    is_intentionally_missing "$p" && continue
    [[ -f "$p" ]] && continue
    printf 'MISSING: %s (referenced at %s)\n' "$p" "${NEED_EXIST[$p]}" >>"$WORK/missing.txt"
done < <(printf '%s\n' "${!NEED_EXIST[@]}" | sort)
if [[ -s "$WORK/missing.txt" ]]; then
    sort -u "$WORK/missing.txt" >"$WORK/missing_u.txt"
    emit_and_count "$WORK/missing_u.txt"
fi

# ===========================================================================
# ④ FORBIDDEN-NAME — 相对 git ls-tree -r HEAD 的 *新增* m<N>_ 前缀夹具
#    _archive/ 豁免：§7.4-A 要求把历史 m<N>_ 件 git mv 进 _archive/，归档动作本身
#    会让文件出现在新路径，不该被判为「新增违规」。
# ===========================================================================
if ! git rev-parse --verify -q HEAD >/dev/null 2>&1; then
    fail_fatal "git HEAD 不可解析，无法执行 ④ 增量命名检查（不静默跳过）"
fi
git -C "$REPO_ROOT" ls-tree -r --name-only HEAD -- tests/fixtures 2>/dev/null \
    | grep -v '^tests/fixtures/_archive/' | sed 's|.*/||' | sort -u >"$WORK/head_base.txt" || true
[[ -f "$WORK/head_base.txt" ]] || : >"$WORK/head_base.txt"
: >"$WORK/name.txt"
while IFS= read -r p; do
    b="${p##*/}"
    [[ "$b" =~ ^m[0-9]+_ ]] || continue
    # 按 basename 与 HEAD 比对（固定字符串全行匹配）：basename 已在 HEAD 任意目录
    # 出现过 → 存量件（包含 git mv 换目录），不算「新增」
    grep -qxF -- "$b" "$WORK/head_base.txt" && continue
    printf 'FORBIDDEN-NAME: %s (新增 m<N>_ 前缀 fixture, §7.2 禁止; 请改名 <domain>_<feature>)\n' "$p" >>"$WORK/name.txt"
done <"$WORK/live.txt"
if [[ -s "$WORK/name.txt" ]]; then
    sort -u "$WORK/name.txt" >"$WORK/name_u.txt"
    emit_and_count "$WORK/name_u.txt"
fi

# ===========================================================================
# ⑤ DIR-MISMATCH — 目录-扩展名一致性
# ===========================================================================
while IFS= read -r p; do
    rel="${p#tests/fixtures/}"
    top="${rel%%/*}"
    base="${p##*/}"
    ext="${p##*.}"
    bad=""
    case "$top" in
        c) [[ "$ext" == "cpp" ]] && bad=".cpp must not live in tests/fixtures/c/" ;;
        cpp) [[ "$ext" == "c" ]] && bad=".c must not live in tests/fixtures/cpp/" ;;
        cpp_abi_*) [[ "$ext" == "c" ]] && bad=".c must not live in tests/fixtures/cpp_abi_*/" ;;
        asm) [[ "$base" != asm_* ]] && bad="only asm_* prefixed files allowed in tests/fixtures/asm/" ;;
    esac
    if [[ -n "$bad" ]]; then
        printf 'DIR-MISMATCH: %s (%s)\n' "$p" "$bad"
        errors=$((errors + 1))
    fi
done <"$WORK/live.txt"

# ===========================================================================
# ② UNREFERENCED — 非白名单目录下每个 .c/.s/.cpp/.h 必须被引用 >= 1 次
# ===========================================================================
while IFS= read -r p; do
    is_whitelisted "$p" && continue
    if [[ ! -v REFERENCED["$p"] ]]; then
        printf 'UNREFERENCED: %s (no include_str!/.join/literal/script reference found; add a #[test] or git mv into _archive/)\n' "$p"
        errors=$((errors + 1))
    fi
done <"$WORK/live.txt"

# ===========================================================================
# 退出语义：任何提前失败路径都不会走到这里，也不会打印成功文案
# ===========================================================================
echo "---"
echo "scanned=$scanned source files (tests/*.rs, tests/integration/*.rs, tests/common/*.rs, scripts/*.sh, .github/workflows/*.yml)"
echo "fixtures=$(wc -l <"$WORK/live.txt" | tr -d ' ') candidate files under tests/fixtures (excluding _archive/)"
echo "whitelist: ${WHITELIST_DIRS[*]} + tests/fixtures/cpp_abi_*"
if [[ $errors -gt 0 ]]; then
    echo "共 $errors 个问题"
    exit 1
fi
echo "all fixture checks passed (scanned=$scanned)."
exit 0
