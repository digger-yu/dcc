#!/bin/bash
# ===========================================================================
# scripts/check_sqlite_suite.sh — SQLite 官方 TCL 套件（gcc 基线 vs dcc 各档）
#
# 判据:
#   * 基线 = gcc 编译的 testfixture 跑同一套件。SQLite 官方套件在部分环境下
#     本来就有失败项（权限、平台、可选扩展），那些不算 dcc 的问题。
#   * 判红条件（二者任一，exit 1）:
#       DCC-BUILD-FAIL  dcc 编不出 testfixture
#       SUITE-DIFF      dcc 的失败集合里出现了 gcc 没有的测试名
#   * gcc 自己也失败的测试不判红 —— 否则测试用例从第一天起就长期红。
#   * 退出码（CI 要靠它区分「真回归」与「已知不完」）:
#       0 PASS          全部档跑完且无新增失败名
#       1 FAIL          有 DCC-BUILD-FAIL / SUITE-DIFF
#       2 ERROR         前置不满足（缺依赖、SUITE_USER 建不出来、基线自己跑不完……）
#       3               抢不到机器级锁 / OUT 已被另一个实例占用
#       4 INCOMPLETE    只有「没跑到汇总行」这一类问题，且**无新增失败名**。
#                     历史上这一档是为了容住「dcc 在 quick.test 中途 SIGSEGV」那族
#                     缺陷（最后一个已在 2026-10-27 修完：四档都能跑到汇总行，见
#                     docs/dcc-lua-sqlite-build-verify-report.md §4.4）。今天再出现
#                     rc=4 就是新问题，必须看日志，而不是当已知项吞掉；把它归 exit 1
#                     的话，超时/机器抖动也会判红，所以仍留一档，由调用方决定当黄当红。
#                     中途停止本身依旧在日志与汇总表里点名（SUITE-INCOMPLETE / INCOMPLETE）。
#
# 为什么必须非 root: test/attach.test 的 attach-6.2 会把文件 chmod 0000 再
# 断言打不开。root 无视权限，该用例直接 puts "Tests do not work when run as
# root" 并 exit 1，整套提前结束。脚本因此在 root 下自动降权到 $SUITE_USER。
# CI（GitHub hosted runner）本来就是非 root 的 runner 用户，走的是「不降权」
# 那条分支 —— 那条分支同样必须成立，所以降级用的 runuser 缺失时直接报错，
# 而不是让套件跑到 attach.test 才发现是权限问题。
#
# 为什么用源码树副本: 构建会往树里写 Makefile/产物/临时数据库，直接在
# $SQLITE_SRC 里跑会污染源树，也过不了"运行用户需要写权限"这一关。
#
# 依赖: bash>=4, tclsh + tcl.h + tclConfig.sh (tcl8.6-dev), unzip,
#       $SQLITE_SRC（由 scripts/setup_sqlite_suite.sh 准备）
#
# 构建方式（为什么不是直接 `make testfixture CC=dcc`）:
#   main.mk 的 testfixture 规则是「一条命令把 88 个 TU 一起编译并链接」。gcc 这么
#   做没问题；dcc 的多文件入口是**单进程**顺序扫完所有 TU 且内存不回收，本机
#   (7.6G) 扫到最后一个 sqlite3.c（9.5M 源码，单 TU 峰值 RSS 2.7G）之前就被 OOM
#   杀掉（make error 137）。于是这里拆成：capture 桩编译器拿到 make 的真实 argv
#   → 逐 TU 独立进程编译 → 一次链接。argv 由 make 自己打印，-D/-I 的权威仍是
#   main.mk，脚本里不手抄第二份；gcc 基线与 dcc 各档走同一份 argv，严格可比。
#
# 用法（手工执行，不挂 CI 触发器，和 torture 一样）:
#   bash scripts/setup_sqlite_suite.sh
#   bash scripts/check_sqlite_suite.sh
#   LEVELS="0 1 2 3" bash scripts/check_sqlite_suite.sh
#   LEVELS="0,1,2,3" bash scripts/check_sqlite_suite.sh   # 逗号同样接受
#   SUITE="test/quick.test test/veryquick.test" bash scripts/check_sqlite_suite.sh
# ===========================================================================
set -o pipefail

