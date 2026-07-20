#!/bin/bash
set -e
export PATH=/home/ds/.cargo/bin:$PATH
cd /home/ds/dcc
echo "=== -O0 ==="
./target/release/dcc -std=gnu23 -O0 -o /tmp/m16_attr_o0 tests/fixtures/m16_function_attr.c
/tmp/m16_attr_o0
echo "o0_exit=$?"
echo "=== -O2 ==="
./target/release/dcc -std=gnu23 -O2 -o /tmp/m16_attr_o2 tests/fixtures/m16_function_attr.c
/tmp/m16_attr_o2
echo "o2_exit=$?"
