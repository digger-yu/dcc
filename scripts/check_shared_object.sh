#!/bin/bash
# ===========================================================================
# scripts/check_shared_object.sh — dcc `-shared` 产物：ELF 结构断言 + 真实加载
#
# 为什么单列一个脚本：docs/dcc-lua-sqlite-build-verify-report.md §3.4 对五个
# `libs/*.so` 的描述是**叙述性表格**，不是可复跑的门禁。本脚本把它变成判据：
#
#   A. 结构：每个 libN.so 由 dcc 自己编（`-fPIC -shared`，走宿主 ld，不得回退
#      host cc —— 构建日志里必须出现 "via host ld"），然后断言
#        * ET_DYN、无 PT_INTERP；
#        * `.dynsym` 里 T 类 `luaopen_*` 有且仅有一个，且名字符合预期；
#        * U 类 `lua_*` 条数符合预期（未定义符号是**设计的一部分**，由主程序
#          的 `-Wl,-E` 在加载期提供）；
#        * `.dynamic` 有 DT_GNU_HASH、无 TEXTREL；
#        * `-Wl,-soname` 写进了 DT_SONAME；DT_NEEDED 含 libc.so.6。
#   B. 加载：五个模块全部真的 `dlopen` + `dlsym`：
#        * 最小 C 宿主：RTLD_LAZY + dlsym 入口（lib2/lib21 的 U 符号在调用前
#          不需要解析，所以这一步对所有模块都应成立）；
#        * Lua 宿主（-Wl,-E 构建）：调用入口并检查行为（lib11 返回 "exported"，
#          lib2 设置全局 x/y，lib1 的 table.id 返回栈深…）。lib21 依赖 lib2 的
#          `luaopen_lib2`，用 LD_PRELOAD 把 lib2 变成全局可见后再加载 —— 这是
#          上游套件不加载它、从而留下空白的那个模块。
#   C. 组合边界：`-flto -shared` 与 `-shared --whole-archive` 走同一套断言。
#      whole-archive 另加**语义对照**：两个成员的归档里，未被引用的成员也必须
#      进入 `.dynsym`（不包 whole-archive 时则不得进入），否则 flag 等于被吞。
#   D. musl：探测 musl 工具链；本机没有时明确记 SKIP 与原因，不算 PASS。
#
# 前置：
#   * dcc 已构建（默认 $REPO/target/release/dcc）；
#   * Lua 源码/测试树（默认 ~/lua-5.5.1、~/lua-5.5.1-tests）——只需要
#     `libs/lib*.c` 与一个导出 `lua_*` 的宿主解释器；
#   * readelf/nm/ar/gcc（宿主工具，只用于校验与最小宿主，不参与 .so 构建）。
#
# 用法：
#   bash scripts/check_shared_object.sh
#   DCC=target/debug/dcc bash scripts/check_shared_object.sh
#   LUA=$HOME/lua-5.5.1 LIBS=$HOME/lua-5.5.1-tests/libs ...
#   KEEP=1 ...    # 保留 WORK 目录
#
# 退出：0 = 全部断言通过；1 = 有失败；2 = 前置不满足。
# ===========================================================================
set -u -o pipefail
LC_ALL=C
export LC_ALL

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO=$(cd "$HERE/.." && pwd)
DCC=${DCC:-$REPO/target/release/dcc}
LUA=${LUA:-$HOME/lua-5.5.1}
LIBS=${LIBS:-$HOME/lua-5.5.1-tests/libs}
LUA_HOST=${LUA_HOST:-$LUA/src/lua}
WORK=${WORK:-/tmp/dcc_shared_check}
SHARED_LEVEL=${SHARED_LEVEL:-2}
KEEP=${KEEP:-0}

fails=0
skips=0
SUMMARY=$WORK/summary.txt

say() { echo "$@" | tee -a "$SUMMARY"; }
die() { echo "ERROR: $*" >&2; exit 2; }

[ -x "$DCC" ] || die "找不到 dcc：$DCC（先 cargo build）"
for c in nm readelf ar gcc timeout; do
    command -v "$c" >/dev/null || die "缺工具：$c"
done
[ -d "$LIBS" ] || die "缺 libs 目录：$LIBS"
for f in lib1.c lib11.c lib2.c lib21.c lib22.c; do
    [ -f "$LIBS/$f" ] || die "缺 $LIBS/$f（Lua 官方测试树）"