DCC=${DCC:-/root/dcc/target/release/dcc}
REFCC=${REFCC:-gcc}
SQLITE_SRC=${SQLITE_SRC:-$HOME/sqlite-src-3540000}
OUT=${OUT:-/tmp/sqlite_suite}
SUITE=${SUITE:-test/quick.test}
LEVELS=${LEVELS:-2}
SUITE_USER=${SUITE_USER:-sqltest}
WITH_TCL=${WITH_TCL:-}
BUILD_TIMEOUT_S=${BUILD_TIMEOUT_S:-3600}
TEST_TIMEOUT_S=${TEST_TIMEOUT_S:-3600}
# BUILD_ONLY=1：只建各档的 testfixture（连同逐 TU 对象集）与每档二进制副本，**不跑套件**。
# 给 `check_sqlite_verify.sh` 现场补前置用（它只需要对象集/fixture，不需要全量套件）。
BUILD_ONLY=${BUILD_ONLY:-0}

mkdir -p "$OUT" || exit 1
# 与 check_lua_suite_diff.sh 共用同一把机器级独占锁：两个官方套件测试用例脚本都靠墙钟/CPU
# 才能稳定（套件里 recoverfault/timeout 一类用例对机器负载敏感），并发互跑会把对方
# 的基线污染成假红。整段跑完之前独占机器。
#
# `DCC_GATE_LOCK_HELD=1` 表示调用方（同一台机器上的上层脚本）**已经**持着这把锁：
# 此时不得再抢（flock 同文件会让子进程死等，父进程在等子进程返回 ⇒ 自锁）。锁的
# 独占语义仍然成立，因为持有的就是父进程本身。
if [ -z "${DCC_GATE_LOCK_HELD:-}" ]; then
    exec 8>"${DCC_GATE_LOCK:-/tmp/dcc-suite-gate.lock}" || exit 1
    if ! flock -w 0 8; then
        echo "ERROR: another dcc suite gate is running (lock: ${DCC_GATE_LOCK:-/tmp/dcc-suite-gate.lock})" >&2
        exit 3
    fi
else
    echo "DCC_GATE_LOCK_HELD=1 ⇒ 沿用调用方的机器级锁，不重复抢"
fi
# 同一个 OUT 只允许一个实例：两实例会互相覆盖 build/run 目录与失败集合，汇总里出现
# 两段交错的记录（rc/失败清单来自不同二进制），既可能假红也可能假绿。
exec 9>"$OUT/.lock" || exit 1
if ! flock -w 0 9; then
  echo "ERROR: another instance is already using $OUT (lock: $OUT/.lock)" >&2
  exit 3
fi
SUMMARY="$OUT/summary.txt"
: > "$SUMMARY"
say() { echo "$*" | tee -a "$SUMMARY"; }
fails=0
incomplete=0

die() {
  say "ERROR: $*"
  exit 2
}

# ---- 列表入参归一 ---------------------------------------------------------
# SUITE / LEVELS 既可能来自命令行 env（习惯写空格分隔），也可能来自 CI 的
# workflow_dispatch 输入框（人手很容易写成 "0,1,2,3"）。而 `for L in $LEVELS` 只按
# 空白切词：逗号版整串变成一个档，最后拼给编译器的是 `-O0,1,2,3`，dcc 报
# "unknown flag"，汇总里则出现一个根本不存在的档位（CI 上真实翻车过一遍）。
# 这里统一成空格 + 保序去重，再逐个校验，让 "O2"/"2x" 这种拼错在展开 make
# 之前就点名报错，而不是等到编译器的 stderr 里去找。
split_list() { # $1 = 逗号/分号/空白混合的列表 → stdout = 空格分隔、保序去重
  local raw=$1 x out="" seen=""
  set -f
  raw=${raw//,/ }
  raw=${raw//;/ }
  for x in $raw; do
    case " $seen " in
      *" $x ") continue ;;
    esac
    seen="$seen $x"
    out="$out $x"
  done
  set +f
  echo "${out# }"
}

SUITE=$(split_list "$SUITE")
LEVELS=$(split_list "$LEVELS")
[ -n "$SUITE" ] || die "SUITE is empty"
# BUILD_ONLY 允许 LEVELS 为空：调用方（check_sqlite_verify.sh）在「只缺 gcc 参照档、
# dcc 各档都在」时正是这样传的，此时只需补 ref。非 BUILD_ONLY 仍然要求至少一档。
[ -n "$LEVELS" ] || [ "$BUILD_ONLY" = 1 ] || die "LEVELS is empty"
for L in $LEVELS; do
  case $L in
    '' | *[!0-9]*)
      die "LEVELS 里的 '$L' 不是优化档位：只接受 0-9，分隔符空格或逗号均可（\"0 1 2 3\" / \"0,1,2,3\"）" ;;
  esac
