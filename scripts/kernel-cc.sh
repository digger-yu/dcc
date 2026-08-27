#!/usr/bin/env bash
# kernel-cc.sh - Kernel build compiler wrapper
#
# .S files: try dcc first, fall back to gcc on failure
# .c files: use gcc (dcc const_eval doesn't handle all
#           kernel offsetof patterns yet)
#
# dcc improvements for assembly support:
# - DEFINE() .ascii output in -S mode (asm-offsets.h)
# - Continuation line handling matches GCC behavior
# - Char literal in asm comments (# don't) no longer
#   breaks macro expansion on subsequent lines
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

# .S files: try dcc, fall back to gcc
if [[ -n "$src" && "$src" == *.S ]]; then
    if "$DCC" "$@" 2>/tmp/dcc_asm_err; then
        exit 0
    fi
    # dcc failed, fall back to gcc
    exec "$GCC" "$@"
fi

# Everything else: use gcc
exec "$GCC" "$@"
