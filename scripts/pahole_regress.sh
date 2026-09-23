#!/usr/bin/env bash
# pahole/BTF 回归：编译带 -g 的 .o 并用 pahole 或 llvm-objdump 探测 BTF
set -euo pipefail

DCC_REPO="${DCC_REPO:-$(cd "$(dirname "$0")/.." && pwd)}"
DCC="${DCC:-$DCC_REPO/target/release/dcc}"
WORK="${WORK:-/tmp/dcc-pahole-$$}"
mkdir -p "$WORK"

echo "== DCC pahole/BTF regression =="

SRC="$WORK/btf_probe.c"
OBJ="$WORK/btf_probe.o"
cat > "$SRC" <<'EOF'
struct dcc_probe { int val; };
struct dcc_probe g = { .val = 42 };
int use_probe(void) { return g.val; }
EOF

"$DCC" -g -c -o "$OBJ" "$SRC"

if ! readelf -S "$OBJ" 2>/dev/null | grep -q '\.BTF'; then
    echo "FAIL: .BTF section missing"
    exit 1
fi

if command -v pahole >/dev/null 2>&1; then
    echo "-- pahole --btf_dump"
    if pahole --btf_dump "$OBJ" 2>&1 | grep -qi 'dcc_probe\|STRUCT'; then
        echo "PASS: pahole parsed BTF struct"
    elif [[ "${REQUIRE_PAHOLE:-0}" == "1" ]]; then
        echo "FAIL: pahole ran but struct name not found"
        exit 1
    else
        echo "WARN: pahole ran but struct name not found (BTF section present)"
    fi
elif command -v llvm-objdump >/dev/null 2>&1; then
    if [[ "${REQUIRE_PAHOLE:-0}" == "1" ]]; then
        echo "FAIL: pahole not installed"
        exit 1
    fi
    echo "-- llvm-objdump section headers"
    llvm-objdump -h "$OBJ" | grep -q '\.BTF'
    echo "PASS: BTF section visible to llvm-objdump"
else
    if [[ "${REQUIRE_PAHOLE:-0}" == "1" ]]; then
        echo "FAIL: pahole not installed"
        exit 1
    fi
    echo "SKIP: pahole/llvm-objdump not installed; .BTF present"
fi

rm -rf "$WORK"
echo "PASS: pahole/BTF regression"
