#!/bin/bash
cd /home/ds/linux-6.1
DCC=/home/ds/dcc-debug/target/release/dcc
GCC_INC=$(gcc -print-file-name=include)

echo "=== Test: is CONFIG_HZ defined after -include? ==="
echo '__CONFIG_HZ_VALUE__' | $DCC -nostdinc -isystem "$GCC_INC" \
  -I./arch/x86/include \
  -I./arch/x86/include/generated \
  -I./include \
  -include ./include/linux/kconfig.h \
  -D__KERNEL__ \
  -E -P -x c - 2>&1

echo ""
echo "=== Test: What does CONFIG_HZ expand to? ==="
echo 'CONFIG_HZ' | $DCC -nostdinc -isystem "$GCC_INC" \
  -I./arch/x86/include \
  -I./arch/x86/include/generated \
  -I./include \
  -include ./include/linux/kconfig.h \
  -D__KERNEL__ \
  -E -P -x c - 2>&1

echo ""
echo "=== Test: what does HZ expand to? ==="
echo '#include <linux/jiffies.h>
HZ_VALUE_IS: HZ' | $DCC -nostdinc -isystem "$GCC_INC" \
  -I./arch/x86/include \
  -I./arch/x86/include/generated \
  -I./include \
  -include ./include/linux/kconfig.h \
  -D__KERNEL__ \
  -E -P -x c - 2>&1 | grep 'HZ_VALUE_IS'

echo ""
echo "=== Test: check include/linux/jiffies.h HZ check ==="
grep -n 'Invalid value of HZ' include/linux/jiffies.h
head -50 include/linux/jiffies.h | grep -n 'HZ\|error'
