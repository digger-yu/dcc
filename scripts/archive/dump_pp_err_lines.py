#!/usr/bin/env python3
"""从 asm-offsets 预处理输出中提取语义错误行上下文。"""
import os
import subprocess

LINES = [32824, 39224, 68890, 68896]
CTX = 3

os.chdir(os.path.expanduser("~/linux-7.1"))
dcc = os.path.expanduser("~/dcc-agent-linux66/target/release/dcc")
cmd_line = open("arch/x86/kernel/.asm-offsets.s.cmd").readline()
cmd = cmd_line.split(":=", 1)[1].strip().replace("gcc", dcc, 1)
cmd = cmd.replace(" -S ", " -E ").replace(".s ", ".i ")
subprocess.run(cmd, shell=True, check=False)
pp = "arch/x86/kernel/asm-offsets.i"
if not os.path.exists(pp):
    print("missing", pp)
    raise SystemExit(1)
text = open(pp, encoding="utf-8", errors="replace").read().splitlines()
print("total_lines", len(text))
for ln in LINES:
    print(f"\n=== line {ln} ===")
    for i in range(max(0, ln - 1 - CTX), min(len(text), ln + CTX)):
        mark = ">>" if i + 1 == ln else "  "
        print(f"{mark} {i+1}: {text[i][:240]}")
