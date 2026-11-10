/* tests/fixtures/c/bool_conversion_nonzero.c
 *
 * §5.30：任何标量转换成 `_Bool` 的结果是 `(value != 0)`（C11 6.3.1.2p1），
 * 不是「按宽度截断到 8 位」。`_Bool` 只占 1 字节，所以 `movzbq %al` 看着像
 * 同一个转换，实际是另一个运算：0x200 的低字节是 0，截断把真值变成假。
 *
 * 内核现场（kbuild14 boot 的停点）：`arch/x86/kernel/apic/io_apic.c` 关掉 IOAPIC
 * 之后，`irq_startup()` 里的
 *
 *     WARN_ON_ONCE(!irqd_is_activated(d));
 *
 * 触发。`irqd_is_activated()` 是 `kernel/irq/chip.c` 里
 *
 *     static inline bool irqd_is_activated(struct irq_data *d)
 *     { return __irqd_to_state(d) & IRQD_ACTIVATED; }
 *
 * 其中 `IRQD_ACTIVATED` 是 bit 9（0x200）、`__irqd_to_state` 返回
 * `d->common->state`。dcc 编出的本地副本是
 *
 *     and $0x200, %rax
 *     movzbq %al, %rax          ← bit 9 不在低字节 ⇒ 恒为 0
 *
 * 于是 `state == 0x30200`（激活位置位）仍判「未激活」。连锁后果：
 * `..TIMER: vector=0xEF`（gcc 是 0x30）→ 8254 未被认领 → 退回 8259A/ExtINT →
 * `kernel BUG at kernel/irq/matrix.c:196`（`BUG_ON(!test_and_clear_bit(0x30,
 * cm->alloc_map))`）→ oops → `rewind_stack_and_make_dead` 读
 * `%gs:cpu_current_top_of_stack`（此时仍为 0）→ rsp=0xffffffffffffff58 → #PF→#DF。
 *
 * 失败码（成功返回 0；`assert_all_levels_match_gcc` 要求两侧退出码相等且为 0）：
 *   1  具名局部 `_Bool` 赋值（0x200）
 *   2  全局 `_Bool` 赋值
 *   3  结构成员 `_Bool` 赋值
 *   4  数组元素 `_Bool` 赋值
 *   5  指针解引用 `_Bool` 赋值
 *   6  局部 `_Bool` 声明初始化（旧实现连截断都没发）
 *   7  `_Bool` 形参传递（实参 0x200 应传 1）
 *   8  `_Bool` 返回值（常量 512）
 *   9  `_Bool` 返回值（位与运算）
 *   10 显式 `(_Bool)` cast
 *   11 `(_Bool)0.5`（浮点真值必须为真；截断成整数会得 0）
 *   12 复合赋值 `b += 0x200`
 *   13 内核 `irqd_is_activated` 形状（bit 9 判据）
 *   14 赋值表达式的值（`long v = (b = 0x200)` 必须得 1）
 *   15 `unsigned char` 源（同宽 cast，0x100 → 真）
 *   16 位运算结果直接赋给 `_Bool`
 *   17 `_Bool` 数组初始化式
 *   18 三目/逻辑结果赋给 `_Bool` 回归（本来就该是 0/1）
 *   19 `_Bool` 返回值经变量中转（形参 → 返回）
 *   20 `_Bool` 全局静态初始化的常量折叠（512 → 1）
 */

typedef unsigned long ulong;

/* ---- 内核 irq_data 的形状（kernel/irq/chip.c:__irqd_to_state 同构） ---- */
#define IRQD_ACTIVATED (1UL << 9) /* 0x200 */
#define IRQD_PER_CPU (1UL << 11)

struct irq_common_data {
	unsigned int state_use_accessors_dummy;
	ulong state;
};

struct irq_data {
	unsigned int irq;
	ulong hwirq;
	struct irq_common_data *common;
};

static struct irq_common_data gcommon;
static struct irq_data gd = { .irq = 0, .common = &gcommon };

