#!/usr/bin/env bash
# 阶段 E 性能冒烟：mmap + 分块 codegen + -O2
set -euo pipefail
source ~/.cargo/env
cd "$(dirname "$0")/.."
DCC="${DCC:-./target/release/dcc}"
TMP="${TMPDIR:-/tmp}/dcc-phase-e-$$"
mkdir -p "$TMP"
trap 'rm -rf "$TMP"' EXIT

echo "=== Phase E bench ==="

cat > "$TMP/multi.c" <<'EOF'
static int helper(void) { return 3; }
int foo(void) { return helper() + 1; }
int bar(void) { return helper() + 2; }
int main(void) { return foo() + bar(); }
EOF

echo "[1] serial (-O2 -c, DCC_NO_PARALLEL=1)"
export DCC_NO_PARALLEL=1
{ time "$DCC" -O2 -c -o "$TMP/serial.o" "$TMP/multi.c"; } 2>&1 | tee "$TMP/serial.time"

echo "[2] parallel (-O2 -c, std::thread worker pool)"
unset DCC_NO_PARALLEL
{ time "$DCC" -O2 -c -o "$TMP/batch.o" "$TMP/multi.c"; } 2>&1 | tee "$TMP/batch.time"

echo "[3] O2 branch fold"
cat > "$TMP/fold.c" <<'EOF'
int main(void) { if (1) return 42; return 0; }
EOF
"$DCC" -O2 -c -o "$TMP/fold.o" "$TMP/fold.c"
objdump -d "$TMP/fold.o" | grep -E 'mov.*\$0x2a|mov.*\$0x42' && echo "O2 fold OK"

echo "[4] aarch64 hello"
"$DCC" --target aarch64-linux-gnu -c -o "$TMP/a64.o" tests/fixtures/smoke/hello.c
readelf -h "$TMP/a64.o" | grep -q 'AArch64' && echo "aarch64 OK"

echo "=== done ==="
