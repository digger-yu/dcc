#!/usr/bin/env bash
# 安装 Rust fixdep 到内核树，替代 gcc/dcc 编译的 C fixdep
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
DCC_REPO="${DCC_REPO:-$(cd "$SCRIPT_DIR/.." && pwd)}"
KERNEL_DIR="${KERNEL_DIR:-$HOME/linux-6.6}"
OUT="$KERNEL_DIR/scripts/basic/fixdep"

echo "== install Rust fixdep =="
echo "repo:   $DCC_REPO"
echo "kernel: $KERNEL_DIR"

if [[ ! -f "$DCC_REPO/Cargo.toml" ]]; then
    echo "FAIL: DCC repo not found at $DCC_REPO"
    exit 1
fi

cd "$DCC_REPO"
# 非交互 SSH 可能没有 cargo 在 PATH
if [[ -f "$HOME/.cargo/env" ]]; then
    # shellcheck disable=SC1091
    source "$HOME/.cargo/env"
fi
cargo build --release --bin fixdep --quiet
install -m 755 "$DCC_REPO/target/release/fixdep" "$OUT"

# Kbuild 记录 savedcmd（供 if_changed 比对）
mkdir -p "$(dirname "$OUT")"
printf 'savedcmd_fixdep := cargo fixdep (Rust, no gcc)\n' \
    > "$KERNEL_DIR/scripts/basic/.fixdep.cmd"

echo "PASS: fixdep installed (Rust, no gcc for build or runtime)"
