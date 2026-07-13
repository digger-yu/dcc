#!/bin/bash
cd /home/ds/linux-6.1
DCC=/home/ds/dcc-debug/target/release/dcc
GCC_INC=$(gcc -print-file-name=include)

echo "=== Test: include guard effectiveness ==="
echo '#include <linux/types.h>
#include <linux/types.h>
done' | $DCC -nostdinc -isystem "$GCC_INC" \
  -I./arch/x86/include -I./arch/x86/include/generated -I./include \
  -include ./include/linux/kconfig.h -D__KERNEL__ \
  -E -P -x c - 2>/dev/null | wc -l

echo ""
echo "=== Test: single include output ==="
echo '#include <linux/types.h>
done' | $DCC -nostdinc -isystem "$GCC_INC" \
  -I./arch/x86/include -I./arch/x86/include/generated -I./include \
  -include ./include/linux/kconfig.h -D__KERNEL__ \
  -E -P -x c - 2>/dev/null | wc -l

echo ""
echo "=== Test: count unique vs duplicate #include lines ==="
$DCC -nostdinc -isystem "$GCC_INC" \
  -I./arch/x86/include -I./arch/x86/include/generated -I./include \
  -I./arch/x86/include/uapi -I./arch/x86/include/generated/uapi \
  -I./include/uapi -I./include/generated/uapi \
  -include ./include/linux/compiler-version.h \
  -include ./include/linux/kconfig.h -D__KERNEL__ \
  -E kernel/panic.c 2>/dev/null | sort | uniq -c | sort -rn | head -20

echo ""
echo "=== Test: GCC for comparison ==="
gcc -nostdinc -isystem "$GCC_INC" \
  -I./arch/x86/include -I./arch/x86/include/generated -I./include \
  -I./arch/x86/include/uapi -I./arch/x86/include/generated/uapi \
  -I./include/uapi -I./include/generated/uapi \
  -include ./include/linux/compiler-version.h \
  -include ./include/linux/kconfig.h -D__KERNEL__ \
  -E kernel/panic.c 2>/dev/null | wc -l
echo "GCC lines"
