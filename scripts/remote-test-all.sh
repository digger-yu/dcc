#!/bin/bash
set -e
export PATH=/home/ds/.cargo/bin:$PATH
cd /home/ds/dcc
echo "=== m15 tests ==="
cargo test --test integration m15_ --release 2>&1
echo "=== m16 tests ==="
cargo test --test integration m16_ --release 2>&1
echo "=== Done ==="
