#!/bin/bash
# ===========================================================================
# scripts/gen_coverage_index.sh — 测试覆盖索引全量生成器（L-C 的核心）
#
# 权威规格: docs/test_coverage.md（本文的结构 / 索引的维护契约 / 主题划分口径 /
#           判据与 Tier 口径 / C++ ABI 五主题）+ tests/README.md §2.1（主题子文件规则）
#           + tests/fixtures/README.md（fixture 引用面）
# 依赖: 仅 bash + coreutils + awk/sed/grep/find —— 不使用 python/perl/ruby/jq
#
# 输入:
#   tests/integration/*.rs        15 个主题子文件（被壳 include!，非独立 target）
#   tests/*.rs                    6 个顶层 target（含壳 tests/integration.rs）
#   tests/common/harness.rs       公共 harness —— 判据面的真值来源
#   tests/common/harness/*.rs     harness 子模块（elf / cpp_frontend / driver_cli）
#   tests/fixtures/               磁盘真值（fixture 解析与存在性）
# 输出:
#   只重写 docs/test_coverage.md 中
#     <!-- BEGIN GENERATED --> ... <!-- END GENERATED -->
#   包裹的区域；区间外的人工校订内容一字不改（落盘前用哈希双向核对）。
#   全部内容由代码 + 磁盘真值推导、不含时间戳，因此重复执行字节级幂等。
#
# ---------------------------------------------------------------------------
# 判据推断表 —— 由函数体调用的助手推断；一个函数体命中多条时按优先级取最强：
#   report-only > cross-check > sanitize > run > compile
#   * #[ignore] 覆盖一切 → report-only（默认 cargo test 不执行，只出报告）
#   * 函数体既无已登记助手、也无 Command::new / assert / expect / panic 痕迹
#     → JAC-UNINFERABLE（exit 1）。「空壳测试」不允许静默按 compile 入账。
#   * 助手名单本身是实测的：harness 的全部 pub(crate) fn + 各顶层 target 的
#     本地 fn。新增助手没登记判据 → JAC-UNMAPPED（exit 1），不会悄悄降级。
#
#   run         编译产物真的跑起来（主机或 qemu），观察退出码/输出
#               run_exe, run_exe_bounded, run_compile_and_run,
#               run_compile_and_run_flags, qemu_runtime_target
#   compile     只看编译产物（.o / .s / stderr / IR），不承诺运行时
#               run_syntax_test, run_object_test, run_object_test_flags,
#               run_asm_output_test, run_asm_output_test_flags,
#               compile_asm_text_with_env,
#               run_compile_o, run_cpp_object_test,
#               compile_legacy_asm_object, run_cpp_with_mode, path_decision_in,
#               assert_bytes_present, assert_disasm_present, extract_function_body,
#               object_symbol_size, readelf_relocations, elf64_sections,
#               elf64_section_data, elf64_syms, elf64_func_symbols,
#               elf64_undefined_symbols, elf64_dynamic_needed, elf64_read_cstr,
#               assert_kernel_corpus_object, dcc_compile_module,
#               parse_offload_section, compile_to_object, compile_timed,
#               compile_runs, assert_all_identical,
#               assert_no_pid_shaped_hostlink_name, run_dcc, assert_snapshot,
#               validate_diagnostic_snapshot,
#               seg_asm_body, seg_has_prefix, asm_fallback_compile,
#               elf64_rela, elf64_section_names, elf64_syms_by_type, elf64_all_rela,
#               dcc_link_exe, dcc_obj_bytes, assert_dynamically_linked,
#               dcc_link_capture_flags, dcc_asm_with_pipe
#   cross-check 与外部真值逐字节/逐槽对照（g++ / gcc / 宿主链接器），或与另一条
#               lowering 路径对照（HIR vs legacy）
#               run_gcc_cross_check, run_gcc_cross_check_opt,
#               run_gcc_two_tu_cross_check,
#               cpp_gcc_cross_check, cpp_coroutine_cross_check,
#               cpp_std20_exit_code_cross_check, gxx_native_cpp_exit_code,
#               lower_both, run_host_link_and_run, run_host_link_and_run_with,
#               host_link_objects_and_run, host_link_cpp_object_and_run,
#               gxx_truth, dcc_result, check_one, check_group,
#               assert_all_levels_match_gcc
#   sanitize    带 -fsanitize=（UBSan/ASan）运行，或断言「运行时必须失败」
#               run_sanitized_binary, assert_runtime_fails_with, assert_exit_code
#   aux         只做路径/字符串/门控准备，不贡献判据
#               tool_exists, host_cc_available, gcc_src_tree_or_skip,
#               fixtures_dir, fixtures, temp_dir, write_src, dcc_cmd, dcc,
#               profile_src, offload_fixture, pgo_runtime_template, snap_path,
#               normalize_stderr, strip_ansi, utf8_char_len, fold_temp_root,
#               dcc_args_for, stderr_text, write_tmp, write_tmp_stable, temp_path,
#               fixture_root, work_dir, gxx_available, fixtures_in, all_groups,
#               empty_class, callable_key, class_key, is_backend_internal,
#               owner_name, expected_entities, collect_expected, push_if_defined,
#               native_entities, legacy_definition_lines, legacy_defines,
#               line_mentions, legacy_plain_definition_names, asm_body
#
# 本轮从主题子文件上移到 harness 的 13 个助手，类别按「助手自己做什么」定，
# 不按「调用它的用例最终证明了什么」定（运行与 gcc 对拍由调用方自己的 run_* /
# cross-check 助手贡献，不会被以下标签掩盖或抬高）：
#   compile    seg_asm_body / seg_has_prefix：只从已发出的 .s 文本里取函数体、
#              看段前缀（与 extract_function_body / path_decision_in 同类）；
#              asm_fallback_compile / dcc_asm_with_pipe / dcc_obj_bytes：只跑 dcc
#              拿 .s / .o / stderr；elf64_rela / elf64_section_names /
#              elf64_syms_by_type / elf64_all_rela：只解析产物字节（与
#              elf64_sections / elf64_syms 同一族）；assert_dynamically_linked：
#              只断言产物的 .interp / DT_NEEDED / `file` 读数；
#              dcc_link_exe / dcc_link_capture_flags：只回答「编链是否成功 +
#              stderr + 产物在哪」——宿主 cc 在这两条里是流水线上的一环，不是被
#              逐槽对照的真值，助手也不运行产物，所以记 compile 而不是 cross-check。
#   cross-check assert_all_levels_match_gcc：把同一份夹具逐档（-O0/-O1/-O2/-O3）
#              交给 gcc 与 dcc 各自编+跑，退出码逐条对照 ⇒ 真正的 gcc 对拍。
#   aux        asm_body：只做汇编文本归一（摘掉 `.file` 行），不取产物、不断言，
#              与 normalize_stderr / strip_ansi 同类，不贡献判据。
#
# Tier 推断（§9 权威表；本脚本只区分 0/3 两档 —— Tier 1/2 是 CI job 的划分，
# 不是用例本身的属性）:
#   T3  #[ignore]、#[cfg(feature = "...")]（正向特征门控，默认不构建）、
#       #[cfg(any())]（永不构建）、或函数体调用 gcc_src_tree_or_skip（需 GCC 源树）
#   T0  其余全部：`cargo test --release` 默认执行
#       #[cfg(unix)] / #[cfg(target_arch = "...")] / #[cfg(not(feature = ...))]
#       在 Linux CI 的默认 feature 下恒成立 → 仍是 T0，但一律作为标注写出，
#       不留「看不见的门控」。
#
# ---------------------------------------------------------------------------
# 校验（任何一条不成立 → 打印纯 ASCII 标签 + exit 1；失败时既不打印成功文案，
# 也不留下半成品文档）:
#   FATAL / PRE           前置：repo root、输入清单、文档标记（缺失即终止）
#   STRUCT-INCLUDE        壳里 `include!("` 条数 == 主题子文件个数
#   STRUCT-USE            主题子文件出现 `^use ` / `^mod `（§5.2 规则 2 禁止）
#   STRUCT-HELPER         主题子文件出现非测试的 `^fn`（§14：fn 数 == test 数）
#   STRUCT-DANGLE         `#[test]` 之后紧跟的不是 `^fn`（悬挂属性 = 用例被吃掉）
#   COUNT-PER-FILE        逐文件生成行数 == 该文件 `grep -c '#\[test\]'`
#   COUNT-TOTAL           主题文件生成行数之和 == `grep -h '#\[test\]' | wc -l`
#   FIXTURE-ABSENT        （报告项，不阻断）静态可解析的引用指向磁盘上不存在的
#                         夹具；索引里该路径前加 ⚠，脚本结尾汇总条数
#   JAC-UNMAPPED          harness 或顶层 target 的助手未登记进上面的判据表
#   JAC-UNINFERABLE       某个测试函数体推不出任何判据（空壳用例）——硬红：
#                         负例探针 `#[test] fn x() {}` 就是被它拦下的
#   JAC-DISABLED          （报告项，不阻断）空壳且被 #[cfg(any())] 永久门控 →
#                         判据写 disabled、Tier 写 T3
#   WRITE-MISMATCH        待落盘文档的生成区行数或区外内容与预期不符
# 用法: bash scripts/gen_coverage_index.sh
# ===========================================================================
set -o pipefail
export LC_ALL=C

