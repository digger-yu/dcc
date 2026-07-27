#!/bin/bash
# DCC kernel build wrapper
set -e
cd /home/di/linux-7.1
export PATH="/tmp/flex-extract/usr/bin:/tmp/bison-extract/usr/bin:/tmp/m4-extract/usr/bin:/home/di/dcc/target/release:/usr/bin:/bin:$PATH"
export C_INCLUDE_PATH="/tmp/libssl-extract/usr/include/x86_64-linux-gnu:/tmp/libssl-extract/usr/include:/tmp/libelf-extract/usr/include:${C_INCLUDE_PATH:-}"
# 排除 /tmp/libelf-extract 的 pkgconfig，让 kernel 的 libelf 链接走系统的 libelf.so.1
# (libelf.a 需要 -lz -lzstd，否则 ld 会失败)
case ":${PKG_CONFIG_PATH:-}:" in
    *:/tmp/libelf-extract/usr/lib/x86_64-linux-gnu/pkgconfig:*) ;;
    *) export PKG_CONFIG_PATH="/tmp/libelf-extract/usr/lib/x86_64-linux-gnu/pkgconfig:${PKG_CONFIG_PATH:-}" ;;
esac
export PKG_CONFIG_PATH=$(echo "$PKG_CONFIG_PATH" | tr ':' '\n' | grep -v '/tmp/libelf-extract' | paste -sd:)
export LIBRARY_PATH="/tmp/lib-shim:/tmp/libssl-extract/usr/lib/x86_64-linux-gnu:${LIBRARY_PATH:-}"
export LD_LIBRARY_PATH="/tmp/lib-shim:/tmp/libssl-extract/usr/lib/x86_64-linux-gnu:/usr/lib/x86_64-linux-gnu:${LD_LIBRARY_PATH:-}"
export DCC_REPO=/home/di/dcc
export DCC_BIN=/home/di/dcc/target/release/dcc
export KCFLAGS=-std=gnu11

exec make -j4 \
    HOSTCC=gcc HOSTCXX=gcc \
    CC=/home/di/dcc/scripts/dcc-kernel-cc.sh \
    "$@"
