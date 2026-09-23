#!/bin/bash
# Cleanup dead code - Archive backup files and baseline files
# Created: September 22, 2026

set -e

echo "=== DCC Backend Dead Code Cleanup ==="

ARCHIVE_DIR="/root/dcc/.archive/$(date +%Y%m%d_%H%M%S)_cleanup"
mkdir -p "$ARCHIVE_DIR"

echo "Archive directory: $ARCHIVE_DIR"
echo ""

# Step 1: Archive ARM files
echo "Step 1: Archiving ARM backend dead code..."
if [ -f "src/codegen/arm/baseline_arm.rs" ]; then
    mkdir -p "$ARCHIVE_DIR/arm"
    mv src/codegen/arm/baseline_arm.rs "$ARCHIVE_DIR/arm/"
    echo "  ✓ Archived: baseline_arm.rs ($(du -sh $ARCHIVE_DIR/arm/ | cut -f1))"
fi

# Move all backup directories
for dir in src/codegen/*/backup; do
    if [ -d "$dir" ] && [ "$(ls -A $dir)" ]; then
        mkdir -p "$ARCHIVE_DIR/$(basename $(dirname $dir))/backup"
        mv "$dir"/* "$ARCHIVE_DIR/$(basename $(dirname $dir))/backup/" 2>/dev/null || true
        rmdir "$dir" 2>/dev/null || true
        echo "  ✓ Cleaned up: $(dirname $dir)/backup/"
    fi
done

# Step 2: Clean empty directories if any
echo ""
echo "Step 2: Removing empty directories..."
find src/codegen -type d -empty -delete 2>/dev/null || true

# Step 3: Report cleanup statistics
echo ""
echo "=== Cleanup Complete ==="
echo "Archived files to: $ARCHIVE_DIR"
echo ""

if [ -d "$ARCHIVE_DIR" ]; then
    archived_size=$(du -sh "$ARCHIVE_DIR" | cut -f1)
    echo "Total archived size: $archived_size"
    
    # Count archived files
    file_count=$(find "$ARCHIVE_DIR" -type f | wc -l)
    echo "Files archived: $file_count"
fi

echo ""
echo "✅ Workspace cleaned successfully!"
echo "Backup preserved in: $ARCHIVE_DIR"
echo ""
