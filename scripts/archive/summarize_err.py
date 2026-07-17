#!/usr/bin/env python3
import re
text = open("/tmp/asm-offsets-new.err", errors="replace").read()
text = re.sub(r"\x1b\[[0-9;]*m", "", text)
lines = text.splitlines()
errs = [l for l in lines if "error[E" in l]
print("count", len(errs))
parse = [l for l in errs if re.search(r"error\[E0", l)]
sem = [l for l in errs if re.search(r"error\[E1", l)]
print("parse_errors", len(parse))
print("semantic_errors", len(sem))
for l in parse[:10]:
    print(l)
for l in sem:
    print(l)