DOC="docs/test_coverage.md"
BEGIN_MARK='<!-- BEGIN GENERATED -->'
END_MARK='<!-- END GENERATED -->'
FAILS=0
WARNS=0

fatal() { # 前置失败：立即终止，绝不打印任何成功文案
    echo "FATAL: $*"
    echo "gen_coverage_index: 未完成（不写文档、不打印成功文案）"
    exit 1
}

note_err() {
    echo "$*"
    FAILS=$((FAILS + 1))
}

# 报告项：如实登记进索引并打印，但不阻断生成 —— 它们的修复动作在 tests/**
# 与夹具侧（由测试归属工作流承接），不属于本生成器的职责。不阻断 ≠ 不显示：
# 这类用例在索引里带 ⚠ / disabled 标记，一眼就能看出来。
warn_err() {
    echo "$*"
    WARNS=$((WARNS + 1))
}

bail() { # 累积型错误统一出口（在写文档之前）
    if [[ $FAILS -gt 0 ]]; then
        echo "共 $FAILS 个问题 -> FAIL（文档未改动）"
        exit 1
    fi
}

# --- REPO_ROOT 由 BASH_SOURCE 解析；解析失败立即 FATAL ----------------------
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." 2>/dev/null && pwd)"
if [[ -z "${REPO_ROOT:-}" || ! -d "$REPO_ROOT/tests/integration" ]]; then
    fatal "repo root 解析失败: '${REPO_ROOT:-<empty>}' 或 tests/integration 不存在"
fi
cd "$REPO_ROOT" || fatal "无法进入仓库根目录: $REPO_ROOT"
command -v awk >/dev/null 2>&1 || fatal "awk 不可用（抽取器依赖 awk）"

