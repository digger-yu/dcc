#!/usr/bin/env bash
set -u

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

DCC="${DCC:-./target/debug/dcc}"
TMP="${TMPDIR:-/tmp}/dcc-kernel-features-$$"
mkdir -p "$TMP"
trap 'rm -rf "$TMP"' EXIT

pass=0
fail=0

run_rel() {
    local source="$1"
    local name="$2"
    local object="$TMP/$name.o"
    local executable="$TMP/$name"

    if "$DCC" -c "$source" -o "$object" &&
       gcc "$object" -o "$executable" 2>/dev/null &&
       timeout 10 "$executable"; then
        echo "PASS $name"
        pass=$((pass + 1))
    else
        local status=$?
        echo "FAIL $name (status=$status)"
        fail=$((fail + 1))
    fi
}

cargo build

run_rel tests/fixtures/smoke/elf_attrs.c p2_elf_attrs
run_rel tests/fixtures/smoke/va_list.c p4_va_list
run_rel tests/fixtures/smoke/nested_func.c nested_function
run_rel tests/fixtures/smoke/atomic.c p5_atomic
run_rel tests/fixtures/smoke/runtime_features.c p6_runtime_features
run_rel tests/fixtures/smoke/switch.c p7_switch_control_flow

attrs_object="$TMP/attrs-readelf.o"
if "$DCC" -c tests/fixtures/smoke/elf_attrs.c -o "$attrs_object"; then
    sections="$(readelf -SW "$attrs_object")"
    symbols="$(readelf -sW "$attrs_object")"
    relocs="$(readelf -rW "$attrs_object")"

    if [[ "$sections" == *".init.text"* &&
          "$sections" == *".data.custom"* &&
          "$relocs" == *".rela.init.text"* &&
          "$relocs" == *".rela.data.custom"* ]]; then
        if echo "$symbols" | grep -q "weak_function" &&
           echo "$symbols" | grep "weak_function" | grep -q "WEAK" &&
           echo "$symbols" | grep -q "hidden_bss" &&
           echo "$symbols" | grep "hidden_bss" | grep -qE "HIDDEN|Vis.*HIDDEN"; then
            echo "PASS elf_metadata"
            pass=$((pass + 1))
        else
            echo "FAIL elf_metadata"
            fail=$((fail + 1))
            readelf -SWsWr "$attrs_object"
        fi
    else
        echo "FAIL elf_metadata"
        fail=$((fail + 1))
        readelf -SWsWr "$attrs_object"
    fi
else
    echo "FAIL elf_metadata (compile)"
    fail=$((fail + 1))
fi

echo "=== $pass passed, $fail failed ==="
test "$fail" -eq 0
