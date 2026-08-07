#!/usr/bin/env python3
"""复现 current_thread_info 宏展开差异。"""
import os, subprocess, tempfile

KERNEL = os.path.expanduser("~/linux-7.1")
DCC = os.path.expanduser("~/dcc-agent-linux66/target/release/dcc")
FLAGS = (
    f"-nostdinc -I{KERNEL}/include -I{KERNEL}/arch/x86/include "
    f"-I{KERNEL}/include/generated -I{KERNEL}/arch/x86/include/generated "
    f"-include {KERNEL}/include/linux/kconfig.h -D__KERNEL__ -std=gnu11 -E"
)

snippet = r"""
#include "linux/compiler-version.h"
#include "linux/kconfig.h"
#include "linux/compiler_types.h"
#include "linux/thread_info.h"
#include "linux/bitops.h"
void f(void) {
    (void)test_syscall_work(SECCOMP);
}
"""

path = "/tmp/ti_bitops_test.c"
open(path, "w").write(snippet)
for name, exe in [("gcc", "gcc"), ("dcc", DCC)]:
    r = subprocess.run(f"{exe} {FLAGS} {path}", shell=True, capture_output=True, text=True)
    print(f"=== {name} rc={r.returncode} ===")
    if r.stderr:
        print(r.stderr[:800])
    for l in r.stdout.splitlines():
        if "void f" in l or "current_thread" in l or "secure" in l:
            print(l[:500])

# 纯宏链（不依赖头文件）
macro_snip = r"""
#define current ((void*)0)
#define current_thread_info() ((struct thread_info *)current)
struct thread_info { unsigned long syscall_work; };
#define bitop(op, nr, addr) \
  ((__builtin_constant_p(nr) ? const##op(nr, addr) : op(nr, addr)))
#define _test_bit(nr, addr) (addr)
#define test_bit(nr, addr) bitop(_test_bit, nr, addr)
#define test_syscall_work(fl) test_bit(1, &current_thread_info()->syscall_work)
void g(void) { (void)test_syscall_work(X); }
"""
path2 = "/tmp/ti_pure_test.c"
open(path2, "w").write(macro_snip)
for name, exe in [("gcc", "gcc"), ("dcc", DCC)]:
    r = subprocess.run(f"{exe} -std=gnu11 -E {path2}", shell=True, capture_output=True, text=True)
    print(f"\n=== pure {name} ===")
    for l in r.stdout.splitlines():
        if "void g" in l or "current_thread" in l or "struct thread_info" in l:
            print(l)
