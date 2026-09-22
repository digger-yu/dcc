#!/bin/bash
# scripts/probe_backend_features.sh - 后端能力**行为**探针
#
# 第一性原理：能力矩阵的唯一 truth 是「产物里到底有什么」，不是文档、也不是
# features.rs 里的手工声明。本脚本把三者钉在一起：
#     dcc 实测产物  ==  docs/backend_feature_matrix.md  ==  FeatureSet::*() 声明
#
# 为什么重写（旧版四条致命缺陷，全部实测确证）：
#   1) TEST_INPUTS 指向不存在的 tests/input/ → 50/50 全是 MISSING_FIXTURE，
#      却**恒返回 exit 0**，门禁形同虚设。真实 fixtures 在 tests/fixtures/c/。
#   2) compile_fixture() 调的是 `gcc -cc1`（gcc 根本没有 -cc1），从未调用 dcc。
#   3) run_probes() 只对 ARCHES[0]/ARCHES[1] 采样，逐架构循环体是占位符 `:`。
#   4) md 输出用 `IFS='_'` 反解 key，而 triple 与 feature 名里都可能有 `_`/`:`，
#      导致列被撕裂（`| x86 \| 64-unknown-linux-gnu_Atomics |`）。
#
# 判据分三级，**强度不混用**（这是本脚本的核心约定）：
#   artifact : 产物里存在可指纹化的证据（节名 / UND 符号 / ELF-COFF 头），
#              可以据此判言过其实 → 与声明冲突时**门禁变红**
#   accept   : 只判「接受该构造并发出了非空 .text」。它能抓到 E0xxx 拒绝，
#              抓不到「静默 no-op」→ 与声明冲突时**只告警**，不作门禁依据
#   none     : 该能力在单个 -c 产物里根本没有可观测指纹 → 输出 NA，探针**弃权**
#
# 能力探针输入一律用 tests/fixtures/probe/*.c（能力隔离的专用 fixture）。
# 不复用 tests/fixtures/c/ 里同名主题的文件，因为它们常掺入别的能力：
# 例 `bitfield.c` 在 aarch64/riscv64 上因 `sizeof(struct)` 报 E0312
# (SizeAlignOffset)，用它探 Bitfield 会得出**假阴性**。
#
# 用法:
#   scripts/probe_backend_features.sh [选项]
#     -o FMT | --output FMT   输出格式: tsv | md | json | none  (默认 tsv)
#     --dcc PATH              被测 dcc (默认 target/release/dcc)
#     --fixtures DIR          探针 fixture 目录 (默认 tests/fixtures/probe)
#     --check DOC             把生成的 md 块与 DOC 比对 + 与 features.rs 声明比对
#                             (CI 门禁入口; 真不一致 → 非零退出)
#     --keep-work             保留临时目录并打印路径 (调试用)
#   不带选项时等价于 `-o tsv`，输出到 stdout。`-o none` 只跑不打印（给 --check 用）。
#
# 退出码（把「已知缺陷」与「矩阵说谎」分档，否则门禁会被已知缺陷磨成噪音而失效）:
#   0  探针全部跑完，且（--check 时）三方一致，且无容器不可解析的后端
#   1  能力矩阵说谎：文档漂移 / artifact 级实测 NO 却仍声明 / 声明的 Feature 没有判据
#   2  用法错误 / 环境缺失（dcc 不存在、fixture 缺失、readelf/objdump 不可用）
#   3  三方一致，但存在**容器不可解析**的后端产物（现状：mips64 .o 被 objdump 拒绝）
#      —— 这是已单独立项的后端缺陷，不是矩阵问题；CI 用独立步骤把它保持为红

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

DCC="target/release/dcc"
FIXTURES="tests/fixtures/probe"
OUTPUT_FMT="tsv"
CHECK_DOC=""
KEEP_WORK=0

while [ $# -gt 0 ]; do
    case "$1" in
        -o|--output)  OUTPUT_FMT="${2:-}"; shift 2 ;;
        --dcc)        DCC="${2:-}";        shift 2 ;;
        --fixtures)   FIXTURES="${2:-}";   shift 2 ;;
        --check)      CHECK_DOC="${2:-}";  shift 2 ;;
        --keep-work)  KEEP_WORK=1;         shift ;;
        -h|--help)    sed -n '2,48p' "$0"; exit 0 ;;
        *)            echo "错误: 未知参数 $1" >&2; exit 2 ;;
    esac
done

case "$OUTPUT_FMT" in tsv|md|json|none) ;; *) echo "错误: -o 只支持 tsv|md|json|none" >&2; exit 2 ;; esac