SHELL_RS="tests/integration.rs"
HARNESS_SRCS=()
[[ -f tests/common/harness.rs ]] && HARNESS_SRCS+=(tests/common/harness.rs)
if [[ -d tests/common/harness ]]; then
    for h in tests/common/harness/*.rs; do [[ -f "$h" ]] && HARNESS_SRCS+=("$h"); done
fi
[[ ${#HARNESS_SRCS[@]} -gt 0 ]] || fatal "未发现 tests/common/harness*.rs（判据面真值缺失）"
[[ -f "$SHELL_RS" ]] || fatal "$SHELL_RS 不存在（壳文件是主题归属的权威顺序）"
[[ -d tests/fixtures ]] || fatal "tests/fixtures 不存在（夹具磁盘真值缺失）"
[[ -f "$DOC" ]] || fatal "$DOC 不存在：生成器只重写既有文档，不代为新建"

for m in "$BEGIN_MARK" "$END_MARK"; do
    c=$(grep -cF -- "$m" "$DOC" 2>/dev/null || true)
    [[ "${c:-0}" == "1" ]] || fatal "$DOC 中 '$m' 出现 ${c:-0} 次（必须恰好 1 次）"
done
awk -v B="$BEGIN_MARK" -v E="$END_MARK" '
index($0, B) > 0 && !b { b = NR } index($0, E) > 0 && !e { e = NR }
END { exit (b > 0 && e > b) ? 0 : 1 }' "$DOC" ||
    fatal "$DOC 的 BEGIN/END GENERATED 标记顺序错误（END 必须在 BEGIN 之后）"

# --- 输入清单 ---------------------------------------------------------------
# 主题文件顺序 = 壳里 include! 的书写顺序（人工组织的语义顺序，不做字母排序）
THEME_FILES=()
while IFS= read -r inc; do
    [[ -n "$inc" ]] || continue
    [[ -f "$inc" ]] || fatal "壳 include! 指向不存在的文件: $inc"
    THEME_FILES+=("$inc")
done < <(grep -oE 'include!\("[^"]+"\)' "$SHELL_RS" | sed -E 's|include!\("([^"]+)"\)|tests/\1|')
[[ ${#THEME_FILES[@]} -gt 0 ]] || fatal "$SHELL_RS 中没有 include!（扫描面为 0，拒绝假绿）"

TOP_FILES=()
for t in tests/*.rs; do
    [[ -f "$t" ]] || continue
    [[ "$t" == "$SHELL_RS" ]] && continue
    TOP_FILES+=("$t")
done
[[ ${#TOP_FILES[@]} -gt 0 ]] || fatal "tests/*.rs 顶层 target 为 0（扫描面为空，拒绝假绿）"
ALL_FILES=("${THEME_FILES[@]}" "${TOP_FILES[@]}" "$SHELL_RS")
for f in "${ALL_FILES[@]}"; do [[ -r "$f" ]] || fatal "输入不可读: $f"; done

WORK="$(mktemp -d)" || fatal "mktemp 失败"
trap 'rm -rf "$WORK"' EXIT

# ---------------------------------------------------------------------------
# 判据表（与文件头注释逐条对应；label 只允许 run/compile/cross-check/sanitize/aux）
# ---------------------------------------------------------------------------
CRIT_TABLE='
run_exe run
run_exe_bounded run
run_compile_and_run run
run_compile_and_run_flags run
qemu_runtime_target run
run_syntax_test compile
run_object_test compile
run_object_test_flags compile
run_asm_output_test compile
run_asm_output_test_flags compile
compile_asm_text_with_env compile
run_compile_o compile
run_cpp_object_test compile
compile_legacy_asm_object compile
run_cpp_with_mode compile
path_decision_in compile
assert_bytes_present compile
assert_disasm_present compile
extract_function_body compile
object_symbol_size compile
readelf_relocations compile
elf64_sections compile
elf64_section_data compile
elf64_syms compile
elf64_func_symbols compile
elf64_undefined_symbols compile
elf64_dynamic_needed compile
elf64_read_cstr compile
assert_kernel_corpus_object compile
dcc_compile_module compile
parse_offload_section compile
compile_to_object compile
compile_timed compile
compile_runs compile
assert_all_identical compile
assert_no_pid_shaped_hostlink_name compile
run_dcc compile
assert_snapshot compile
validate_diagnostic_snapshot compile
seg_asm_body compile
seg_has_prefix compile
asm_fallback_compile compile
elf64_rela compile
elf64_section_names compile
elf64_syms_by_type compile
elf64_all_rela compile
dcc_link_exe compile
dcc_obj_bytes compile
assert_dynamically_linked compile
dcc_link_capture_flags compile
dcc_asm_with_pipe compile
run_gcc_cross_check cross-check
run_gcc_cross_check_opt cross-check
run_gcc_two_tu_cross_check cross-check
assert_all_levels_match_gcc cross-check
cpp_gcc_cross_check cross-check
cpp_coroutine_cross_check cross-check
cpp_std20_exit_code_cross_check cross-check
gxx_native_cpp_exit_code cross-check
lower_both cross-check
run_host_link_and_run cross-check
run_host_link_and_run_with cross-check
host_link_objects_and_run cross-check
host_link_cpp_object_and_run cross-check
gxx_truth cross-check
dcc_result cross-check
check_one cross-check
check_group cross-check
run_sanitized_binary sanitize
assert_runtime_fails_with sanitize
assert_exit_code sanitize
tool_exists aux
host_cc_available aux
gcc_src_tree_or_skip aux
fixtures_dir aux
fixtures aux
temp_dir aux
write_src aux
dcc_cmd aux
dcc aux
profile_src aux
offload_fixture aux
pgo_runtime_template aux
snap_path aux
normalize_stderr aux
strip_ansi aux
utf8_char_len aux
fold_temp_root aux
dcc_args_for aux
stderr_text aux
write_tmp aux
write_tmp_stable aux
temp_path aux
fixture_root aux
work_dir aux
gxx_available aux
fixtures_in aux
all_groups aux
empty_class aux
callable_key aux
class_key aux
is_backend_internal aux
owner_name aux
expected_entities aux
collect_expected aux
push_if_defined aux
native_entities aux
legacy_definition_lines aux
legacy_defines aux
line_mentions aux
legacy_plain_definition_names aux
asm_body aux
dwarf_gdb_unavailable aux
dwarf_gdb_build compile
dwarf_gdb_build_env compile
gdb_batch run
gdb_bp_addr aux
readelf_attr compile
readelf_int compile
readelf_debug_info_dump compile
parse_debug_info compile
var_location aux
dwarf_reg_name aux
'
declare -A CRIT
while IFS=' ' read -r h lab; do
    [[ -n "$h" ]] || continue
    case "$lab" in
        run | compile | cross-check | sanitize | aux) ;;
        *) fatal "判据表条目非法: '$h $lab'" ;;
    esac
    CRIT["$h"]="$lab"
done <<<"$CRIT_TABLE"

# --- 夹具磁盘真值 → basename 索引（供「只给文件名」的引用收敛）-------------
find tests/fixtures -type f \( -name '*.c' -o -name '*.s' -o -name '*.cpp' -o -name '*.h' \) \
    -print 2>/dev/null | sort >"$WORK/fixtures.txt" || fatal "find tests/fixtures 失败"
[[ -s "$WORK/fixtures.txt" ]] || fatal "tests/fixtures 下未发现任何夹具源文件"
declare -A BASE_PATHS BASE_COUNT
while IFS= read -r p; do
    b="${p##*/}"
    BASE_PATHS["$b"]+="$p"$'\n'
    BASE_COUNT["$b"]=$(( ${BASE_COUNT["$b"]:-0} + 1 ))
done <"$WORK/fixtures.txt"

# --- 助手真值之一：harness 的 pub(crate) fn（判据面的权威来源）-------------
HARNESS_FNS="$(grep -hoE '^pub\(crate\) (async )?fn [A-Za-z0-9_]+' "${HARNESS_SRCS[@]}" |
    awk '{ print $NF }' | sort -u | tr '\n' ' ')"
[[ -n "${HARNESS_FNS// /}" ]] || fatal "harness 中没有 pub(crate) fn（判据面真值缺失）"

for h in $HARNESS_FNS; do
    [[ -v CRIT["$h"] ]] || note_err "JAC-UNMAPPED: harness 助手 $h 未登记判据（见脚本头部推断表）"
