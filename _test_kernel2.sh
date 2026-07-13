#!/bin/bash
cd /home/ds/linux-6.1
DCC=/home/ds/dcc-debug/target/release/dcc
GCC_INC=$(gcc -print-file-name=include)

# Try a simpler file first
$DCC -nostdinc -isystem "$GCC_INC" \
  -I./arch/x86/include \
  -I./arch/x86/include/generated \
  -I./include \
  -I./arch/x86/include/uapi \
  -I./arch/x86/include/generated/uapi \
  -I./include/uapi \
  -I./include/generated/uapi \
  -include ./include/linux/compiler-version.h \
  -include ./include/linux/kconfig.h \
  -D__KERNEL__ \
  -c kernel/panic.c -o /tmp/panic.o 2>&1 | head -40
echo "EXIT: $?"

echo ""
echo "=== Try even simpler: kernel/locking/spinlock.c ==="
$DCC -nostdinc -isystem "$GCC_INC" \
  -I./arch/x86/include \
  -I./arch/x86/include/generated \
  -I./include \
  -D__KERNEL__ \
  -E kernel/locking/spinlock.c 2>&1 | tail -5
echo "EXIT: $?"

echo ""
echo "=== Try kernel/exit.c ==="
$DCC -nostdinc -isystem "$GCC_INC" \
  -I./arch/x86/include \
  -I./arch/x86/include/generated \
  -I./include \
  -D__KERNEL__ \
  -E kernel/exit.c 2>&1 | wc -l
echo "Preprocessed lines"

echo ""
echo "=== Simple: kernel/sys_ni.c ==="
$DCC -nostdinc -isystem "$GCC_INC" \
  -I./arch/x86/include \
  -I./arch/x86/include/generated \
  -I./include \
  -D__KERNEL__ \
  -c kernel/sys_ni.c -o /tmp/sys_ni.o 2>&1 | head -20
echo "EXIT: $?"
