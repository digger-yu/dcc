#!/usr/bin/env bash
# 把「只有真树才复现」的预处理产物 .i 自动缩减成最小复现件（ddmin，按行成块删）。
#
# 用法:
#   reduce_unit.sh [-k 必须保留的片段]... <起点 .i> <符号名> <输出 .i>
#   reduce_unit.sh --selftest
#
# 三条判据同时成立，才允许删掉这一块：
#   1. 候选件仍含住全部「必须保留」片段。默认是 `__pt_no_sw_bit` 那一族的三件：
#      陷阱声明、`pt_feature` 的函数头、`PT_FEAT_DMA_INCOHERENT` 位号；
#      换目标时用 -k 传自己的片段（可重复，逐条按字面 grep -F 匹配）。
#   2. dcc 能编过这个候选件。
#   3. 产物里仍有该符号的**未定义引用**——即缺陷还在。
#
# 判据为什么只在 dcc 一侧（不是「gcc 无引用 ∧ dcc 有引用」）：`-E` 之后的产物对 gcc 并
# 不是可编的输入（`guard()` 那族「原型带 gnu_inline、定义不带」的成对声明在预处理后散了，
# gcc 当硬错误；`struct __filename_head;` 一类也是 -E 的副产物）。为保真不在 gcc 侧做语法
# 手术——那会把「同一件」变成两件。恒假这件事改由**缩减后仍保留的源码结构**自证：必须保留
# 片段要求候选件同时含住陷阱声明、`pt_feature` 的两道守卫与位号，即折叠应当发生在
# 「守卫 ∧ 位常量」这一对常数上。
#
# 为什么值得自动化：这类形状藏在 7 万行预处理产物里（`__map_range` 一个函数的 AST 打印
# 就有 1.6 亿字符），手切必然切坏（类型/内联链互相引用），而按块删 + 三判据能自己走到
# 「最小的那一片」。起点 .i 用 scripts/kernel_pp_unit.sh 从内核 TU 现做。
#
# 退出码: 0 = 已产出（并复核）；1 = 起点件本身不满足判据；2 = 用法/环境。
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DCC="${DCC:-$ROOT/target/debug/dcc}"
DCC_FLAGS=(-O2 -nostdinc -D__KERNEL__ -std=gnu11 -m64 -mcmodel=kernel
           -fno-stack-protector -ffreestanding)
MUST_KEEP=()
CUR=()
TMP=""
WD=""

usage() {
    sed -n '2,10p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
    exit 2
}

die() { echo "reduce_unit.sh: $*" >&2; exit 2; }

# probe <候选文件> <符号> <工作目录>：三条判据全过才返回 0。
probe() {
    local cand="$1" sym="$2" wd="$3" need
    for need in "${MUST_KEEP[@]}"; do
        grep -qF -- "$need" "$cand" || return 1
    done
    "$DCC" "${DCC_FLAGS[@]}" -c -o "$wd/r_dcc.o" "$cand" >"$wd/cc.log" 2>&1 || return 1
    nm -u "$wd/r_dcc.o" 2>/dev/null | grep -qF -- "$sym"
}