done

# ---- 依赖检查 -------------------------------------------------------------
case "${BASH_VERSINFO[0]:-0}" in
  '' | 0 | 1 | 2 | 3) die "bash >= 4 required" ;;
esac
[ -x "$DCC" ] || die "DCC not executable: $DCC"
command -v "$REFCC" >/dev/null 2>&1 || die "reference compiler missing: $REFCC"
command -v tclsh >/dev/null 2>&1 || die "tclsh missing (apt-get install tcl-dev)"
# 发行版把 tclConfig.sh 放在 /usr/lib/tcl8.6 或 /usr/lib/<triplet>/tcl8.6，
# 不同版本不一致，这里直接找，找不到才报错。
if [ -z "$WITH_TCL" ]; then
  cfg=$(find /usr/lib /usr/local/lib -maxdepth 3 -name tclConfig.sh 2>/dev/null | head -1)
  [ -n "$cfg" ] && WITH_TCL=$(dirname "$cfg")
fi
[ -n "$WITH_TCL" ] && [ -f "$WITH_TCL/tclConfig.sh" ] ||
  die "tclConfig.sh not found (apt-get install tcl-dev)"
if [ -z "$(find /usr/include /usr/local/include -name tcl.h 2>/dev/null | head -1)" ]; then
  die "tcl.h missing (apt-get install tcl-dev)"
fi
[ -f "$SQLITE_SRC/configure" ] || die "no configure in $SQLITE_SRC"
[ -d "$SQLITE_SRC/test" ] || die "no test/ in $SQLITE_SRC (run scripts/setup_sqlite_suite.sh)"

n_test=$(find "$SQLITE_SRC/test" -maxdepth 1 -name '*.test' | wc -l)
say "suite source: $SQLITE_SRC ($n_test .test files)"
say "run: SUITE='$SUITE' LEVELS='$LEVELS'"
# 编译器指纹：同一轮的各档对象集必须来自同一个二进制。事后重编 dcc 再重跑单档，
# 或中途把 target/release/dcc 换掉，都能靠这一行查出「证据不是同一个编译器出的」。
say "compiler: $DCC sha256=$(sha256sum "$DCC" | awk '{print $1}')"

# ---- 工作副本 + 降权 ------------------------------------------------------
WORK="$OUT/run"
rm -rf "$WORK"
mkdir -p "$WORK" || die "cannot create $WORK"
say "copying source tree to $WORK/src ..."
cp -a "$SQLITE_SRC" "$WORK/src" || die "copy failed"

RUN=()
if [ "$(id -u)" -eq 0 ]; then
  # root 下必须降权：root 绕过权限断言，attach.test 直接 exit 1 把整套提前带走。
  # runuser 缺失（精简镜像）时不能默默当 root 跑 —— 那拿到的红灯是假的。
  command -v runuser >/dev/null 2>&1 ||
    die "running as root but runuser(1) missing (util-linux): refusing to run the suite as root"
  if ! id "$SUITE_USER" >/dev/null 2>&1; then
    useradd -m "$SUITE_USER" || die "cannot create user $SUITE_USER"
    say "created user: $SUITE_USER"
  fi
  chown -R "$SUITE_USER" "$WORK" || die "chown failed"
  # -u BASH_ENV/-u ENV: 调用方的 shell 启动钩子常指向 root 私有路径，降权后
  # 每次 bash 启动都会 "权限不够"，还会把 configure 的编译器探测带偏。
  RUN=(runuser -u "$SUITE_USER" -- env -u BASH_ENV -u ENV)
  say "dropping privileges to: $SUITE_USER (SQLite refuses to run as root)"