die2() { echo "错误: $*" >&2; exit 2; }
warn() { echo "警告: $*" >&2; }

[ -x "$DCC" ] || die2 "找不到可执行 dcc: $DCC (先跑 cargo build --release)"
[ -d "$FIXTURES" ] || die2 "探针 fixture 目录不存在: $FIXTURES"
for tool in readelf objdump; do
    command -v "$tool" >/dev/null 2>&1 || die2 "缺少依赖工具: $tool"
done
for f in "$FIXTURES"/*.c; do
    [ -f "$f" ] || die2 "fixture 目录里没有 .c: $FIXTURES"
done

WORK="$(mktemp -d)"
cleanup() { [ "$KEEP_WORK" = 1 ] || rm -rf "$WORK"; }
trap cleanup EXIT
[ "$KEEP_WORK" = 1 ] && echo "workdir=$WORK" >&2

# ---------------------------------------------------------------------------
# 1. 架构清单：从 src/cli.rs 单一来源推导，不再抄第二份表
# ---------------------------------------------------------------------------
# Arch::ALL 是枚举全集，Arch::has_backend() 是开关，二者都在 cli.rs。
# 以前脚本自己写死一份 triple 表，新增后端时探针不会覆盖它却仍然 exit 0。
arch_all_variants() {
    awk '
        /pub const ALL: \[Arch;/ { inall = 1; next }
        inall && /\];/ { exit }
        inall { gsub(/[^A-Za-z0-9_]/, "", $0); sub(/^Arch/, "", $0); if ($0 != "") print $0 }
    ' src/cli.rs
}
# has_backend() 里返回 false 的变体 = 未注册后端
arch_disabled_variants() {
    awk '
        /pub fn has_backend\(self\) -> bool \{/ { inf = 1; next }
        inf && /^    \}/ { exit }
        inf && /=> false/ { for (i = 1; i <= NF; i++) if ($i ~ /^Arch::/) { t = $i; gsub(/[^A-Za-z0-9_]/, "", t); sub(/^Arch/, "", t); print t } }
    ' src/cli.rs
}

map_triple() {
    # Arch 变体名 -> 本仓库规范 triple。新增后端时若漏改本表，
    # 下面的数量/一致性断言会立刻变红，不会静默漏测。
    case "$1" in
        X86_64)   echo "x86_64-linux-gnu" ;;
        AArch64)  echo "aarch64-linux-gnu" ;;
        RiscV64)  echo "riscv64-linux-gnu" ;;
        Arm)      echo "arm-linux-gnueabi" ;;
        Mips)     echo "mips-linux-gnu" ;;
        Mips64)   echo "mips64-linux-gnu" ;;
        RiscV32)  echo "riscv32-unknown-elf" ;;
        *)        echo "" ;;
    esac
}

ALL_VARIANTS="$(arch_all_variants)"
DISABLED_VARIANTS="$(arch_disabled_variants | sort -u)"
[ -n "$ALL_VARIANTS" ] || die2 "无法从 src/cli.rs 解析 Arch::ALL"

ENABLED_LIST=""
for v in $ALL_VARIANTS; do
    if echo "$DISABLED_VARIANTS" | grep -qx "$v"; then continue; fi
    t="$(map_triple "$v")"
    [ -n "$t" ] || die2 "Arch::$v 已注册但 probe 脚本没有对应 triple 映射 (需同步 map_triple)"
    ENABLED_LIST="$ENABLED_LIST $v=$t"
done
ENABLED_COUNT=$(echo $ENABLED_LIST | wc -w)
REG_SOME_COUNT=$(awk '
    /pub fn backend_for\(/ { inf = 1 }
    inf && /^}/ { exit }
    inf && /=> Some\(/ { n++ }
    END { print n + 0 }
' src/codegen/registry.rs)
if [ "$ENABLED_COUNT" != "$REG_SOME_COUNT" ]; then
    die2 "cli.rs 推导的已注册架构数 ($ENABLED_COUNT) != registry.rs backend_for 的 Some() 分支数 ($REG_SOME_COUNT)"
fi

# ---------------------------------------------------------------------------
# 2. 声明侧：FeatureSet 构造函数 -> 特性 token；arch -> 用了哪个构造函数
# ---------------------------------------------------------------------------
# token 规范化: Feature::X -> X ; InlineAsm(AsmLevel::Full) -> InlineAsm:Full ;
#               Dwarf(4) -> Dwarf:4
FEATURES_RS="src/codegen/features.rs"
parse_feature_tokens() {
    # 先剥掉 `Feature::` 之前的前缀（riscv64_cross 里是 `fs.features.push(Feature::…`）。
    sed -e 's/.*Feature::/Feature::/' \
        -e 's/Feature::\([A-Za-z0-9_]*\)(\(AsmLevel::\|SanKind::\|ObjectFmt::\)\([A-Za-z0-9_]*\)).*/\1:\3/' \
        -e 's/Feature::\([A-Za-z0-9_]*\)(\([0-9]*\)).*/\1:\2/' \
        -e 's/Feature::\([A-Za-z0-9_]*\).*/\1/'
}
# 输出 "<ctor>\t<token>" 与 "<ctor>\t+<token>"(push) 与 "<ctor>\t=<base ctor>"
parse_features_ctors() {
    # 先剖掉 // 注释: 后端源码里常有「见 FeatureSet::tier1_cross() 文档注释」这类
    # 提及, 不剖就会被当成该后端自己的声明读到。
    sed 's://.*::' "$FEATURES_RS" | awk -v OFS='\t' '
        /^    pub fn ([a-z0-9_]+)\(\) -> Self \{/ {
            ctor = $0; sub(/.*pub fn /, "", ctor); sub(/\(\).*/, "", ctor); next
        }
        /^    \}/ { ctor = ""; next }
        ctor != "" && /Self::[a-z0-9_]+\(\)/ {
            base = $0; sub(/.*Self::/, "", base); sub(/\(\).*/, "", base);
            if (base != ctor) print ctor, "=" base
        }
        ctor != "" && /Feature::/ { print ctor, $0 }
    ' | while IFS=$'\t' read -r c payload; do
        [ -n "$c" ] || continue
        case "$payload" in
            # 继承标记行（`=base`）原样透传, 不能当 Feature token 规范化
            =*) printf '%s\t%s\n' "$c" "$payload" ;;
            *)  printf '%s\t%s\n' "$c" "$(echo "$payload" | parse_feature_tokens)" ;;
        esac
    done
}
CTOR_TABLE="$WORK/ctors.tsv"
parse_features_ctors >"$CTOR_TABLE"

