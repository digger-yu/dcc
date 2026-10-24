#!/bin/bash
# ===========================================================================
# meta_gate.sh — 方案 §5 的 S−1 元判据：代理量到底能不能预测墙钟
#
# 为什么必须先跑这个再谈任何机制：本机没有 PMU，我们只有三类静态代理（体量 KB、
# 每步执行条数、每步 taken 分支）与一个墙钟比值。历史上「静态条数变好而墙钟变差」
# 已经发生过不止一次（删 135 条 to-next jmp 慢 3.8%；R_RAW 11397 比 11366 慢）。如果
# 代理与墙钟的秩相关立不住，后面每个阶段的「静态判据」都只是换个单位的墙钟，
# 会形成方案第 4 轮指出的循环论证。所以这道判据放在所有机制之前：**不成立就先修判据。**
#
# 做法：用三根正交滴定砝码造出已知大小的扰动，联立解出三个斜率
#   DCC_HANDLER_PAD=k  在臂标签**之前**插 k 个 nop ⇒ 只长体量，动态一条不执行
#   DCC_EXEC_PAD=k     在臂标签**之后**插 k 个 nop ⇒ 每步多执行 k 条，不分支
#   DCC_TAKEN_PAD=k    在臂标签**之后**插 k 组 `jmp f; f:` ⇒ 每步多 k 条且多 k 次 taken
# 另加 DCC_FN_SHIFT 一族作**噪声对照**（只移位、三轴全零）：它们测出来的差就是
# 布局敏感带本身，不参与拟合，只用来给残差定噪声底。
#
# 模型：Δms = a·ΔKB + b·Δ(条/步) + c·Δ(taken/步)，过原点（基准臂三轴皆 0）。
# 判读：
#   * ρ（预测 vs 实测的 Spearman 秩相关）≥ 0.8 ⇒ 代理可用，进入 S0；
#   * |ρ| < 0.8 ⇒ 代理失效，**停在判据层**，不许开始任何机制；
#   * |a| < 0.3 ms/KB ⇒ 前端不是瓶颈，方案 §3 的 P2/P3/P6 全部字节叙事预期收益归零，
#     必须重写每步预算（这一条与 `aa_layout.sh` 的既有阈值同口径，不要另立标准）。
#
# 用法:
#   DCC=/root/dcc/target/release/dcc LUA_SRC=/root/lua-5.5.1 R=15 \
#     bash scripts/meta_gate.sh [lua|sqlite]
#   ARMSPEC="pad64:DCC_HANDLER_PAD=64 take1:DCC_TAKEN_PAD=1 ..." 覆盖默认臂组
#   OUTDIR=/tmp/meta_gate  RAW=/tmp/meta_gate.raw
# 口径：计时与判据全部走 scripts/ab_pair.sh（轮内配对 + median + 95%CI + 符号检验），
# 本脚本不自己 best-of-N。绑核沿用 CPU=3（与 bench_lua_sqlite.sh 历史数据同口径）。
# ===========================================================================
set -o pipefail
set -u

MODE="${1:-lua}"
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="${DCC_REPO:-$(cd "$HERE/.." && pwd)}"
DCC="${DCC:-$REPO/target/release/dcc}"
LUA_SRC="${LUA_SRC:-/root/lua-5.5.1}"
SQLITE_SRC="${SQLITE_SRC:-/root/sqlite}"
OUTDIR="${OUTDIR:-/tmp/meta_gate}"
RAW="${RAW:-/tmp/meta_gate.raw}"
R="${R:-15}"
CPU="${CPU:-3}"
SYM="${SYM:-luaV_execute}"
mkdir -p "$OUTDIR"
die() { echo "meta_gate: $*" >&2; exit 1; }
[ -x "$DCC" ] || die "DCC 不存在: $DCC（先 cargo build --release -j2）"

# ---- 臂组：三根砝码各两档 + 组合验证臂 + FN_SHIFT 噪声对照 ----
if [ -z "${ARMSPEC:-}" ]; then
  ARMSPEC="pad32:DCC_HANDLER_PAD=32 pad128:DCC_HANDLER_PAD=128 exec2:DCC_EXEC_PAD=2 exec5:DCC_EXEC_PAD=5 take1:DCC_TAKEN_PAD=1 take3:DCC_TAKEN_PAD=3 mix:DCC_EXEC_PAD=2:DCC_TAKEN_PAD=2 null:DCC_FN_SHIFT=0 sh64:DCC_FN_SHIFT=64 sh256:DCC_FN_SHIFT=256"