done
[ -x "$LUA_HOST" ] || die "缺 Lua 宿主：$LUA_HOST（先构建 Lua）"
LUA_EXPORTS=$(nm -D "$LUA_HOST" 2>/dev/null | awk '$2=="T" && $3 ~ /^lua_/{c++} END{print c+0}')
[ "${LUA_EXPORTS:-0}" -ge 90 ] || die "Lua 宿主只导出 $LUA_EXPORTS 个 T lua_*（<90）；必须用 -Wl,-E 构建，否则 .so 无法解析"

rm -rf "$WORK"
mkdir -p "$WORK/so" "$WORK/lto" "$WORK/wa" "$WORK/log"
: >"$SUMMARY"

say "dcc      = $DCC"
say "lua host = $LUA_HOST（T lua_* = $LUA_EXPORTS）"
say "libs     = $LIBS"
say "work     = $WORK"
say ""

DCC_SHA=$(sha256sum "$DCC" | awk '{print $1}')
say "dcc sha256（开跑前）= $DCC_SHA"
say ""

INCS=(-I"$LUA/src")   # libN.c 的 lua.h/lauxlib.h 在这里；与套件 makefile 的 -I$(LUA_DIR) 同形

# 期望表来自报告 §3.4 的实测表：T 入口名 + U lua_* 条数。
expect_entry() {
    case "$(basename "$1")" in
    lib1.so) echo luaopen_lib1_sub ;;
    lib11.so) echo luaopen_lib11 ;;
    lib2.so) echo luaopen_lib2 ;;
    lib21.so) echo luaopen_lib21 ;;
    lib2-v2.so) echo luaopen_lib2 ;;
    *) return 1 ;;
    esac
}
expect_u() {
    case "$(basename "$1")" in
    lib1.so) echo 8 ;;
    lib11.so) echo 0 ;;
    lib2.so) echo 4 ;;
    lib21.so) echo 0 ;;
    lib2-v2.so) echo 12 ;;
    *) return 1 ;;
    esac
}

# ---- 结构断言 -------------------------------------------------------------
assert_elf() { # $1=so $2=tag
    local so=$1 tag=$2 name entry uexp tcount ucount
    name=$(basename "$so")
    entry=$(expect_entry "$so") || { say "  $tag $name FAIL: 期望表里没有这个模块"; return 1; }
    uexp=$(expect_u "$so")
    if ! readelf -h "$so" >/dev/null 2>&1; then
        say "  $tag $name FAIL: 不是可读的 ELF"; return 1
    fi
    if ! readelf -h "$so" | grep -qE 'Type: +DYN'; then
        say "  $tag $name FAIL: e_type 不是 ET_DYN"; return 1
    fi
    if readelf -l "$so" | grep -q 'INTERP'; then
        say "  $tag $name FAIL: 共享库带了 PT_INTERP"; return 1
    fi
    tcount=$(nm -D --defined-only "$so" | awk '$2=="T" && $3 ~ /^luaopen_/{c++} END{print c+0}')
    if [ "$tcount" != 1 ]; then
        say "  $tag $name FAIL: .dynsym 里 T luaopen_* 有 $tcount 个（期望 1）"
        return 1
    fi
    if ! nm -D --defined-only "$so" | awk -v e="$entry" '$2=="T" && $3==e{f=1} END{exit !f}'; then
        say "  $tag $name FAIL: 缺少 T $entry"
        return 1
    fi
    ucount=$(nm -D "$so" | awk '$1=="U" && $2 ~ /^lua_/{c++} END{print c+0}')
    if [ "$ucount" != "$uexp" ]; then
        say "  $tag $name FAIL: U lua_* 有 $ucount 条（期望 $uexp；未定义符号是加载期绑定的一部分，不能被链接器吞掉）"
        return 1
    fi
    if ! readelf -d "$so" | grep -q 'GNU_HASH'; then
        say "  $tag $name FAIL: .dynamic 缺 DT_GNU_HASH"; return 1
    fi
    if readelf -d "$so" | grep -q 'TEXTREL'; then
        say "  $tag $name FAIL: 存在 TEXTREL"; return 1
    fi
    if ! readelf -d "$so" | grep -qE "SONAME.*\[$name\]"; then
        say "  $tag $name FAIL: DT_SONAME 不是 $name"
        return 1
    fi
    if ! readelf -d "$so" | grep -qE 'NEEDED.*libc\.so\.6'; then
        say "  $tag $name FAIL: DT_NEEDED 缺 libc.so.6"
        return 1
    fi
    say "  $tag $name  ET_DYN · T luaopen_* = 1（$entry）· U lua_* = $ucount · GNU_HASH · SONAME · 无 TEXTREL ✓"
    return 0
}

