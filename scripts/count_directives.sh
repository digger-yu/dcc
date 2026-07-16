#!/bin/bash
awk 'NR>=5283 && NR<=6160 && /^[[:space:]]*#/ {print NR":"$0}' /home/ds/gcc-src/libiberty/regex.c | head -40
echo "count:" $(awk 'NR>=5283 && NR<=6160 && /^[[:space:]]*#/ {c++} END{print c+0}' /home/ds/gcc-src/libiberty/regex.c)
