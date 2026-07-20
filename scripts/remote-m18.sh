#!/bin/bash
# M18 remote build & test runner
# Usage: bash scripts/remote-m18.sh
set -e

export PATH=/usr/bin:/usr/local/bin:/usr/local/sbin:/usr/sbin:/sbin:/bin:$HOME/.cargo/bin

cd /home/ds/dcc

echo "=== Reset remote working tree ==="
git checkout .
git clean -fd

echo "=== Git pull --rebase ==="
git pull --rebase 2>&1 || echo "pull failed, continuing"

echo "=== Verify source updated ==="
git log --oneline -1
git status --short

echo "=== Build release ==="
cargo build --release --bin dcc 2>&1 | tail -3

echo "=== Run m18_* integration tests ==="
cargo test --test integration m18_ -- --test-threads=1 2>&1 | tail -30