# 最小 C 宿主：只要 dlopen(RTLD_LAZY) + dlsym 成功。模块的 lua_* 由加载它的
# 真实宿主提供，这里不调用入口，因此 LAZY 足以覆盖“能被加载器接受”这一点。
cat >"$WORK/dlhost.c" <<'EOF'
#include <dlfcn.h>
#include <stdio.h>
int main(int argc, char **argv) {
    void *h;
    void *f;
    if (argc != 3) return 2;
    h = dlopen(argv[1], RTLD_LAZY);
    if (!h) { fprintf(stderr, "dlopen: %s\n", dlerror()); return 1; }
    f = dlsym(h, argv[2]);
    if (!f) { fprintf(stderr, "dlsym: %s\n", dlerror()); return 1; }
    dlclose(h);
    return 0;
}
EOF
gcc -O2 -o "$WORK/dlhost" "$WORK/dlhost.c" -ldl || die "最小宿主编译失败"

assert_load_min() { # $1=so
    local so=$1 entry
    entry=$(expect_entry "$so")
    if "$WORK/dlhost" "$so" "$entry" >>"$WORK/log/dlhost.log" 2>&1; then
        say "  $(basename "$so")  dlopen(RTLD_LAZY)+dlsym($entry) ✓"
    else
        say "  $(basename "$so") FAIL: 最小宿主加载失败（见 $WORK/log/dlhost.log）"
        return 1
    fi
}

# Lua 宿主上的行为断言。$1=模块文件 $2=标签；脚本内联成 $WORK/load-<tag>.lua。
assert_load_lua() { # $1=so $2=tag $3=入口 $4=lua 代码
    local so=$1 tag=$2 entry=$3 body=$4
    cat >"$WORK/load-$tag.lua" <<EOF
local path, entry = "$so", "$entry"
local f = assert(package.loadlib(path, entry))
$body
EOF
    if LD_LIBRARY_PATH="$(dirname "$so")${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" \
        "$LUA_HOST" "$WORK/load-$tag.lua" >>"$WORK/log/lua-$tag.log" 2>&1; then
        say "  $tag  真实 Lua 宿主加载并调用 ✓"
    else
        say "  $tag FAIL: Lua 宿主加载/调用失败（见 $WORK/log/lua-$tag.log）"
        return 1
    fi
}

build_so() { # $1=src $2=out $3=tag；成功时 stderr 必须写着 via host ld
    local src=$1 out=$2 tag=$3
    local log=$WORK/log/build-$tag.log
    if ! "$DCC" -fPIC -O"$SHARED_LEVEL" -shared -Wl,-soname,"$(basename "$out")" \
        "${INCS[@]}" -o "$out" "$src" >"$log" 2>&1; then
        say "  $tag FAIL: dcc 构建失败（见 $log）"
        tail -3 "$log" | sed 's/^/      /' | tee -a "$SUMMARY"
        return 1
    fi
    if ! grep -q 'via host ld' "$log"; then
        say "  $tag FAIL: 构建没有走宿主 ld 直驱（日志里没有 'via host ld'）"
        tail -3 "$log" | sed 's/^/      /' | tee -a "$SUMMARY"
        return 1
    fi
    return 0
}

# ---- A/B：五个模块（基础形态）--------------------------------------------
say "=== A. 五个 libN.so（-fPIC -shared，dcc 自编）==="
MODULES="lib1.so lib11.so lib2.so lib21.so lib2-v2.so"
for m in $MODULES; do
    case "$m" in
    lib1.so) src=$LIBS/lib1.c ;;
    lib11.so) src=$LIBS/lib11.c ;;
    lib2.so) src=$LIBS/lib2.c ;;
    lib21.so) src=$LIBS/lib21.c ;;
    lib2-v2.so) src=$LIBS/lib22.c ;;
    esac
    build_so "$src" "$WORK/so/$m" "base-$m" || { fails=$((fails + 1)); continue; }
    assert_elf "$WORK/so/$m" "base" || { fails=$((fails + 1)); continue; }
    assert_load_min "$WORK/so/$m" || { fails=$((fails + 1)); continue; }
