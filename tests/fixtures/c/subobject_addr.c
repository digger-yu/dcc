/* 子对象地址形态（`InstKind::MemberAddr`）的回归夹具。
 *
 * 这些形状的共同点：基址是「对象地址 + 字段偏移」，而不是某个具名对象的基址。
 * IR 过去只有 `AddrOf { base: 名字 }`，于是 `z->data[i]` 这类下标只能把 `data` 的
 * **内容**当地址解引用 —— 实测 -O2 读出栈垃圾而 -O0 正确（静默误编译，见
 * docs/ssa-pass-upgrade-plan.md §7.29）。现在由 `MemberAddr` 表达，四个档位都必须
 * 与 gcc 一致。
 *
 * 只打印整数：夹具有地址输出的话 gcc 差分对照会产生与编译器无关的假 MISMATCH。
 */
#include <stdio.h>

struct P {
	int a;
	int b;
};

struct Z {
	int count;
	int data[0];
};

struct F {
	int count;
	int data[];
};

/* &p->b：标量子对象的取址（过去直接 unsupported 的那一侧）。 */
static int
via_member_addr(struct P *p)
{
	int *q = &p->b;
	*q = 42;
	return p->a + *(&p->b);
}

/* &v[1] 之后继续下标：指针 + 偏移再偏移。 */
static int
via_ptr_index(int n)
{
	int v[4];

	v[2] = 100;
	int *q = &v[1];
	q[1] = n;
	return q[1] + v[2];
}

/* 零长数组成员的下标：读与写必须共用同一份衰减（只修一侧是历史上的重复错误）。 */
static int
via_zero_len(struct Z *z)
{
	z->data[3] = 11;
	return z->data[3] + z->count;
}

/* 柔性数组成员。 */
static int
via_flexible(struct F *f)
{
	f->data[0] = 1;
	f->data[2] = 5;
	return f->data[2] - f->data[0] + f->count;
}

int
main(void)
{
	int buf[64];
	struct P p;
	struct Z *z;
	struct F *f;
	int i;

	for (i = 0; i < 64; i++) {
		buf[i] = 0;
	}
	p.a = 5;
	p.b = 9;
	z = (struct Z *) buf;
	z->count = 100;
	f = (struct F *) buf;
	f->count = 0;

	printf("member=%d\n", via_member_addr(&p));
	printf("ptridx=%d\n", via_ptr_index(4));
	printf("zerolen=%d\n", via_zero_len(z));
	printf("flexible=%d\n", via_flexible(f));
	printf("zerolen_again=%d\n", z->data[3]);
	return 0;
}