fi
# 每臂记下 (name, Δ条/步, Δtaken/步)；ΔKB 由产物实测。
# 注意：臂定义串不能用 awk 拆——`out=out (out=="")?"":" " $i` 会被 mawk 当函数调用解析，
# 结果每臂的 env 都是空串，所有臂其实是同一个二进制（这种错不会报错，只会让计时自比自）。
# 这里一律用 bash 参数展开。
knob_val() { # 取本臂的 DCC_EXEC_PAD 值 = 每步多执行的条数
  case "$1" in *DCC_EXEC_PAD=*) x="${1##*DCC_EXEC_PAD=}"; echo "${x%%:*}";; *) echo 0;; esac
}
knob_take() { # 取本臂的 DCC_TAKEN_PAD 值 = 每步多出的 taken 分支数
  case "$1" in *DCC_TAKEN_PAD=*) x="${1##*DCC_TAKEN_PAD=}"; echo "${x%%:*}";; *) echo 0;; esac
}

# ---- 造臂：只重编含大 switch/标签的那个 TU，再让 make 只重链接 ----
build_arm() { # $1=dst $2=env 串（可空）
  local dst="$1" envs="$2"
  if [ "$MODE" = sqlite ]; then
    ( cd "$SQLITE_SRC" && env $envs "$DCC" -O2 -c -o sqlite3.o sqlite3.c >/dev/null 2>&1 \
        && env $envs "$DCC" -O2 -o /tmp/meta_sq_shell shell.c sqlite3.o -lm >/dev/null 2>&1 ) || return 1
    cp /tmp/meta_sq_shell "$dst" || return 1
  else
    [ -f "$LUA_SRC/src/lua" ] || die "先跑一次 scripts/bench_lua_sqlite.sh 或手工 make posix 建立基线"
    ( cd "$LUA_SRC/src" && env $envs "$DCC" -O2 -std=gnu99 -DLUA_USE_POSIX -c -o lvm.o lvm.c >/dev/null 2>&1 \
        && make posix CC="$DCC" MYCFLAGS="-std=gnu99 -O2" AR="ar rcu" RANLIB=ranlib >/dev/null 2>&1 ) || return 1
    cp "$LUA_SRC/src/lua" "$dst" || return 1
  fi
  chmod +x "$dst"
}

sym_bytes() { # $1=binary → 符号字节数
  nm -S "$1" 2>/dev/null | awk -v s="$SYM" 'NF>=4 && $4==s {print $2}' |
    awk '{v=0; h=$1; for(i=1;i<=length(h);i++){ c=substr(h,i,1); d=index("0123456789abcdef",tolower(c))-1; v=v*16+d } print v }'
}
sym_insns() { # $1=binary → 函数体内指令数（防续行口径）
  objdump -d -M intel --no-show-raw-insn "$1" 2>/dev/null |
    awk -v s="$SYM" '$0 ~ ("<" s ">:"){f=1;next} f&&/^[0-9a-f]+ <.*>:/{exit} f&&/^[[:space:]]+[0-9a-f]+:/{n++} END{print n+0}'
}

# 基准臂 = 不带任何旋钮
build_arm "$OUTDIR/base" "" || die "基准臂构建失败"
BB=$(sym_bytes "$OUTDIR/base"); BI=$(sym_insns "$OUTDIR/base")
[ "${BB:-0}" -gt 0 ] || die "拿不到 $SYM 的大小（nm 里没这个符号？）"
echo "base: $SYM bytes=$BB insns=$BI"

# ---- 逐臂构建并记录三轴 ----
: > "$OUTDIR/axes"
for spec in $ARMSPEC; do
  name="${spec%%:*}"
  rest="${spec#*:}"
  envs="${rest//:/ }"
  dst="$OUTDIR/$name"
  build_arm "$dst" "$envs" || { echo "meta_gate: 臂 $name 构建失败，跳过" >&2; continue; }
  b=$(sym_bytes "$dst"); i=$(sym_insns "$dst")
  x2=$(knob_val "$spec"); x3=$(knob_take "$spec")
  [ -n "$x2" ] || x2=0
  [ -n "$x3" ] || x3=0
  printf "%s %s %s %s %s %s\n" "$name" "$dst" "$b" "$i" "$x2" "$x3" >> "$OUTDIR/axes"
  printf "臂 %-8s env=[%s] bytes=%s insns=%s Δ条/步=%s Δtaken/步=%s\n" "$name" "$envs" "$b" "$i" "$x2" "$x3" >&2
done

