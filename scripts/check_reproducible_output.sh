#!/bin/bash
# scripts/check_reproducible_output.sh - 编译产物稳定性门禁
#
# 第一性原理：编译器的两条独立性质必须分别可验证，且**判据不能混用**——
#   determinism : 同一二进制、同一输入连跑 N 次，产物字节必须全等（可重现构建）
#   drift       : 两个二进制（重构前/后）对同一输入，产物必须等价（零回归）
#
# 为什么单独存在: `tests/integration.rs` 里的 cross 测试只校验 ELF magic，
# 抓不到"每次跑都不一样"；而人工 golden diff 又抓不到"顺序置换"。
#
# ⚠️ 判据教训（本脚本的核心价值）：
#   1) drift 模式**不能**直接用单次 sha256 比对。历史上这样会假报 137/5958 条"漂移"，
#      全部源于 `-S` 文本的行序抖动（内容其实 100% 相同）。因此 drift 对文本产物
#      按 `LC_ALL=C sort` 后的内容比对（行序无关），对二进制产物按原样 sha256 比对。
#   2) 但 determinism 模式**绝不能**沿用排序判据——排序会把"行序每次都不一样"
#      抹平成"内容一致"，从而把真正的不可复现误判为通过。可复现构建要求的是
#      字节级全等，所以 determinism 一律用**原样 sha256（不排序）**。
#      这两个判据的强度差异是刻意的：drift 问"语义变了吗"，determinism 问"字节一样吗"。
#
# 为什么 -O0 与 -O2 要分开看（默认 MODES 里两类都列）：
#   -O0 基本是"AST 直译"，不跑 SSA 管线，历史上一直是确定的；-O2 会走
#   `run_ssa_passes` (AST→SSA→AST 往返)，任何建立在 HashMap/HashSet 迭代序上的
#   决定（局部变量声明序、alias set 编号、临时变量类型择优）都会直接改写栈槽偏移，
#   表现为 `.text` 里单字节抖动。两者失效模式完全不同，混在一起跑会互相掩盖：
#   只测 -O0 会漏掉优化器泄漏，只测 -O2 会在 -O0 回归时无法定位。
#   因此默认覆盖 `-c|-S`(= -O0) 与 `-O2 -c`；需要 -O3 时用 --with-o3 显式加。
#
# ✅ 链接模式（不带 -c）由 --with-link 显式覆盖，默认不跑：
#   这条性质曾不可复现的根因是 `src/driver.rs::run_with_host_link` 把临时 .o 命名为
#   `dcc-hostlink-<pid>-<src>.o`，宿主 ld 会按 basename 登记一个 STT_FILE 符号送进
#   .symtab/.strtab ⇒ 每次编译 PID 不同 ⇒ 字节不同（-O0 也一样抖，与优化档无关）。
#   现在 PID 只留在**目录**部分（实测 `readelf -sW` 里 ld 只登 basename），产物确定。
#   为什么默认不跑：链接要真调宿主 cc/ld（比 -c 慢一个量级），而且交叉后端的
#   host-link 今天会因 "file in wrong format" 直接失败——所以 --with-link 用
#   **独立小 fixture 集**（LINK_SOURCES，6 个）且默认只跑 x86_64。
#   为什么必须用「含文件作用域 static」的 fixture：宿主 ld 只为**含 LOCAL 符号**的
#   输入 .o 登记 STT_FILE，全 GLOBAL 的 TU 根本不触发这条通道（锁会变假绿）。
#   最小复现（修复前 distinct=5，修复后恒为 1）：
#     for i in $(seq 5); do dcc --target x86_64-linux-gnu -O0 \
#       tests/fixtures/c/stmt_expr.c -o /tmp/e_$i; done; sha256sum /tmp/e_* | sort -u | wc -l
#
# 用法:
#   scripts/check_reproducible_output.sh                          # determinism, 默认 corpus
#   scripts/check_reproducible_output.sh determinism --runs 5
#   scripts/check_reproducible_output.sh determinism --with-o3    # 额外覆盖 -O3
#   scripts/check_reproducible_output.sh determinism --with-link  # 额外覆盖链接模式
#   scripts/check_reproducible_output.sh drift --old /path/dcc_before --new /path/dcc_after
#
# 环境变量:
#   DCC           被检二进制 (determinism 模式), 默认 target/release/dcc
#   CORPUS        fixture 目录, 默认 tests/fixtures/c (收集 *.c / *.cpp / *.cc)
#   ARCHES        空格分隔 triple, 默认全部 6 个已注册后端
#   MODES         以 | 分隔的编译模式列表, 默认 "-c|-S|-O2 -c"
#                 (单个模式串可以含空格, 所以用 | 而不是空格分隔)
#   LINK_ARCHES   --with-link 用的 triple, 默认 x86_64-linux-gnu (host-link 只此可用)
#   LINK_MODES    --with-link 用的模式, 默认 "-O0|-O2" (不带 -c 即链接模式)
#   LINK_SOURCES  --with-link 用的 fixture 基名(无扩展名), 默认 6 个能触发 STT_FILE 的
#
# 退出码: 0 全部稳定 / 等价；1 检出问题；2 用法或环境错误

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

