#!/usr/bin/env bash
# prepare0 窄目标验收：devicetable-offsets / bounds / modpost file2alias
set -euo pipefail

KERNEL_DIR="${KERNEL_DIR:-$HOME/linux-7.1}"
DCC_REPO="${DCC_REPO:-$(cd "$(dirname "$0")/.." && pwd)}"
DCC="${DCC:-$DCC_REPO/target/release/dcc}"
WORK="${WORK:-/tmp/dcc-prepare0-$$}"
mkdir -p "$WORK"

echo "== DCC prepare0 narrow check =="
echo "kernel: $KERNEL_DIR"
echo "dcc:    $DCC"

if [[ ! -x "$DCC" ]]; then
    echo "FAIL: dcc not found at $DCC"
    exit 1
fi

cd "$KERNEL_DIR"

if [[ ! -f include/generated/autoconf.h ]]; then
    echo "Regenerating autoconf.h..."
    make olddefconfig
    make include/generated/autoconf.h
fi

INC=(
    -nostdinc
    -I./arch/x86/include -I./arch/x86/include/generated
    -I./include -I./arch/x86/include/uapi
    -I./arch/x86/include/generated/uapi
    -I./include/uapi -I./include/generated/uapi
    -include ./include/linux/compiler-version.h
    -include ./include/linux/kconfig.h
    -include ./include/linux/compiler_types.h
    -D__KERNEL__ -std=gnu11
)

echo "-- devicetable-offsets.c (-S)"
"$DCC" "${INC[@]}" -S -o "$WORK/devicetable-offsets.s" \
    scripts/mod/devicetable-offsets.c
grep -q 'SIZE_' "$WORK/devicetable-offsets.s"

echo "-- kernel/bounds.c (-S)"
"$DCC" "${INC[@]}" -S -o "$WORK/bounds.s" kernel/bounds.c

echo "-- scripts/mod/file2alias.c (-c)"
"$DCC" "${INC[@]}" -c -o "$WORK/file2alias.o" scripts/mod/file2alias.c
[[ -s "$WORK/file2alias.o" ]]

echo "NOTE: modpost full link → scripts/modpost_link.sh"

echo "PASS: prepare0 narrow targets OK"
rm -rf "$WORK"
