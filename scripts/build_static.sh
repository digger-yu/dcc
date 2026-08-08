#!/bin/bash
# Build dcc as a fully static binary using musl
#
# This script builds dcc without any dependency on the host's
# dynamic linker (ld.so), producing a portable single-file binary.
#
# Prerequisites:
#   - musl-tools (on Debian/Ubuntu: apt install musl-tools)
#   - Rust musl target: rustup target add x86_64-unknown-linux-musl
#
# Usage:
#   ./scripts/build_static.sh [release|debug]
#
# Output:
#   target/x86_64-unknown-linux-musl/{release|debug}/dcc

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

# Default to release build
BUILD_TYPE="${1:-release}"

# Detect architecture
ARCH=$(uname -m)
case "$ARCH" in
    x86_64)
        TARGET="x86_64-unknown-linux-musl"
        ;;
    aarch64)
        TARGET="aarch64-unknown-linux-musl"
        ;;
    riscv64)
        TARGET="riscv64gc-unknown-linux-musl"
        ;;
    *)
        echo "Error: Unsupported architecture $ARCH"
        exit 1
        ;;
esac

echo "Building dcc static binary for $TARGET..."

# Check if musl target is installed
if ! rustup target list --installed | grep -q "$TARGET"; then
    echo "Installing musl target: $TARGET"
    rustup target add "$TARGET"
fi

cd "$PROJECT_DIR"

# Build with musl target
if [ "$BUILD_TYPE" = "release" ]; then
    cargo build --release --target "$TARGET"
    OUTPUT="target/$TARGET/release/dcc"
else
    cargo build --target "$TARGET"
    OUTPUT="target/$TARGET/debug/dcc"
fi

if [ -f "$OUTPUT" ]; then
    echo ""
    echo "Build successful!"
    echo "Binary: $OUTPUT"
    echo ""
    echo "Verifying static linking..."
    
    # Check if binary is statically linked
    if file "$OUTPUT" | grep -q "statically linked"; then
        echo "✓ Binary is statically linked"
    else
        echo "⚠ Warning: Binary may not be fully static"
    fi
    
    # Show binary info
    echo ""
    echo "Binary info:"
    file "$OUTPUT"
    ls -lh "$OUTPUT"
    
    # Show dependencies (should be none for static)
    echo ""
    echo "Dynamic dependencies:"
    if command -v ldd &> /dev/null; then
        ldd "$OUTPUT" 2>&1 || echo "(none - statically linked)"
    else
        echo "(ldd not available)"
    fi
else
    echo "Error: Build failed, output not found: $OUTPUT"
    exit 1
fi
