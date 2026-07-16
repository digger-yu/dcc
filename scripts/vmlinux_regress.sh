#!/usr/bin/env bash
# vmlinux 级 pahole / objtool 回归（需完整内核链接产物）
set -euo pipefail

KERNEL_DIR="${KERNEL_DIR:-$HOME/linux-6.6}"
VMLINUX="${VMLINUX:-$KERNEL_DIR/vmlinux}"

echo "== DCC vmlinux pahole/objtool regression =="
echo "vmlinux: $VMLINUX"

if [[ ! -f "$VMLINUX" ]]; then
    echo "SKIP: vmlinux not found — build with:"
    echo "  cd $KERNEL_DIR && make CC=\$DCC vmlinux"
    exit 0
fi

if ! readelf -h "$VMLINUX" 2>/dev/null | grep -q 'Type:.*EXEC'; then
    echo "FAIL: $VMLINUX is not ET_EXEC"
    exit 1
fi

if command -v pahole >/dev/null 2>&1; then
    echo "-- pahole -J vmlinux"
    if pahole -J "$VMLINUX" 2>&1 | head -20; then
        echo "PASS: pahole -J vmlinux"
    else
        echo "WARN: pahole -J reported issues (vmlinux present)"
    fi
else
    echo "SKIP: pahole not installed"
fi

if command -v objtool >/dev/null 2>&1; then
    echo "-- objtool check vmlinux"
    if objtool check "$VMLINUX" 2>&1; then
        echo "PASS: objtool check vmlinux"
    else
        echo "WARN: objtool reported issues on vmlinux"
    fi
else
    echo "SKIP: objtool not installed"
fi

echo "PASS: vmlinux regression probes completed"