# 展开继承（tier1_cross + push 形式），输出 ctor -> 排序后的 token 集。
# 第二个参数是已访问 ctor 的逗号集合: bash 的 local 变量在递归里各自独立,
# 所以必须把「路径」当参数传, 不能用全局累加器（那样会误判成环）。
ctor_tokens2() {
    local ctor="$1" seen=",$2," base="" extra="" c payload
    case "$seen" in *",$ctor,"*) die2 "FeatureSet 构造函数继承成环: $ctor" ;; esac
    while IFS=$'\t' read -r c payload; do
        [ "$c" = "$ctor" ] || continue
        case "$payload" in
            =*) base="${payload#=}" ;;
            *)  extra="$extra$payload"$'\n' ;;
        esac
    done <"$CTOR_TABLE"
    {
        [ -n "$base" ] && ctor_tokens2 "$base" "$seen$ctor,"
        printf '%s' "$extra"
    } | grep -v '^$' | sort -u
}
# 每个 ctor 的实际 token 集（含继承）写进 cache 文件，供 arch 查询
while read -r ctor; do
    [ -n "$ctor" ] || continue
    ctor_tokens2 "$ctor" "" >"$WORK/ctor.$ctor"
done < <(cut -f1 "$CTOR_TABLE" | sort -u)

# arch 变体 -> 它用的 ctor（从后端源码里读，不写死）
ctor_of_variant() {
    local v="$1" src toks
    case "$v" in
        X86_64)  src="src/codegen/x86_64/backend.rs src/codegen/x86_64.rs" ;;
        AArch64) src="src/codegen/aarch64/backend.rs src/codegen/aarch64.rs" ;;
        RiscV64) src="src/codegen/riscv64.rs" ;;
        Arm)     src="src/codegen/arm.rs" ;;
        Mips)    src="src/codegen/mips.rs" ;;
        Mips64)  src="src/codegen/mips64.rs" ;;
        *)       src="" ;;
    esac
    for s in $src; do
        [ -f "$s" ] || continue
        toks=$(sed 's://.*::' "$s" | grep -o 'FeatureSet::[a-z0-9_]*()' | head -1)
        if [ -n "$toks" ]; then
            echo "${toks#FeatureSet::}" | sed 's/()//'
            return 0
        fi
    done
    return 1
}