# ---- 同场配对计时：base 是下标 0，gcc 作为漂移协变量一起跑 ----
GCC="${GCC:-/tmp/bench_lua_sqlite.txt.artifacts/lua_gcc_O2}"
ARG_LUA=/tmp/fib.lua
[ -f "$ARG_LUA" ] || { printf 'local function fib(n) if n<2 then return n end return fib(n-1)+fib(n-2) end print(fib(32))\n' > "$ARG_LUA"; }
if [ "$MODE" = sqlite ]; then
  # sqlite 模式的 ARG 是经 stdin 喂给 shell 的 SQL 文件（沿用 bench_lua_sqlite.sh 的那一份）
  RUNARG="${SQLARG:-/tmp/bench_sql.sql}"
  [ -f "$RUNARG" ] || die "MODE=sqlite 需要 SQLARG=<sql 文件>（先用 scripts/bench_lua_sqlite.sh 生成）"
else
  RUNARG="$ARG_LUA"
fi
ARMSTR="base=$OUTDIR/base"
while read -r name path b i x2 x3; do ARMSTR="$ARMSTR $name=$path"; done < "$OUTDIR/axes"
[ -x "$GCC" ] && ARMSTR="$ARMSTR gcc=$GCC"
echo "--- ab_pair: $ARMSTR"
ARMS="$ARMSTR" R="$R" CPU="$CPU" BASE=0 RAW="$RAW" ARG="$RUNARG" \
  bash "$HERE/ab_pair.sh" "$MODE" | tee "$OUTDIR/ab.txt"

# ---- 拟合三斜率 + Spearman ρ ----
BB0=$(sym_bytes "$OUTDIR/base")   # ΔKB 的零点：必须是基准臂，不是第一个扰动臂
echo "--- 拟合与秩相关（见下方表格）---"
# 组装「臂下标 → 名字」的顺序（与 ab_pair 的 ARMS 顺序严格一致）
NAMES_CSV="base"
while read -r name path b i x2 x3; do NAMES_CSV="$NAMES_CSV $name"; done < "$OUTDIR/axes"
[ -x "$GCC" ] && NAMES_CSV="$NAMES_CSV gcc"

