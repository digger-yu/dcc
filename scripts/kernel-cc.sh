#!/usr/bin/env bash
# kernel-cc.sh - Kernel build compiler wrapper
#
# Both .c and .S files: try dcc first, fall back to gcc on failure
# Exception: VDSO files always use gcc (requires PIC/relocations dcc doesn't support)
#
# dcc improvements:
# - DEFINE() .ascii output in -S mode (asm-offsets.h)
# - Continuation line handling matches GCC behavior
# - Char literal in asm comments (# don't) no longer
#   breaks macro expansion on subsequent lines
# - offsetof/sizeof/alignof constant folding in SSA passes
set -euo pipefail

DCC="${DCC:-/root/dcc/target/release/dcc}"
GCC="${GCC:-gcc}"

# Find the source file (last argument that ends in .c or .S)
src=""
for arg in "$@"; do
    case "$arg" in
        *.c|*.S) src="$arg" ;;
    esac
done

# VDSO files require PIC/relocations that dcc doesn't support yet
if [[ -n "$src" && "$src" == */vdso/* ]]; then
    exec "$GCC" "$@"
fi

# Try dcc for both .c and .S files, fall back to gcc on failure
if [[ -n "$src" && ("$src" == *.c || "$src" == *.S) ]]; then
    if "$DCC" "$@" 2>/tmp/dcc_err; then
        exit 0
    fi
    # dcc failed, fall back to gcc
    exec "$GCC" "$@"
fi

# Everything else: use gcc
exec "$GCC" "$@"
