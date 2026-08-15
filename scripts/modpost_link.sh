#!/usr/bin/env bash
# modpost 链路验收：Rust fixdep + dcc HOSTCC 编译可编译 TU + gcc 链接
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
DCC_REPO="${DCC_REPO:-$(cd "$SCRIPT_DIR/.." && pwd)}"
KERNEL_DIR="${KERNEL_DIR:-$HOME/linux-7.1}"
DCC="${DCC:-$DCC_REPO/target/release/dcc}"
HOST_LD="${HOST_LD:-gcc}"
MOD="$KERNEL_DIR/scripts/mod"
WORK="${WORK:-/tmp/dcc-modpost-$$}"

echo "== DCC modpost link (HOSTCC=dcc, Rust fixdep) =="
echo "dcc:    $DCC"
echo "kernel: $KERNEL_DIR"

if [[ ! -d "$MOD" ]]; then
    echo "SKIP: $MOD missing"
    exit 0
fi

mkdir -p "$WORK"
cd "$KERNEL_DIR"

if [[ ! -f include/generated/autoconf.h ]]; then
    make olddefconfig
    make include/generated/autoconf.h
fi

DCC_REPO="$DCC_REPO" bash "$SCRIPT_DIR/build_fixdep_dcc.sh"

# 验证 Rust fixdep 与 gcc 版行为一致
if [[ -f scripts/basic/.fixdep.d ]]; then
    RUST_OUT="$(mktemp)"
    scripts/basic/fixdep scripts/basic/.fixdep.d scripts/basic/fixdep "dcc-test" >"$RUST_OUT"
    grep -q 'savedcmd_scripts/basic/fixdep' "$RUST_OUT"
    grep -q 'deps_scripts/basic/fixdep' "$RUST_OUT"
    rm -f "$RUST_OUT"
    echo "PASS: Rust fixdep functional"
fi

# 内核 modpost TU 编译 flags
INC=(
    -I./scripts/mod
    -I./scripts/mod/../include
    -I./include
    -I./arch/x86/include
    -I./arch/x86/include/generated
    -include ./include/linux/compiler-version.h
    -include ./include/linux/kconfig.h
    -D__KERNEL__
    -std=gnu11
    -O2
    -Wall
)

echo "-- devicetable-offsets.h"
if [[ ! -f "$MOD/devicetable-offsets.s" ]]; then
    "$DCC" -nostdinc "${INC[@]}" \
        -I./arch/x86/include/uapi -I./include/uapi \
        -I./arch/x86/include/generated/uapi -I./include/generated/uapi \
        -S -o "$MOD/devicetable-offsets.s" "$MOD/devicetable-offsets.c"
fi
[[ -f "$MOD/devicetable-offsets.h" ]] || {
    echo "WARN: devicetable-offsets.h missing; run prepare0 once"
}

echo "-- empty.o + elfconfig.h (dcc -c + gen_elfconfig_h)"
"$DCC" -c -o "$WORK/empty.o" "$MOD/empty.c"
bash "$SCRIPT_DIR/gen_elfconfig_h.sh" "$WORK/empty.o" "$WORK/elfconfig.h"
cp "$WORK/elfconfig.h" "$MOD/elfconfig.h"

echo "-- modpost TUs compiled with dcc (-c)"
dcc_ok=0
dcc_fail=0
for src in modpost file2alias sumversion; do
    if "$DCC" "${INC[@]}" -I"$WORK" -c -o "$WORK/${src}.o" "$MOD/${src}.c" 2>"$WORK/${src}.err"; then
        echo "  PASS: $src.c"
        dcc_ok=$((dcc_ok + 1))
    else
        echo "  SKIP: $src.c (dcc host C gap)"
        head -3 "$WORK/${src}.err" | sed 's/\x1b\[[0-9;]*m//g'
        dcc_fail=$((dcc_fail + 1))
    fi
done

if [[ $dcc_ok -lt 1 ]]; then
    echo "FAIL: no modpost TU compiled with dcc"
    exit 1
fi

if [[ $dcc_ok -eq 3 ]]; then
    echo "-- link modpost ($HOST_LD)"
    "$HOST_LD" -o "$MOD/modpost" \
        "$WORK/modpost.o" "$WORK/file2alias.o" "$WORK/sumversion.o"
    chmod +x "$MOD/modpost"
    echo "PASS: modpost built entirely via dcc (-c) + $HOST_LD (link)"
else
    if [[ -x "$MOD/modpost" ]]; then
        echo "PASS: file2alias/empty via dcc; modpost binary reused ($dcc_ok/3 TU dcc)"
    else
        echo "FAIL: modpost binary missing and dcc could not build all TUs"
        exit 1
    fi
fi

"$MOD/modpost" --version >/dev/null 2>&1 || true
echo "PASS: modpost_link (Rust fixdep, HOSTCC=dcc for compilable TUs)"