# 把 CUR 去掉 [off, off+cnt) 这一段写到 $TMP 试一次；判据全过就就地采用。
try_drop() {
    local off="$1" cnt="$2"
    local -a cand=()
    (( off > 0 )) && cand+=("${CUR[@]:0:off}")
    (( off + cnt < ${#CUR[@]} )) && cand+=("${CUR[@]:off+cnt}")
    (( ${#cand[@]} == 0 )) && return 1
    printf '%s\n' "${cand[@]}" >"$TMP"
    probe "$TMP" "$SYM" "$WD" || return 1
    CUR=("${cand[@]}")
}

reduce() {
    local src="$1" dst="$2" total rounds=0 n=8 chunk i progressed
    [[ -r "$src" ]] || die "读不到起点件: $src"
    mapfile -t CUR <"$src"
    total=${#CUR[@]}
    (( total > 0 )) || die "起点件是空的: $src"

    WD="$(mktemp -d "${TMPDIR:-/tmp}/reduce_unit.XXXXXX")" || die "建不了临时目录"
    TMP="$WD/cand.i"
    # shellcheck disable=SC2064  # WD 在 trap 安装时就要固定
    trap 'rm -rf "$WD"' EXIT

    printf '%s\n' "${CUR[@]}" >"$TMP"
    if ! probe "$TMP" "$SYM" "$WD"; then
        echo "起点件本身就不满足判据（dcc 编不过，或产物里没有 $SYM 的引用）：" >&2
        echo "先看 ${WD}/cc.log，或按真 .i 里的实际写法校正 -k 片段" >&2
        exit 1
    fi

    while :; do
        rounds=$((rounds + 1))
        (( rounds > 4000 )) && break
        (( ${#CUR[@]} < 2 )) && break
        chunk=$(( ${#CUR[@]} / n ))
        (( chunk < 1 )) && chunk=1
        i=0
        progressed=0
        while (( i < ${#CUR[@]} )); do
            if try_drop "$i" "$chunk"; then
                progressed=1
            else
                i=$(( i + chunk ))
            fi
        done
        (( progressed )) && continue
        (( n == 1 )) && break
        n=$(( n / 2 ))
        (( n < 1 )) && n=1
    done

    printf '%s\n' "${CUR[@]}" >"$dst"
    printf '%s\n' "${CUR[@]}" >"$TMP"
    if probe "$TMP" "$SYM" "$WD"; then
        echo "原始 ${total} 行 → 缩减后 ${#CUR[@]} 行；判据复核通过（dcc 编过且仍引用 ${SYM}）"
    else
        echo "原始 ${total} 行 → 缩减后 ${#CUR[@]} 行；判据复核**失败**（结论不可用）" >&2
        exit 1
    fi
    echo "输出：$dst"
}

# 自检：正例必须真的缩小，三个反例必须逐个退出 1（少一条就会把「判据失效」当成功）。
selftest() {
    local wd ok=0 bad=0 n
    [[ -x "$DCC" ]] || { echo "selftest SKIP: 没有可执行的 dcc（$DCC）"; return 2; }
    command -v nm >/dev/null || { echo "selftest SKIP: 没有 nm"; return 2; }
    wd="$(mktemp -d "${TMPDIR:-/tmp}/reduce_unit_st.XXXXXX")" || return 2
    # shellcheck disable=SC2064
    trap 'rm -rf "$wd"' EXIT

    local decl='extern void __pt_no_sw_bit(void);'
    local hdr='int pt_feature(const struct pt_common *common,'
    local mac='#define PT_FEAT_DMA_INCOHERENT 8ul'
    local body='    if (!(common->features & PT_FEAT_DMA_INCOHERENT))
        __pt_no_sw_bit();
    return bit != 0;'

    # 正例：核心 + 40 行可删的填充
    {
        echo "$decl"
        echo 'struct pt_common { unsigned long features; };'
        echo "$mac"
        echo "$hdr"
        echo '         unsigned int bit)'
        echo '{'
        echo "$body"
        echo '}'
        for n in $(seq 1 40); do echo "extern int filler_$n;"; done
    } >"$wd/orig.i"

    local st=(-k "$decl" -k "$hdr" -k 'PT_FEAT_DMA_INCOHERENT')
    if bash "${BASH_SOURCE[0]}" "${st[@]}" "$wd/orig.i" __pt_no_sw_bit "$wd/min.i" >"$wd/log" 2>&1
    then
        local before after
        before=$(wc -l <"$wd/orig.i")
        after=$(wc -l <"$wd/min.i")
        if (( after < before )) && grep -qF '    __pt_no_sw_bit();' "$wd/min.i"; then
            echo "PASS 正例：$before 行 → $after 行，且陷阱调用行仍在"
            ok=$((ok + 1))
        else
            echo "FAIL 正例：没有缩小，或把陷阱调用行删掉了（$before → $after）"
            sed 's/^/    /' "$wd/log"
            bad=$((bad + 1))
        fi
    else
        echo "FAIL 正例：退出码非 0"
        sed 's/^/    /' "$wd/log"
        bad=$((bad + 1))
    fi

    # 反例 1：必须保留片段缺失（声明少了 extern，其余照旧）
    sed 's/^extern void __pt_no_sw_bit(void);$/void __pt_no_sw_bit(void);/' \
        "$wd/orig.i" >"$wd/bad_keep.i"
    # 反例 2：能编过但没有该符号引用
    sed '/__pt_no_sw_bit();/d' "$wd/orig.i" >"$wd/bad_sym.i"
    # 反例 3：有引用但编不过
    sed 's/^}$/    int broken = ;\n}/' "$wd/orig.i" >"$wd/bad_cc.i"

    local c
    for c in bad_keep bad_sym bad_cc; do
        if bash "${BASH_SOURCE[0]}" "${st[@]}" "$wd/$c.i" __pt_no_sw_bit "$wd/$c.out" \
            >"$wd/$c.log" 2>&1
        then
            echo "FAIL 反例 $c：本应拒绝却退出 0"
            bad=$((bad + 1))
        else
            echo "PASS 反例 $c：已拒绝（rc=1）"
            ok=$((ok + 1))
        fi
    done

    echo "=== selftest: pass=$ok fail=$bad ==="
    (( bad == 0 ))
}

while (( $# > 0 )); do
    case "$1" in
        --selftest) selftest; exit $? ;;
        -k) (( $# >= 2 )) || usage; MUST_KEEP+=("$2"); shift 2 ;;
        -h|--help) usage ;;
        --) shift; break ;;
        -*) die "未知选项: $1" ;;
        *) break ;;
    esac
done

(( $# == 3 )) || usage
SYM="$2"
if (( ${#MUST_KEEP[@]} == 0 )); then
    MUST_KEEP=('extern void __pt_no_sw_bit(void);'
               'pt_feature(const struct pt_common *common,'
               'PT_FEAT_DMA_INCOHERENT')
fi
[[ -x "$DCC" ]] || die "dcc 不可执行: $DCC（用 DCC=… 指定）"
command -v nm >/dev/null || die "没有 nm（binutils）"

reduce "$1" "$3"
