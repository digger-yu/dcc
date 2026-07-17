#!/usr/bin/env python3
import os
import subprocess

os.chdir(os.path.expanduser("~/linux-6.6"))
dcc = os.path.expanduser("~/dcc-agent-linux66/target/release/dcc")
line = open("arch/x86/kernel/.asm-offsets.s.cmd").readline()
cmd = line.split(":=", 1)[1].strip().replace("gcc", dcc, 1)
parts = cmd.split()
out_i = "/tmp/asm-offsets-dcc.i"
new_parts = []
skip_next = False
for p in parts:
    if skip_next:
        skip_next = False
        continue
    if p == "-S":
        new_parts.append("-E")
        continue
    if p in ("-fverbose-asm", "-o"):
        if p == "-o":
            skip_next = True
        continue
    new_parts.append(p)
new_parts.append("arch/x86/kernel/asm-offsets.c")
cmd = " ".join(new_parts)
print("CMD tail:", " ".join(new_parts[-5:]))
r = subprocess.run(cmd, shell=True, capture_output=True, text=True)
print("exit", r.returncode, "stdout bytes", len(r.stdout))
if r.returncode != 0:
    print(r.stderr[:3000])
    raise SystemExit(1)
open(out_i, "w").write(r.stdout)
text = r.stdout.splitlines()
print("total lines", len(text))
for n in [5425, 5507]:
    ln = text[n - 1]
    print(f"\n=== line {n} col6={ln[5:20] if len(ln)>5 else ''} len={len(ln)} ===")
    print(ln[:1200])
    if len(ln) > 680:
        print("...[col680:760]...", repr(ln[680:760]))