# ---------------------------------------------------------------------------
# 3. 探针判据
# ---------------------------------------------------------------------------
# 每条: 特性 token | fixture | 额外 flags | 判据种类 | 判据参数 | 证据级别
# 证据级别: artifact / accept / none
SPEC="$WORK/spec.tsv"
cat >"$SPEC" <<'SPECEOF'
CoreArith|core_arith.c|-c|text_nonempty|-|accept
CoreCall|core_call.c|-c|und_sym|callee|artifact
Float|float_basic.c|-c|text_nonempty|-|accept
LongDouble|long_double_basic.c|-c|text_nonempty|-|accept
Int128|int128_basic.c|-c|text_nonempty|-|accept
Bitfield|bitfield_basic.c|-c|text_nonempty|-|accept
VaList|valist_basic.c|-c|text_nonempty|-|accept
Tls|tls_basic.c|-c|all_sections|.tdata,.tbss|artifact
Atomics|atomic_basic.c|-c|text_nonempty|-|accept
InlineAsm:Minimal|asm_minimal.c|-c|text_nonempty|-|accept
InlineAsm:Partial|asm_partial.c|-c|text_nonempty|-|accept
InlineAsm:Full|asm_full.c|-c|text_nonempty|-|accept
Vector|vector_basic.c|-c|text_nonempty|-|accept
Sanitizer:Address|asan_basic.c|-fsanitize=address|und_sym|__asan_load4|artifact
Sanitizer:UndefinedBehavior|ubsan_basic.c|-fsanitize=undefined|und_sym|__ubsan_handle_divrem_overflow|artifact
CfiUnwind|core_call.c|-g|any_section|.eh_frame|artifact
StackProtect|stack_prot_basic.c|-fstack-protector-strong|und_sym|__stack_chk_fail|artifact
Lto|lto_basic.c|-flto|any_section|.dcc.lto,.llvm.lto|artifact
BoundsCheck|bounds_basic.c|-fbounds-check|text_nonempty|-|accept
Trapv|trapv_basic.c|-ftrapv|und_sym|__dcc_trapv_overflow|artifact
ObjectFormat:Elf|core_arith.c|-c|elf_rel|-|artifact
ObjectFormat:Coff|core_arith.c|-c|coff|-|artifact
Dwarf:4|core_call.c|-g|all_sections|.debug_info,.debug_line,.debug_abbrev|artifact
ParallelCodegen||-|abstain|-|none
RiscVBorrowedRegs||-|abstain|-|none
SPECEOF

# 新增 Feature 却忘了加判据 → 立刻变红（旧版就是靠 catch-all 把未知特性
# 当成 baseline 直接算过，等于给未测能力自动盖章）
DECLARED_TOKENS="$WORK/declared_tokens"
cat "$WORK"/ctor.* 2>/dev/null | sort -u >"$DECLARED_TOKENS"
while read -r tok; do
    [ -n "$tok" ] || continue
    awk -F'|' -v t="$tok" '$1 == t { found = 1 } END { exit !found }' "$SPEC" \
        || die2 "Feature::$tok 有声明但探针没有判据 (在 SPEC 里补一行)"
done <"$DECLARED_TOKENS"

# ObjectFormat:Coff 必须用 windows triple 去问，否则这一格探的是「默认容器是不是 ELF」
# 而不是「能不能产 COFF」——那是重复探 ObjectFormat:Elf，而且永远探不出真相。
# 实测：这些 windows triple **全部被 dcc 接受**，但除 x86_64 外产出的仍是 ELF
# （`od -An -tx1 -N2` = `7f45`，不是 COFF 的 `6486`）——即「要 COFF 给 ELF」是静默的，
# 不报错。这一条已记进 docs/backend_feature_matrix.md 的已知行为分歧。
probe_objfmt_coff_triple() {
    case "$1" in
        X86_64)   echo "x86_64-w64-windows-msvc" ;;
        AArch64)  echo "aarch64-w64-windows-gnu" ;;
        RiscV64)  echo "riscv64-w64-windows-msvc" ;;
        Arm)      echo "arm-w64-windows-msvc" ;;
        Mips)     echo "mips-w64-windows-gnu" ;;
        Mips64)   echo "mips64-w64-windows-gnu" ;;
        # 新增架构漏配 windows triple 时宁可炸，也不要静默地用 Linux triple 探出一个
        # 看似成立的 ❌（那会把手工谎报伪装成实测结论）。
        *)        return 1 ;;
    esac
}

elf_rel_ok() { # $1=obj  -> 容器可解析 且 e_type==REL 且 e_machine 可识别
    local obj="$1" hdr
    objdump -h "$obj" >/dev/null 2>&1 || return 1
    hdr=$(LC_ALL=C readelf -h "$obj" 2>/dev/null) || return 1
    echo "$hdr" | awk -F':  *' '/^  Type:/    { if ($2 !~ /^REL/) exit 1;      found_t=1 } END { exit found_t ? 0 : 1 }' || return 1
    echo "$hdr" | awk -F':  *' '/^  Machine:/ { if ($2 ~ /<unknown>/) exit 1;  found_m=1 } END { exit found_m ? 0 : 1 }' || return 1
    return 0
}
elf_machine() {
    LC_ALL=C readelf -h "$1" 2>/dev/null | awk -F':  *' '/^  Machine:/ {sub(/^ +/,"",$2); print $2; exit}'
}
coff_ok() { [ "$(od -An -tx1 -N2 "$1" 2>/dev/null | tr -d ' \n')" = "6486" ]; }

