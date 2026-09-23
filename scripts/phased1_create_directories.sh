#!/bin/bash
# scripts/phased1_create_directories.sh
# Phase 1: Create entire directory tree BEFORE any file extraction
# User Requirement: Directory-first approach, not incremental validation
# Date: Tuesday, September 22, 2026
#
# This script creates all target directories for the modularized architecture
# following the specification from MODULARIZATION_PLAN.md

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
TARGET_DIR="$ROOT_DIR/src/codegen/x86_64"

echo "=== Phase 1: Creating Directory Structure ==="
echo "Root directory: $ROOT_DIR"
echo "Target directory: $TARGET_DIR"
echo ""

# Create all target directories in one command chain
mkdir -p \
    "$TARGET_DIR/asm/planner" \
    "$TARGET_DIR/asm/constraint" \
    "$TARGET_DIR/asm/template" \
    "$TARGET_DIR/asm/reg" \
    "$TARGET_DIR/emit/expr" \
    "$TARGET_DIR/mem" \
    "$TARGET_DIR/special" \
    "$TARGET_DIR/parallel" \
    "$TARGET_DIR/tests" \
    "$TARGET_DIR/elf_side"

echo "✅ Created directory tree:"
echo "   asm/planner/"
echo "   asm/constraint/"
echo "   asm/template/"
echo "   asm/reg/"
echo "   emit/expr/"
echo "   mem/"
echo "   special/"
echo "   parallel/"
echo "   tests/"
echo "   elf_side/"
echo ""

# Verify creation
echo "Verifying directory structure..."
ls -la "$TARGET_DIR" | grep -E "^d.*\(asm\|emit\|mem\|special\|parallel\|tests\|elf_side\)" || echo "⚠️ Some directories may be missing"

echo ""
echo "Phase 1 complete! Ready for content extraction."
