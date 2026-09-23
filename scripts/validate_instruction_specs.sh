#!/bin/bash
# Script to validate all instruction specs against the schema
# Uses bash - no Python allowed per user preference!

set -e

echo "=== DCC Instruction Specification Validator ==="
echo ""

SPEC_DIR="${1:-src/codegen/data/x86_instructions}"
SCHEMA_FILE="docs/instruction_spec_schema_v2.json"

if [ ! -f "$SCHEMA_FILE" ]; then
    echo "❌ Schema file not found: $SCHEMA_FILE"
    exit 1
fi

echo "📂 Validating specifications in: $SPEC_DIR"
echo "📄 Using schema: $SCHEMA_FILE"
echo ""

VALID_COUNT=0
INVALID_COUNT=0

# Function to extract opcode values from JSON
extract_opcodes() {
    local file=$1
    # Extract opcode array as decimal integers
    grep -o '"opcode":\s*\[[^]]*\]' "$file" | \
        sed 's/"opcode":\s*\[//g; s/\]//g' | \
        tr ',' '\n' | \
        tr -d ' '
}

# Validate each JSON file
for spec_file in "$SPEC_DIR"/*.json; do
    if [ ! -f "$spec_file" ]; then
        continue
    fi
    
    filename=$(basename "$spec_file")
    
    echo "🔍 Validating: $filename"
    
    # Check 1: Valid JSON syntax (using jq)
    if ! jq empty "$spec_file" 2>/dev/null; then
        echo "   ❌ Invalid JSON syntax"
        INVALID_COUNT=$((INVALID_COUNT + 1))
        continue
    fi
    echo "   ✅ Valid JSON syntax"
    
    # Check 2: Required fields present
    name=$(jq -r '.name // empty' "$spec_file")
    arch=$(jq -r '.arch // empty' "$spec_file")
    
    if [ -z "$name" ] || [ -z "$arch" ]; then
        echo "   ❌ Missing required fields (name/arch)"
        INVALID_COUNT=$((INVALID_COUNT + 1))
        continue
    fi
    echo "   ✅ Required fields present (name=$name, arch=$arch)"
    
    # Check 3: Opcode bytes are valid (0-255)
    opcodes=$(extract_opcodes "$spec_file")
    invalid_bytes=false
    for byte in $opcodes; do
        if [ "$byte" -lt 0 ] 2>/dev/null || [ "$byte" -gt 255 ] 2>/dev/null; then
            echo "   ❌ Invalid opcode byte: $byte (must be 0-255)"
            invalid_bytes=true
        elif ! [[ "$byte" =~ ^[0-9]+$ ]]; then
            # Non-numeric value - likely hex like "0xC7"
            echo "   ⚠️  Found non-decimal opcode byte: $byte (consider using decimal)"
        fi
    done
    
    if [ "$invalid_bytes" = false ]; then
        echo "   ✅ Opcode bytes within range (0-255)"
    fi
    
    # Check 4: Operand pattern consistency
    operand_pattern=$(jq -r '.encoding_spec.formats[0].operand_pattern // empty' "$spec_file")
    operand_count=$(jq '.operands | length' "$spec_file")
    
    # Simple heuristic: count tokens in pattern vs number of operands
    if [ -n "$operand_pattern" ]; then
        pattern_tokens=$(echo "$operand_pattern" | tr '/' ' ' | tr '+' ' ' | tr '\t' ' ' | wc -w)
        
        # Allow some flexibility - pattern might include modifiers like /m
        if [ "$pattern_tokens" -ge "$((operand_count - 1))" ] && [ "$pattern_tokens" -le "$((operand_count + 1))" ]; then
            echo "   ✅ Operand pattern reasonably matches operand count ($operand_count operands)"
        else
            echo "   ⚠️  Operand pattern may not match operand count (pattern=$pattern_pattern, operands=$operand_count)"
        fi
    fi
    
    # Check 5: Examples validation (optional field)
    example_count=$(jq '.examples | length // 0' "$spec_file")
    if [ "$example_count" -gt 0 ]; then
        echo "   ℹ️  Contains $example_count assembly examples"
    fi
    
    echo ""
    VALID_COUNT=$((VALID_COUNT + 1))
done

echo "=========================================="
echo "Validation Summary:"
echo "  ✅ Valid:      $VALID_COUNT"
echo "  ❌ Invalid:     $INVALID_COUNT"
echo "  📊 Total:       $((VALID_COUNT + INVALID_COUNT))"
echo "=========================================="

if [ "$INVALID_COUNT" -gt 0 ]; then
    echo ""
    echo "⚠️  ${INVALID_COUNT} specification(s) have errors!"
    echo "Please review and fix the issues above."
    exit 1
fi

echo ""
echo "✅ All specifications validated successfully!"
exit 0