else
  # CI（GitHub hosted runner）走这一支：作业本身就是非 root 的 runner 用户，
  # 权限类用例按真实约束跑。不降权也就不会把编译器副本存到 $OUT/tools，
  # 因此 $DCC 必须对**当前这个非 root 用户**真的能跑起来 —— 提前叫一次
  # --version，把“权限/缺共享库”和“编译器本身坏了”分开。
  say "running as: $(id -un) (uid=$(id -u)) — 非 root，权限类用例按真实约束执行"
  "$DCC" --version >/dev/null 2>&1 ||
    die "cannot execute $DCC as $(id -un): $(ls -l "$DCC" 2>&1)"
fi

as_user() { # $1 = 在 $WORK/src 下执行的 shell 片段
  if [ "${#RUN[@]}" -eq 0 ]; then
    bash -c "$1"
  else
    "${RUN[@]}" bash -c "$1"
  fi
}

# /dev/null 必须对**实际跑套件的那个人**可写：任何工具把 `-o /dev/null` 当输出路径
# （gcc/dcc 会先 unlink 再新建）都会把这个节点重建成权限 755 的字符设备，之后
# configure 里的探测子进程写不了它，报出的是「cannot create /dev/null」+「No working C
# compiler found」——完全不像权限问题。先拿真正要用的身份试一次，把名字点出来。
if ! as_user "test -w /dev/null"; then
  whoami_running=$(as_user "id -un")
  die "/dev/null is not writable by $whoami_running ($(stat -c '%F %a %U' /dev/null 2>/dev/null)); some tool used '-o /dev/null' and recreated the node — fix with: chmod 666 /dev/null"
fi

SRC="$WORK/src"
# 降权后 CC 必须对 $SUITE_USER 可执行：dcc 通常在 /root 下（0700），直接把它
# 传给 make 会 "/bin/sh: 1: .../dcc: Permission denied"（错误 126，看起来像编
# 译器坏了）。把编译器复制到 $OUT/tools 里再给 make 用，路径对谁都可读可执行。
DCC_BIN=$DCC
if [ "$(id -u)" -eq 0 ] && [ "${#RUN[@]}" -ne 0 ]; then
  mkdir -p "$OUT/tools" || die "cannot create $OUT/tools"
  cp -f "$DCC" "$OUT/tools/dcc" || die "cannot stage $DCC"
  chmod 755 "$OUT/tools/dcc" "$OUT/tools" "$OUT" || die "chmod failed on $OUT/tools"
  DCC_BIN="$OUT/tools/dcc"
  say "staged compiler for $SUITE_USER: $DCC_BIN"
fi

say "configure ..."
as_user "cd '$SRC' && ./configure --with-tcl='$WITH_TCL'" >"$OUT/configure.log" 2>&1 ||
  die "configure failed, see $OUT/configure.log"

# capture 桩编译器：只把收到的参数一行一个写进 $CAPTURE_OUT，然后成功退出。
# 让 make 自己打印 recipe 的 argv（含 main.mk 的全部 -D/-I 与 $TCL_LIB_SPEC 展开值），
# 避免在本脚本里手抄一份编译参数 —— 手抄的那份和 main.mk 漂移时，测的就不是
# 官方套件了。
mkdir -p "$OUT/tools" || die "cannot create $OUT/tools"
CAP_BIN="$OUT/tools/capture-cc.sh"
cat > "$CAP_BIN" <<'CAP'
#!/bin/bash
printf '%s\n' "$@" > "${CAPTURE_OUT:?CAPTURE_OUT not set}"
CAP
chmod 755 "$CAP_BIN"

