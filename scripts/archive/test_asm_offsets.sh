#!/usr/bin/env bash
# 用 dcc 单独编译 asm-offsets.c，验证 parser 修复
set -euo pipefail

KERNEL_DIR="${KERNEL_DIR:-$HOME/linux-7.1}"
DCC="${DCC:-$HOME/dcc-agent-linux66/target/release/dcc}"
CMD_FILE="$KERNEL_DIR/arch/x86/kernel/.asm-offsets.s.cmd"

if [[ ! -f "$CMD_FILE" ]]; then
    echo "missing $CMD_FILE — run make prepare once with gcc first" >&2
    exit 1
fi

CMD=$(head -1 "$CMD_FILE" | cut -d: -f2- | sed 's/^ //' | sed "s|^gcc|$DCC|")
rm -f "$KERNEL_DIR/arch/x86/kernel/asm-offsets.s"
echo "Running: ${CMD:0:120}..."
set +e
eval "$CMD" 2>/tmp/asm-offsets-new.err
rc=$?
set -e

echo "exit=$rc"
head -30 /tmp/asm-offsets-new.err
echo "ERRCOUNT=$(grep -c 'error\[' /tmp/asm-offsets-new.err || true)"
if [[ -f "$KERNEL_DIR/arch/x86/kernel/asm-offsets.s" ]]; then
    echo "OK: asm-offsets.s generated ($(wc -l < "$KERNEL_DIR/arch/x86/kernel/asm-offsets.s") lines)"
else
    echo "FAIL: asm-offsets.s not generated"
    exit "$rc"
fi
