#!/bin/bash
# M18 remote test runner
set -e

export PATH=/usr/bin:/usr/local/bin:/usr/local/sbin:/usr/sbin:/sbin:/bin:$HOME/.cargo/bin

cd /home/ds/dcc

echo "=== Reset remote working tree ==="
git checkout .
git clean -fd

echo "=== Pull --rebase ==="
git pull --rebase 2>&1 | tail -3

echo "=== Verify HEAD ==="
git log --oneline -1
git status --short

echo "=== Build release ==="
cargo build --release --bin dcc 2>&1 | tail -3

echo "=== Run m18_* integration tests ==="
cargo test --test integration m18_ -- --test-threads=1 2>&1 | tail -40
