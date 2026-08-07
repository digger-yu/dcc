#!/usr/bin/env python3
import os
import subprocess

os.chdir(os.path.expanduser("~/linux-7.1"))
DCC = os.path.expanduser("~/dcc-agent-linux66/target/release/dcc")
line = open("arch/x86/kernel/.asm-offsets.s.cmd").readline()
cmd = line.split(":=", 1)[1].strip().replace("gcc", DCC, 1).replace("-S", "-E").replace(
    "arch/x86/kernel/asm-offsets.s", "/tmp/asm-offsets-dcc.i"
)
subprocess.run(cmd, shell=True, check=False)
text = open("/tmp/asm-offsets-dcc.i").read().splitlines()
patterns = ["pcpu_hot", "might_resched", "get_cpu_gdt", "get_current", "percpu"]
for pat in patterns:
    for i, ln in enumerate(text, 1):
        if pat in ln:
            print(f"=== {pat} line {i} ===")
            print(ln[:500])
            break
