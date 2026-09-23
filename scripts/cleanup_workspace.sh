#!/bin/bash
# Phase 4 Cleanup - Remove temporary documentation and organize workspace
# Created: September 22, 2026

set -e

echo "=== DCC Workspace Cleanup ==="
echo "Cleaning up redundant files and organizing structure..."
echo ""

ARCHIVE_DIR="/root/dcc/.archive_phase_c_phase4_$(date +%Y%m%d_%H%M%S)"
mkdir -p "$ARCHIVE_DIR"

echo "Creating archive directory: $ARCHIVE_DIR"
echo ""

# Step 1: Archive all the intermediate progress reports (keep only final ones)
echo "Step 1: Archiving intermediate progress reports..."
INTERMEDIATE_REPORTS=(
    "phase2_progress_report.md"
    "PHASE_C_EXECUTIVE_SUMMARY.md"
    "session_d_parallel_execution_summary.md"
    "phase_c_complete_summary.md"
    "phase_c_final_migration_complete.md"
    "phase3_expression_backend_complete.md"
    "phase3_load_operand_complete.md"
    "phase3_optimization_plan.md"
    "phase4_optimization_plan.md"
    "phase4_quick_status.md"
    "phase4_optimization_progress_report_1.md"
    "DELIVERABLES_LIST.md"
    "PAGE_PHASE4_COMPLETION_REPORT.md"
    "PAGE_PHASE4_IMPLEMENTATION_SUMMARY.md"
    "PURE_README.md"
    "CODEGEN_STRUCTURE_CLARIFICATION.md"
    "MODULARIZATION_PLAN.md"
    "PATHA_TURNS_36_37_SUMMARY.md"
    "PB_WK2D1_EXTRACTION_SUMMARY.md"
    "PHASE1B_MIGRATION_ATTEMPT.md"
    "PHASE1_MIGRATION_EXPERIMENT.md"
    "PHASE_A_COMPLETE_SUMMARY.md"
    "PHASE_A_DAY1_TO_DAY4_SUMMARY.md"
    "PHASE_A_DAY3_REGISTERS_MODULE.md"
    "PHASE_A_FINAL_STATUS.md"
    "PHASE_A_FINAL_VERIFICATION.md"
    "PHASE_A_PLUS_TEMPLATES_COMPLETE.md"
    "PHASE_A_WEEK2_3_COMPLETION_SUMMARY.md"
    "PHASE_A_WEEK2_3_VERIFICATION.md"
    "PHASE_B_COMPLETION_STATUS.md"
    "PHASE_B_EXECUTION_SUMMARY.md"
    "PHASE_B_PROGRESS_DAY1.md"
    "PHASE_B_PROGRESS_REPORT.md"
    "PROGRESS_REPORT_TURN30.md"
)

for report in "${INTERMEDIATE_REPORTS[@]}"; do
    if [ -f "$report" ]; then
        cp "$report" "$ARCHIVE_DIR/"
        rm "$report"
        echo "  ✓ Archived: $report"
    fi
done

# Step 2: Keep only the essential summary files at root level
KEEP_FILES=(
    "README.md"
    "FINAL_COMPLETION_SUMMARY.md"
    "PHASE_C_ABSOLUTE_COMPLETE_ALL_ISAs.md"
    "PHASE_C_COMPLETE_EXECUTION_REPORT.md"
    "PHASE_C_COMPLETE_STATUS.md"
    "PHASE_C_FINAL_FINAL_SUMMARY.md"
    "FINAL_PHASE4_STATUS.md"
)

echo ""
echo "Step 2: Final report files are already kept at root level."
echo ""

# Step 3: Clean backup files
echo "Step 3: Cleaning backup files..."
if [ -f "src/codegen/riscv64.rs.bak" ]; then
    mv src/codegen/riscv64.rs.bak "$ARCHIVE_DIR/"
    echo "  ✓ Moved: riscv64.rs.bak -> $ARCHIVE_DIR/"
fi

# Step 4: Check for other backup/old files
echo ""
echo "Step 4: Looking for other backup files..."
find . -name "*.bak" -o -name "*.orig" | while read file; do
    dir=$(dirname "$file")
    mkdir -p "$ARCHIVE_DIR/$dir"
    mv "$file" "$ARCHIVE_DIR/$file"
    echo "  ✓ Moved: $file -> $ARCHIVE_DIR/$file"
done

# Step 5: Report stats
echo ""
echo "=== Cleanup Complete ==="
echo ""
echo "Archive created at: $ARCHIVE_DIR"
echo "Files archived: $(ls $ARCHIVE_DIR/*.md 2>/dev/null | wc -l | tr -d ' ')"
echo "Root-level docs remaining:"
ls *.md 2>/dev/null | wc -l | tr -d ' '
echo " documentation files (kept minimal)"
echo ""
echo "✅ Workspace cleaned successfully!"
echo ""
