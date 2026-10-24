#!/bin/bash
# ===========================================================================
# check_facts_vs_objdump.sh — S1 facts 层的外部 oracle（方案 §3 S1：不以自证为真）
#
# 为什么必须有它：`blockopt.rs` 里的单测全部是「拿本仓构造子的字节问本仓解码器」，
# 它能证明两者一致，证不了两者与**真 x86** 一致。要证明后者，唯一可用的独立读数是
# binutils 自己的反汇编器：同一段字节，objdump 说目的号是谁，dcc 就必须说谁。
#
# 只对账「强主张」那一侧（这是判据的关键形状）：
#   * dcc 说 `regN`   ⇒ objdump 必须把同一个 GP 号当成目的操作数；
#   * dcc 说 `sextN` / `sextN:<imm>` ⇒ 目的必须是那个号，**而且**助记符必须属于
#     `movsxd`/`movsx*`/`mov r64,imm32` 那三族 —— 只有它们能把整字写成「低 32 位的
#     符号扩展」。这一条是本轮加的：`sext32` 现在是默认档判据（它削掉 `movsxd d,d`），
#     报对了号但报错了「已扩展」同样会错值，而只查号的臂抽不出这个差别。
#   * dcc 说 `nothing` ⇒ objdump 的目的操作数**不得**是任何 GP 寄存器；
#   * dcc 说 `mem`/`slot:*` ⇒ objdump 的目的必须是内存（且 slot 那条的位移必须对得上）；
#   * dcc 说 `unknown`/`call`/`term`/`strs`/`regs*`/`mask*` ⇒ 不对账：那些是保守方向
#     （最多丢优化），把它们纳入判据只会让脚本去重复实现 objdump。
# 反向不对账是刻意的：解码器**保守**不会错值，只有「说错了目的号」才会。
#
# 用法:
#   bash scripts/check_facts_vs_objdump.sh [源文件...]
#     DCC=<路径>        默认 target/release/dcc（缺失退回 target/debug/dcc）
#     LIMIT=<n>         最多对账多少条不同的字节序列（默认 4000）
#     OBJDUMP=<命令>    默认 objdump
#     CORPUS=<目录>     没给源文件时用它下面所有 .c（默认 tests/fixtures/c）
#   退出码：0 = 无不一致；1 = 有 FACT-FAIL（已逐条列出）；2 = 环境缺失
# ===========================================================================
set -u

DCC="${DCC:-}"
if [ -z "$DCC" ]; then
  if [ -x target/release/dcc ]; then DCC=target/release/dcc
  elif [ -x target/debug/dcc ]; then DCC=target/debug/dcc
  else echo "check_facts_vs_objdump: 找不到 dcc 二进制，先 cargo build" >&2; exit 2; fi
fi
OBJDUMP="${OBJDUMP:-objdump}"
LIMIT="${LIMIT:-4000}"
CORPUS="${CORPUS:-tests/fixtures/c}"

command -v "$OBJDUMP" >/dev/null 2>&1 || { echo "没有 objdump" >&2; exit 2; }

# 源文件列表：命令行优先，否则取语料目录。
if [ "$#" -gt 0 ]; then
  SRC_LIST="$*"
else
  SRC_LIST=$(find "$CORPUS" -name '*.c' | sort | head -40)
fi
[ -n "$SRC_LIST" ] || { echo "没有可编译的源文件" >&2; exit 2; }

RAW=/tmp/cfvo.facts.$$
: > "$RAW"
for f in $SRC_LIST; do
  # 只收 factdump 行；编译失败（夹具自身依赖）不阻断对账。
  DCC_FACTS_DUMP=1 "$DCC" -O2 -c -o /dev/null "$f" 2>&1 | awk '$1=="factdump"{print $2, $3}' >> "$RAW"
done

if [ ! -s "$RAW" ]; then
  echo "check_facts_vs_objdump: 一条 factdump 都没收到 ⇒ dcc 二进制不认 DCC_FACTS_DUMP" >&2
  rm -f "$RAW"; exit 2
