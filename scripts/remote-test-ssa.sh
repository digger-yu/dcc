#!/bin/bash
set -e
export PATH=/home/ds/.cargo/bin:$PATH
cd /home/ds/dcc
echo "=== SSA unit tests ==="
cargo test --bin dcc ssa 2>&1
echo "=== Done ==="
