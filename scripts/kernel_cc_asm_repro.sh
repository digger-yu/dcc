#!/bin/bash
# §5.11 缺陷的可执行复现：命名 "+m" 输出与不可见 "=@ccz" 输出同处一条 asm 时，
# dcc 在 -O2 下把内存操作的地址绑到了条件码变量的帧槽上。
#
# 内核 arch/x86/include/asm/rmwcc.h 的 __GEN_RMWcc 就是这个形状，
# 于是 arch_atomic_try_cmpxchg() 换错了地址、返回垃圾条件码，
# qspinlock 拿到野 prev → mov %rax,(%rcx) → #GP → early panic。
#
# 用法: bash scripts/kernel_cc_asm_repro.sh [DCC 路径]
# 退出码：0 = 两端一致（缺陷已修）；1 = dcc 与 gcc 不一致（缺陷仍在）。
set -uo pipefail
DCC=${1:-/root/dcc/target/release/dcc}
work=$(mktemp -d /tmp/ccasm.XXXXXX)
trap 'rm -rf "$work"' EXIT

cat > "$work/t.c" <<'EOF'
#include <stdbool.h>
#include <stdio.h>

/* 复刻内核 __GEN_RMWcc：模板里只引用 %[new]/%[var]，"=@ccz" 是不可见输出。 */
static inline bool try_cmpxchg(int *v, int old, int new)
{
	bool c;
	asm volatile("lock cmpxchgl %[new], %[var]"
		     : [var] "+m" (*v), "=@ccz" (c)
		     : [new] "r" (new), "a" (old)
		     : "memory");
	return c;
}

int main(void)
{
	int v = 5;
	bool fail = try_cmpxchg(&v, 7, 9);	/* old != v → ZF=0 → 0 */
	bool ok   = try_cmpxchg(&v, 5, 3);	/* old == v → ZF=1 → 1 */
	printf("fail=%d ok=%d v=%d\n", fail, ok, v);
	return (fail == 0 && ok == 1 && v == 3) ? 0 : 1;
}
EOF

want=$(gcc -O2 -o "$work/g" "$work/t.c" && "$work/g"; echo "rc=$?")
got=$("$DCC" -O2 -o "$work/d" "$work/t.c" 2>/dev/null && "$work/d"; echo "rc=$?")

echo "gcc -O2 : $want"
echo "dcc -O2 : $got"
if [ "$want" = "$got" ]; then
	echo "RESULT: PASS（@cc 不可见输出与命名 +m 输出的绑定一致）"
	exit 0
fi
echo "RESULT: FAIL（dcc 把 %[var] 绑到了 c 的帧槽，见 docs/dcc-kernel-build-verify-report.md §5.11）"
exit 1
