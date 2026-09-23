#!/usr/bin/env bash
# QEMU 启动矩阵：
#   1) boot-smoke  — 无 initrd，检测 boot log + VFS panic（向后兼容）
#   2) init-test   — 有 initrd，检测 "Run /init as init process"
set -euo pipefail

KERNEL="${KERNEL:-$HOME/linux-7.1/arch/x86/boot/bzImage}"
DCC_REPO="${DCC_REPO:-$(cd "$(dirname "$0")/.." && pwd)}"
DCC="${DCC:-$DCC_REPO/target/release/dcc}"
INITRD="${INITRD:-$DCC_REPO/initramfs/initramfs.cpio.gz}"
TIMEOUT_SEC="${QEMU_TIMEOUT:-15}"
SKIP_INIT_TEST="${SKIP_INIT_TEST:-0}"

echo "== DCC QEMU boot matrix =="
echo "compiler: $DCC"
echo "kernel:   $KERNEL"
echo "initrd:   $INITRD"

if [[ ! -f "$KERNEL" ]]; then
    if [[ "${REQUIRE_BZIMAGE:-0}" == "1" || "${REQUIRE_QEMU_BOOT:-0}" == "1" ]]; then
        echo "FAIL: bzImage not found"
        exit 1
    fi
    echo "SKIP: bzImage not found — run:"
    echo "  BUILD_BZIMAGE=1 bash scripts/kernel_build_smoke.sh"
    exit 0
fi

# Once a bootable artifact exists, QEMU becomes a hard gate by default.
REQUIRE_QEMU_BOOT="${REQUIRE_QEMU_BOOT:-1}"

if ! command -v qemu-system-x86_64 >/dev/null 2>&1; then
    if [[ "$REQUIRE_QEMU_BOOT" == "1" ]]; then
        echo "FAIL: qemu-system-x86_64 not installed"
        exit 1
    fi
    echo "SKIP: qemu-system-x86_64 not installed"
    exit 0
fi

pass=0
fail=0

# --- boot-smoke: 无 initrd，检测基本启动 + VFS panic --------------------
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
        echo "FAIL: $name — timed out without required boot log string"
        fail=$((fail + 1))
    else
        echo "FAIL: $name — no required boot log string (rc=$rc); check $log"
        tail -10 "$log"
        fail=$((fail + 1))
    fi
    rm -f "$log"
}

# --- init-test: 有 initrd，检测内核到达 init ------------------------------
run_init_test() {
    local name="$1"
    shift
    local log
    log="$(mktemp)"
    echo "-- init: $name"
    set +e
    timeout "$TIMEOUT_SEC" qemu-system-x86_64 \
        -kernel "$KERNEL" -initrd "$INITRD" \
        -m 512M -no-reboot -display none \
        "$@" 2>"$log" | tee -a "$log"
    local rc=${PIPESTATUS[0]}
    set -e

    # 主要验证：内核尝试执行 /init
    if grep -q 'Run /init as init process' "$log"; then
        echo "PASS: $name — kernel reached init (/init)"
        # 如果 init 脚本也成功执行，额外确认
        if grep -q 'DCC kernel: init reached successfully' "$log"; then
            echo "  (+ init script executed successfully)"
        fi
        pass=$((pass + 1))
    elif grep -q 'No working init found' "$log"; then
        echo "FAIL: $name — kernel could not find/init init"
        fail=$((fail + 1))
    elif grep -qiE 'Kernel panic|Oops:|BUG:' "$log"; then
        echo "FAIL: $name — panic/oops before reaching init"
        grep -iE 'Kernel panic|Oops:|BUG:' "$log" | head -5
        fail=$((fail + 1))
    elif [[ $rc -eq 124 ]]; then
        echo "FAIL: $name — timed out before init (rc=$rc)"
        tail -10 "$log"
        fail=$((fail + 1))
    else
        echo "FAIL: $name — no init log found (rc=$rc); check $log"
        tail -10 "$log"
        fail=$((fail + 1))
    fi
    rm -f "$log"
}

# --- 执行 boot-smoke 矩阵（无 initrd）------------------------------------
run_matrix "nographic-serial" \
    -nographic -append "console=ttyS0 panic=-1"

run_matrix "serial-stdio" \
    -serial mon:stdio -append "console=ttyS0 panic=-1"

# --- 执行 init 测试（有 initrd）------------------------------------------
if [[ "$SKIP_INIT_TEST" == "0" ]]; then
    if [[ -f "$INITRD" ]]; then
        run_init_test "initramfs-nographic" \
            -nographic -append "console=ttyS0 panic=-1"
    else
        echo "-- SKIP: init-test (initramfs not found; run: bash scripts/build_initramfs.sh)"
    fi
fi

if [[ $fail -gt 0 ]]; then
    echo "FAIL: QEMU matrix ($pass pass, $fail fail)"
    exit 1
fi

echo "PASS: QEMU matrix ($pass configurations)"
