/* 回归（docs/dcc-kernel-build-verify-report.md §5.11）：SSA→AST 重建必须把
 * `InstKind::InlineAsm::output_vars` **逐位**代回 `outputs`。
 *
 * 修复前 `output_vars` 只为「操作数本身是 Ident」的输出 push 一项，压缩过的列表让
 * 第 i 个写侧变量落到第 j 个操作数上：内核 `arch/x86/include/asm/rmwcc.h` 的
 * `__GEN_RMWcc` 形状（命名 `"+m"(*v)` + 模板里不可见的 `"=@ccz"(c)`）在 -O2 编出
 * `lock cmpxchgl %rdx, -64(%r13)` —— `%[var]` 绑到了 `c` 自己的帧槽，`v` 永不写回、
 * `c` 读回垃圾条件码，全程无诊断。qspinlock 因此拿到野 `prev`，dcc-only vmlinux 在
 * 0.15 s 就 #GP → #DF early panic。
 *
 * 各档必须与 gcc 同值：`case_*` 全部 return 0，非 0 即绑错。
 */
#include <stdbool.h>
#include <stdio.h>

static int case_rmwcc(void)
{
	int v = 5;
	bool fail, ok;
	__asm__ volatile("lock cmpxchgl %[new], %[var]"
	                 : [var] "+m"(v), "=@ccz"(fail)
	                 : [new] "r"(9), "a"(7)
	                 : "memory");
	__asm__ volatile("lock cmpxchgl %[new], %[var]"
	                 : [var] "+m"(v), "=@ccz"(ok)
	                 : [new] "r"(3), "a"(5)
	                 : "memory");
	return (fail == 0 && ok == 1 && v == 3) ? 0 : 1;
}

/* 非 Ident 输出排在 Ident 输出之前：旧列表把 `x` 的写侧变量错代到 `a[1]` 位上。 */
static int case_mem_then_reg(void)
{
	int a[4] = {0, 0, 0, 0};
	int x = 0;
	__asm__ volatile("movl $5, %0\n\tmovl $7, %1" : "=m"(a[1]), "=r"(x));
	return (a[1] == 5 && x == 7 && a[0] == 0 && a[2] == 0) ? 0 : 1;
}

/* 条件码输出在中间位：两侧 Ident 输出都被它「顶」一位才会暴露错位。
 * 模板把 lo/hi 都真写出来，绑错就当场可见（写进别人的槽 = 值丢 + 别人被盖）。 */
static int case_cc_middle(void)
{
	int v = 8;
	int lo = 0, hi = 0;
	bool z;
	__asm__ volatile("addl %[k], %[var]\n\t"
	                 "movl $1, %[l]\n\t"
	                 "movl $2, %[h]"
	                 : [var] "+m"(v), "=@ccz"(z), [l] "=&r"(lo), [h] "=&r"(hi)
	                 : [k] "r"(0)
	                 : "memory");
	/* addl $0, 8 → 结果 8，ZF=0；lo/hi 由模板真写。四个绑定必须各自落在自己的位上。 */
	return (v == 8 && z == false && lo == 1 && hi == 2) ? 0 : 1;
}

/* 同一变量作为 asm 输出被写两次：SSA 会给它分两个版本，代回的名字必须是它**自己**
 * 那个版本在 AST 里的名字（`local_splits::ast_name`），不能塌回基名。 */
static int case_versioned_home(void)
{
	int r = 0;
	__asm__ volatile("movl $11, %0" : "=r"(r));
	int a = r;
	r = 0;
	__asm__ volatile("movl $22, %0" : "=r"(r));
	int b = r;
	return (a == 11 && b == 22) ? 0 : 1;
}

/* `"+m"` / `"+r"` 是**读写**输出：旧值必须活着。旧建模把 asm 的输出版本当成基名
 * 的唯一 dest，SCCP 于是认为 `v = 5` 没有读者，把整条存储删掉（实测 -O2 返回
 * 未初始化栈位的垃圾，`"+r"` 同构）——`non_propagatable_names` 现在也把
 * `output_vars` 计作一次写。
 * 常量初值与运行期初值两臂都要钉：前者暴露常量折叠，后者暴露版本依赖。 */
static volatile int g_seed;

static int case_inout_keeps_old_value(void)
{
	int s = g_seed;
	int a = 5, b = 5, c = s, d = s;
	__asm__ volatile("addl $1, %0" : "+m"(a) : : "memory");
	__asm__ volatile("addl $1, %0" : "+r"(b) : : "memory");
	__asm__ volatile("addl $1, %0" : "+m"(c) : : "memory");
	__asm__ volatile("addl $1, %0" : "+r"(d) : : "memory");
	return (a == 6 && b == 6 && c == s + 1 && d == s + 1) ? 0 : 1;
}

int main(void)
{
	struct {
		const char *name;
		int (*fn)(void);
	} cases[] = {
		{ "rmwcc", case_rmwcc },
		{ "mem_then_reg", case_mem_then_reg },
		{ "cc_middle", case_cc_middle },
		{ "versioned_home", case_versioned_home },
		{ "inout_old_value", case_inout_keeps_old_value },
	};
	int bad = 0;
	for (unsigned i = 0; i < sizeof(cases) / sizeof(cases[0]); i++) {
		int rc = cases[i].fn();
		printf("%s=%d\n", cases[i].name, rc);
		bad += rc;
	}
	return bad == 0 ? 0 : 1;
}
