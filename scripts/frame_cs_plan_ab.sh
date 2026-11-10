#!/bin/bash
# callee-saved 按需化（报告 §5.42）的帧收益 A/B。
#
# 用法: bash scripts/frame_cs_plan_ab.sh <构建树> <对象相对路径...>
#   bash scripts/frame_cs_plan_ab.sh /root/pvh-lmc3 \
#        kernel/sched/fair.o kernel/sched/build_policy.o
#
# 口径：用 kbuild 自己写下的 `<dir>/.<base>.o.cmd` 命令行把同一个 TU 重编两遍 ——
# 一遍默认（按需 cs），一遍 `DCC_CS_PLAN_ALL=1`（`compute_callee_plan` 直接回旧口径
# 0x0F，四个保存槽全预留）。`hdr = 16 + 8·cs + 8·temps` 在 cs=4 时与旧的
# `48 + 8·temps` 逐字节相等，所以这是**同一个二进制**上的对照：帧大小之差就是
# 按需化本身的收益，不含「不同日期的编译器」这种混淆项。
#
# 每个函数的帧 = 序言里的 push 数 × 8 + `sub rsp, imm`（取第一条），与
# `scripts/kernel_frame_hist.sh` 的直方图口径同源，只是这里按函数名配对求和。
#
# 产物落在 `${TMPDIR:-/tmp}/frame_cs_ab.$$`，跑完保留（里面有逐函数 json）。
set -euo pipefail

BUILD=${1:?build tree (O= dir)}
shift
[ $# -gt 0 ] || { echo "usage: $0 <build-tree> <obj-rel>..." >&2; exit 2; }
DCC_BIN=${DCC_BIN:-$(cd "$(dirname "$0")/.." && pwd)/target/release/dcc}
TMP=${TMPDIR:-/tmp}/frame_cs_ab.$$
mkdir -p "$TMP"
echo "build=$BUILD dcc=$DCC_BIN work=$TMP"

rebuild() { # <obj-rel> <out.o> [ENV=VAL ...]
    local obj=$1 out=$2; shift 2
    local dir base cmdfile cmd
    dir=$(dirname "$obj"); base=$(basename "$obj" .o)
    cmdfile="$BUILD/$dir/.$base.o.cmd"
    [ -f "$cmdfile" ] || { echo "no cmd file: $cmdfile" >&2; return 2; }
    cmd=$(sed -n 's/^savedcmd_[^:]*:= //p' "$cmdfile" | head -1)
    [ -n "$cmd" ] || { echo "empty cmd: $cmdfile" >&2; return 2; }
    # 注意：`.cmd` 里 objtool 那一段也指向同一个对象（`cc … -o <obj> ; objtool … <obj>`），
    # 所以改道必须换掉**所有**出现，否则 objtool 会把构建树里那个正式对象再处理一遍。
    cmd=${cmd//$obj/$out}
    cmd=$(printf '%s' "$cmd" | sed -E "s#-Wp,-MMD,[^ ]+#-Wp,-MMD,$out.d#")
    ( cd "$BUILD" && env DCC_BIN="$DCC_BIN" "$@" bash -c "$cmd" >/dev/null 2>&1 )
}

for obj in "$@"; do
    tag=${obj//\//_}
    rebuild "$obj" "$TMP/$tag.on.o"
    rebuild "$obj" "$TMP/$tag.off.o" DCC_CS_PLAN_ALL=1
    echo "== $obj"
    python3 - "$TMP/$tag.on.o" "$TMP/$tag.off.o" "$TMP/$tag.json" <<'PY'
import json, struct, subprocess, sys, re

def load(path):
    sec = subprocess.run(["readelf", "-SW", path], capture_output=True, text=True).stdout
    sects = {}
    for line in sec.splitlines():
        m = re.match(r"\s*\[\s*(\d+)\]\s+(\S+)\s+(\S+)\s+([0-9a-fA-F]+)\s+([0-9a-fA-F]+)\s+([0-9a-fA-F]+)", line)
        if m:
            sects[int(m.group(1))] = (int(m.group(5), 16), int(m.group(6), 16))
    sym = subprocess.run(["readelf", "-sW", path], capture_output=True, text=True).stdout
    funcs = []
    for line in sym.splitlines():
        m = re.match(r"\s*\d+:\s+([0-9a-fA-F]+)\s+(\d+)\s+FUNC\s+\S+\s+\S+\s+(\S+)\s+(\S+)\s*$", line)
        if m and m.group(3).isdigit():
            funcs.append((m.group(4), int(m.group(3)), int(m.group(1), 16), int(m.group(2))))
    blob = open(path, "rb").read()
    out = {}
    for name, sidx, val, size in funcs:
        if name in out or sidx not in sects or val >= sects[sidx][1]:
            continue
        start = sects[sidx][0] + val
        raw = blob[start:start + min(size or 160, 160)]
        i, pushes, sub = 0, 0, None
        while i < min(len(raw), 160):
            if raw[i:i+4] == b"\xf3\x0f\x1e\xfa":
                i += 4
            elif raw[i] in (0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57):
                pushes += 1; i += 1
            elif raw[i] == 0x41 and i + 1 < len(raw) and 0x50 <= raw[i+1] <= 0x57:
                pushes += 1; i += 2
            elif raw[i:i+3] in (b"\x48\x89\xe5", b"\x49\x89\xe5"):
                i += 3
            elif raw[i:i+3] == b"\x48\x81\xec":
                sub = struct.unpack("<i", raw[i+3:i+7])[0]; break
            elif raw[i:i+3] == b"\x48\x83\xec":
                sub = raw[i+3]; break
            elif raw[i:i+4] == b"\x48\x8d\x64\x24":
                sub = -struct.unpack("<b", raw[i+4:i+5])[0]; break
            elif raw[i:i+4] == b"\x48\x8d\xa4\x24":
                sub = -struct.unpack("<i", raw[i+4:i+8])[0]; break
            else:
                break
        if sub is None:
            if pushes == 0:
                continue
            sub = 0
        out[name] = pushes * 8 + sub
    return out

on, off, outp = sys.argv[1], sys.argv[2], sys.argv[3]
b, a = load(on), load(off)   # a = 旧口径（四槽），b = 按需
ks = [k for k in a if k in b]
sa, sb = sum(a[k] for k in ks), sum(b[k] for k in ks)
imp = sum(1 for k in ks if b[k] < a[k])
wor = sum(1 for k in ks if b[k] > a[k])
def pct(d, p):
    v = sorted(d[k] for k in ks)
    return v[min(len(v) - 1, int(len(v) * p))]
json.dump({"on_demand": {k: b[k] for k in ks}, "four_slots": {k: a[k] for k in ks}}, open(outp, "w"))
print(f"  funcs={len(ks)}  four-slot={sa} B  on-demand={sb} B  delta={sb-sa} B "
      f"({(sb-sa)*100.0/sa:+.2f} %)  improved={imp} worse={wor}")
print(f"  p50 {pct(a,0.5)} -> {pct(b,0.5)}  p75 {pct(a,0.75)} -> {pct(b,0.75)}  max {max(a.values())} -> {max(b.values())}")
PY
done