# 逐 TU 编译 + 一次链接的驱动器（参数：cc argv文件 obj目录）。
TU_SCRIPT="$WORK/build-tu.sh"
cat > "$TU_SCRIPT" <<'TU'
#!/bin/bash
set -o pipefail
ccstr=$1
argv=$2
objdir=$3
INCDEFS=()
mapfile -t A <"$argv" || { echo "ARGV-READ-FAIL $argv"; exit 1; }
read -ra CC <<<"$ccstr" || exit 1
n=${#A[@]}
oidx=-1
for ((i = 0; i < n; i++)); do
  if [ "${A[$i]}" = "-o" ] && [ "${A[i + 1]}" = "testfixture" ]; then oidx=$i; break; fi
done
[ "$oidx" -ge 0 ] || { echo "ARGV-PARSE-FAIL: 找不到 '-o testfixture'"; exit 1; }
FLAGS=("${A[@]:0:oidx}")
SRCS=()
TAIL=()
for ((i = oidx + 2; i < n; i++)); do
  case ${A[$i]} in
    *.c) SRCS+=("${A[$i]}") ;;
    -I* | -D*) TAIL+=("${A[$i]}"); INCDEFS+=("${A[$i]}") ;;
    *) TAIL+=("${A[$i]}") ;;
  esac
done
[ "${#SRCS[@]}" -gt 0 ] || { echo "ARGV-PARSE-FAIL: 没有 .c 源文件"; exit 1; }
# $TCL_INCLUDE_SPEC（-I/usr/include/tcl8.6）在 recipe 里排在源文件**之后**，即链接尾部。
# 单命令 recipe 下编译与链接共用这一串参数，所以 tclsqlite.h 找得到 tcl.h；拆成
# 「逐 TU 编译 + 一次链接」后必须把尾部的 -I/-D 也喂给编译，否则 test1.c 直接
# fatal error: tcl.h: 没有那个文件或目录。
echo "TU: ${#SRCS[@]} 个源文件，链接尾部 ${#TAIL[@]} 个参数（其中 ${#INCDEFS[@]} 个 -I/-D 同时用于编译）"
mkdir -p "$objdir" || exit 1
objs=()
for src in "${SRCS[@]}"; do
  o="$objdir/${src//\//_}.o"
  if ! "${CC[@]}" "${FLAGS[@]}" "${INCDEFS[@]}" -c "$src" -o "$o" 2>>"$objdir/warn.log"; then
    echo "COMPILE-FAIL $src"
    exit 1
  fi
  objs+=("$o")
done
"${CC[@]}" "${FLAGS[@]}" -o testfixture "${objs[@]}" "${TAIL[@]}" || {
  echo "LINK-FAIL"
  exit 1
}
TU
chmod 755 "$TU_SCRIPT"

build() { # $1=cc $2=cflags $3=log $4=tag
  local cc=$1 cflags=$2 log=$3 tag=$4
  local argv="$WORK/argv-$tag.txt"
  local objdir="$WORK/obj-$tag"
  rm -f "$SRC/testfixture" "$argv"
  # 1) 用桩编译器骗过 recipe，只为拿到 argv（make 认为目标已生成，退出码 0）
  as_user "cd '$SRC' && CAPTURE_OUT='$argv' timeout $BUILD_TIMEOUT_S make testfixture CC='$CAP_BIN' CFLAGS='$cflags'" \
    >"$log" 2>&1 || { say "CAPTURE-FAIL $tag（见 $log）"; return 1; }
  [ -s "$argv" ] || { say "CAPTURE-EMPTY $tag（见 $log）"; return 1; }
  # 2) 逐 TU 独立进程编译 + 一次链接（每个 TU 一个进程，内存随进程退出归还）
  as_user "cd '$SRC' && timeout $BUILD_TIMEOUT_S bash '$TU_SCRIPT' '$cc' '$argv' '$objdir'" \
    >>"$log" 2>&1 || { tail -3 "$log" | sed 's/^/  /' | tee -a "$SUMMARY"; return 1; }
  [ -x "$SRC/testfixture" ] || return 1
  # 每档留一份带档名的副本：`testfixture` 会被下一档覆盖，而「哪档对应哪个二进制」
  # 不得靠记忆或时间戳去推（那正是本仓反复踩过的影子副本坑）。
  cp -f "$SRC/testfixture" "$WORK/testfixture-$tag" || return 1
}