run_probe() { # $1=variant $2=triple $3=fixture $4=flags $5=kind $6=arg  -> 打印 verdict<TAB>evidence
    local variant="$1" triple="$2" fixture="$3" flags="$4" kind="$5" arg="$6"
    local obj="$WORK/$variant.o" out rc
    if [ "$kind" = "abstain" ]; then printf 'NA\tno observable fingerprint in a -c object\n'; return 0; fi

    case "$kind" in
        coff)
            triple="$(probe_objfmt_coff_triple "$variant")" || { printf 'NA\t该架构没有已知的 windows triple, 无法探 COFF\n'; return 0; }
            ;;
    esac

    # 容器整体不可解析时，所有依赖 binutils 的判据都只能得「无法判定」而不是「不支持」。
    # coff 类判据走 od 读 magic，与 ELF 容器无关，所以不在这里短路。
    if is_unverifiable_variant "$variant"; then
        case "$kind" in
            text_nonempty|all_sections|any_section|und_sym|elf_rel)
                printf 'UNK\t容器不可解析(objdump -h 拒绝其 -c 产物), 无法判定\n'; return 0 ;;
        esac
    fi
    out=$("$DCC" --target "$triple" $flags -c "$FIXTURES/$fixture" -o "$obj" 2>&1 >/dev/null) ; rc=$?
    if [ $rc -ne 0 ]; then
        printf 'NO\tdcc failed (rc=%d): %s\n' "$rc" "$(echo "$out" | grep -m1 -E '^error' || echo "exit $rc")"
        return 0
    fi
    case "$kind" in
        text_nonempty)
            local hex
            hex=$(objdump -h "$obj" 2>/dev/null | awk '$1 ~ /^[0-9]+$/ && $2 == ".text" {print $3; exit}')
            if [ -z "$hex" ]; then printf 'NO\tobject rejected by binutils (objdump -h)\n'; return 0; fi
            if [ $((16#$hex)) -gt 0 ]; then printf 'YES\t.text=%d bytes\n' "$((16#$hex))"
            else printf 'NO\t.text is empty\n'; fi
            ;;
        all_sections)
            local miss="" s n_all=0 IFS=','
            for s in $arg; do
                n_all=$((n_all + 1))
                section_names "$obj" | grep -qx -- "$s" || miss="$miss $s"
            done
            if [ -n "$miss" ]; then printf 'NO\tmissing:%s\n' "$miss"; else printf 'YES\tall %d section(s) present\n' "$n_all"; fi
            ;;
        any_section)
            local s hit="" IFS=','
            for s in $arg; do
                section_names "$obj" | grep -qx -- "$s" && hit="$hit $s"
            done
            if [ -n "$hit" ]; then printf 'YES\tfound:%s\n' "$hit"; else printf 'NO\tnone of (%s)\n' "$arg"; fi
            ;;
        und_sym)
            if ! und_syms "$obj" | grep -qx -- "$arg"; then
                printf 'NO\tno UND reference to %s\n' "$arg"
            else
                printf 'YES\treferences UND %s\n' "$arg"
            fi
            ;;
        elf_rel)
            if elf_rel_ok "$obj"; then
                printf 'YES\tET_REL + Machine: %s\n' "$(elf_machine "$obj")"
            else
                printf 'NO\tobject not parseable by binutils (objdump -h rejected)\n'
            fi
            ;;
        coff)
            # 把实际用的 triple 写进证据: 这一行与其他行不是同一个 triple, 不写出来就没人
            # 能仅凭矩阵列名反推出它问的是什么。
            if coff_ok "$obj"; then printf 'YES\t%s -> COFF magic 0x8664\n' "$triple"
            else printf 'NO\t%s -> 不是 COFF (magic=%s)\n' "$triple" "$(od -An -tx1 -N2 "$obj" | tr -d ' \n')"; fi
            ;;
    esac
}

und_syms() {
    # 注意 NF 可能为 0（readelf 末尾空行），$(NF-1) 会变成 $-1 让 awk 报运行时错误。
    # 排除 Type==FILE：arm 后端会把外部函数引用错误地标成 FILE 符号，
    # 名字对但链接时不会被解析，算不得“发出了对外调用”（实测：
    # `readelf -sW` 给出 `1: 00010012 0 FILE GLOBAL DEFAULT UND callee`）。
    LC_ALL=C readelf -sW "$1" 2>/dev/null \
        | awk 'NF >= 7 && $(NF-1) == "UND" && $4 != "FILE" { print $NF }'
}

