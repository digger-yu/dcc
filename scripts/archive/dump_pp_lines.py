#!/usr/bin/env python3
import os
import subprocess

os.chdir(os.path.expanduser("~/linux-6.6"))
line = open("arch/x86/kernel/.asm-offsets.s.cmd").readline()
cmd = line.split(":=", 1)[1].strip().replace("-S", "-E").replace(
    "arch/x86/kernel/asm-offsets.s", "/tmp/asm-offsets.i"
)
subprocess.run(cmd, shell=True, check=False)
lines = open("/tmp/asm-offsets.i").readlines()
for start, end, label in [(4013, 4018, "4016"), (5422, 5428, "5425"), (5504, 5510, "5507")]:
    print(f"=== {label} ===")
    print("".join(lines[start:end]))
