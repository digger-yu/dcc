#!/bin/bash
DCC=/home/ds/dcc/target/release/dcc
SRC=/home/ds/dcc
PASS=0
FAIL=0
for t in huge hugeval ieee754 nan offsetof_full nested_full cwsc computed_goto asm_goto_outputs atomic_c fpclassify float_lit; do
  echo "=== test_$t ==="
  out=$($DCC $SRC/examples/dev/test_$t.c -o /tmp/test_$t 2>&1)
  if [ $? -ne 0 ]; then
    echo "  COMPILE FAILED: $out"
    FAIL=$((FAIL+1))
    continue
  fi
  /tmp/test_$t 2>&1
  rc=$?
  if [ $rc -eq 0 ]; then
    echo "  PASS (exit=$rc)"
    PASS=$((PASS+1))
  else
    echo "  FAIL (exit=$rc)"
    FAIL=$((FAIL+1))
  fi
done
echo "===="
echo "PASS=$PASS FAIL=$FAIL"
