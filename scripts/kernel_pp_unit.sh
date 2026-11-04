#!/usr/bin/env bash
# 把内核 TU 的预处理产物（dcc -E 的结果）做成「最小复现件」的脚本对：
#   make-i.sh <BUILD 目录> <目标 .o（相对 BUILD）> [输出 .i 路径]
# 做法与 scripts/kernel_replay_detached.sh 同源（读 `.<目标>.o.cmd` 里的 `savedcmd_…`，
# 在第一个 `;` 处截断丢掉 objtool 那一段），差别只有两点：
#   * 把 `-c -o <目标.o>` 换成 `-E`，产物写到标准输出；
#   * 丢掉 `-Wp,-MMD,…`（预处理依赖文件与最小件无关）。
# 为什么需要它：`__pt_no_sw_bit` 那类「折叠停在哪一格」的问题，只有拿一个**不依赖任何
# 头文件**的单文件才能反复试、才能自动缩减；从 `.i` 出发还能保证语法与真树逐字相同
# （宏都已展开，不会因为改 include 路径而改变形状）。
set -uo pipefail

bdir="${1:?BUILD 目录（O= 的那个）}"
tgt="${2:?目标，相对 BUILD，如 drivers/iommu/generic_pt/fmt/iommu_amdv1.o}"
out="${3:-}"

python3 - "$bdir" "$tgt" "$out" <<'PY'
import re, shlex, subprocess, sys
bdir, tgt, out = sys.argv[1], sys.argv[2], sys.argv[3]
cmdfile = f"{bdir}/{tgt}.cmd".replace(f"/{tgt.split('/')[-1]}.cmd", "") + ""
# .cmd 的名字是 `.<basename>.o.cmd`，放在目标的目录里
d, b = tgt.rsplit('/', 1)
cmdfile = f"{bdir}/{d}/.{b}.cmd"
text = open(cmdfile, encoding='utf-8', errors='replace').read()
m = re.search(r'^savedcmd_' + re.escape(tgt) + r'\s*:=(.*)$', text, re.M)
if not m:
    sys.exit(f"{cmdfile}: 找不到 savedcmd_{tgt}")
# 只取编译那一段：`;` 之后是 objtool/fixdep，它们不是编译器旗标
args = shlex.split(m.group(1).split(';')[0].strip())
keep = []
i = 0
while i < len(args):
    a = args[i]
    if a == '-o':
        i += 2
        continue
    if a in ('-c', '-Werror'):
        i += 1
        continue
    if a.startswith('-Wp,-MMD'):
        i += 1
        continue
    keep.append(a)
    i += 1
keep.insert(1, '-E')
r = subprocess.run(keep, cwd=bdir, capture_output=True, text=True)
if r.returncode != 0:
    sys.exit(f"-E 失败 rc={r.returncode}: {r.stderr[:300]}")
if out:
    open(out, 'w').write(r.stdout)
    print(f"{out}: {r.stdout.count(chr(10))} 行")
else:
    sys.stdout.write(r.stdout)
PY