done

say ""
say "=== B. 真实 Lua 宿主：入口行为（含套件不加载的 lib2/lib21）==="
if [ -f "$WORK/so/lib1.so" ]; then
    assert_load_lua "$WORK/so/lib1.so" lib1 luaopen_lib1_sub \
        'local t = f()
assert(type(t) == "table", "luaopen_lib1_sub 必须返回 table")
assert(type(t.id) == "function", "table.id 缺失")
assert(select("#", t.id(1, 2, 3)) == 3, "id 返回的值个数不是参数个数")' || fails=$((fails + 1))
fi
if [ -f "$WORK/so/lib11.so" ] && [ -f "$WORK/so/lib1.so" ]; then
    # lib11 的 lib1_export 由 lib1.so 提供（上游 attrib.lua 先加载 lib1）。
    # LD_PRELOAD 把 lib1 变成全局可见，模拟这一加载顺序。
    cat >"$WORK/load-lib11.lua" <<EOF
local f = assert(package.loadlib("$WORK/so/lib11.so", "luaopen_lib11"))
assert(f() == "exported", "lib11 必须返回字符串 exported")
EOF
    if LD_PRELOAD="$WORK/so/lib1.so" LD_LIBRARY_PATH="$WORK/so" \
        "$LUA_HOST" "$WORK/load-lib11.lua" >>"$WORK/log/lua-lib11.log" 2>&1; then
        say "  lib11  经 LD_PRELOAD=lib1.so 加载并返回 exported ✓"
    else
        say "  lib11 FAIL: 加载/调用失败（见 $WORK/log/lua-lib11.log）"
        fails=$((fails + 1))
    fi
fi
if [ -f "$WORK/so/lib2.so" ]; then
    assert_load_lua "$WORK/so/lib2.so" lib2 luaopen_lib2 \
        'local t = f("A", "B")
assert(type(t) == "table", "luaopen_lib2 必须返回 table")
assert(x == "A" and y == "B", "lib2 没有把参数写进全局 x/y")' || fails=$((fails + 1))
fi
if [ -f "$WORK/so/lib2-v2.so" ]; then
    assert_load_lua "$WORK/so/lib2-v2.so" lib2-v2 luaopen_lib2 \
        'local t = f()
assert(type(t) == "table", "lib2-v2 必须返回 table")
assert(type(t.newstr) == "function", "lib2-v2 缺 newstr（lib22.c 的导出）")
assert(t.id() == true, "lib2-v2 的 id 应返回 true")' || fails=$((fails + 1))
fi
# lib21 的入口调用 lib2 的 luaopen_lib2；上游套件从不加载它（§3.4 的空白）。
# LD_PRELOAD 让 lib2.so 全局可见，lib21 的未定义符号在加载期解析。
if [ -f "$WORK/so/lib21.so" ] && [ -f "$WORK/so/lib2.so" ]; then
    cat >"$WORK/load-lib21.lua" <<EOF
local f = assert(package.loadlib("$WORK/so/lib21.so", "luaopen_lib21"))
local t = f("A", "B")
assert(type(t) == "table", "luaopen_lib21 必须通过 lib2 返回 table")
assert(x == "A" and y == "B", "lib21->lib2 的参数传递不对")
EOF
    if LD_PRELOAD="$WORK/so/lib2.so" LD_LIBRARY_PATH="$WORK/so" \
        "$LUA_HOST" "$WORK/load-lib21.lua" >>"$WORK/log/lua-lib21.log" 2>&1; then
        say "  lib21  经 LD_PRELOAD=lib2.so 加载并调用 ✓（套件从未覆盖的模块）"
    else
        say "  lib21 FAIL: 加载/调用失败（见 $WORK/log/lua-lib21.log）"
        fails=$((fails + 1))
    fi
fi