done
bail

# ---------------------------------------------------------------------------
# 抽取器（awk）。主题子文件按 §5.2 是「纯 #[test] fn 序列」，所以
#   测试体 = [fn 行, 下一个 #[test] 的前一行]，再从尾部裁掉空行与注释行
# 这样既不需要脆弱的花括号配对（夹具里的 C 源码会在第 0 列写出 `}` / `struct ...`，
# 计数法必然被带偏），也不会把下一个用例的 /// 文档注释（里面常写夹具名）
# 误算进上一个用例。属性由 #[test] 的上下两侧一起收集（两种书写顺序都在用）。
# ---------------------------------------------------------------------------
cat >"$WORK/extract.awk" <<'AWK'
function norm(s) { sub(/^#\[/, "", s); sub(/\][ \t]*$/, "", s); gsub(/[ \t]+/, " ", s); return s }
BEGIN {
    n = split(HN, _h, /[ \t]+/)
    for (i = 1; i <= n; i++) if (_h[i] != "") isH[_h[i]] = 1
}
{ L[NR] = $0 }
END {
    total = NR
    for (i = 1; i <= total; i++) {
        if (L[i] !~ /^#\[/ || L[i] !~ /^#\[test\][ \t]*$/) continue
        T = i
        attrs = ""
        for (b = T - 1; b >= 1; b--) {           # 回扫紧邻的属性块
            bl = L[b]
            if (bl ~ /^[ \t]*$/ || bl ~ /^(\/\/|\/\*|\*)/) continue
            if (bl ~ /^#!\[/) break
            if (bl !~ /^#\[/) break
            a = norm(bl)
            if (a ~ /^ignore/ || a ~ /^cfg\(/) attrs = a ";" attrs
        }
        j = T + 1                                # 前扫到 fn
        while (j <= total && (L[j] ~ /^#\[/ || L[j] ~ /^(\/\/|\/\*|\*)/ || L[j] ~ /^[ \t]*$/)) {
            if (L[j] ~ /^#\[/ && L[j] !~ /^#!\[/) {
                a = norm(L[j])
                if (a ~ /^ignore/ || a ~ /^cfg\(/) attrs = attrs a ";"
            }
            j++
        }
        if (j > total || L[j] !~ /^fn /) {
            print "D\t" F "\t" T "\t" (substr(L[j], 1, 60) == "" ? "-" : substr(L[j], 1, 60)) >> OUTD
            continue
        }
        fnname = substr(L[j], 4)
        sub(/[<( \t].*$/, "", fnname)
        e = total
        for (k = j + 1; k <= total; k++) {
            if (L[k] ~ /^#\[test\][ \t]*$/) { e = k - 1; break }
        }
        while (e > j && (L[e] ~ /^[ \t]*$/ || L[e] ~ /^(\/\/|\/\*|\*)/)) e--
        ign = (attrs ~ /(^|;)ignore/) ? 1 : 0
        feat = 0
        cfgs = ""
        nc = split(attrs, AA, ";")
        for (q = 1; q <= nc; q++) {
            if (AA[q] == "") continue
            if (AA[q] ~ /^cfg\(feature[ \t]*=/) feat = 1
            if (AA[q] ~ /^cfg\(/) cfgs = cfgs AA[q] "|"
        }
        helps = ""; fixtok = ""; signal = 0
        for (k = j; k <= e; k++) {
            line = L[k]
            rest = line
            while (match(rest, /[A-Za-z_][A-Za-z0-9_]*[ \t]*\(/)) {
                tok = substr(rest, RSTART, RLENGTH)
                sub(/[ \t]*\($/, "", tok)
                rest = substr(rest, RSTART + RLENGTH)
                if (tok in isH) {
                    if (index(" " helps " ", " " tok " ") == 0) helps = helps " " tok
                    signal = 1
                }
            }
            if (line ~ /Command::new|assert!|assert_eq!|assert_ne!|panic!\(|\.expect\(|todo!\(/) signal = 1
            if (line ~ /include_str!|[Ff]ixture/) {
                r2 = line
                while (match(r2, /"[^"]*\.(c|s|cpp|h)"/)) {
                    t = substr(r2, RSTART + 1, RLENGTH - 2)
                    r2 = substr(r2, RSTART + RLENGTH)
                    if (index("|" fixtok "|", "|" t "|") == 0) fixtok = fixtok t "|"
                }
            }
            r3 = line
            while (match(r3, /"cpp_abi_[a-z0-9_]+"/)) {
                t = substr(r3, RSTART + 1, RLENGTH - 2)
                r3 = substr(r3, RSTART + RLENGTH)
                if (index("|" fixtok "|", "|" t "|") == 0) fixtok = fixtok t "|"
            }
        }
        # 空字段一律写占位符 -：tab 属于 IFS 空白，bash read 会塌陷连续 tab，
        # 空字段若留空就会让后面所有字段错位。
        # 逐文件调用抽取器，所以输出必须用 >> 追加（> 会在每个新进程里截断，
        # 只剩最后一个文件的行）。输入文件在循环前已 : > 清空。
        print "T\t" F "\t" fnname "\t" ign "\t" feat "\t" (cfgs == "" ? "-" : cfgs) "\t" \
            (helps == "" ? "-" : helps) "\t" (fixtok == "" ? "-" : fixtok) "\t" signal >> OUT
    }
}
AWK

# --- 助手真值之二：^fn 里没有被 #[test] 驱动的那些（顶层 target 的本地助手）--
# 必须先于抽取器算出：它和 harness 的 pub(crate) fn 一起构成「判据真值面」。
awk '
FNR == 1 { t = 0 }
/^[ \t]*$/ || /^(\/\/|\/\*|\*)/ { next }
/^#\[/ { if ($0 ~ /^#\[test\][ \t]*$/) t = 1; next }
/^fn / { if (!t) { n = substr($0, 4); sub(/[<( \t].*$/, "", n); print FILENAME "\t" n } t = 0; next }
{ t = 0 }' "${ALL_FILES[@]}" >"$WORK/localfns.tsv" || fatal "本地助手扫描失败"
LOCAL_FNS="$(awk -F'\t' '$1 !~ /^tests\/integration\// { print $2 }' "$WORK/localfns.tsv" | sort -u | tr '\n' ' ')"
HELPER_NAMES="$HARNESS_FNS$LOCAL_FNS"

: >"$WORK/tests.tsv"
: >"$WORK/dangle.tsv"
for f in "${ALL_FILES[@]}"; do
    awk -v F="$f" -v HN="$HELPER_NAMES" \
        -v OUT="$WORK/tests.tsv" -v OUTD="$WORK/dangle.tsv" \
        -f "$WORK/extract.awk" "$f" || fatal "抽取器在 $f 上失败"
done

# --- 结构性校验 -------------------------------------------------------------
while IFS=$'\t' read -r file name; do
    [[ -n "$file" ]] || continue
    if [[ "$file" == tests/integration/* ]]; then
        note_err "STRUCT-HELPER: $file 定义了非测试 fn \`$name\`（§14 要求 ^fn 数 == #[test] 数）"
    elif [[ ! -v CRIT["$name"] ]]; then
        note_err "JAC-UNMAPPED: $file 的本地助手 \`$name\` 未登记判据（见脚本头部推断表）"
    fi
done <"$WORK/localfns.tsv"

while IFS=$'\t' read -r kind file line snippet; do
    [[ "$kind" == "D" ]] || continue
    note_err "STRUCT-DANGLE: $file:$line 的 #[test] 之后不是 fn（→ ${snippet:-EOF}）"
done <"$WORK/dangle.tsv"

for f in "${THEME_FILES[@]}"; do
    bad="$(grep -nE '^(use|mod)[ (]' "$f" 2>/dev/null || true)"
    [[ -z "$bad" ]] || while IFS= read -r hit; do
        note_err "STRUCT-USE: $f:${hit%%:*} 出现顶层 use/mod（§5.2 规则 2 禁止）"
    done <<<"$bad"
done
bail

# ---------------------------------------------------------------------------
# 行渲染：夹具 token → 仓库相对路径 + 存在性；助手 → 判据；属性 → Tier + 标注
# ---------------------------------------------------------------------------
resolve_tok() { # $1=token → stdout: 仓库相对路径（解析不出则空）
    local t="$1" cnt first
    case "$t" in
        *fixtures/*) printf 'tests/fixtures/%s' "${t##*fixtures/}"; return ;;
        cpp_abi_*)   printf 'tests/fixtures/%s' "$t"; return ;;
    esac
    if [[ -f "tests/fixtures/$t" ]]; then printf 'tests/fixtures/%s' "$t"; return; fi
    cnt="${BASE_COUNT[${t##*/}]:-0}"
    if [[ "$cnt" -ge 1 ]]; then
        first="${BASE_PATHS[${t##*/}]%%$'\n'*}"
        printf '%s' "$first"
        return
    fi
    return 0
}

: >"$WORK/rows.tsv"    # file \t fn \t fixture_cell \t crit \t tier_cell
: >"$WORK/gaps.tsv"    # file \t fn \t 磁盘上不存在的夹具引用
while IFS=$'\t' read -r kind file fn ign feat cfgs helps fixtok signal; do
    [[ "$kind" == "T" ]] || continue
    # 占位符归零（见抽取器里的空字段说明）
    [[ "$cfgs" == "-" ]] && cfgs=""
    [[ "$helps" == "-" ]] && helps=""
    [[ "$fixtok" == "-" ]] && fixtok=""
    tier="T0"
    note=""
    never=0
    # 文件级内部属性 #![cfg(...)] 作用于本文件全部用例
    if grep -qE '^#!\[cfg' "$file" 2>/dev/null; then
        note="${note:+$note }file-cfg($(sed -nE 's/^#!\[cfg\((.*)\)\]$/\1/p' "$file" | head -1))"
    fi
    # cfgs 用 | 分隔（条目内含空格，不能靠单词分割）
    CFG_ARR=()
    IFS='|' read -ra CFG_ARR <<<"$cfgs"
    for a in "${CFG_ARR[@]}"; do
        [[ -n "$a" ]] || continue
        # 正向特征门控 / 永不构建 → Tier 3；#[cfg(not(feature = ...))]、
        # #[cfg(unix)]、#[cfg(target_arch = ...)] 在 Linux CI 默认 feature 下恒成立
        # → 仍是 T0，但一律作为标注写出，不留看不见的门控。
        case "$a" in
            'cfg(feature'*) tier="T3" ;;
            'cfg(any())') tier="T3"; never=1 ;;
        esac
        note="${note:+$note }$a"
    done
    # 判据：按优先级取最强
    best=0
    crit=""
    for h in $helps; do
        lab="${CRIT[$h]}"
        [[ "$lab" == "aux" ]] && continue
        rank=1
        case "$lab" in
            run) rank=2 ;;
            sanitize) rank=3 ;;
            cross-check) rank=4 ;;
        esac
        if (( rank > best )); then best=$rank; crit="$lab"; fi
    done
    case " $helps " in *" gcc_src_tree_or_skip "*) tier="T3" ;; esac
    if [[ "$ign" == "1" ]]; then
        crit="report-only"
        tier="T3"
        note="${note:+$note }ignored"
    elif [[ -z "$crit" ]]; then
        if [[ "$signal" == "1" ]]; then
            crit="compile"
        elif [[ "$never" == "1" ]]; then
            # 函数体为空且被 #[cfg(any())] 永久排除在构建之外：它今天不可能跑。
            # 这不是“推不出”而是“确定没断言”，写进 disabled，不谎报 compile。
            crit="disabled"
            tier="T3"
            warn_err "JAC-DISABLED: $file::$fn 被 #[cfg(any())] 永久门控且函数体无断言（判据记为 disabled）"
        else
            note_err "JAC-UNINFERABLE: $file::$fn 的函数体推不出判据（无已登记助手，也没有 Command::new / assert / expect）"
            crit="-"
        fi
    fi
    # fixture 单元格
    nfx=0
    fxlist=""
    absent=""
    IFS='|' read -ra FX <<<"$fixtok"
    for t in "${FX[@]}"; do
        [[ -n "$t" ]] || continue
        p="$(resolve_tok "$t")"
        if [[ -z "$p" ]]; then
            case "$t" in
                *fixtures/* | cpp_abi_*)
                    warn_err "FIXTURE-ABSENT: $file::$fn 引用 \`$t\` 在 tests/fixtures 下不存在"
                    printf '%s\t%s\t%s\n' "$file" "$fn" "$t" >>"$WORK/gaps.tsv" ;;
            esac
            continue
        fi
        if [[ ! -e "$p" ]]; then
            warn_err "FIXTURE-ABSENT: $file::$fn → \`$p\` 不存在（文上无夹具，用例很可能静默跳过）"
            printf '%s\t%s\t%s\n' "$file" "$fn" "$p" >>"$WORK/gaps.tsv"
            case "|$absent|" in *"|$p|"*) continue ;; esac
            absent="|$p"
            case "|$fxlist|" in *"|$p|"*) continue ;; esac
            fxlist="$fxlist|$p"
            nfx=$((nfx + 1))
            continue
        fi
        case "|$fxlist|" in *"|$p|"*) continue ;; esac
        fxlist="$fxlist|$p"
        nfx=$((nfx + 1))
    done
    cell='(inline)'
    if (( nfx > 0 )); then
        IFS='|' read -ra LIST <<<"${fxlist#|}"
        cell=""
        shown=0
        for p in "${LIST[@]}"; do
            [[ -n "$p" ]] || continue
            if (( shown >= 3 )); then
                cell="$cell \`+$(( nfx - shown )) more\`"
                break
            fi
            mark=''
            case "|$absent|" in *"|$p|"*) mark='⚠' ;; esac
            cell="${cell:+$cell }${mark}\\\`${p#tests/}\\\`"
            shown=$((shown + 1))
        done
    fi
    tier_cell="$tier"
    [[ -n "$note" ]] && tier_cell="$tier · $note"
    printf '%s\t%s\t%s\t%s\t%s\n' "$file" "$fn" "$cell" "$crit" "$tier_cell" >>"$WORK/rows.tsv"
done <"$WORK/tests.tsv"
bail

# ---------------------------------------------------------------------------
# 交叉校验（写文档之前）—— §10.1 的「索引与代码不会悄悄漂移」
# ---------------------------------------------------------------------------
theme_expect=$(grep -h '#\[test\]' tests/integration/*.rs 2>/dev/null | wc -l | tr -d ' ')
theme_rows=$(awk -F'\t' '$1 ~ /^tests\/integration\// { c++ } END { print c + 0 }' "$WORK/rows.tsv")
[[ "$theme_rows" == "$theme_expect" ]] ||
    note_err "COUNT-TOTAL: 生成 ${theme_rows} 行 != grep -c '#[test]' tests/integration/*.rs 之和 ${theme_expect}"

for f in "${THEME_FILES[@]}"; do
    got=$(awk -F'\t' -v F="$f" '$1 == F { c++ } END { print c + 0 }' "$WORK/rows.tsv")
    want=$(grep -c '#\[test\]' "$f")
    [[ "$got" == "$want" ]] || note_err "COUNT-PER-FILE: $f 生成 $got 行 != #[test] $want 行"
done

inc_cnt=$(grep -c 'include!("' "$SHELL_RS")
file_cnt=$(ls tests/integration/*.rs 2>/dev/null | wc -l | tr -d ' ')
[[ "$inc_cnt" == "$file_cnt" ]] ||
    note_err "STRUCT-INCLUDE: $SHELL_RS 的 'include!(\"' 条数 $inc_cnt != tests/integration/*.rs 个数 $file_cnt"

shell_tests=$(grep -c '#\[test\]' "$SHELL_RS")
[[ "$shell_tests" == "0" ]] || note_err "STRUCT-USE: $SHELL_RS 内仍有 $shell_tests 个 #[test]（壳必须为 0）"

theme_fn=$(grep -h '^fn ' tests/integration/*.rs 2>/dev/null | wc -l | tr -d ' ')
[[ "$theme_fn" == "$theme_expect" ]] ||
    note_err "STRUCT-HELPER: tests/integration/*.rs 的 ^fn 数 $theme_fn != #[test] 数 $theme_expect"

# ---------------------------------------------------------------------------
# §10.2 C++ ABI 夹具族对账：人工小节「C++ ABI 五主题」引用的行号与组集合，
# 必须与 tests/cpp_abi_gcc_cross_check.rs 的代码真值逐条一致。行号是抄来的，
# 代码不会自己打招呼就移动，所以这里把它变成会红的断言（ABI-CITE-STALE）。
# ---------------------------------------------------------------------------
ABI_RS="tests/cpp_abi_gcc_cross_check.rs"
ABI_META_FN="cpp_abi_groups_are_all_covered"
if [[ ! -f "$ABI_RS" ]]; then
    note_err "ABI-GROUP-DRIFT: $ABI_RS 不存在，无法对账 C++ ABI 主题小节"
else
    : >"$WORK/abi.tsv"   # group \t fn_line \t call_line \t fixture_count \t covered
    covered_set="$(awk '/const COVERED/ { f = 1; next } f && /\];/ { exit } f' "$ABI_RS" |
        grep -oE '"cpp_abi_[a-z0-9_]+"' | tr -d '"' | sort -u)"
    for d in tests/fixtures/cpp_abi_*; do
        [[ -d "$d" ]] || continue
        g="${d##*/}"
        call_line="$(grep -nE "check_group\(\"$g\"\)" "$ABI_RS" | head -1 | cut -d: -f1)"
        fn_line="$(grep -nE "^fn ${g}_group" "$ABI_RS" | head -1 | cut -d: -f1)"
        n_fx=$(find "$d" -maxdepth 1 -type f -name '*.cpp' 2>/dev/null | wc -l | tr -d ' ')
        cov=no
        printf '%s\n' "$covered_set" | grep -qx -- "$g" && cov=yes
        if [[ -z "$call_line" || -z "$fn_line" ]]; then
            note_err "ABI-GROUP-DRIFT: 夹具组 $g 在 $ABI_RS 里找不到 \`fn ${g}_group\` 或 \`check_group(\"$g\")\` 调用点"
            continue
        fi
        [[ "$cov" == "yes" ]] ||
            note_err "ABI-GROUP-DRIFT: 夹具组 $g 未登记进 $ABI_RS 的 COVERED 常量（\`$ABI_META_FN\` 会红）"
        (( n_fx > 0 )) || note_err "ABI-GROUP-DRIFT: 夹具组 $g 里没有 .cpp 夹具（\`check_group\` 对空组断言失败）"
        printf '%s\t%s\t%s\t%s\t%s\n' "$g" "$fn_line" "$call_line" "$n_fx" "$cov" >>"$WORK/abi.tsv"
    done
    [[ -s "$WORK/abi.tsv" ]] || note_err "ABI-GROUP-DRIFT: tests/fixtures/ 下没有 cpp_abi_* 夹具目录"

    # 文档侧引用的小节行必须同时给出 fn 定义行号与 check_group 调用行号。
    # 比对前剥掉反引号，否则文档里的 (`:203`) 形态会被误判为未引用。
    while IFS=$'\t' read -r g fn_line call_line n_fx cov; do
        [[ -n "$g" ]] || continue
        row="$(awk -v B="$BEGIN_MARK" -v needle="check_group(\"$g\")" '
            index($0, B) > 0 { exit } index($0, needle) > 0 { print; exit }' "$DOC")"
        if [[ -z "$row" ]]; then
            note_err "ABI-CITE-STALE: 人工校订区没有 \`check_group(\"$g\")\` 的引用行"
            continue
        fi
        row="${row//\`/}"
        [[ "$row" == *"${g}_group"* ]] ||
            note_err "ABI-CITE-STALE: $g 的引用行没有点名驱动函数 \`${g}_group\`"
        [[ "$row" == *":$fn_line)"* ]] ||
            note_err "ABI-CITE-STALE: $g 的 \`${g}_group\` 定义行实际在 $ABI_RS:$fn_line，文档未引用 (:$fn_line)"
        [[ "$row" == *":$call_line)"* ]] ||
            note_err "ABI-CITE-STALE: $g 的 check_group 调用点实际在 $ABI_RS:$call_line，文档未引用 (:$call_line)"
        [[ "$row" == *"| $n_fx |"* ]] ||
            note_err "ABI-CITE-STALE: $g 目录下实际有 $n_fx 条 .cpp 夹具，文档未给出该数量"
    done <"$WORK/abi.tsv"

    # 反向：文档提到的组必须在磁盘上（防止小节里留着已删除的组名）
    # 匹配串 = check_group("<名>")：13 字前缀 + 名 + 2 字后缀，故名从 RSTART+13 起、长 RLENGTH-15
    doc_groups="$(awk -v B="$BEGIN_MARK" 'index($0, B) > 0 { exit }
        { while (match($0, /check_group\("cpp_abi_[a-z0-9_]+"\)/)) {
              s = substr($0, RSTART + 13, RLENGTH - 15); print s
              $0 = substr($0, RSTART + RLENGTH) } }' "$DOC" | sort -u)"
    disk_groups="$(cut -f1 "$WORK/abi.tsv" 2>/dev/null | sort -u)"
    drift="$(comm -13 <(printf '%s\n' "$disk_groups") <(printf '%s\n' "$doc_groups") | tr -d ' ')"
    [[ -z "$drift" ]] || note_err "ABI-GROUP-DRIFT: 文档引用了磁盘上不存在的夹具组: $drift"
fi
bail

# ---------------------------------------------------------------------------
# 渲染生成区（统计 + 全量索引；数据行的唯一锚是行首 "| \`tests/"）
# ---------------------------------------------------------------------------
top_rows=$(awk -F'\t' '$1 !~ /^tests\/integration\// { c++ } END { print c + 0 }' "$WORK/rows.tsv")
{
    echo '> 本区域由 `scripts/gen_coverage_index.sh` 从 tests/ 代码与 tests/fixtures/ 磁盘真值'
    echo '> 全量生成；手工编辑会被下一次生成覆盖。人工校订小节都在本标记之外。'
    echo
    echo "## 生成区统计"
    echo
    echo "| 指标 | 实测值 |"
    echo "|------|--------|"
    echo "| 主题子文件（\`tests/integration/*.rs\`） | ${#THEME_FILES[@]} |"
    echo "| 主题子文件里的 \`#[test]\` | $theme_expect |"
    echo "| 顶层 target（\`tests/*.rs\`，含壳） | $(( ${#TOP_FILES[@]} + 1 )) |"
    echo "| 顶层 target 里的 \`#[test]\`（不含主题） | $top_rows |"
    echo "| 索引数据行合计 | $((theme_rows + top_rows)) |"
    echo
    echo "### 判据分布"
    echo
    echo "| 判据 | 用例数 |"
    echo "|------|--------|"
    awk -F'\t' '{ c[$4]++ } END { for (k in c) printf "%s\t%d\n", k, c[k] }' "$WORK/rows.tsv" |
        sort | while IFS=$'\t' read -r k v; do echo "| $k | $v |"; done
    echo
    echo "### Tier 分布（§9 权威表；T1/T2 是 CI job 划分，不是用例属性）"
    echo
    echo "| Tier | 用例数 |"
    echo "|------|--------|"
    awk -F'\t' '{ split($5, a, " "); c[a[1]]++ } END { for (k in c) printf "%s\t%d\n", k, c[k] }' \
        "$WORK/rows.tsv" | sort | while IFS=$'\t' read -r k v; do echo "| $k | $v |"; done
    echo
    echo "### C++ ABI 夹具族（磁盘真值 + 代码真值，与人工小节逐条对账）"
    echo
    echo "| 组 | 驱动 fn 行 | \`check_group()\` 行 | .cpp 夹具数 | 已登记 COVERED |"
    echo "|----|------------|----------------------|-------------|----------------|"
    while IFS=$'\t' read -r g fn_line call_line n_fx cov; do
        [[ -n "$g" ]] || continue
        echo "| \`$g\` | \`:$fn_line\` | \`:$call_line\` | $n_fx | $cov |"
    done <"$WORK/abi.tsv"
    echo
    echo "### 报告项：引用了磁盘上不存在夹具的用例"
    echo
    echo '这些行不阻断生成（修复动作在 `tests/**` 与夹具侧），但索引不掩盖它们：'
    echo '对应 fixture 单元格在生成区里带 ⚠。'
    echo
    if [[ -s "$WORK/gaps.tsv" ]]; then
        # 首列不加反引号：生成区数据行的唯一锚是行首 "| \`tests/"，不能误伤
        echo "| 主题文件/target | 测试函数 | 磁盘上不存在的引用 |"
        echo "|-----------------|----------|----------------------|"
        while IFS=$'\t' read -r gf gfn gp; do
            [[ -n "$gf" ]] || continue
            printf '| %s | `%s` | `%s` |\n' "$gf" "$gfn" "${gp#tests/}"
        done <"$WORK/gaps.tsv"
    else
        echo "无。全部静态可解析的夹具引用都能在磁盘上找到。"
    fi
    echo
    echo '### 逐文件用例数（与磁盘 `grep -c '"'"'#[test]'"'"'` 逐文件核对）'
    echo
    echo "| 文件（相对 tests/） | 索引行数 | 一致 |"
    echo "|---------------------|----------|------|"
    for f in "${THEME_FILES[@]}" "${TOP_FILES[@]}"; do
        got=$(awk -F'\t' -v F="$f" '$1 == F { c++ } END { print c + 0 }' "$WORK/rows.tsv")
        echo "| ${f#tests/} | $got | yes |"
    done
    echo
    echo "## 全量索引"
    echo
    echo '列义：**判据** = 该用例实际验收的东西（`compile` 只看编译产物 / `run` 跑起来看退出码'
    echo '或输出 / `cross-check` 与 g++、宿主链接器或另一条 lowering 路径对照 / `sanitize` 带'
    echo 'sanitizer 运行或断言运行时失败 / `report-only` = `#[ignore]`，默认不执行；`disabled` ='
    echo '被 `#[cfg(any())]` 永久排除在构建之外）；**Tier** = §9 档位（`T0` = cargo test --release'
    echo '默认执行，`T3` = 特征门控 / 永不构建 / 需 GCC 源树）。Tier 列 `·` 之后是该用例携带的'
    echo '门控标注（含文件级 `#![cfg(unix)]`）。fixture 列的 `(inline)` 表示源码在函数体内联'
    echo '构造、没有外部夹具；`+N more` 表示同一用例还引用了更多夹具（完整清单由'
    echo '`scripts/check_fixtures.sh` 逐个把关）；⚠ 前缀表示该引用在磁盘上找不到夹具。'
    echo
    echo "| 主题文件 | 测试函数 | fixture | 判据(compile/run/cross-check/sanitize) | Tier |"
    echo "|----------|----------|---------|--------------------------------------|------|"
    for f in "${THEME_FILES[@]}" "${TOP_FILES[@]}"; do
        awk -F'\t' -v F="$f" '$1 == F { printf "%s\t%s\t%s\t%s\t%s\n", $1, $2, $3, $4, $5 }' \
            "$WORK/rows.tsv" |
            while IFS=$'\t' read -r file fn cell crit tier; do
                printf '| `%s` | `%s` | %s | %s | %s |\n' "$file" "$fn" "$cell" "$crit" "$tier"
            done
    done
} >"$WORK/gen.md"

# ---------------------------------------------------------------------------
# 只重写 BEGIN/END 区间；区外内容用哈希双向核对（人工校订区不得被覆盖）
# ---------------------------------------------------------------------------
awk -v B="$BEGIN_MARK" -v E="$END_MARK" -v GEN="$WORK/gen.md" '
index($0, B) > 0 && !done {
    print
    while ((getline line < GEN) > 0) print line
    close(GEN)
    done = 1; skipping = 1
    next
}
skipping { if (index($0, E) > 0) { skipping = 0; print } ; next }
{ print }
' "$DOC" >"$WORK/doc.new" || fatal "拼接生成区失败"

[[ $(grep -cF -- "$BEGIN_MARK" "$WORK/doc.new") == "1" ]] || fatal "内部错误：拼接后 BEGIN 标记不唯一"
[[ $(grep -cF -- "$END_MARK" "$WORK/doc.new") == "1" ]] || fatal "内部错误：拼接后 END 标记不唯一"

region_rows=$(awk -v B="$BEGIN_MARK" -v E="$END_MARK" '
index($0, B) > 0 { inb = 1; next } index($0, E) > 0 { inb = 0 }
inb && /^\| `tests\// { c++ } END { print c + 0 }' "$WORK/doc.new")
expect_rows=$(wc -l <"$WORK/rows.tsv" | tr -d ' ')
[[ "$region_rows" == "$expect_rows" ]] ||
    note_err "WRITE-MISMATCH: 待落盘文档生成区 $region_rows 行 != 抽取 $expect_rows 行"

region_theme=$(awk -v B="$BEGIN_MARK" -v E="$END_MARK" '
index($0, B) > 0 { inb = 1; next } index($0, E) > 0 { inb = 0 }
inb && /^\| `tests\/integration\// { c++ } END { print c + 0 }' "$WORK/doc.new")
[[ "$region_theme" == "$theme_expect" ]] ||
    note_err "WRITE-MISMATCH: 待落盘文档主题区 $region_theme 行 != 磁盘真值 $theme_expect"

# 区外内容（BEGIN 之前 + END 之后）必须逐字节不变
outside_cmp() {
    diff -q <(awk -v B="$BEGIN_MARK" 'index($0, B) > 0 { exit } { print }' "$DOC") \
            <(awk -v B="$BEGIN_MARK" 'index($0, B) > 0 { exit } { print }' "$WORK/doc.new") >/dev/null &&
    diff -q <(awk -v E="$END_MARK" 'f { print } index($0, E) > 0 { f = 1 }' "$DOC") \
            <(awk -v E="$END_MARK" 'f { print } index($0, E) > 0 { f = 1 }' "$WORK/doc.new") >/dev/null
}
outside_cmp || note_err "WRITE-MISMATCH: 生成区之外的人工校订内容发生变化（拒绝落盘）"
bail

cat "$WORK/doc.new" >"$DOC" || fatal "写入 $DOC 失败"

# --- 落盘后复核（幂等性的直接证据）-----------------------------------------
final_theme=$(awk -v B="$BEGIN_MARK" -v E="$END_MARK" '
index($0, B) > 0 { inb = 1; next } index($0, E) > 0 { inb = 0 }
inb && /^\| `tests\/integration\// { c++ } END { print c + 0 }' "$DOC")
final_all=$(awk -v B="$BEGIN_MARK" -v E="$END_MARK" '
index($0, B) > 0 { inb = 1; next } index($0, E) > 0 { inb = 0 }
inb && /^\| `tests\// { c++ } END { print c + 0 }' "$DOC")
if [[ "$final_theme" != "$theme_expect" ]]; then
    echo "FATAL: 落盘后主题行数 $final_theme != 磁盘真值 $theme_expect"
    exit 1
fi
b_end=$(grep -nF -- "$END_MARK" "$DOC" | cut -d: -f1)
b_begin=$(grep -nF -- "$BEGIN_MARK" "$DOC" | cut -d: -f1)
echo "scanned: ${#THEME_FILES[@]} theme files + ${#TOP_FILES[@]} top-level targets + shell"
echo "rows: total=$final_all (integration=$final_theme, expect=grep -h '#[test]' tests/integration/*.rs =$theme_expect)"
echo "doc: $DOC 生成区 $((b_end - b_begin - 1)) 行 / 全文 $(( $(wc -l <"$DOC") )) 行"
echo "reported (不阻断、已写进生成区): $WARNS"
echo "coverage index generated (cross-checks passed)"
exit 0
