#!/bin/sh
# 运行时闭环 initramfs 的 /init：先搭起 proc/sys/dev，再跑 /t
# （系统调用/fork/exec/wait/管道/定时器/中断/ext4），最后走模块闭环。
# 所有参与 dcc-vs-gcc 对比的行都用 RT| / MOD| 前缀，内核日志不参与对比。
mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t devtmpfs devtmpfs /dev 2>/dev/null || true
mkdir -p /mnt

# --- 诊断行（不参与 dcc-vs-gcc 对比：无 RT|/MOD| 前缀）------------------
echo "DIAG|proc_count|$(ls /proc 2>/dev/null | wc -l)"
echo "DIAG|proc_version|$(cat /proc/version 2>&1 | head -c 50)"
echo "DIAG|proc_modules|$(cat /proc/modules 2>&1 | head -c 80)"
echo "DIAG|proc_intr|$(cat /proc/interrupts 2>&1 | head -c 80)"
echo "DIAG|dev_list|$(ls /dev 2>&1 | head -c 200)"
echo "DIAG|dev_vda|$(ls -l /dev/vda 2>&1 | head -c 80)"
# ext4 journal superblock 物理块 8（镜像上 magic c03b3998）：验证块设备读路径
echo "DIAG|jsb|$(dd if=/dev/vda bs=4096 skip=8 count=1 2>/dev/null | hexdump -C 2>/dev/null | head -1 | head -c 90)"
# 网卡/总线探针（仅诊断，不参与对比）
echo "DIAG|net_list|$(ls /sys/class/net 2>&1 | tr '\n' ' ')"
echo "DIAG|virtio_devs|$(ls /sys/bus/virtio/devices 2>/dev/null | tr '\n' ' ')"
echo "DIAG|virtio_bind|$(for d in /sys/bus/virtio/devices/*; do [ -e "$d" ] && echo "$(basename $d):$(basename $(readlink -f $d/driver 2>/dev/null) 2>/dev/null)"; done | tr '\n' ' ')"
echo "DIAG|cpu_online|$(cat /sys/devices/system/cpu/online 2>&1)"
echo "DIAG|smpboot|$(dmesg | grep -a 'smp: Brought up' | head -c 80)"
echo "DIAG|pci_net|$(dmesg | grep -a 'class 0x0200' | head -c 120)"

/t
echo "RT|status|$?"

# --- 模块闭环：树内 configs.ko + out-of-tree hello.ko -------------------
if insmod /configs.ko; then echo "MOD|configs|loaded"; else echo "MOD|configs|FAILED"; fi
if insmod /hello.ko; then echo "MOD|hello|loaded"; else echo "MOD|hello|FAILED"; fi
cat /proc/hello 2>/dev/null
if grep -q '^hello ' /proc/modules; then echo "MOD|hello|listed"; else echo "MOD|hello|not-listed"; fi
dmesg | grep -q 'hello: loaded (dcc-built module)' && echo "MOD|hello|dmesg-ok"
if rmmod hello; then echo "MOD|hello|unloaded"; else echo "MOD|hello|unload-FAILED"; fi
if [ -e /proc/hello ]; then echo "MOD|proc|still-present"; else echo "MOD|proc|removed"; fi
if grep -q '^configs ' /proc/modules; then echo "MOD|configs|resident"; fi
echo "MOD|done"

poweroff -f