DCC="${DCC:-target/release/dcc}"
CORPUS="${CORPUS:-tests/fixtures/c}"
ARCHES="${ARCHES:-x86_64-linux-gnu aarch64-linux-gnu arm-linux-gnueabi mips-linux-gnu mips64-linux-gnu riscv64-linux-gnu}"
# 默认覆盖 -O0 (-c / -S) 与 -O2 (-c)。-O2 是历史上唯一抖动的档位，必须常驻门禁。
MODES="${MODES:--c|-S|-O2 -c}"
RUNS=3
MODE_ARG=""
OLD_BIN=""
NEW_BIN=""
WITH_O3=0
WITH_LINK=0
# --with-link 的专用集。三者都可被环境变量覆盖，默认值是实测出来的最小集：
# 只取能链接成功、且含文件作用域 static（才会触发 ld 的 STT_FILE 通道）的 fixture。
LINK_ARCHES="${LINK_ARCHES:-x86_64-linux-gnu}"
LINK_MODES="${LINK_MODES:--O0|-O2}"
LINK_SOURCES="${LINK_SOURCES:-stmt_expr attr_copy bitfield_runtime pointer struct typeof}"

die() { echo "错误: $*" >&2; exit 2; }

while [ $# -gt 0 ]; do
    case "$1" in
        determinism|drift) [ -z "$MODE_ARG" ] || die "只能指定一个子命令"; MODE_ARG="$1" ;;
        --runs)   RUNS="${2:-}";   shift ;;
        --old)    OLD_BIN="${2:-}";   shift ;;
        --new)    NEW_BIN="${2:-}";   shift ;;
        --dcc)    DCC="${2:-}";       shift ;;
        --with-o3) WITH_O3=1 ;;
        --with-link) WITH_LINK=1 ;;
        *) die "未知参数: $1" ;;
    esac
    shift
done
[ -n "$MODE_ARG" ] || MODE_ARG="determinism"
case "$RUNS" in ''|*[!0-9]*) die "--runs 必须是正整数, 收到: '$RUNS'" ;; esac
[ "$RUNS" -ge 2 ] || die "--runs 至少为 2 (单次无从比较)"

# --with-o3 在默认模式集后追加 -O3 -c (显式开关, 因为全量 -O3 会明显拉长 CI 时间)
if [ "$WITH_O3" -eq 1 ]; then
    MODES="$MODES|-O3 -c"
fi

case "$MODE_ARG" in
    determinism)
        [ -x "$DCC" ] || die "找不到可执行文件 $DCC (先 cargo build --release)"
        ;;
    drift)
        [ -n "$OLD_BIN" ] && [ -n "$NEW_BIN" ] || die "drift 模式需要 --old 与 --new"
        [ -x "$OLD_BIN" ] && [ -x "$NEW_BIN" ] || die "--old/--new 必须存在且可执行"
        ;;
    *) die "未知子命令: $MODE_ARG" ;;