awk -v names="$NAMES_CSV" -v axesf="$OUTDIR/axes" -v rawf="$RAW" -v bb0="$BB0" -v steps="${STEPS:-38770357}" '
  function sortv(v, m,   i, j, t) { for (i=1;i<=m;i++) for (j=i+1;j<=m;j++) if (v[j]<v[i]) { t=v[i]; v[i]=v[j]; v[j]=t } }
  function median(v, m) { sortv(v, m); return v[int((m+1)/2)] }
  BEGIN { ns=split(names, nm, " ") }
  { rd[$1 "," $2] = $3; if ($1+0 > mx) mx=$1+0 }
  END {
    RN = mx + 1
    # 每臂绝对中位 ms
    for (a=0; a<ns; a++) { for (i=0;i<RN;i++) tmp[i+1]=rd[i "," a]; mabs[a]=median(tmp, RN) }
    # 三轴
    na=0
    while ((getline line < axesf) > 0) { split(line, p, " "); na++; an[na]=p[1]; ab[na]=p[3]; ai[na]=p[4]; ax2[na]=p[5]+0; ax3[na]=p[6]+0 }
    close(axesf)
    # 把臂名映射到下标
    for (a=0;a<ns;a++) idx[nm[a+1]]=a
    printf "%-10s %9s %9s %8s %8s %10s %10s\n", "臂", "ΔKB", "Δ条/步", "Δtak/步", "ms", "Δms", "Δms/步(µs)"
    nf=0
    for (k=1;k<=na;k++) {
      a=idx[an[k]]
      dkb=(ab[k]-bb0)/1024; dms=mabs[a]-mabs[idx["base"]]
      isnull = (substr(an[k],1,2)=="sh" || an[k]=="null")
      if (!isnull) { nf++; X1[nf]=dkb; X2[nf]=ax2[k]; X3[nf]=ax3[k]; Y[nf]=dms }
      printf "%-10s %9.3f %9.2f %8.2f %8.2f %10.3f %10.3f %s\n", an[k], dkb, ax2[k], ax3[k], mabs[a], dms, dms*1e6/steps, (isnull ? "(噪声对照，不入拟合)" : "")
    }
    if (nf < 4) { print "臂数不足，无法拟合"; exit 1 }
    # 过原点最小二乘 3 元
    for (i=1;i<=3;i++) for (j=1;j<=3;j++) A[i,j]=0
    for (k=1;k<=nf;k++) { x[1]=X1[k]; x[2]=X2[k]; x[3]=X3[k]
      for (i=1;i<=3;i++) { for (j=1;j<=3;j++) A[i,j]+=x[i]*x[j]; B[i]+=x[i]*Y[k] } }
    # 不另做对称填充：正规矩阵 XtX 在上一个双重循环里已逐 (i,j) 累加，本身就是对称的。
    # （这里曾经写成 `for (j=1;i<=j;j++)`，条件随 j 递增恒真 ⇒ 死循环。）
    # 注：awk 程序包在单引号里，注释与字串一律不得出现撇号，否则 shell 会在那一行把脚本斩断。
    solve(A, B, 3, C)
    printf "斜率  a=%.3f ms/KB   b=%.4f ms/(条/步)   c=%.4f ms/(taken/步)\n", C[1], C[2], C[3]
    printf "换算  b=%.3f ms/百万执行条   c=%.3f ms/百万 taken   (基准臂 %.2f ms，每步 %.2f ns)\n",
           C[2]*1e6/steps, C[3]*1e6/steps, mabs[idx["base"]], mabs[idx["base"]]*1e6/steps
    # 残差与噪声带
    sse=0; for (k=1;k<=nf;k++) { pr=C[1]*X1[k]+C[2]*X2[k]+C[3]*X3[k]; e=Y[k]-pr; sse+=e*e; P[k]=pr }
    rms=sqrt(sse/nf)
    smax=0; dupabs=-1; for (k=1;k<=na;k++) { a=idx[an[k]]; d2=mabs[a]-mabs[idx["base"]]; if (d2<0) d2=-d2;
      if (substr(an[k],1,2)=="sh" && d2>smax) smax=d2
      if (an[k]=="null" && d2>dupabs) dupabs=d2 }
    printf "拟合 RMS=%.3f ms；dup 地板（逐字节相同的 null 臂）=%.3f ms（%.2f%%）；FN_SHIFT 位置带最大=%.3f ms\n",
           rms, dupabs, (dupabs<0?0:dupabs*100/mabs[idx["base"]]), smax
    printf "判据用法：任何机制的折算收益必须 ≥ 2×位置带（%.1f ms）才能算收益；落在 %.1f ms 内一律判「不可判」。\n",
           2*smax, 2*smax
    # Spearman ρ：预测 vs 实测（全体拟合臂）
    for (k=1;k<=nf;k++) { r1=0; r2=0; for (j=1;j<=nf;j++) { if (P[j]<P[k]) r1++; if (Y[j]<Y[k]) r2++ } R1[k]=r1+1; R2[k]=r2+1 }
    ds=0; for (k=1;k<=nf;k++) ds+=(R1[k]-R2[k])*(R1[k]-R2[k])
    rho = 1 - 6*ds/(nf*(nf*nf-1))
    printf "Spearman ρ(预测,实测)=%.3f  判据线 ρ≥0.80 ⇒ %s\n", rho, (rho>=0.80?"代理可用，进入 S0":"代理失效，停在判据层修判据")
    printf "绝对斜率 |a|<0.3 ms/KB ⇒ 前端不是瓶颈：%s\n", (C[1]<0.3 && C[1]>-0.3 ? "成立，P2/P3/P6 的字节叙事必须重写" : "不成立，字节类机制可以计入预期")
  }
  function solve(A, B, n, C,   i, j, k, t, piv) {
    for (k=1;k<=n;k++) {
      piv=k; for (i=k+1;i<=n;i++) if (A[i,k] > A[piv,k]) piv=i
      for (j=1;j<=n+1;j++) { t=A[k,j]; A[k,j]=A[piv,j]; A[piv,j]=t }
      if (A[k,k]==0) { C[k]=0; continue }
      for (i=k+1;i<=n;i++) { f=A[i,k]/A[k,k]; for (j=k;j<=n;j++) A[i,j]-=f*A[k,j]; B[i]-=f*B[k] }
    }
    for (i=n;i>=1;i--) { s=B[i]; for (j=i+1;j<=n;j++) s-=A[i,j]*C[j]; C[i]=s/A[i,i] }
  }
' "$RAW"

# 收尾：把 lua 树恢复成无旋钮的基线产物。否则下一次 bench / 差分用的 lvm.o 会带着最后一个臂的旋钮。
cd "$LUA_SRC/src" && "$DCC" -O2 -std=gnu99 -DLUA_USE_POSIX -c -o lvm.o lvm.c >/dev/null 2>&1 && \
  make posix CC="$DCC" MYCFLAGS="-std=gnu99 -O2" AR="ar rcu" RANLIB=ranlib >/dev/null 2>&1 && \
  echo "[收尾] $LUA_SRC/src 已回到无旋钮基线产物"
