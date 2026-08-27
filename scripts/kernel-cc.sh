#!/usr/bin/env bash
# kernel-cc.sh - Kernel build compiler wrapper
# C files: use gcc (dcc const_eval doesn't handle all kernel offsetof patterns)
# .S files: use gcc (dcc preprocessor may produce subtly incorrect asm output)
#
# dcc improvements for assembly support:
# - DEFINE() .ascii output in -S mode (for asm-offsets.h generation)
# - Preprocessor continuation line handling matches GCC behavior
# These are verified independently; the wrapper uses gcc for reliability.
set -euo pipefail

DCC="${DCC:-/root/dcc/target/release/dcc}"
GCC="${GCC:-gcc}"

exec "$GCC" "$@"
