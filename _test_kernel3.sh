#!/bin/bash
cd /home/ds/linux-6.1
DCC=/home/ds/dcc-debug/target/release/dcc
GCC_INC=$(gcc -print-file-name=include)

echo "=== Check panic_notifier.h content ==="
cat include/linux/panic_notifier.h

echo ""
echo "=== Check COND_SYSCALL macro ==="
grep -n 'COND_SYSCALL' kernel/sys_ni.c | head -5
grep -rn 'define COND_SYSCALL' include/ | head -5

echo ""
echo "=== Preprocess sys_ni.c ==="
$DCC -nostdinc -isystem "$GCC_INC" \
  -I./arch/x86/include \
  -I./arch/x86/include/generated \
  -I./include \
  -D__KERNEL__ \
  -E kernel/sys_ni.c 2>&1 | head -60

echo ""
echo "=== Preprocess kernel/exit.c line count ==="
$DCC -nostdinc -isystem "$GCC_INC" \
  -I./arch/x86/include \
  -I./arch/x86/include/generated \
  -I./include \
  -D__KERNEL__ \
  -E kernel/exit.c 2>&1 | head -20

echo ""
echo "=== Check what happens with simple __initdata ==="
echo 'int __initdata test_var;' | $DCC -E -P -x c - 2>&1
echo "---"
echo 'int __initdata test_var;' | $DCC -c -x c - -o /tmp/test_initdata.o 2>&1