# ---- C1：-flto -shared -----------------------------------------------------
say ""
say "=== C1. -flto -shared（同一套结构断言 + 最小宿主加载）==="
for m in $MODULES; do
    case "$m" in
    lib1.so) src=$LIBS/lib1.c ;;
    lib11.so) src=$LIBS/lib11.c ;;
    lib2.so) src=$LIBS/lib2.c ;;
    lib21.so) src=$LIBS/lib21.c ;;
    lib2-v2.so) src=$LIBS/lib22.c ;;
    esac
    log=$WORK/log/build-lto-$m.log
    if ! "$DCC" -fPIC -O"$SHARED_LEVEL" -flto -shared -Wl,-soname,"$m" \
        "${INCS[@]}" -o "$WORK/lto/$m" "$src" >"$log" 2>&1; then
        say "  lto-$m FAIL: 构建失败（见 $log）"
        tail -3 "$log" | sed 's/^/      /' | tee -a "$SUMMARY"
        fails=$((fails + 1))
        continue
    fi
    if ! grep -q 'via host ld' "$log"; then
        say "  lto-$m FAIL: 没有走宿主 ld 直驱"
        fails=$((fails + 1))
        continue
    fi
    assert_elf "$WORK/lto/$m" "lto" || { fails=$((fails + 1)); continue; }
    assert_load_min "$WORK/lto/$m" || { fails=$((fails + 1)); continue; }
done

# ---- C2：-shared --whole-archive ------------------------------------------
say ""
say "=== C2. -shared --whole-archive（lib11 -> lib1.a 的真实形状 + 语义对照）==="
# 真实形状：lib11.c 只声明 lib1_export，静态库提供实现。
if "$DCC" -fPIC -O"$SHARED_LEVEL" "${INCS[@]}" -c "$LIBS/lib1.c" -o "$WORK/wa/lib1.pic.o" \
    >>"$WORK/log/build-wa-lib1.log" 2>&1 &&
    ar rcs "$WORK/wa/lib1.a" "$WORK/wa/lib1.pic.o"; then
    log=$WORK/log/build-wa-lib11.log
    if "$DCC" -fPIC -O"$SHARED_LEVEL" -shared -Wl,-soname,lib11wa.so \
        "${INCS[@]}" -o "$WORK/wa/lib11wa.so" "$LIBS/lib11.c" \
        -Wl,--whole-archive "$WORK/wa/lib1.a" -Wl,--no-whole-archive \
        >"$log" 2>&1; then
        if nm -D --defined-only "$WORK/wa/lib11wa.so" | awk '$2=="T" && $3=="lib1_export"{f=1} END{exit !f}'; then
            say "  lib11wa.so  T lib1_export 来自 --whole-archive 的 lib1.a ✓"
        else
            say "  lib11wa.so FAIL: lib1_export 没有被纳入（whole-archive 未生效）"
            fails=$((fails + 1))
        fi
        if [ -x "$WORK/dlhost" ] && "$WORK/dlhost" "$WORK/wa/lib11wa.so" luaopen_lib11 \
            >>"$WORK/log/dlhost-wa.log" 2>&1; then
            say "  lib11wa.so  dlopen(RTLD_LAZY)+dlsym(luaopen_lib11) ✓"
        else
            say "  lib11wa.so FAIL: 最小宿主加载失败（见 $WORK/log/dlhost-wa.log）"
            fails=$((fails + 1))
        fi
        if LD_LIBRARY_PATH="$WORK/wa" "$LUA_HOST" -e \
            "local f=assert(package.loadlib('$WORK/wa/lib11wa.so','luaopen_lib11')); assert(f()=='exported')" \
            >>"$WORK/log/lua-wa.log" 2>&1; then
            say "  lib11wa.so  Lua 宿主调用返回 exported ✓"
        else
            say "  lib11wa.so FAIL: Lua 宿主调用失败（见 $WORK/log/lua-wa.log）"
            fails=$((fails + 1))
        fi
    else
        say "  lib11wa.so FAIL: 构建失败（见 $log）"
        tail -3 "$log" | sed 's/^/      /' | tee -a "$SUMMARY"
        fails=$((fails + 1))
    fi
else
    say "  FAIL: 静态库准备失败（见 $WORK/log/build-wa-lib1.log）"
    fails=$((fails + 1))
fi

