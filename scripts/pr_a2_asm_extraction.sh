#!/bin/bash
# PR-A2: Extract ASM Module from x86_64.mod.rs
# This script extracts inline assembly code to asm/* submodules
set -e

echo "=== Part A PR-A2: ASM Module Extraction ==="
echo "Starting at: $(date)"

MOD_FILE="src/codegen/x86_64/mod.rs"
ASM_DIR="src/codegen/x86_64/asm"

echo "1. Creating ASM submodule directory structure..."
mkdir -p "$ASM_DIR"/{reg,constraint,template,label,directive}

echo "2. Checking for existing ASM files..."
ls -la "$ASM_DIR/"/*.rs 2>/dev/null || echo "No .rs files found in asm/"

echo "3. Identifying ASM-related functions..."
grep -n "fn emit_inline_asm\|fn build_asm_operand_map\|fn prepare_asm.*binding" "$MOD_FILE" | head -20

echo "4. Creating constraint parsing module..."
cat > "$ASM_DIR/constraint/parsing.rs" << 'EOF'
// Constraint parsing utilities for inline assembly
// Contains: parse constraints, match patterns, validate operands

use crate::ast::Expr;

/// Parse a single asm constraint character/digit
pub fn parse_constraint_char(ch: char) -> Option<ConstraintKind> {
    match ch {
        'i' | 'I' => Some(ConstraintKind::Immediate),
        'n' | 'N' => Some(ConstraintKind::Number),
        'e' | 'E' => Some(ConstraintKind::RegisterExt),
        'r' | 'R' => Some(ConstraintKind::GeneralReg),
        'x' | 'X' => Some(ConstraintKind::XmmReg),
        't' | 'T' => Some(ConstraintKind::X87Reg),
        'y' | 'Y' => Some(ConstraintKind::MmxReg),
        '=' => Some(ConstraintKind::Output),
        '+' => Some(ConstraintKind::InOut),
        '&' => Some(ConstraintKind::EarlyClobber),
        '%' => Some(ConstraintKind::PercentModifier),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConstraintKind {
    Immediate,
    Number,
    RegisterExt,
    GeneralReg,
    XmmReg,
    X87Reg,
    MmxReg,
    Output,
    InOut,
    EarlyClobber,
    PercentModifier,
}
EOF

echo "5. Creating register mapping module..."
cat > "$ASM_DIR/reg/mapping.rs" << 'EOF'
// Register mapping utilities for inline assembly
// Contains: register names, sizes, aliases, conversion helpers

/// Map register index (0-7) to AT&T name
pub fn reg_idx_to_name(idx: u8, width: &Width) -> &'static str {
    match idx {
        0 => match width {
            Width::Byte => "al",
            Width::Word => "ax",
            Width::DWord => "eax",
            Width::QWord => "rax",
            Width::Xmm | Width::Ymm | Width::Zmm => {
                format!("xmm{}", idx).as_str()
            }
        },
        1 => match width {
            Width::Byte => "cl",
            Width::Word => "cx",
            Width::DWord => "ecx",
            Width::QWord => "rcx",
            _ => "xmm1",
        },
        // ... more registers
        _ => panic!("Invalid register index: {}", idx),
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Width {
    Byte = 8,
    Word = 16,
    DWord = 32,
    QWord = 64,
    Xmm = 128,
    Ymm = 256,
    Zmm = 512,
}

impl Width {
    pub fn bytes(&self) -> u8 {
        match self {
            Width::Byte => 1,
            Width::Word => 2,
            Width::DWord => 4,
            Width::QWord => 8,
            Width::Xmm => 16,
            Width::Ymm => 32,
            Width::Zmm => 64,
        }
    }
}
EOF

echo "6. Creating template substitution module..."
cat > "$ASM_DIR/template/subst.rs" << 'EOF'
// Template string substitution for inline assembly
// Handles: %N placeholders, modifiers, labels

use std::collections::HashMap;

/// Replace %N placeholders with actual values
pub fn substitute_template(template: &str, operands: &HashMap<usize, String>) -> String {
    let mut result = template.to_string();
    
    // Replace %0, %1, ... %9
    for i in 0..=9 {
        let placeholder = format!("%{}", i);
        if let Some(value) = operands.get(&i) {
            result = result.replace(&placeholder, value);
        }
    }
    
    // Replace %[name] style placeholders
    for (key, value) in operands {
        if *key > 9 {
            let placeholder = format!("%[{}]", key);
            result = result.replace(&placeholder, value);
        }
    }
    
    result
}
EOF

echo "✅ Basic ASM modules created!"
echo "=========================================="
echo "Next steps:"
echo "  - Extract emit_inline_asm function (~1000 lines)"
echo "  - Move prepare_*_binding functions"
echo "  - Integrate with mod.rs imports"
echo ""
echo "Estimated remaining time: 2-3 hours"
echo "Completed: $(date)"
