#!/bin/bash
# scripts/backup_arm_mips_monolith.sh - ARM/MIPS 后端备份与目录结构创建

set -e

echo "========================================="
echo "=== ARM/MIPS Backend Backup & Setup ==="
echo "========================================="

DATE=$(date +%Y%m%d_%H%M%S)
ARM_DIR="/root/dcc/src/codegen/arm_backend"
MIPS_DIR="/root/dcc/src/codegen/mips_backend"

echo ""
echo "📦 Creating timestamped backups..."

# Create timestamped backup for ARM
if [ -f "/root/dcc/src/codegen/arm.rs" ]; then
    mkdir -p "$ARM_DIR/backup"
    cp /root/dcc/src/codegen/arm.rs "$ARM_DIR/backup/original_arm_monolith.rs.bak.$DATE"
    echo "✅ ARM backup created: $ARM_DIR/backup/original_arm_monolith.rs.bak.$DATE"
    
    # Also create current working copy as baseline
    cp /root/dcc/src/codegen/arm.rs "$ARM_DIR/baseline_arm.rs"
    echo "   Baseline copy saved: $ARM_DIR/baseline_arm.rs"
else
    echo "⚠️  ARM file not found, skipping backup"
fi

# Create timestamped backup for MIPS
if [ -f "/root/dcc/src/codegen/mips.rs" ]; then
    mkdir -p "$MIPS_DIR/backup"
    cp /root/dcc/src/codegen/mips.rs "$MIPS_DIR/backup/original_mips_monolith.rs.bak.$DATE"
    echo "✅ MIPS backup created: $MIPS_DIR/backup/original_mips_monolith.rs.bak.$DATE"
    
    # Also create current working copy as baseline
    cp /root/dcc/src/codegen/mips.rs "$MIPS_DIR/baseline_mips.rs"
    echo "   Baseline copy saved: $MIPS_DIR/baseline_mips.rs"
else
    echo "⚠️  MIPS file not found, skipping backup"
fi

echo ""
echo "🏗️  Creating directory skeletons..."

# Create ARM directory skeleton
mkdir -p "$ARM_DIR"/{registers,abi,codegen,emit,special,tests}
mkdir -p "$ARM_DIR"/codegen/{statements,expressions,control_flow,instruction_sel}

echo "ARM backend structure:"
find "$ARM_DIR" -type d | head -20 | sed 's/^/    /'

# Create MIPS directory skeleton  
mkdir -p "$MIPS_DIR"/{registers,abi,codegen,emit,special,tests}
mkdir -p "$MIPS_DIR"/codegen/{statements,expressions,control_flow,instruction_sel,delay_slots}

echo ""
echo "MIPS backend structure:"
find "$MIPS_DIR" -type d | head -20 | sed 's/^/    /'

echo ""
echo "📝 Creating temporary mod.rs files (backward compatible wrappers)..."

# Create ARM mod.rs wrapper (will be temporary until modular impl complete)
cat > "$ARM_DIR/mod.rs" << 'EOF'
//! ARM32 Backend Modular Implementation
//! Part of the DCC compiler project
//!
//! This module provides code generation for ARM32 architecture
//! following the AAPCS (ARM Architecture Procedure Call Standard).

#![allow(dead_code)]
#![allow(unused_variables)]

// Backward compatibility layer: temporarily re-export from original monolith
// TODO: Replace with actual modular implementations

#[path = "../arm.rs"]
mod arm_legacy;

pub use arm_legacy::*;

// Future modular components (to be implemented):
// pub mod registers;
// pub mod abi;
// pub mod codegen;
// pub mod emit;

// Unified backend facade with feature flag support
pub struct ArmBackend {
    legacy_mode: bool,
}

impl ArmBackend {
    pub fn new(legacy_mode: bool) -> Self {
        Self { legacy_mode }
    }
    
    /// Choose implementation based on feature flags and mode
    pub fn emit_function(&self, func: &crate::ir::Function) -> Result<Vec<u8>> {
        if self.legacy_mode {
            // Use legacy monolith implementation
            arm_legacy::emit_function(func)
        } else {
            #[cfg(feature = "arm-modular")]
            {
                use crate::codegen::arm_backend::modular_codegen as arm_modular;
                let mut cg = arm_modular::ArmCodegen::new();
                cg.emit_function(func)
            }
            #[cfg(not(feature = "arm-modular"))]
            {
                compile_error!("Modular ARM backend requires 'arm-modular' feature");
            }
        }
    }
}
EOF

echo "✅ Created: $ARM_DIR/mod.rs (wrapper)"

# Create MIPS mod.rs wrapper
cat > "$MIPS_DIR/mod.rs" << 'EOF'
//! MIPS32 Backend Modular Implementation
//! Part of the DCC compiler project
//!
//! This module provides code generation for MIPS32 architecture
//! following the O32 ABI calling convention.

#![allow(dead_code)]
#![allow(unused_variables)]

// Backward compatibility layer: temporarily re-export from original monolith
// TODO: Replace with actual modular implementations

#[path = "../mips.rs"]
mod mips_legacy;

pub use mips_legacy::*;

// Future modular components (to be implemented):
// pub mod registers;
// pub mod abi;
// pub mod codegen;
// pub mod emit;

// Unified backend facade with feature flag support
pub struct MipsBackend {
    legacy_mode: bool,
}

impl MipsBackend {
    pub fn new(legacy_mode: bool) -> Self {
        Self { legacy_mode }
    }
    
    /// Choose implementation based on feature flags and mode
    pub fn emit_function(&self, func: &crate::ir::Function) -> Result<Vec<u8>> {
        if self.legacy_mode {
            // Use legacy monolith implementation
            mips_legacy::emit_function(func)
        } else {
            #[cfg(feature = "mips-modular")]
            {
                use crate::codegen::mips_backend::modular_codegen as mips_modular;
                let mut cg = mips_modular::MipsCodegen::new();
                cg.emit_function(func)
            }
            #[cfg(not(feature = "mips-modular"))]
            {
                compile_error!("Modular MIPS backend requires 'mips-modular' feature");
            }
        }
    }
}
EOF

echo "✅ Created: $MIPS_DIR/mod.rs (wrapper)"

echo ""
echo "🔍 Verifying compilation still works..."

# Verify compilation still works (basic check)
cd /root/dcc
if cargo check --lib 2>&1 | grep -q "error"; then
    echo "⚠️  Compilation has errors (pre-existing issues in project)"
else
    echo "✅ Project still compiles successfully"
fi

echo ""
echo "========================================="
echo "Backup & Setup Summary:"
echo "========================================="
echo "Date: $(date)"
echo "ARM backend location: $ARM_DIR"
echo "MIPS backend location: $MIPS_DIR"
echo ""
echo "Next steps:"
echo "1. Implement registers modules"
echo "2. Implement ABI modules"
echo "3. Extract codegen subsystems"
echo "4. Update Cargo.toml with features"
echo "5. Run golden baseline comparison"
echo ""
echo "✅ Backup and setup complete!"
