#!/usr/bin/env bash
# objtool/ORC 回归：验证 .eh_frame FDE 与 .orc_unwind_ip（无 objtool 时用 readelf）
set -euo pipefail

DCC_REPO="${DCC_REPO:-$(cd "$(dirname "$0")/.." && pwd)}"
DCC="${DCC:-$DCC_REPO/target/release/dcc}"
WORK="${WORK:-/tmp/dcc-objtool-$$}"
mkdir -p "$WORK"

echo "== DCC objtool/eh_frame regression =="

SRC="$WORK/unwind.c"
OBJ="$WORK/unwind.o"
cat > "$SRC" <<'EOF'
int foo(void) { return 1; }
int bar(void) { return foo() + 2; }
EOF

"$DCC" -g -fno-omit-frame-pointer -c -o "$OBJ" "$SRC"

for sec in .eh_frame .orc_unwind_ip; do
    if ! readelf -S "$OBJ" 2>/dev/null | grep -q "$sec"; then
        echo "FAIL: missing section $sec"
        exit 1
    fi
done

EH_SIZE=$(readelf -S "$OBJ" | awk '/\.eh_frame/ {getline; gsub(/^0+/,"",$1); print $1; exit}')
if [[ "${EH_SIZE:-0}" -lt 32 ]]; then
    echo "FAIL: .eh_frame too small (${EH_SIZE:-0} bytes); expected CIE+FDE"
    exit 1
fi

if command -v objtool >/dev/null 2>&1; then
    echo "-- objtool check"
    objtool check "$OBJ"
    echo "PASS: objtool check"
else
    echo "SKIP: objtool not installed; eh_frame/ORC sections verified via readelf"
fi

rm -rf "$WORK"
echo "PASS: objtool/eh_frame regression"
