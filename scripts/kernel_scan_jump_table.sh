#!/bin/bash
# 扫描 vmlinux 的 __jump_table：逐条解码 code 目标处的第一条指令。
#
# __jump_table 没有独立输出节（链接脚本把它并进了某个 PROGBITS 节，实测是
# .rodata），边界靠 __start___jump_table / __stop___jump_table 两个符号。
# 条目 16 字节：s32 code（相对 &entry->code）; s32 target（相对）; u64 key
#
# 判定：首字节 0xeb → 2 字节跳转（正常）；0xe9 → 5 字节跳转（正常）；
# 其余（尤其 0x90，即 .text 原位 NOP 洞）→ 坏条目。
set -uo pipefail
VMLINUX=${1:-/root/pvh-lmc/vmlinux}
tmp=$(mktemp -d /tmp/jtscan.XXXXXX)
trap 'rm -rf "$tmp"' EXIT

# 全部 PROGBITS 节：name addr off size（readelf 输出十六进制、无 0x）
readelf -SW "$VMLINUX" | awk '
    {
        line = $0
        if (!sub(/^ *\[ *[0-9]+\] +/, "", line)) next
        n = split(line, a, / +/)
        if (a[2] != "PROGBITS") next
        print a[1], a[3], a[4], a[5]
    }' >"$tmp/secs.txt"

addr2off() { # <addr> -> 设 OFF / SEC；失败返回 1
    local addr=$1 saddr ssize
    while read -r SEC saddr soff ssize; do
        saddr=$((0x$saddr))
        ssize=$((0x$ssize))
        if ((addr >= saddr && addr < saddr + ssize)); then
            OFF=$((0x$soff + addr - saddr))
            return 0
        fi
    done <"$tmp/secs.txt"
    return 1
}

read -r jt_start jt_stop <<<"$(readelf -sW "$VMLINUX" | awk '
    $8 == "__start___jump_table" { s = $2 }
    $8 == "__stop___jump_table"  { e = $2 }
    END { print s, e }')"
[[ -n "${jt_start:-}" && -n "${jt_stop:-}" ]] || { echo "FAIL: 缺 __start/__stop___jump_table 符号"; exit 1; }
jt_start=$((0x$jt_start)); jt_stop=$((0x$jt_stop))

n=$(((jt_stop - jt_start) / 16))
addr2off "$jt_start" || { echo "FAIL: 定位不到 __jump_table 所在节"; exit 1; }
jt_file_off=$OFF jt_sec=$SEC
echo "jump_table: start=$jt_start stop=$jt_stop entries=$n section=$jt_sec off=$jt_file_off"

od -An -tu4 -j "$jt_file_off" -N $((n * 16)) -w16 "$VMLINUX" >"$tmp/entries.txt" || exit 1

good=0 bad_nop=0 bad_other=0 out_of_text=0
declare -A bad_kind
bad_kind=()
note_bad() { bad_kind["$1"]=$(( ${bad_kind["$1"]:-0} + 1 )); }
i=0
while read -r code target key_lo key_hi; do
    ((code >= 2147483648)) && code=$((code - 4294967296))
    addr=$((jt_start + i * 16 + code))
    i=$((i + 1))
    if ! addr2off "$addr"; then
        out_of_text=$((out_of_text + 1))
        note_bad "out_of_section"
        continue
    fi
    case "$SEC" in
        .text|.init.text|.altinstr_aux|.altinstr_replacement|.exit.text) ;;
        *) out_of_text=$((out_of_text + 1))
           note_bad "sec:$SEC"
           continue ;;
    esac
    # 前 8 字节，够覆盖 0f1f 家族的 5 字节 NOP 与 66 90
    bytes=$(od -An -tu1 -j "$OFF" -N8 "$VMLINUX" | tr -s ' ' | sed 's/^ //')
    set -- $bytes
    b0=$1 b1=${2:-0} b2=${3:-0} b3=${4:-0}
    if ((b0 == 0x90)); then
        # 单字节 NOP = insn.length 1 → arch_jump_entry_size 必 BUG
        bad_nop=$((bad_nop + 1))
    elif ((b0 == 0xeb || b0 == 0xe9)); then
        good=$((good + 1))
    elif ((b0 == 0x66 && b1 == 0x90)); then
        good=$((good + 1))
    elif ((b0 == 0x0f && b1 == 0x1f)); then
        # NOP r/m：modrm + 可选 SIB/disp，目标站位应为 5 字节形式
        mod=$((b2 >> 6)) rm=$((b2 & 7)) len=3
        ((rm == 4)) && ((mod != 3)) && len=$((len + 1))
        case $mod in
            1) len=$((len + 1)) ;;
            2) len=$((len + 4)) ;;
            0) ((rm == 5)) && len=$((len + 4)) ;;
        esac
        if ((len == 5)); then good=$((good + 1)); else
            bad_other=$((bad_other + 1))
            note_bad "nop_len_$len"
        fi
    else
        bad_other=$((bad_other + 1))
        note_bad "bytes:$(printf '%02x %02x %02x %02x' "$b0" "$b1" "$b2" "$b3")"
    fi
done <"$tmp/entries.txt"

echo "ok=$good bad_nop=$bad_nop bad_other=$bad_other out_of_text=$out_of_text"
for k in "${!bad_kind[@]}"; do echo "  $k -> ${bad_kind[$k]}"; done | sort
((bad_nop == 0 && bad_other == 0 && out_of_text == 0)) && echo "RESULT: PASS" || echo "RESULT: FAIL"
