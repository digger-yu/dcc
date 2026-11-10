/*
 * `PreservedDecl` 初始化式里的**基名读者**必须计入逐点活性（报告 §5.38）。
 *
 * SSA 无法表达的聚合初始化（`{ .f = x }` / `(T){ … }`）会原样保留成
 * `InstKind::PreservedDecl`，重建时直接回填 `Decl::Var`（`rebuild.rs`）——
 * 发射期按**基名**查家取值。这条路径上 IR 里没有任何 use 边
 * （`collect_uses_kind` 对 `PreservedDecl` 返回空集），于是同一条声明里读到的
 * 两个值被判成「互不干涉」，绑到同一个物理号，后定义的那个把先定义的覆盖掉。
 *
 * 内核形状（`net/core/dev.c`，-O2 内联后）：
 *
 *     static int call_netdevice_notifiers_extack(unsigned long val,
 *                                                struct net_device *dev,
 *                                                struct netlink_ext_ack *extack)
 *     {
 *             struct netdev_notifier_info info = { .dev = dev, .extack = extack };
 *             return call_netdevice_notifiers_info(val, &info);
 *     }
 *
 * `__inl0$_dev` 与 `__inl0$_extack` 在 IR 里只有定义、没有读者 ⇒ 同绑 %r12，
 * `.dev` 拿到 `.extack`（NULL），boot 在 `dev_net(NULL)` 读 +0x108 处 #PF。
 *
 * 失败码（实测旧产物在 -O2/-O3 给出 31）：
 *   1  = 内核原形（两个内联形参进同一聚合初始化，`.dev` 被 `.extack` 覆盖）
 *   2  = 三字段聚合初始化（同一个号被三个内联形参反复覆盖）
 *   4  = 嵌套聚合成员（内层 InitList 的基名同样没有 use 边）
 *   8  = 循环体里的聚合初始化（每一次迭代都是同一条 PreservedDecl）
 *   16 = 复合字面量 `(T){ … }` 作为局部初始化式
 *
 * 每个入口都经 volatile 函数指针调用：不让 -O2 把整个函数体折进 `main` ——
 * 折进去后形参变成常量、号分配退化，缺陷就看不见了（本夹具第一版四档全绿）。
 */
#include <stdio.h>

struct dev {
	int id;
};
struct ack {
	int id;
};

/* ---- 1. 内核原形 ----------------------------------------------------- */
struct ninfo {
	struct dev *dev;
	struct ack *extack;
};

static struct dev *g_seen;
static int g_seen_extack;

static int notifier_info_cb(unsigned long val, struct ninfo *info)
{
	(void)val;
	g_seen = info->dev;
	if (info->extack)
		g_seen_extack = 1;
	return 0;
}

static int notifiers_extack(unsigned long val, struct dev *dev, struct ack *extack)
{
	struct ninfo info = { .dev = dev, .extack = extack };

	return notifier_info_cb(val, &info);
}

static int call_notifiers(unsigned long val, struct dev *dev)
{
	return notifiers_extack(val, dev, (struct ack *)0);
}

/* ---- 2. 三字段 ------------------------------------------------------- */
struct triple {
	void *a;
	void *b;
	void *c;
};

static void *t_seen_a, *t_seen_b, *t_seen_c;

static int triple_cb(struct triple *p)
{
	t_seen_a = p->a;
	t_seen_b = p->b;
	t_seen_c = p->c;
	return 0;
}

static int triple_mid(void *a, void *b, void *c)
{
	struct triple t = { .a = a, .b = b, .c = c };

	return triple_cb(&t);
}

static int triple_entry(void *a, void *b, void *c)
{
	return triple_mid(a, b, c);
}

/* ---- 4. 嵌套聚合成员 ------------------------------------------------- */
struct inner {
	void *p;
	void *q;
};
struct outer {
	struct inner in;
	void *r;
};

static void *n_seen_p, *n_seen_q, *n_seen_r;

static int nested_cb(struct outer *o)
{
	n_seen_p = o->in.p;
	n_seen_q = o->in.q;
	n_seen_r = o->r;
	return 0;
}

static int nested_mid(void *a, void *b, void *c)
{
	struct outer o = { .in = { .p = a, .q = b }, .r = c };

	return nested_cb(&o);
}

static int nested_entry(void *a, void *b, void *c)
{
	return nested_mid(a, b, c);
}

/* ---- 8/16 共用的一对字段 --------------------------------------------- */
struct pair2 {
	void *x;
	void *y;
};

static void *d_seen_x, *d_seen_y;

static int pair_cb(struct pair2 *p)
{
	d_seen_x = p->x;
	d_seen_y = p->y;
	return 0;
}

/* ---- 8. 循环体里的聚合初始化 ---------------------------------------- */
static int loop_mid(void *a, void *b, int n)
{
	int acc = 0;

	for (int i = 0; i < n; i++) {
		struct pair2 p = { .x = a, .y = b };

		acc += pair_cb(&p);
	}
	return acc;
}

static int loop_entry(void *a, void *b)
{
	return loop_mid(a, b, 3);
}

/* ---- 16. 复合字面量作为初始化式 -------------------------------------- */
static int cl_mid(void *a, void *b)
{
	struct pair2 p = (struct pair2){ .x = a, .y = b };

	return pair_cb(&p);
}

static int cl_entry(void *a, void *b)
{
	return cl_mid(a, b);
}

int main(void)
{
	struct dev d = { 7 };
	int bad = 0;
	int a = 0x11, b = 0x22, c = 0x33;

	static int (*volatile vp_call)(unsigned long, struct dev *) = call_notifiers;
	static int (*volatile vp_triple)(void *, void *, void *) = triple_entry;
	static int (*volatile vp_nested)(void *, void *, void *) = nested_entry;
	static int (*volatile vp_loop)(void *, void *) = loop_entry;
	static int (*volatile vp_cl)(void *, void *) = cl_entry;

	/* 1 */
	g_seen = (struct dev *)0;
	g_seen_extack = 0;
	vp_call(1, &d);
	if (g_seen != &d || g_seen_extack)
		bad |= 1;

	/* 2 */
	t_seen_a = t_seen_b = t_seen_c = (void *)0;
	vp_triple(&a, &b, &c);
	if (t_seen_a != &a || t_seen_b != &b || t_seen_c != &c)
		bad |= 2;

	/* 4 */
	n_seen_p = n_seen_q = n_seen_r = (void *)0;
	vp_nested(&a, &b, &c);
	if (n_seen_p != &a || n_seen_q != &b || n_seen_r != &c)
		bad |= 4;

	/* 8 */
	d_seen_x = d_seen_y = (void *)0;
	if (vp_loop(&a, &b) != 0)
		bad |= 8;
	if (d_seen_x != &a || d_seen_y != &b)
		bad |= 8;

	/* 16 */
	d_seen_x = d_seen_y = (void *)0;
	if (vp_cl(&a, &b) != 0)
		bad |= 16;
	if (d_seen_x != &a || d_seen_y != &b)
		bad |= 16;

	printf("bad=%d\n", bad);
	return bad;
}