# 失败集合: 只取测试名。行格式为 "! xxx-1.2 expected: ... got: ..."，
# 汇总行 "! N errors out of M tests" 用数字前缀排除。
extract_fails() {
  grep -E '^! ' "$1" 2>/dev/null |
    grep -vE '^! +[0-9]+ errors out of' |
    sed -E 's/^! +//' |
    awk '{print $1}' |
    sort -u
}

complete() { grep -qE '[0-9]+ errors out of [0-9]+ tests' "$1" 2>/dev/null; }

run_suite() { # $1=tag
  rm -f "$SRC/test.db" "$SRC/test.db-journal" 2>/dev/null
  as_user "cd '$SRC' && timeout $TEST_TIMEOUT_S ./testfixture $SUITE" >"$OUT/run-$1.log" 2>&1
  rc=$?
  extract_fails "$OUT/run-$1.log" >"$OUT/fails-$1.txt"
  if ! complete "$OUT/run-$1.log"; then
    say "SUITE-INCOMPLETE $1 (rc=$rc) — no summary line, see $OUT/run-$1.log"
    incomplete=$((incomplete + 1))
    return 1
  fi
  # 套件自己的汇总行（"N errors out of M tests"）进汇总，报告里要引用它
  say "  $1: rc=$rc, \"$(grep -E '[0-9]+ errors out of [0-9]+ tests' "$OUT/run-$1.log" | tail -1)\""
  echo "$rc"
}

say ""
say "=== baseline: $REFCC -O2 ==="
# BUILD_REF=0 表示调用方已经有可用的参照档（verify 补齐缺档时会这么传）：省掉一次整树
# 复制 + configure + 88 TU 的基线构建，也避免「ref 构建失败」把正交的 dcc 档一起带走。
BUILD_REF=${BUILD_REF:-1}
if [ "$BUILD_REF" = 1 ]; then
  if ! build "$REFCC" "-O2 -g" "$OUT/build-ref.log" "ref"; then
    if [ "$BUILD_ONLY" = 1 ]; then
      say "REF-BUILD-FAIL（继续建 dcc 档，见 $OUT/build-ref.log）"
    else
      die "$REFCC could not build testfixture, see $OUT/build-ref.log"
    fi
  fi
else
  say "BUILD_REF=0 ⇒ 复用调用方已有的 gcc 参照档，不重建"
fi
if [ "$BUILD_ONLY" = 1 ]; then
  # 只建不跑：基线无需「跑得完」（没有比较发生），但参照档必须存在，否则调用方
  # 下一步链 fixture 时会拿不到东西。
  for L in $LEVELS; do
    say ""
    say "=== dcc -O$L (BUILD_ONLY) ==="
    if ! build "$DCC_BIN" "-O$L -g" "$OUT/build-dcc-O$L.log" "dcc-O$L"; then
      say "DCC-BUILD-FAIL -O$L (see $OUT/build-dcc-O$L.log)"
      tail -3 "$OUT/build-dcc-O$L.log" | sed 's/^/  /' | tee -a "$SUMMARY"
      fails=$((fails + 1))
    fi
  done
  say ""
  say "=== summary ==="
  if [ "$fails" = 0 ]; then
    say "RESULT: PASS (BUILD_ONLY: 对象集与每档 testfixture 已备齐)"
  else
    say "RESULT: FAIL ($fails build issue(s))"
  fi
  exit "$fails"
fi
# 基线自己都跑不完时，后面的 comm -13 拿的是一个残缺参照集 —— 那既能造出假绿
# （dcc 根本没跑到那段）也能造出假红（环境把两个都打断）。当场停，不当「已知不完」处理。
if ! run_suite "ref" >/dev/null; then
  die "$REFCC baseline did not complete, see $OUT/run-ref.log（参照集不可用，不再比 dcc）"