# 语义对照：w1 被引用、w2 不被引用。whole-archive 下两者都必须是 T；
# 不包 whole-archive 时 w2 不得出现 —— 否则说明 flag 被吞或总是全量拉入。
printf 'int w1(void){ return 1; }\n' >"$WORK/wa/w1.c"
printf 'int w2(void){ return 2; }\n' >"$WORK/wa/w2.c"
printf 'extern int w1(void);\nint w_entry(void){ return w1(); }\n' >"$WORK/wa/wmain.c"
if "$DCC" -fPIC -O"$SHARED_LEVEL" -c "$WORK/wa/w1.c" -o "$WORK/wa/w1.o" >>"$WORK/log/wa-sem.log" 2>&1 &&
    "$DCC" -fPIC -O"$SHARED_LEVEL" -c "$WORK/wa/w2.c" -o "$WORK/wa/w2.o" >>"$WORK/log/wa-sem.log" 2>&1 &&
    ar rcs "$WORK/wa/libw.a" "$WORK/wa/w1.o" "$WORK/wa/w2.o"; then
    if "$DCC" -fPIC -O"$SHARED_LEVEL" -shared -o "$WORK/wa/libw-all.so" "$WORK/wa/wmain.c" \
        -Wl,--whole-archive "$WORK/wa/libw.a" -Wl,--no-whole-archive \
        >>"$WORK/log/wa-sem.log" 2>&1; then
        if nm -D --defined-only "$WORK/wa/libw-all.so" | awk '$2=="T" && $3=="w2"{f=1} END{exit !f}'; then
            say "  whole-archive 语义  T w2（未被引用的成员）也进入 .dynsym ✓"
        else
            say "  whole-archive 语义 FAIL: 未被引用的 w2 没有进入 .dynsym（flag 被吞）"
            fails=$((fails + 1))
        fi
    else
        say "  whole-archive 语义 FAIL: libw-all.so 构建失败（见 $WORK/log/wa-sem.log）"
        fails=$((fails + 1))
    fi
    if "$DCC" -fPIC -O"$SHARED_LEVEL" -shared -o "$WORK/wa/libw-plain.so" "$WORK/wa/wmain.c" \
        "$WORK/wa/libw.a" >>"$WORK/log/wa-sem.log" 2>&1; then
        if nm -D --defined-only "$WORK/wa/libw-plain.so" | awk '$2=="T" && $3=="w2"{f=1} END{exit !f}'; then
            say "  whole-archive 语义 FAIL: 不包 whole-archive 时 w2 也进来了（对照失效）"
            fails=$((fails + 1))
        else
            say "  whole-archive 语义  不包时 w2 不进入 .dynsym（对照组）✓"
        fi
    else
        say "  whole-archive 语义 FAIL: libw-plain.so 构建失败（见 $WORK/log/wa-sem.log）"
        fails=$((fails + 1))
    fi
else
    say "  whole-archive 语义 FAIL: w1/w2 归档准备失败（见 $WORK/log/wa-sem.log）"
    fails=$((fails + 1))
fi

# ---- D：musl ---------------------------------------------------------------
say ""
say "=== D. 非 glibc 工具链（musl）==="
MUSL_CC=""
for c in musl-gcc x86_64-linux-musl-gcc; do
    if command -v "$c" >/dev/null 2>&1; then MUSL_CC=$c; break; fi
done
if [ -z "$MUSL_CC" ] && [ ! -e /lib/ld-musl-x86_64.so.1 ]; then
    say "  SKIP: 本机没有 musl 工具链（musl-gcc / x86_64-linux-musl-gcc / ld-musl 均缺失）"
    say "        报告 README 的目标表声称支持 musl，但这一条只能在 musl rootfs 上验证；"
    say "        本行是环境缺口，不是通过。"
    skips=$((skips + 1))
else
    say "  检测到 musl：$MUSL_CC。dcc 的宿主 ld 直驱按 glibc crt 布局发现启动文件，"
    say "  在 musl rootfs 上的结果需要在那种环境里实跑；本脚本不把它降级成 PASS。"
    skips=$((skips + 1))
fi

# ---- 总结 ------------------------------------------------------------------
say ""
say "=== summary ==="
now_sha=$(sha256sum "$DCC" | awk '{print $1}')
say "    dcc sha256（收尾时）= $now_sha"
if [ "$now_sha" != "$DCC_SHA" ]; then
    say "  COMPILER-CHANGED: 跑的过程中 $DCC 被重写了，本轮证据不来自同一个编译器"
    fails=$((fails + 1))
fi
say "    checks=$(grep -c '✓' "$SUMMARY")  fails=$fails  skips=$skips"
if [ "$fails" = 0 ]; then
    say "RESULT: PASS"
else
    say "RESULT: FAIL ($fails issue(s))"
fi
# 日志不自动删：失败时要能按日志定位，KEEP=1 只是把整个 WORK 也留下（默认本来就留）。
exit "$fails"