esac
[ -d "$CORPUS" ] || die "corpus 目录不存在: $CORPUS"

# 语料收集: 不只 `*.c`——`tests/fixtures/cpp` 这类 corpus 只有 .cpp, 早期版本会
# 静默得到 0 个组合并报“✅ 全部可复现”, 那是假阳性通过。
shopt -s nullglob
SOURCES=("$CORPUS"/*.c "$CORPUS"/*.cpp "$CORPUS"/*.cc)
shopt -u nullglob
[ "${#SOURCES[@]}" -ge 1 ] || die "corpus 里没有 .c/.cpp/.cc fixture: $CORPUS"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# 产物指纹。
#   $1 bin  $2 arch  $3 src  $4 mode(可含空格)  $5 out  $6 policy
# policy=strict → 一律原样 sha256 (determinism 用)
# policy=sorted → 文本(-S)按排序内容, 二进制原样 sha256 (drift 用)
# 返回 "<exit_code>|<hash>"; 编译失败时 hash 为空, 于是"失败"与"成功但同内容"
# 不会被混为一谈。
fingerprint() {
    local bin="$1" arch="$2" src="$3" mode="$4" out="$5" policy="$6"
    local rc=0
    # mode 故意不加引号: "-g -c" 必须展开成两个 argv, 不能当成单个参数。
    "$bin" --target "$arch" $mode "$src" -o "$out" >/dev/null 2>&1 || rc=$?
    local content=""
    if [ -f "$out" ]; then
        case "$mode" in
            *-S*)
                if [ "$policy" = sorted ]; then
                    content=$(LC_ALL=C sort "$out" | sha256sum | cut -d' ' -f1)
                else
                    content=$(sha256sum "$out" | cut -d' ' -f1)
                fi
                ;;
            *) content=$(sha256sum "$out" | cut -d' ' -f1) ;;
        esac
    fi
    rm -f "$out"
    printf '%s|%s' "$rc" "$content"
}

# 单个 (src, arch, mode) 组合的比对体。determinism 与 drift 共用它，因此
# --with-link 那一小圈不会带着一套 subtly 不同的判据（以前两处逻辑写两遍，
# 改一处忘另一处就是门禁静默失效的经典入口）。
# 副作用：更新 total / bad，并把最后一次编译的退出码写进 LAST_RC。
check_combo() { # $1=src $2=name $3=arch $4=mode
    local src="$1" name="$2" arch="$3" mode="$4"
    local prev="" cur fo fn i
    LAST_RC=0
    total=$((total + 1))
    case "$MODE_ARG" in
    determinism)
        for i in $(seq 1 "$RUNS"); do
            cur=$(fingerprint "$DCC" "$arch" "$src" "$mode" "$TMP/d.out" "$POLICY")
            LAST_RC="${cur%%|*}"
            if [ -n "$prev" ] && [ "$cur" != "$prev" ]; then
                echo "NONDET  $name | $arch | [$mode] | run$i=$cur prev=$prev"
                bad=$((bad + 1))
                break
            fi
            prev="$cur"
        done
        ;;
    drift)
        fo=$(fingerprint "$OLD_BIN" "$arch" "$src" "$mode" "$TMP/a.out" "$POLICY")
        fn=$(fingerprint "$NEW_BIN" "$arch" "$src" "$mode" "$TMP/b.out" "$POLICY")
        LAST_RC="${fn%%|*}"
        if [ "$fo" != "$fn" ]; then
            echo "DRIFT   $name | $arch | [$mode] | old=$fo new=$fn"
            bad=$((bad + 1))
        fi
        ;;
    esac
}

# 产物里 `dcc-hostlink-` 的出现次数——即「临时 .o 名是否真的进了可执行文件」。
# 这个量决定了链接模式那几条锁是不是**活的**：宿主 ld 只为含 LOCAL 符号的输入
# .o 登记 STT_FILE，一旦这条通道挂掉（换了 fixture 集、ld 行为变了、或有人把
# 临时 .o 改到子目录里），连跑恒等会变成真的恒等但**不再守着任何东西**。
link_channel_hits() { # $1=bin $2=src $3=mode $4=arch
    local bin="$1" src="$2" mode="$3" arch="$4" n=0
    if "$bin" --target "$arch" $mode "$src" -o "$TMP/l.out" >/dev/null 2>&1; then
        n=$(grep -a -c 'dcc-hostlink-' "$TMP/l.out" 2>/dev/null || true)
    fi
    rm -f "$TMP/l.out"
    printf '%s' "${n:-0}"
}

total=0; bad=0
POLICY=strict
if [ "$MODE_ARG" = drift ]; then POLICY=sorted; fi
printf '%s 模式 (指纹=%s): corpus=%s arch=%s mode=%s\n' \
    "$MODE_ARG" "$POLICY" "$CORPUS" "$(wc -w <<<"$ARCHES")" "$(tr '|' ' ' <<<"$MODES")"

IFS='|' read -r -a MODE_ARR <<< "$MODES"
[ "${#MODE_ARR[@]}" -ge 1 ] || die "MODES 为空"

for src in "${SOURCES[@]}"; do
    fname=$(basename "$src")
    name="${fname%.*}"
    for arch in $ARCHES; do
        for mode in "${MODE_ARR[@]}"; do
            [ -n "$mode" ] || continue
            check_combo "$src" "$name" "$arch" "$mode"
        done
    done
done

# --with-link: 在上面的 -c/-S 矩阵之外，额外跑一小圈链接模式。
# 为什么单独开一个循环而不是往 MODES 里加一个空 mode：链接模式只适用于
# x86_64（交叉后端的 host-link 会被宿主 ld 拒收），混在主循环里会把
# 332×5 个无关组合全部乘上一遍失败路径，既慢又把“失败”当成了可比对的基准。
if [ "$WITH_LINK" -eq 1 ]; then
    IFS='|' read -r -a LINK_MODE_ARR <<< "$LINK_MODES"
    LINK_BIN="$DCC"
    [ "$MODE_ARG" = drift ] && LINK_BIN="$NEW_BIN"
    LINK_HITS=0
    for n in $LINK_SOURCES; do
        lsrc="$CORPUS/$n.c"
        [ -f "$lsrc" ] || die "--with-link 的 fixture 不存在: $lsrc"
        for arch in $LINK_ARCHES; do
            for mode in "${LINK_MODE_ARR[@]}"; do
                [ -n "$mode" ] || continue
                check_combo "$lsrc" "$n" "$arch" "$mode"
                # 精心选过的小集上链接失败 = 这轮改动弄坏了 host-link（或 fixture
                # 漂了），当失败报，不静默跳过——静默跳过正是假绿灯的来源。
                if [ "$LAST_RC" != 0 ]; then
                    echo "LINKFAIL $n | $arch | [$mode] | dcc 退出码 $LAST_RC, 未产出可比较的链接产物"
                    bad=$((bad + 1))
                fi
                LINK_HITS=$((LINK_HITS + $(link_channel_hits "$LINK_BIN" "$lsrc" "$mode" "$arch")))
            done
        done
    done
    echo "链接模式 STT_FILE 通道命中数: $LINK_HITS (共 $(( $(wc -w <<<"$LINK_SOURCES") * ${#LINK_MODE_ARR[@]} )) 格)"
    if [ "$LINK_HITS" -eq 0 ]; then
        echo "LINKLOCK-VACUOUS 产物里没有任何 dcc-hostlink- 名字, 链接模式锁当前抓不到 PID 类泄漏"
        bad=$((bad + 1))
    fi
fi

echo "───────────────────────────────────────"
echo "检查组合数: $total"
if [ "$bad" -eq 0 ]; then
    case "$MODE_ARG" in
        determinism) echo "✅ 全部可复现 (每组合 ${RUNS} 次连跑产物字节一致, 含行序)" ;;
        drift)       echo "✅ 零漂移 (old/new 等价, 二进制原样比对, 文本按排序内容比对)" ;;
    esac
    exit 0
fi
echo "❌ 检出 $bad 个问题组合"
exit 1
