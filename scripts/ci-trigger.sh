#!/usr/bin/env bash
# scripts/ci-trigger.sh
# 第一性原理: 不浪费时间手敲命令
# suckless: 5 行执行, 错误立刻退出 (set -e)

set -euo pipefail

cd "$(dirname "$0")/.."

REMOTE="${1:-origin}"
BRANCH="$(git rev-parse --abbrev-ref HEAD)"

echo "==> dcc CI push helper"
echo "    repo:    $(git remote -v | grep fetch | awk '{print $2}')"
echo "    branch:  $BRANCH"
echo "    commit:  $(git log -1 --oneline)"
echo ""

# 防御: 主分支直推,但不强制 --force
if [ "$BRANCH" = "main" ] || [ "$BRANCH" = "develop" ]; then
  echo "==> pushing $BRANCH to $REMOTE (this will trigger CI)"
  git push "$REMOTE" "$BRANCH"
else
  echo "==> not on main/develop, push current branch"
  git push -u "$REMOTE" "$BRANCH"
fi

echo ""
echo "==> done. CI URL:"
echo "    https://github.com/digger-yu/dcc/actions"
echo ""
echo "    (e2e workflow runs in ~5 min, full kernel bench in ~60 min)"