section_names() {
    objdump -h "$1" 2>/dev/null | awk '$1 ~ /^[0-9]+$/ && NF >= 3 {print $2}'
}

# 容器整体不可解析 = 该架构产物根本没被 binutils 认出来（mips64 现状）
INVALID_ELF_VARIANTS=""
for entry in $ENABLED_LIST; do
    v="${entry%%=*}"; t="${entry#*=}"
    "$DCC" --target "$t" -c "$FIXTURES/core_arith.c" -o "$WORK/chk_$v.o" >/dev/null 2>&1 || true
    if ! objdump -h "$WORK/chk_$v.o" >/dev/null 2>&1; then
        INVALID_ELF_VARIANTS="$INVALID_ELF_VARIANTS $v"
    fi
done
is_unverifiable_variant() {
    local u
    for u in $INVALID_ELF_VARIANTS; do
        [ "$u" = "$1" ] && return 0
    done
    return 1
}

# ---------------------------------------------------------------------------
# 4. 跑探针
# ---------------------------------------------------------------------------
RESULTS="$WORK/results.tsv"   # variant | feature | verdict | evidence
: >"$RESULTS"
while IFS='|' read -r feat fixture flags kind arg level; do
    [ -n "$feat" ] || continue
    for entry in $ENABLED_LIST; do
        v="${entry%%=*}"; t="${entry#*=}"
        res="$(run_probe "$v" "$t" "$fixture" "$flags" "$kind" "$arg")"
        verdict="${res%%$'	'*}"
        evidence="${res#*$'	'}"
        printf '%s\t%s\t%s\t%s\n' "$v" "$feat" "$verdict" "$evidence" >>"$RESULTS"
    done
done < <(grep -v '^#' "$SPEC")

# 校验结果表覆盖度：架构数 × 判据数，一格都不能少（旧版只采 2 个架构的样）
SPEC_ROWS=$(awk 'NF' "$SPEC" | wc -l)
EXPECT_CELLS=$((ENABLED_COUNT * SPEC_ROWS))
GOT_CELLS=$(wc -l <"$RESULTS")
[ "$GOT_CELLS" = "$EXPECT_CELLS" ] || die2 "结果格数 $GOT_CELLS != 预期 $EXPECT_CELLS"

LEVEL_OF="$WORK/levels"
awk -F'|' 'NF {print $1 "\t" $6}' "$SPEC" >"$LEVEL_OF"

level_of()    { awk -F'\t' -v f="$1" '$1==f {print $2; exit}' "$LEVEL_OF"; }
cell_verdict(){ awk -F'\t' -v v="$1" -v f="$2" '$1==v && $2==f {print $3; exit}' "$RESULTS"; }
cell_evidence(){ awk -F'\t' -v v="$1" -v f="$2" '$1==v && $2==f {print $4; exit}' "$RESULTS"; }
cell_symbol() {
    # 符号与证据级别**绑定**，避免把弱证据画成 ✅：
    #   ✅/❌ = artifact 级（可指纹化，可作门禁）
    #   ◐/✗  = accept 级（仅“接受且发出非空 .text”，抓不到静默 no-op）
    #   ?     = 该架构 -c 产物容器不可解析，无法判定（不等于不支持）
    #   —     = 探针弃权（单个 -c 产物里没有可观测指纹）
    local v="$1" f="$2" lvl vd
    lvl="$(level_of "$f")"
    vd="$(cell_verdict "$v" "$f")"
    case "$vd" in
        UNK) echo "?" ;;
        YES) if [ "$lvl" = "artifact" ]; then echo "✅"; else echo "◐"; fi ;;
        NO)  if [ "$lvl" = "artifact" ]; then echo "❌"; else echo "✗"; fi ;;
        *)   echo "—" ;;
    esac
}

# 每个架构的 FeatureSet 声明集（从 features.rs 解析而来）落地成文件，供表格与断言复用
decl_file_of() {
    local v="$1" ctor
    ctor="$(ctor_of_variant "$v" || true)"
    [ -n "$ctor" ] || return 1
    [ -f "$WORK/ctor.$ctor" ] || return 1
    echo "$WORK/ctor.$ctor"
}
is_declared() { # $1=variant $2=token
    local df
    df="$(decl_file_of "$1" 2>/dev/null)" || return 2
    grep -qx -- "$2" "$df"
}

