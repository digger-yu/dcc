#!/usr/bin/env python3
import os
import subprocess

os.chdir(os.path.expanduser("~/linux-7.1"))
dcc = os.path.expanduser("~/dcc-agent-linux66/target/release/dcc")
line = open("arch/x86/kernel/.asm-offsets.s.cmd").readline()
cmd = line.split(":=", 1)[1].strip().replace("gcc", dcc, 1)
out = "arch/x86/kernel/asm-offsets.s"
if os.path.exists(out):
    os.remove(out)
r = subprocess.run(cmd, shell=True, capture_output=True, text=True)
err = (r.stderr or "") + (r.stdout or "")
open("/tmp/asm-offsets-new.err", "w").write(err)
print("exit", r.returncode)
lines = err.splitlines()
errors = [l for l in lines if "error[" in l or "error[E" in l]
print("error_count", len(errors))
for l in lines[:40]:
    print(l)
print("---")
for l in errors[:15]:
    print(l)
print("exists", os.path.exists(out))
