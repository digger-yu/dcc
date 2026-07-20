#!/bin/bash
set -e
export PATH=/home/ds/.cargo/bin:$PATH
cd /home/ds/dcc
echo "=== Test m16_ ==="
cargo test --test integration m16_ --release 2>&1
echo "=== Done ==="
