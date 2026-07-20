#!/bin/bash
set -e
export PATH=/home/ds/.cargo/bin:$PATH
cd /home/ds/dcc
echo "=== Build ==="
cargo build --bin dcc --release 2>&1
echo "=== Done ==="
