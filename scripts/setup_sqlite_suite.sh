#!/bin/bash
# ===========================================================================
# scripts/setup_sqlite_suite.sh — 准备 SQLite 官方 TCL 测试套件源码树
#
# 和 scripts/setup_torture.sh 同一角色：本地与 CI 走同一个入口，避免两边拿到
# 的套件不一样。输出是**完整源码树**（含 test/*.test 与 configure），不是
# amalgamation —— amalgamation 只有 shell.c / sqlite3.c，没有测试。
#
# 默认落在 $HOME 下，因为 CI runner 的用户没有 /root 写权限。
#
# 产物: $SQLITE_SRC（默认 $HOME/sqlite-src-3540000）
# 退出: 0 = 套件就绪；非 0 = 拿不到（缺网、zip 不对、缺 test/ 目录）
#
# 用法:
#   bash scripts/setup_sqlite_suite.sh
#   SQLITE_SRC=/tmp/sq SQLITE_ZIP=/path/sqlite-src-3540000.zip \
#     bash scripts/setup_sqlite_suite.sh
# ===========================================================================
set -o pipefail

SQLITE_SRC=${SQLITE_SRC:-$HOME/sqlite-src-3540000}
SQLITE_ZIP=${SQLITE_ZIP:-$HOME/sqlite-src-3540000.zip}
SQLITE_URL=${SQLITE_URL:-}
# 官方下载目录按年份分，同版本的 zip 可能挂在当年或上一年
SQLITE_URL_CANDIDATES=${SQLITE_URL_CANDIDATES:-"https://sqlite.org/2026/sqlite-src-3540000.zip
https://sqlite.org/2025/sqlite-src-3540000.zip
https://sqlite.org/2024/sqlite-src-3540000.zip"}

say() { echo "[setup-sqlite] $*"; }

# 已经就绪就直接返回：判据是 configure + test/ 下的 .test 文件，而不是目录存在
if [ -f "$SQLITE_SRC/configure" ] && [ -d "$SQLITE_SRC/test" ]; then
  n=$(find "$SQLITE_SRC/test" -maxdepth 1 -name '*.test' | wc -l)
  if [ "$n" -gt 0 ]; then
    say "already present: $SQLITE_SRC ($n .test files)"
    exit 0
  fi
fi

if [ ! -f "$SQLITE_ZIP" ]; then
  if ! command -v curl >/dev/null 2>&1; then
    say "ERROR: curl missing and no local zip at $SQLITE_ZIP"
    exit 1
  fi
  got=0
  for url in ${SQLITE_URL:-$SQLITE_URL_CANDIDATES}; do
    say "downloading: $url"
    if curl -fL --retry 2 --connect-timeout 30 --max-time 900 \
      -o "$SQLITE_ZIP.part" "$url"; then
      mv "$SQLITE_ZIP.part" "$SQLITE_ZIP"
      got=1
      break
    fi
    rm -f "$SQLITE_ZIP.part"
  done
  if [ "$got" -eq 0 ]; then
    say "ERROR: all download URLs failed"
    say "  hint: pass SQLITE_ZIP=/path/to/sqlite-src-*.zip to use a local copy"
    exit 1
  fi
fi

case "$SQLITE_ZIP" in
  *.zip)
    command -v unzip >/dev/null 2>&1 || {
      say "ERROR: unzip missing"
      exit 1
    }
    say "unpacking $(basename "$SQLITE_ZIP")"
    rm -rf "$SQLITE_SRC"
    tmp=$(mktemp -d)
    unzip -q "$SQLITE_ZIP" -d "$tmp" || {
      say "ERROR: unzip failed"
      rm -rf "$tmp"
      exit 1
    }
    inner=$(find "$tmp" -maxdepth 1 -mindepth 1 -type d | head -1)
    if [ -z "$inner" ]; then
      say "ERROR: zip has no top-level directory"
      rm -rf "$tmp"
      exit 1
    fi
    mv "$inner" "$SQLITE_SRC"
    rm -rf "$tmp"
    ;;
  *)
    say "ERROR: unsupported archive (only .zip): $SQLITE_ZIP"
    exit 1
    ;;
esac

if [ ! -f "$SQLITE_SRC/configure" ] || [ ! -d "$SQLITE_SRC/test" ]; then
  say "ERROR: unpacked tree lacks configure or test/"
  exit 1
fi
n=$(find "$SQLITE_SRC/test" -maxdepth 1 -name '*.test' | wc -l)
say "ready: $SQLITE_SRC ($n .test files)"
