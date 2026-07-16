#!/bin/bash
export PATH="/usr/bin:/bin:$HOME/.cargo/bin:$PATH"
DCC=/home/ds/dcc/target/release/dcc
sed -n '1921,1930p' /home/ds/gcc-src/libiberty/regex.c > /tmp/tr_def.c
echo "==== preprocess TRANSLATE def only ===="
$DCC -E /tmp/tr_def.c 2>&1
