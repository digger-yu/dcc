#!/usr/bin/env bash
# 从 empty.o 的 ELF 头生成 elfconfig.h（dcc 编译的 mk_elfconfig 宿主程序尚不可用）
set -euo pipefail

EMPTY_O="${1:?empty.o path}"
OUT="${2:?elfconfig.h path}"

if [[ ! -f "$EMPTY_O" ]]; then
    echo "FAIL: $EMPTY_O missing"
    exit 1
fi

# EI_CLASS(4) EI_DATA(5)
hex="$(od -An -tx1 -j4 -N2 "$EMPTY_O" | tr -d ' \n\t')"
class="${hex:0:2}"
data="${hex:2:2}"

case "$class" in
    01) kclass="ELFCLASS32" ;;
    02) kclass="ELFCLASS64" ;;
    *) echo "FAIL: unknown ELF class $class"; exit 1 ;;
esac

case "$data" in
    01) kdata="ELFDATA2LSB" ;;
    02) kdata="ELFDATA2MSB" ;;
    *) echo "FAIL: unknown ELF data $data"; exit 1 ;;
esac

if [[ "$(getconf LONG_BIT 2>/dev/null || echo 64)" -eq 64 ]]; then
    hclass="ELFCLASS64"
else
    hclass="ELFCLASS32"
fi

# 主机字节序
if printf '\001\002' | od -An -tx1 | grep -q '01 02'; then
    hdata="ELFDATA2MSB"
else
    hdata="ELFDATA2LSB"
fi

cat >"$OUT" <<EOF
#define KERNEL_ELFCLASS $kclass
#define KERNEL_ELFDATA $kdata
#define HOST_ELFCLASS $hclass
#define HOST_ELFDATA $hdata
EOF

echo "PASS: generated $OUT from $EMPTY_O"
