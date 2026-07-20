#!/bin/bash
set -e
source "$HOME/.cargo/env" 2>/dev/null || true
export PATH="$HOME/.cargo/bin:$PATH"
cd /home/ds/dcc
export CARGO_TARGET_DIR=/home/ds/dcc-target-m15
cargo test --bin dcc -- const_eval::tests 2>&1 | tail -40
