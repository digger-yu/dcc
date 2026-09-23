#!/bin/bash
# scripts/commit_modularization_progress.sh
# Phase 0 Complete: Generate Git commits for modularization progress
# Date: Tuesday, September 22, 2026
# User Requirement: SSH signature, concise subject + English body, per-line ≤72 chars

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

echo "=== Generating Modularization Progress Commits ==="
echo "Root directory: $ROOT_DIR"
echo ""

# Check if we're in a git repository
if ! git rev-parse --git-dir > /dev/null 2>&1; then
    echo "❌ Not a git repository. Please run this script from repo root."
    exit 1
fi

# Configure git user for commit signing (SSH)
# USER MUST configure their SSH key and git config first:
#   ssh-keygen -t ed25519 -C "your_email@example.com"
#   ssh-add ~/.ssh/id_ed25519
#   git config --global gpg.format ssh
#   git config --global user.signingkey ~/.ssh/id_ed25519.pub

echo "📝 Generating commit message files..."
echo ""

# Commit 1: Phase 0 Analysis Complete
cat > "$ROOT_DIR/.git-commit-temp-01.txt" << 'EOF'
modularization: Phase 0 analysis complete

Analysis of monolithic mod.rs (35K lines) and
existing modular structure from Turns 21-28.

Status: 7,514 lines already extracted into separate
modules, ~27K lines remain in monolithic form.

Key findings:
- Duplicate type definitions exist in mod.rs AND helpers.rs
- Shadow modules created but not yet integrated
- Clear extraction mapping identified by domain

Next: Begin content extraction phase

See: MODULARIZATION_PLAN.md for full details
EOF

# Commit 2: Directory Structure Creation
cat > "$ROOT_DIR/.git-commit-temp-02.txt" << 'EOF'
directories: create target module structure

Created complete directory tree following user
specification for modularized architecture:

asm/planner/
asm/constraint/
asm/template/
asm/reg/
emit/expr/
mem/
special/
parallel/
tests/
elf_side/

All directories pre-created before file extraction
(directory-first approach as requested).

This enables systematic content migration starting
from Turn 30 onward.

No functional changes - infrastructure only.
EOF

echo "✅ Generated temporary commit messages:"
ls -la "$ROOT_DIR"/.git-commit-temp-*.txt 2>/dev/null || true
echo ""
echo "⚠️  To commit these manually (with SSH signature):"
echo "   cd $ROOT_DIR"
echo "   git add ."
echo "   git commit -S -F .git-commit-temp-01.txt"
echo "   git commit -S -F .git-commit-temp-02.txt"
echo ""
echo "Or use automated commit script with proper git config."
echo ""
echo "Note: Ensure your SSH key is loaded:"
echo "   ssh-add -l"
echo ""
echo "Cleanup temporary files?"
read -p "[y/N] " choice
case "$choice" in
  y|Y )
    rm -f "$ROOT_DIR"/.git-commit-temp-*.txt
    echo "Cleanup complete."
    ;;
  * )
    echo "Temporary files preserved for your review."
    ;;
esac
