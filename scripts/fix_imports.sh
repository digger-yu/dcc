#!/bin/bash
# scripts/fix_imports.sh - Batch import fix for all backend codegen modules
# Phase 1 Workstream A: Import Pattern Standardization

set -e

echo "=== Starting Import Fix ==="

# Define target directory (all backend codegen modules)
TARGET_DIRS="src/codegen/{x86_64,aarch64,riscv64,mips,arm}/codegen"

# Function to add HashMap import if missing
add_hashmap_import() {
    local file=$1
    if grep -q "HashMap" "$file" && ! grep -q "^use std::collections::HashMap;" "$file"; then
        echo "Adding HashMap import to $file"
        sed -i '1a use std::collections::HashMap;' "$file"
    fi
}

# Function to add AST imports if missing critical types
add_ast_imports() {
    local file=$1
    # Check if file uses Expr, Type, Statement but doesn't have ast imports
    if grep -E "Expr|Type|Statement" "$file" | head -1; then
        if ! grep -q "^use crate::ast::" "$file"; then
            echo "Adding AST imports to $file"
            sed -i '1a use crate::ast::{Expr, Type, Statement, Block, Decl, FuncDecl};' "$file"
        fi
    fi
}

# Process riscv64 first (highest priority based on errors)
echo ""
echo "=== Processing riscv64 ==="
for file in src/codegen/riscv64/codegen/*.rs; do
    if [ -f "$file" ]; then
        echo "Checking $file"
        add_hashmap_import "$file"
        add_ast_imports "$file"
    fi
done

# Process aarch64
echo ""
echo "=== Processing aarch64 ==="
for file in src/codegen/aarch64/codegen/*.rs; do
    if [ -f "$file" ]; then
        echo "Checking $file"
        add_hashmap_import "$file"
        add_ast_imports "$file"
    fi
done

# Process mips
echo ""
echo "=== Processing mips ==="
for file in src/codegen/mips/baseline_mips.rs src/codegen/mips/mips64/*.rs 2>/dev/null; do
    if [ -f "$file" ]; then
        echo "Checking $file"
        add_hashmap_import "$file"
        add_ast_imports "$file"
    fi
done

echo ""
echo "=== Import fixes complete ==="
echo "Running cargo build to check progress..."
cargo build 2>&1 | grep "^error" | wc -l
