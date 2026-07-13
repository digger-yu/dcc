#!/bin/bash
cd /home/ds/linux-6.1
DCC=/home/ds/dcc-debug/target/release/dcc
GCC_INC=$(gcc -print-file-name=include)
echo "GCC include: $GCC_INC"

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
  -c init/main.c -o /tmp/main_test.o 2>&1 | head -50
echo "EXIT: $?"
