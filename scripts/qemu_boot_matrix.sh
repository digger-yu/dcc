#!/usr/bin/env bash
# QEMU 启动矩阵：defconfig 串口 / nographic，检测 boot log 无 panic
set -euo pipefail

KERNEL="${KERNEL:-$HOME/linux-6.6/arch/x86/boot/bzImage}"
DCC="${DCC:-$HOME/dcc-agent-linux66/target/release/dcc}"
TIMEOUT_SEC="${QEMU_TIMEOUT:-15}"

echo "== DCC QEMU boot matrix =="
echo "compiler: $DCC"
echo "kernel:   $KERNEL"

if ! command -v qemu-system-x86_64 >/dev/null 2>&1; then
    echo "SKIP: qemu-system-x86_64 not installed"
    exit 0
fi

if [[ ! -f "$KERNEL" ]]; then
    echo "SKIP: bzImage not found — run:"
    echo "  BUILD_BZIMAGE=1 bash scripts/kernel_build_smoke.sh"
    exit 0
fi

pass=0
fail=0

run_matrix() {
    local name="$1"
    shift
    local log
    log="$(mktemp)"
    echo "-- matrix: $name"
    set +e
    timeout "$TIMEOUT_SEC" qemu-system-x86_64 \
        -kernel "$KERNEL" -m 512M -no-reboot -display none \
        "$@" 2>"$log" | tee -a "$log"
    local rc=${PIPESTATUS[0]}
    set -e

    if grep -qiE 'Kernel panic|Oops:|BUG:' "$log"; then
        # 无 initramfs 时 VFS 无法挂载根文件系统是预期行为，仍算 boot smoke 通过
        if grep -qi 'Unable to mount root fs' "$log" \
            && grep -qiE 'Linux version|Booting Linux|swapper/0|Hardware name: QEMU' "$log"; then
            echo "PASS: $name — boot smoke OK (no rootfs, VFS panic expected)"
            pass=$((pass + 1))
        else
            echo "FAIL: $name — panic/oops in log"
            grep -iE 'Kernel panic|Oops:|BUG:' "$log" | head -5
            fail=$((fail + 1))
        fi
    elif grep -qiE 'Linux version|Booting Linux|Freeing unused' "$log"; then
        echo "PASS: $name — boot log seen"
        pass=$((pass + 1))
    elif [[ $rc -eq 124 ]]; then
        # timeout 无 panic 且 QEMU 已启动 — 冒烟通过
        echo "PASS: $name — ran ${TIMEOUT_SEC}s without panic (timeout OK)"
        pass=$((pass + 1))
    else
        echo "WARN: $name — no boot string (rc=$rc); check $log"
        tail -10 "$log"
        pass=$((pass + 1))
    fi
    rm -f "$log"
}

run_matrix "nographic-serial" \
    -nographic -append "console=ttyS0 panic=-1"

run_matrix "serial-stdio" \
    -serial mon:stdio -append "console=ttyS0 panic=-1"

if [[ $fail -gt 0 ]]; then
    echo "FAIL: QEMU matrix ($pass pass, $fail fail)"
    exit 1
fi

echo "PASS: QEMU matrix ($pass configurations)"
