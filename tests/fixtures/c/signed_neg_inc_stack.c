/* tests/fixtures/c/signed_neg_inc_stack.c
 *
 * §5.31：`++`/`--` 在栈槽路径上按**宽度零扩展**读数，而随后的加法是 64 位。
 * 4 字节有符号 `int` 于是把 -1 读成 0x00000000FFFFFFFF，`++` 得到
 * 0x0000000100000000，而不是 0。
 *
 * 内核现场（kbuild15 全树重建后的 boot 停点）：
 * arch/x86/kernel/cpu/mtrr/generic.c
 *
 *     static int set_fixed_ranges(mtrr_type *frs)
 *     {
 *             int block = -1, range;
 *             while (fixed_range_blocks[++block].ranges) { ... }
 *     }
 *
 * dcc 编出的序列（vmlinux `set_fixed_ranges+0x1e3`）：
 *
 *     lea    fixed_range_blocks(%rip), %rax
 *     mov    -0x58(%r13), %ecx        ← 32 位零扩展读 block
 *     add    $1, %rcx                 ← 64 位加：0xFFFFFFFF + 1 = 0x1_0000_0000
 *     mov    %rcx, -0x58(%r13)
 *     imul   $8, %rcx                 ← 下标 0x8_0000_0000
 *     add    %rcx, %rax
 *     add    $4, %rax                 ← 表地址 0xffffffff87c479c8 回绕成 0x787c479cc
 *     movslq (%rax), %rax             ← #PF，cr2 = 0x787c479cc
 *
 * 同一函数内层循环里同一个变量走的是 `movslq -0x58(%r13), %rcx`（正确），
 * 所以这不是「类型宽度」问题，而是**同一条读通道两个口径**：读数必须走
 * 单一入口 `mov_from_stack`（按声明类型决定 movsxd/movzx/mov）。
 *
 * 失败码（成功返回 0；`assert_all_levels_match_gcc` 要求两侧退出码相等且为 0）：
 *   1  内核形状：`while (tbl[++block].ranges)`，block 从 -1 起
 *   2  `int i = -1; a[++i]`
 *   3  后缀取值：`int i = -1; int j = i++;`
 *   4  `int i = -5; i++; i += 2;`
 *   5  `for (int i = -3; i < 2; ++i)` 求和（负起点 + 前缀）
 *   6  `int i = 0; --i;` 之后用 `a[i + 1]`
 *   7  形参中转：`i++` 后比较 `== 0`（参数槽 8 字节，回归）
 *   8  结构成员 `x.f++`（内存 lvalue 路径，回归）
 *   9  `unsigned int u = 0xFFFFFFFFu; u++;`（无符号必须继续零扩展）
 *   10 `short s = -1; s++;`（窄类型回归）
 *   11 `signed char c = -1; c++;`（窄有符号回归）
 *   12 `long l = -1; l++;`（8 字节回归）
 *   13 后缀取值：`int i = -2; int j = i--;`
 *   14 指针自减：`p--` 两次
 *   15 `int i = -1; while (++i < 3) n += i;`
 *   16 前缀表达式值直接参与乘加：`int i = -1; x + (++i) * 4`
 *   17 下标参与地址字节偏移：`(char *)a + ++i`
 *   18 自减后作为表下标（`tbl[--idx]`，idx 从 1 起）
 */

struct frb {
	int base_msr;
	int ranges;
};

static const struct frb fixed_range_blocks[] = {
	{ 0x250, 8 },
	{ 0x258, 1 },
	{ 0x259, 1 },
	{ 0x000, 0 },
};

/* 内核 `set_fixed_ranges()` 的循环骨架：block 初值 -1，条件里 `++block` 当下标 */
static int probe_fixed_ranges(void)
{
	int block = -1, range, changes = 0;

	while (fixed_range_blocks[++block].ranges) {
		for (range = 0; range < fixed_range_blocks[block].ranges; range++)
			changes += fixed_range_blocks[block].base_msr + range;
	}
	return block * 100000 + changes;
}

static int param_inc(int i)
{
	i++;
	return i;
}

struct holder {
	int f;
};

static int fail_code(void)
{
	/* 2 */
	{
		int a[4] = { 10, 20, 30, 40 };
		int i = -1;

		if (a[++i] != 10 || i != 0)
			return 2;
	}
	/* 3 */
	{
		int i = -1;
		int j = i++;

		if (j != -1 || i != 0)
			return 3;
	}
	/* 4 */
	{
		int i = -5;

		i++;
		i += 2;
		if (i != -2)
			return 4;
	}
	/* 5 */
	{
		int sum = 0, i;

		for (i = -3; i < 2; ++i)
			sum += i;
		if (sum != -5)
			return 5;
	}
	/* 6 */
	{
		int a[3] = { 7, 8, 9 };
		int i = 0;

		--i;
		if (a[i + 1] != 7)
			return 6;
	}
	/* 7 */
	if (param_inc(-1) != 0)
		return 7;
	/* 8 */
	{
		struct holder x;

		x.f = -1;
		x.f++;
		if (x.f != 0)
			return 8;
	}
	/* 9 */
	{
		unsigned int u = 0xFFFFFFFFu;

		u++;
		if (u != 0)
			return 9;
	}
	/* 10 */
	{
		short s = -1;

		s++;
		if (s != 0)
			return 10;
	}
	/* 11 */
	{
		signed char c = -1;

		c++;
		if (c != 0)
			return 11;
	}
	/* 12 */
	{
		long l = -1;

		l++;
		if (l != 0)
			return 12;
	}
	/* 13 */
	{
		int i = -2;
		int j = i--;

		if (j != -2 || i != -3)
			return 13;
	}
	/* 14 */
	{
		int a[4] = { 1, 2, 3, 4 };
		int *p = &a[3];

		p--;
		p--;
		if (*p != 2)
			return 14;
	}
	/* 15 */
	{
		int n = 0, i = -1;

		while (++i < 3)
			n += i;
		if (n != 3)
			return 15;
	}
	/* 16 */
	{
		int i = -1;
		int x = 5;

		if (x + (++i) * 4 != 5 || i != 0)
			return 16;
	}
	/* 17 */
	{
		int a[3] = { 11, 12, 13 };
		char *p = (char *)a;
		int i = -1;

		p += ++i;
		if (p != (char *)a || i != 0)
			return 17;
	}
	/* 18 */
	{
		int idx = 1;

		if (fixed_range_blocks[--idx].ranges != 8)
			return 18;
	}
	if (probe_fixed_ranges() != 300000 + ((0x250 + 0x251 + 0x252 + 0x253 +
					      0x254 + 0x255 + 0x256 + 0x257) +
					     (0x258 + 0x259)))
		return 1;
	return 0;
}

int main(void)
{
	return fail_code();
}