# 行 = 能力, 列 = 架构。顺序取 SPEC 书写顺序（按能力分层编排, 比字母序更能读出结构）
FEATURE_ORDER="$(awk -F'|' 'NF {print $1}' "$SPEC")"
VARIANTS="$(for e in $ENABLED_LIST; do echo "${e%%=*}"; done)"

render_generated_block() {
    # 关键约定：生成块里**不写任何时间戳**。带 $(date) 的输出永远无法与仓库里的
    # 文档做 diff 断言——旧版 CI 因此退化成了「文件存在性判断」这种假门禁。
    #
    # 块里同时放**两张矩阵**：实测矩阵 + FeatureSet 声明矩阵。这样单靠一次 diff
    # 就能把「文档 == 实测」和「文档 == 声明」两条边钉住，第三边（实测 == 声明）
    # 由 --check 里的逐格断言负责。
    local f v header sep line
    header="| Feature | 证据 |"
    sep="|---|---|"
    for v in $VARIANTS; do header="$header $v |"; sep="$sep---|"; done
    echo "<!-- BEGIN GENERATED BY scripts/probe_backend_features.sh -->"
    echo "#### 探针实测矩阵"
    echo "$header"
    echo "$sep"
    while read -r f; do
        [ -n "$f" ] || continue
        line="| $f | $(level_of "$f") |"
        for v in $VARIANTS; do line="$line $(cell_symbol "$v" "$f") |"; done
        echo "$line"
    done <<<"$FEATURE_ORDER"
    # 末行把行名与列名对齐地回显实际用的 triple——三方断言里「探针跑的到底是哪个
    # triple」必须是可读的, 否则 CI 红了也查不出是哪一行错位。
    line="| triple (探针实际用的) | — |"
    for v in $VARIANTS; do line="$line $(map_triple "$v") |"; done
    echo "$line"
    echo
    echo "#### FeatureSet 声明矩阵（由 src/codegen/features.rs 解析，非人工维护）"
    echo "$header"
    echo "$sep"
    while read -r f; do
        [ -n "$f" ] || continue
        line="| $f | $(level_of "$f") |"
        for v in $VARIANTS; do
            case "$(is_declared "$v" "$f" && echo Y || echo N)" in
                Y) line="$line 声明 |" ;;
                *) line="$line -- |" ;;
            esac
        done
        echo "$line"
    done <<<"$FEATURE_ORDER"
    line="| FeatureSet 构造函数 | — |"
    for v in $VARIANTS; do line="$line $(ctor_of_variant "$v" 2>/dev/null || echo '?') |"; done
    echo "$line"
    echo "<!-- END GENERATED BY scripts/probe_backend_features.sh -->"
}

render_json() {
    local v f first=1
    echo '{"probes":['
    while IFS=$'\t' read -r v f vd ev; do
        [ "$first" = 1 ] || echo ','
        first=0
        printf '  {"arch":"%s","feature":"%s","level":"%s","verdict":"%s","evidence":"%s"}' \
            "$v" "$f" "$(level_of "$f")" "$vd" "${ev//\"/\\\"}"
    done <"$RESULTS"
    echo
    echo ']}'
}

case "$OUTPUT_FMT" in
  tsv)
    printf 'variant\tfeature\tlevel\tverdict\tevidence\n'
    while IFS=$'\t' read -r v f vd ev; do
        printf '%s\t%s\t%s\t%s\t%s\n' "$v" "$f" "$(level_of "$f")" "$vd" "$ev"
    done <"$RESULTS"
    ;;
  md)  render_generated_block ;;
  json) render_json ;;
  none) ;; # --check 专用: 只要退出码与 stderr 上的 FAIL/WARN, 不污染 stdout
esac

# ---------------------------------------------------------------------------
# 5. --check：三方一致（实测 == 文档 == FeatureSet 声明）
# ---------------------------------------------------------------------------
# STATUS         : 矩阵说谎（文档漂移 / 言过其实）-> exit 1
# CONTAINER_FAIL : 存在容器不可解析的后端产物（已单独立型的已知缺陷）-> exit 3
# 两者分开是因为 CI 对它们的处理语义不同：前者必须当场修后者，后者是红得有原因、
# 且解锁条件已写在 docs/backend_feature_matrix.md 里。混在一个退出码里，
# 就只能整条流水线长期红，久而久之所有人都当它是噪音频于忽略——那门禁又等于不存在。
STATUS=0
CONTAINER_FAIL=0

