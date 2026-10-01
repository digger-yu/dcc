#!/bin/bash
# scripts/check_pipe_structure.sh - P16 软件流水的产物级核对（两个独立读数）
#
# 为什么必须有它：流水有两类失败，而**正确性测试对其中一类完全无感**：
#   (a) 准入闸门被某次重构弄得永不触发 ⇒ pass 退化成死代码；
#   (b) 落地产生了错结构 ⇒ 值错。
# (b) 由 tests/fixtures/opt/ssa_pipe_{chain,reduction,nested}.c 的逐档 gcc 对照护住。
# 但 (a) 能让**所有现有测试与门禁照样全绿** —— 不流水只是「少优化」，产物仍然正确。
# 这正是本模块在 docs/ssa-pass-upgrade-plan.md §7.6 记下的老问题：「它没跑」与
# 「它为什么没跑」在产物上一模一样。
#
# 所以这里用两个互相独立的读数交叉确认：
#   1) `DCC_PIPE_DUMP=1` 的接受/拒因记录（模块自己的可观测面）；
#   2) objdump（权威反汇编，不用自家解码器）比较「默认」与 `DCC_SSA_OFF=pipe`
#      两个二进制里**同一个函数**的反汇编文本。
# 只查 1 会漏「计划排出来了但落地没改变产物」；只查 2 会漏「因管线耦合偶然产生的
# 差异被当成流水生效」。
#
# 已知不做（宁可少判，不做假精确）：**不**要求未流水的函数字节相同。实测关掉本
# pass 会让同模块其它函数的代码一起变（管线是耦合的），那条判据会把正常现象报成
# 缺陷。反向的「这个循环不该流水」由拒因清单核对，而不是由字节核对。
#
# 用法: DCC=... DCC_REPO=... bash scripts/check_pipe_structure.sh
set -o pipefail
DCC=${DCC:-/root/dcc/target/release/dcc}
REPO=${DCC_REPO:-/root/dcc}
FIXTURE="$REPO/tests/fixtures/opt/ssa_pipe_chain.c"
STD=gnu2x

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

fails=0
fail() { echo "PIPE-STRUCT FAIL: $*"; fails=$((fails + 1)); }

if [ ! -x "$DCC" ]; then
    echo "PIPE-STRUCT FAIL: 找不到可执行的 dcc: $DCC"
    exit 1
fi
if [ ! -f "$FIXTURE" ]; then
    echo "PIPE-STRUCT FAIL: 夹具不存在 $FIXTURE"
    exit 1
fi
if ! command -v objdump >/dev/null 2>&1; then
    echo "PIPE-STRUCT FAIL: 需要 objdump 作为权威反汇编器"
    exit 1
fi

# ---- 编译两份：默认（流水开）与 DCC_SSA_OFF=pipe（本 pass 的开关） ----
DCC_PIPE_DUMP=1 "$DCC" -O2 "-std=$STD" -c -o "$TMP/on.o" "$FIXTURE" > "$TMP/dump.txt" 2>&1
[ $? -eq 0 ] || fail "默认编译夹具失败"
DCC_SSA_OFF=pipe "$DCC" -O2 "-std=$STD" -c -o "$TMP/off.o" "$FIXTURE" > /dev/null 2>&1
[ $? -eq 0 ] || fail "关闭 pipe 后编译夹具失败"
[ -f "$TMP/dump.txt" ] || fail "DCC_PIPE_DUMP 没有产生任何输出：可观测面丢失"

# ---- 取单个函数的反汇编文本，抹掉绝对地址与符号偏移以便跨二进制比较 ----
fn_asm() {
    objdump -d --no-show-raw-insn "$1" \
      | awk -v f="$2" 'index($0, "<" f ">:") { infn = 1; next }
                        infn && /^[0-9a-f]+ </ { infn = 0 }
                        infn' \
      | sed -E 's/^[ \t]*[0-9a-f]+:[ \t]*//;
                s/<[a-zA-Z_.0-9]+(\+0x[0-9a-f]+)?>//g;
                s/[ \t]+/ /g'
}

# ---- 1. 被流水的循环：既要有接受记录，也要真的改变产物 ----
PIPES=(ch chain6_10)
for fn in "${PIPES[@]}"; do
    if ! grep -q "^fn $fn header=" "$TMP/dump.txt"; then
        fail "$fn：没有「计划」记录 —— 准入可能已不再触发（pass 变成死代码）"
        continue
    fi
    on=$(fn_asm "$TMP/on.o" "$fn")
    off=$(fn_asm "$TMP/off.o" "$fn")
    if [ -z "$on" ]; then
        fail "$fn：objdump 里取不到该函数的反汇编（函数被删或改名？）"
        continue
    fi
    if [ "$on" = "$off" ]; then
        fail "$fn：计划排出来了但产物与关闭本 pass 时逐行相同 —— 落地未生效"
    fi
done

# ---- 2. 不该流水的循环：必须给出**具体**拒因，而不是静默不跑 ----
# 每条都是 (函数:头部:拒因关键字)。拒因换了措辞也算回归：这段清单就是
# 「为什么这个形状没被流水」的唯一文档化答案。
REJECTS=(
    "ch_store:for.body:归纳变量初值不是区域外的唯一常量定义"
    "red:for.body:无跳迭代重叠"
    "chain6_6:for.body:trip 太短"
    "chain5_24:for.body:无跳迭代重叠"
)
for r in "${REJECTS[@]}"; do
    IFS=: read -r fn hdr why <<< "$r"
    line=$(grep -F "[P16] $fn#$hdr 未流水" "$TMP/dump.txt" | head -1)
    [ -n "$line" ] || { fail "$fn#$hdr：没有拒因记录（准入判定变得不可解释）"; continue; }
    case "$line" in
        *"$why"*) ;;
        *) fail "$fn#$hdr：拒因变了，期望含「$why」，实际「$line」" ;;
    esac
done

# ---- 3. 产物必须可被权威反汇编器完整读出 ----
# 只认 objdump 自己的 `(bad)` 标记：宽匹配 `bad` 会撞上符号名里的子串（假红）。
for o in on off; do
    bad=$(objdump -d "$TMP/$o.o" 2>/dev/null | grep -c "(bad)")
    [ "$bad" -eq 0 ] || fail "$o.o 的 objdump 有 $bad 条 (bad) 指令"
done

# ---- 4. 自检：上面的比较 machinery 不是空转 ----
# 拿两个不同函数的反汇编比一下：如果它们被判为相同，说明 `fn_asm` 取到的是空串
# 或根本没能区分函数，那第 1 节的「不同」就没有意义。
if [ "$(fn_asm "$TMP/on.o" ch)" = "$(fn_asm "$TMP/on.o" chain6_10)" ]; then
    fail "自检：ch 与 chain6_10 的反汇编被判为相同 —— fn_asm 的空/常量输出会让门禁假绿"
fi

echo "--- dump 中的接受/拒绝计数：接受 $(grep -c '^fn ' "$TMP/dump.txt")，拒因 $(grep -c '^\[P16\].*未流水' "$TMP/dump.txt")"
if [ "$fails" -ne 0 ]; then
    echo "check_pipe_structure: FAIL（$fails 项）"
    exit 1
fi
echo "check_pipe_structure: OK（流水确已落地且拒因可解释）"