/* 与 chip.c 的 static inline 同形：返回值类型是 _Bool。 */
__attribute__((noinline)) static _Bool irqd_is_activated_local(struct irq_data *d)
{
	return d->common->state & IRQD_ACTIVATED;
}

/* ---- 其它 _Bool 通道 ---- */
static _Bool gb;
static struct {
	_Bool b;
} gs;
static _Bool garr[4];
static _Bool garr_init[3] = { 512, 0, 0x100 };
static _Bool gstatic = 512; /* 常量折叠：1 */

__attribute__((noinline)) static _Bool ret_const(void)
{
	return 512;
}

__attribute__((noinline)) static _Bool ret_and(int x)
{
	return x & 0x200;
}

__attribute__((noinline)) static _Bool id_bool(_Bool b)
{
	return b;
}

__attribute__((noinline)) static int take_bool(_Bool b)
{
	/* 被调方按 0/1 语义使用形参。 */
	return b + 1;
}

int main(void)
{
	int n = 512; /* 低字节 0，高位置位 */
	long v;
	_Bool lb;
	_Bool *p = &lb;

	/* 1 具名局部赋值 */
	lb = n;
	if (!lb)
		return 1;

	/* 2 全局赋值 */
	gb = n;
	if (!gb)
		return 2;

	/* 3 结构成员 */
	gs.b = n;
	if (!gs.b)
		return 3;

	/* 4 数组元素 */
	garr[1] = n;
	if (!garr[1])
		return 4;

	/* 5 指针解引用 */
	*p = n;
	if (!lb)
		return 5;

	/* 6 声明初始化 */
	{
		_Bool li = n;
		if (!li)
			return 6;
	}

	/* 7 形参传递（实参 0x200，被调方必须看到 1） */
	if (!id_bool(n))
		return 7;
	if (take_bool(n) != 2)
		return 7;

	/* 8/9 返回值 */
	if (!ret_const())
		return 8;
	if (!ret_and(n))
		return 9;

	/* 10 显式 cast */
	{
		_Bool c = (_Bool)n;
		if (!c)
			return 10;
	}

	/* 11 浮点源：0.5 必须为真（截断成整数会得 0） */
	{
		double d = 0.5;
		_Bool c = (_Bool)d;
		if (!c)
			return 11;
	}

	/* 12 复合赋值 */
	{
		_Bool b = 0;
		b += 0x200;
		if (!b)
			return 12;
	}

	/* 13 内核现场：state=0x30200（ACTIVATED 置位）必须判激活 */
	gcommon.state = IRQD_ACTIVATED | IRQD_PER_CPU | 0x30000;
	if (!irqd_is_activated_local(&gd))
		return 13;
	gcommon.state = IRQD_PER_CPU | 0x30000;
	if (irqd_is_activated_local(&gd))
		return 13;

	/* 14 赋值表达式的值等于左值转换后的值 */
	v = (lb = 0x200);
	if (v != 1)
		return 14;

	/* 15 unsigned char 源（与 _Bool 同宽，靠 cast 归一化） */
	{
		unsigned char uc = 0x00;
		_Bool a = (_Bool)uc;
		unsigned short us = 0x100;
		_Bool b = (_Bool)us;
		if (a)
			return 15;
		if (!b)
			return 15;
	}

	/* 16 位运算结果直接赋给 _Bool */
	{
		unsigned u = 0x200;
		_Bool a = (u >> 8) ? 1 : 0;
		_Bool b;
		b = !!(u & 0x200);
		lb = (u & 0x200) != 0;
		if (!a || !b || !lb)
			return 16;
	}

	/* 17 数组初始化式 */
	if (!garr_init[0] || garr_init[1] || !garr_init[2])
		return 17;

	/* 18 三目 / 逻辑结果（本来就该是 0/1，回归项） */
	{
		_Bool a = n ? 1 : 0;
		_Bool b = (n > 0) && (n < 1000);
		if (!a || !b)
			return 18;
	}

	/* 19 形参 → 返回值中转 */
	{
		_Bool r = id_bool(n);
		if (!r)
			return 19;
	}

	/* 20 全局静态初始化常量折叠 */
	if (!gstatic)
		return 20;

	return 0;
}
