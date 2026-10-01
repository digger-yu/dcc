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

/* 指针指数组形参的多重下标 `pa[i][j]`。曾经因为形参声明符 `int (*pa)[3]`
 * 被建为 Array(Ptr(Int),3)（而不是 Ptr(Array(Int,3))）而连 -O0 都静默错值。 */
static int
via_ptr_to_array(int (*pa)[3])
{
	pa[0][0] = 11;
	pa[1][2] = 22;
	return pa[0][0] + pa[1][2];
}

/* 解引用基址的衰减 `(*pa)[i]`：同一个根因的另一侧，且它走 SSA 往返。
 * 曾以为「IR 表达不了」而整函数退 SSA，那是误归因。 */
static int
via_deref_decay(int (*pa)[3])
{
	(*pa)[2] = 7;
	return (*pa)[2] + (*pa)[0];
}

/* 把**类型模型**直接钉在产物上：`int (*pa)[3]` 是一个指针（8），它指向的
 * 是一个 3 int 的数组（12）。只测下标值的话，声明符结合顺序反了也可能
 * 被其它路径抵消，而 sizeof 没有余地。 */
static int
ptr_to_array_sizes(int (*pa)[3])
{
	return (int)sizeof(pa) * 10000 + (int)sizeof(*pa) * 100 + (int)sizeof(pa[0]);
}

int
main(void)
{
	int buf[64];
	int r1[6];
	int r2[6];
	struct P p;
	struct Z *z;
	struct F *f;
	int i;

	for (i = 0; i < 64; i++) {
		buf[i] = 0;
	}
	for (i = 0; i < 6; i++) {
		r1[i] = i;
		r2[i] = 100 + i;
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
	printf("p2a=%d\n", via_ptr_to_array((int (*)[3]) r1));
	printf("p2a_back=%d\n", r1[2]);
	printf("decay=%d\n", via_deref_decay((int (*)[3]) r2));
	printf("decay_back=%d\n", r2[2]);
	printf("sizes=%d\n", ptr_to_array_sizes((int (*)[3]) r1));
	return 0;
}
