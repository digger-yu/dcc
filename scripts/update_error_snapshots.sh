#!/usr/bin/env bash
# 在 Linux 上生成/更新 tests/fixtures/errors/*.stderr.snap
set -euo pipefail
cd "$(dirname "$0")/.."
export DCC_UPDATE_SNAPSHOTS=1
cargo test --test errors -- --nocapture
echo "snapshots updated under tests/fixtures/errors/"
