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

    if "$DCC" -r "$source" -o "$object" &&
       gcc "$object" -o "$executable" &&
       "$executable"; then
        echo "PASS $name"
        pass=$((pass + 1))
    else
        local status=$?
        echo "FAIL $name (status=$status)"
        fail=$((fail + 1))
    fi
}

cargo build

run_rel examples/test_p1_features.c p1_features
run_rel examples/test_p2_elf_attrs.c p2_elf_attrs
run_rel examples/test_p3_builtins.c p3_builtins
run_rel examples/test_p4_va_list.c p4_va_list
run_rel examples/test_nested_func.c nested_function
run_rel examples/test_asm_goto.c asm_goto

attrs_object="$TMP/attrs-readelf.o"
if "$DCC" -r examples/test_p2_elf_attrs.c -o "$attrs_object"; then
    sections="$(readelf -SW "$attrs_object")"
    symbols="$(readelf -sW "$attrs_object")"
    relocs="$(readelf -rW "$attrs_object")"

    if [[ "$sections" == *".init.text"* &&
          "$sections" == *".data.custom"* &&
          "$symbols" == *"WEAK"*"weak_function"* &&
          "$symbols" == *"HIDDEN"*"hidden_bss"* &&
          "$relocs" == *".rela.init.text"* &&
          "$relocs" == *".rela.data.custom"* ]]; then
        echo "PASS elf_metadata"
        pass=$((pass + 1))
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