fi
ref_n=$(wc -l <"$OUT/fails-ref.txt")
say "baseline failures: $ref_n"

declare -A RESULT=()
for L in $LEVELS; do
  say ""
  say "=== dcc -O$L ==="
  if ! build "$DCC_BIN" "-O$L -g" "$OUT/build-dcc-O$L.log" "dcc-O$L"; then
    say "DCC-BUILD-FAIL -O$L (see $OUT/build-dcc-O$L.log)"
    tail -3 "$OUT/build-dcc-O$L.log" | sed 's/^/  /' | tee -a "$SUMMARY"
    RESULT[$L]="BUILD-FAIL"
    fails=$((fails + 1))
    continue
  fi
  if ! run_suite "dcc-O$L" >/dev/null; then
    RESULT[$L]="INCOMPLETE"
    # run_suite 里的“SUITE-INCOMPLETE …”被 >/dev/null 吃掉了，这里必须补一行，
    # 否则日志上“=== dcc -O$L ===”后面是一片空白，看不出是没跑还是跑崩了。
    say "SUITE-INCOMPLETE -O$L（未跑到汇总行；逐档日志 $OUT/run-dcc-O$L.log）"
    # 崩之前跑过的用例照样要查新增失败名：INCOMPLETE 分支原来直接 continue，
    # “无新增失败名”就只是“没算过”，把跑到的部分里的新失败一起吞掉了——
    # 那是另一种假绿。
    comm -13 "$OUT/fails-ref.txt" "$OUT/fails-dcc-O$L.txt" >"$OUT/newfails-O$L.txt"
    new_n=$(wc -l <"$OUT/newfails-O$L.txt")
    if [ "$new_n" -gt 0 ]; then
      say "  SUITE-DIFF -O$L: $new_n test(s) fail under dcc but pass under $REFCC（崩溃前已出现）"
      sed 's/^/  /' "$OUT/newfails-O$L.txt" | tee -a "$SUMMARY"
      RESULT[$L]="DIFF($new_n)"
      fails=$((fails + 1))
    fi
    continue
  fi
  comm -13 "$OUT/fails-ref.txt" "$OUT/fails-dcc-O$L.txt" >"$OUT/newfails-O$L.txt"
  new_n=$(wc -l <"$OUT/newfails-O$L.txt")
  if [ "$new_n" -gt 0 ]; then
    say "SUITE-DIFF -O$L: $new_n test(s) fail under dcc but pass under $REFCC"
    sed 's/^/  /' "$OUT/newfails-O$L.txt" | tee -a "$SUMMARY"
    RESULT[$L]="DIFF($new_n)"
    fails=$((fails + 1))
  else
    say "ok -O$L (baseline failures $ref_n, no new failures)"
    RESULT[$L]="ok"
  fi
done

# ---- 汇总 -----------------------------------------------------------------
say ""
say "=== summary ==="
say "$(printf '%-12s %-10s %-10s %s' tag build fails-new status)"
ref_fails="$ref_n"
for L in $LEVELS; do
  st=${RESULT[$L]:-skipped}
  say "$(printf '%-12s %-10s %-10s %s' "dcc -O$L" "$([ "$st" = BUILD-FAIL ] && echo fail || echo ok)" \
    "$(echo "$st" | sed -n 's/^DIFF(\(.*\))$/\1/p')" "$st")"
done
say "$(printf '%-12s %-10s %-10s %s' "$REFCC -O2" ok "$ref_fails" "baseline")"

if [ "$fails" -eq 0 ] && [ "$incomplete" -eq 0 ]; then
  say "RESULT: PASS"
  exit 0
fi
if [ "$fails" -eq 0 ]; then
  say "RESULT: INCOMPLETE ($incomplete 档没跑到汇总行，无新增失败名 — 中途停止，需查原因；本轮起四档均应能跑到汇总行，见报告 §4.4)"
  exit 4
fi
say "RESULT: FAIL ($fails issue(s))"
exit 1