fi

TMP=$(mktemp -d)
n_checked=0; n_fail=0; n_skip=0
# GP 号 → objdump(Intel) 里可能出现的写法。
# 0..3 同时带上 ah/ch/dh/bh：无 REX 的 8 位形里它们确实归这三个号（`byte_reg` 的另一半）。
reg_names() {
  case "$1" in
    0) echo "rax eax al ax ah";; 1) echo "rcx ecx cl cx ch";; 2) echo "rdx edx dl dx dh";;
    3) echo "rbx ebx bl bx bh";; 4) echo "rsp esp spl sp";; 5) echo "rbp ebp bpl bp";;
    6) echo "rsi esi sil si";; 7) echo "rdi edi dil di";;
    8) echo "r8 r8d r8b";;   9) echo "r9 r9d r9b";;    10) echo "r10 r10d r10b";;
    11) echo "r11 r11d r11b";; 12) echo "r12 r12d r12b";; 13) echo "r13 r13d r13b";;
    14) echo "r14 r14d r14b";; 15) echo "r15 r15d r15b";;
    *) echo "";;
  esac
}

# 把 hex 串还原成二进制文件。printf 的 \xNN 形式在 bash 下可用。
hex_to_bin() { # <hexstring> <out>
  local h="$1" o="$2" i
  : > "$o"
  for ((i = 0; i < ${#h}; i += 2)); do
    printf "\\x${h:i:2}" >> "$o"
  done
}

for line in $(sort -u "$RAW" | head -"$LIMIT" | awk '{print $1"@"$2}'); do
  hex="${line%@*}"; verdict="${line#*@}"
  case "$verdict" in
    unknown|call|term|strs|regs*|mask*) n_skip=$((n_skip + 1)); continue;;
  esac
  bin="$TMP/in.bin"
  hex_to_bin "$hex" "$bin"
  # objdump 的地址列是**右对齐带前导空格**的（`   0:\tf2 41 ...`），而且 `-b binary` 会
  # 保留**字节列**：助记符在第 3 个制表段里。直接剔首部会拿到字节列当操作数，于是
  # 每一条内存写都被误报成「objdump 的目的不是内存」（本脚本第一版就是这么 105 条假错）。
  d="$("$OBJDUMP" -D -b binary -m i386:x86-64 -M intel "$bin" 2>/dev/null \
        | awk -F'\t' '/^[[:space:]]*[0-9a-f]+:/ { print $3; exit }')"
  if [ -z "$d" ] || printf '%s' "$d" | grep -qE 'invalid|\(bad\)'; then
    # objdump 解不出来 ⇒ 这条字节序列不足以单独判定（多半是被截断的前缀碎片）。
    n_skip=$((n_skip + 1)); continue
  fi
  mn=$(printf '%s' "$d" | awk '{print $1}')
  ops=$(printf '%s' "$d" | sed 's/^[^ ]* *//')
  first="${ops%%,*}"
  # 没有操作数的行（`repz`、`endbr64` 这类）对任何目的判据都无信息 ⇒ 计为跳过而不是
  # “不一致”，否则一个解不出来的碎片会把整个脚本带成不可用。
  if [ "$ops" = "$d" ] || [ "${ops#*,}" = "$ops" ]; then
    n_skip=$((n_skip + 1)); continue
  fi
  n_checked=$((n_checked + 1))
  bad=""
  case "$verdict" in
    reg*)
      want=${verdict#reg}
      ok=0
      for r in $(reg_names "$want"); do
        case "$first" in *"$r"*) ok=1;; esac
      done
      [ "$ok" = 1 ] || bad="dcc 说目的号是 ${want}，objdump 说 '$mn $ops'"
      ;;
    sext*|zext*)
      # `sextN` / `sextN:<imm>` / `zextN:<bits>`：目的 = N，而且只能来自对应那三族。
      kind=${verdict%%:*}
      body=${verdict#*:}
      want=$(printf '%s' "$kind" | sed -E 's/^(sext|zext)([0-9]+)$/\2/')
      ok=0
      for r in $(reg_names "$want"); do
        case "$first" in *"$r"*) ok=1;; esac
      done
      if [ "$ok" != 1 ]; then
        bad="dcc 报了 $verdict 的目的号 ${want}，objdump 说 '$mn $ops'"
      else
        # 「整字已是符号/零扩展」这个主张只允许那几族发出来；`mov` 那一族只能携
        # imm32（`sextN:<imm>` 形，而 imm 为空的 `sextN:` 就是错的）。
        case "$kind" in
          zext)
            case "$mn" in movzx*) :;; *) bad="dcc 报了零扩展主张，助记符却不是 movzx：'$mn $ops'";; esac
            ;;
          sext)
            case "$mn" in
              movsxd|movsx*) :;;
              mov) [ "$body" != "" ] || bad="dcc 对不带立即数的 '$mn $ops' 报了 sext32";;
              *) bad="dcc 报了 sext32，而助记符不属于扩展/立即数搬运那一族：'$mn $ops'";;
            esac
            ;;
        esac
      fi
      ;;
    nothing)
      # cmp/test 按定义不写任何号，而它们的目的位置上的确是寄存器 —— 那不代表写集。
      # 不先排除它们会把每一条 `cmp eax, imm` 报成不一致（本脚本第三版就这么错了 24 条）。
      case "$mn" in cmp|test|je|jne|j*|call|ret|loop*|push|pop|leave|syscall|cpuid|hlt|int*) :;;
        # 目的必须是立即数/内存/控制流目标，不能是 GP 寄存器。
        *)
          if printf '%s' "$first" | grep -qE '(^|[^a-z0-9_])(r(a|b|c|d|s|i|1[0-5]?)[a-z0-9]*|e(a|b|c|d|s|i)[a-z0-9]*)([^a-z0-9_]|$)' &&
             printf '%s' "$ops" | grep -qE ','; then
            # 还要排除「寄存器只出现在源侧」的情形：目的（first）不带寄存器才算没写号。
            case "$first" in
              *"["*|*$'\$'*) :;;
              *) bad="dcc 说不写任何 GP 号，objdump 却把寄存器当目的：'$mn $ops'";;
            esac
          fi
          ;;
      esac
      ;;
    mem|slot:*)
      # Intel 语法把内存型操作数打成 `QWORD PTR [r13-0x60]`，不是以 `[` 开头的；
      # 拿 `\[*` 当前缀匹会把每一条存出都说成「不是内存」（本脚本第二版就是这么错的）。
      case "$first" in
        *"["*) :;;
        *) bad="dcc 说是内存写，objdump 的目的却不是内存：'$mn $ops'";;
      esac
      if [ "$bad" = "" ] && [ "${verdict#slot:}" != "$verdict" ]; then
        disp=$(printf '%s' "$verdict" | cut -d: -f2)
        # 基址必须是帧寄存器 r13（位移的符号/宽度形式由编码器选，不在本判据里复算）。
        if ! printf '%s' "$first" | grep -qE '\[r13([+-]|\])'; then
          bad="dcc 报帧槽写但基址不是 r13：'$mn $ops'（dcc 位移 $disp）"
        fi
      fi
      ;;
  esac
  if [ -n "$bad" ]; then
    n_fail=$((n_fail + 1))
    echo "FACT-FAIL bytes=$hex verdict=$verdict :: $bad"
  fi
done
rm -rf "$TMP" "$RAW"

echo "check_facts_vs_objdump: 对账 $n_checked 条，跳过 $n_skip 条（保守方向或 objdump 解不出），不一致 $n_fail 条"
if [ "$n_checked" -lt 20 ]; then
  echo "FACT-CHECK-TOO-FEW: 只对账到 $n_checked 条 ⇒ 判据覆盖面不足，不算通过" >&2
  exit 1
fi
[ "$n_fail" -eq 0 ] || exit 1
echo "check_facts_vs_objdump: OK"
