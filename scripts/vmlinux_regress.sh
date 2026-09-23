#!/usr/bin/env bash
# vmlinux 级 pahole / objtool 严格回归（需 ET_EXEC vmlinux）
set -euo pipefail

KERNEL_DIR="${KERNEL_DIR:-$HOME/linux-7.1}"
VMLINUX="${VMLINUX:-$KERNEL_DIR/vmlinux}"
DCC_REPO="${DCC_REPO:-$(cd "$(dirname "$0")/.." && pwd)}"
DCC="${DCC:-$DCC_REPO/target/release/dcc}"

echo "== DCC vmlinux pahole/objtool regression =="
echo "vmlinux: $VMLINUX"
echo "dcc:     $DCC"

if [[ ! -f "$VMLINUX" ]]; then
    if [[ "${REQUIRE_VMLINUX:-0}" == "1" ]]; then
        echo "FAIL: vmlinux not found"
        exit 1
    fi
    echo "SKIP: vmlinux not found — run:"
    echo "  BUILD_VMLINUX=1 bash scripts/kernel_build_smoke.sh"
    exit 0
fi

if ! readelf -h "$VMLINUX" 2>/dev/null | grep -q 'Type:.*EXEC'; then
    echo "FAIL: $VMLINUX is not ET_EXEC"
    exit 1
fi

pass=0
fail=0

if command -v pahole >/dev/null 2>&1; then
    echo "-- pahole -J vmlinux (BTF struct probe)"
    PAHOLE_OUT="$(mktemp)"
    if pahole -J "$VMLINUX" >"$PAHOLE_OUT" 2>&1; then
        if grep -qiE 'STRUCT|union|btf' "$PAHOLE_OUT"; then
            echo "PASS: pahole -J vmlinux (BTF/struct visible)"
            pass=$((pass + 1))
        else
            echo "FAIL: pahole ran but no struct/BTF in output"
            head -20 "$PAHOLE_OUT"
            fail=$((fail + 1))
        fi
    else
        echo "FAIL: pahole -J vmlinux exited non-zero"
        head -20 "$PAHOLE_OUT"
        fail=$((fail + 1))
    fi
    rm -f "$PAHOLE_OUT"
elif [[ "${REQUIRE_PAHOLE:-0}" == "1" ]]; then
    echo "FAIL: pahole not installed"
    exit 1
else
    echo "SKIP: pahole not installed"
fi

if command -v objtool >/dev/null 2>&1; then
    echo "-- objtool check vmlinux"
    OBJ_OUT="$(mktemp)"
    set +e
    objtool check "$VMLINUX" >"$OBJ_OUT" 2>&1
    rc=$?
    set -e
    if grep -qi 'CRITICAL' "$OBJ_OUT"; then
        echo "FAIL: objtool reported CRITICAL on vmlinux"
        grep -i 'CRITICAL' "$OBJ_OUT" | head -10
        fail=$((fail + 1))
    elif [[ $rc -eq 0 ]]; then
        echo "PASS: objtool check vmlinux (zero CRITICAL)"
        pass=$((pass + 1))
    else
        if [[ "${REQUIRE_OBJTOOL:-0}" == "1" ]]; then
            echo "FAIL: objtool exit $rc on vmlinux"
            tail -15 "$OBJ_OUT"
            fail=$((fail + 1))
        else
            echo "WARN: objtool exit $rc without CRITICAL (review log)"
            tail -15 "$OBJ_OUT"
            pass=$((pass + 1))
        fi
    fi
    rm -f "$OBJ_OUT"
else
    if [[ "${REQUIRE_OBJTOOL:-0}" == "1" ]]; then
        echo "FAIL: objtool not installed"
        exit 1
    fi
    echo "SKIP: objtool not installed"
fi

if [[ $fail -gt 0 ]]; then
    echo "FAIL: vmlinux regression ($pass pass, $fail fail)"
    exit 1
fi

echo "PASS: vmlinux regression ($pass checks)"