if [ -n "$INVALID_ELF_VARIANTS" ]; then
    # 容器不可解析不是「某项能力缺失」，而是「这个 .o 谁也用不了」，单列一条硬失败。
    echo "FAIL: 以下架构的 -c 产物被宿主 objdump 拒绝(容器不可解析), 全部 artifact 级判据无法成立:" >&2
    for v in $INVALID_ELF_VARIANTS; do
        echo "      $v ($(map_triple "$v"))" >&2
    done
    CONTAINER_FAIL=1
fi

if [ -n "$CHECK_DOC" ]; then
    [ -f "$CHECK_DOC" ] || die2 "--check 指向的文档不存在: $CHECK_DOC"
    GENERATED="$WORK/generated.md"
    render_generated_block >"$GENERATED"
    if ! sed -n '/<!-- BEGIN GENERATED BY/,/<!-- END GENERATED BY/p' "$CHECK_DOC" >"$WORK/in_doc.md"; then
        die2 "$CHECK_DOC 里没有 GENERATED 标记块"
    fi
    if [ ! -s "$WORK/in_doc.md" ]; then
        die2 "$CHECK_DOC 的 GENERATED 块为空 (可用 '-o md' 重新生成后粘贴)"
    fi
    if ! diff -u "$WORK/in_doc.md" "$GENERATED" >"$WORK/doc_diff.txt" 2>&1; then
        echo "FAIL: 文档 $CHECK_DOC 与探针实测不一致:" >&2
        cat "$WORK/doc_diff.txt" >&2
        STATUS=1
    else
        echo "OK: 文档表格 == 探针实测 ($(wc -l <"$GENERATED") 行)"
    fi

    # 第三边：FeatureSet 声明 vs 实测。判据的强弱不同，结论的硬软也不同：
    #   artifact 级 NO 而仍被声明 = 拿不出指纹 = 言过其实   -> 红
    #   任何级别但 dcc 直接报错(E0xxx) 而仍被声明 = 编译被拒绝 -> 红（这不是弱证据）
    #   accept 级 NO（能编译、但 .text 空/无产物）而仍被声明  -> 只告警，不作门禁依据
    #   实测 YES 但未声明 = 保守声明（ctor 是多架构共享的交集，下取整合法） -> 只告警
    #   UNK （容器不可解析）不参与比对：它表达的是“测不了”，不是“不支持”
    for entry in $ENABLED_LIST; do
        v="${entry%%=*}"
        ctor="$(ctor_of_variant "$v" || true)"
        [ -n "$ctor" ] || { warn "$v: 无法从后端源码解析它用哪个 FeatureSet 构造函数"; continue; }
        decl_file="$WORK/ctor.$ctor"
        [ -f "$decl_file" ] || { warn "$v: FeatureSet::$ctor() 未解析出任何 token"; continue; }
        while read -r tok; do
            [ -n "$tok" ] || continue
            lvl="$(level_of "$tok")"
            vd="$(cell_verdict "$v" "$tok")"
            [ "$vd" != "UNK" ] || continue
            if grep -qx -- "$tok" "$decl_file"; then
                ev="$(cell_evidence "$v" "$tok")"
                case "$vd" in
                    NO)
                        if [ "$lvl" = "artifact" ] || printf '%s' "$ev" | grep -q 'dcc failed'; then
                            echo "FAIL: $v 声明 Feature::$tok, 但实测不成立[$lvl]: $ev" >&2
                            STATUS=1
                        else
                            warn "$v 声明 Feature::$tok, 但 accept 级探针未通过: $ev"
                        fi ;;
                esac
            elif [ "$lvl" = "artifact" ] && [ "$vd" = "YES" ]; then
                warn "$v 实测有 Feature::$tok 的产物指纹[$lvl], 但声明矩阵里没有（保守声明）"
            fi
        done < <(awk -F'|' 'NF {print $1}' "$SPEC")
    done
fi

# 一行摘要（同时给旧版的 "Probe complete: N passed" 保留可读性, 但不再恒绿）
PASS=$(awk -F'\t' '$3=="YES"' "$RESULTS" | wc -l)
FAILN=$(awk -F'\t' '$3=="NO"' "$RESULTS" | wc -l)
NA=$(awk -F'\t' '$3=="NA"' "$RESULTS" | wc -l)
UNKN=$(awk -F'\t' '$3=="UNK"' "$RESULTS" | wc -l)
echo "Probe complete: $PASS YES / $FAILN NO / $UNKN UNK / $NA NA  (arches=$ENABLED_COUNT, features=$(echo $FEATURE_ORDER | wc -w))" >&2
echo "Probe exit: matrix=$STATUS container=$CONTAINER_FAIL" >&2

if [ "$STATUS" != 0 ]; then exit 1; fi
if [ "$CONTAINER_FAIL" != 0 ]; then exit 3; fi
exit 0


