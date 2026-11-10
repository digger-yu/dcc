/* §5.47: 赋值表达式作数组下标时，下标必须是**赋值结果**（C11 6.5.16p3），
 * 不是左值地址。内核形状（sound/core/seq/seq_clientmgr.c）：
 *
 *   clienttab[client->number = c] = client;
 *
 * 旧行为把 Member/Index 两条内存左值臂的赋值结果留在 rax，而下标消费者
 * 固定请求 rcx（`gen_expr(idx, 1)`），store 用的地址又正好在 rcx —— 于是
 * `imul $8,%rcx,%rcx` 缩放的是 `&client->number`，kbuild23 首次执行即
 * GPF（非规范地址 0xfffc43ff90eab168）。
 * 失败码：1/2/3=->、4/5=.、6/7=下标左值、8/9=解引用、
 *         10/11=赋值结果按左值类型归约、12/13=指针数组（内核原形）、
 *         14/15=嵌套表达式、16=移位计数。 */
struct s {
	int pad[12];
	int n;
	unsigned char ch;
};

static int g[16];
static const char *ptrtab[8];
static struct s gs;

__attribute__((noinline))
static int use_member_arrow(struct s *p, int c)
{
	g[p->n = c] = 100 + c;
	return p->n;
}

__attribute__((noinline))
static void use_member_dot(int c)
{
	g[gs.n = c] = 200 + c;
}

__attribute__((noinline))
static void use_index(int *a, int i, int c)
{
	g[a[i] = c] = 300 + c;
}

__attribute__((noinline))
static void use_deref(int *p, int c)
{
	g[*p = c] = 400 + c;
}

__attribute__((noinline))
static void use_ptr_array(const char **src, int c)
{
	/* 内核原形：clienttab[client->number = c] = client; */
	ptrtab[gs.n = c] = src[c];
}

int main(void)
{
	const char *names[4] = { "zero", "one", "two", "three" };
	struct s s;
	int loc[4];
	int v;
	int x;

	s.n = -1;
	if (use_member_arrow(&s, 3) != 3)
		return 1;
	if (s.n != 3)
		return 2;
	if (g[3] != 103)
		return 3;

	use_member_dot(5);
	if (gs.n != 5)
		return 4;
	if (g[5] != 205)
		return 5;

	loc[0] = loc[1] = loc[2] = 0;
	use_index(loc, 2, 7);
	if (loc[2] != 7)
		return 6;
	if (g[7] != 307)
		return 7;

	v = 9;
	use_deref(&v, 11);
	if (v != 11)
		return 8;
	if (g[11] != 411)
		return 9;

	/* 赋值表达式的值 = 左值转换后的值（按左值类型归约）：uchar 回绕到 44 */
	s.ch = 0;
	v = (s.ch = 300);
	if (v != 44)
		return 10;
	if (s.ch != 44)
		return 11;

	/* 指针数组 + 赋值作下标 + 指针值存入：kbuild23 崩溃的同一形状 */
	gs.n = 0;
	use_ptr_array(names, 2);
	if (gs.n != 2)
		return 12;
	if (ptrtab[2] != names[2] || ptrtab[2][0] != 't')
		return 13;

	/* 赋值嵌在算术表达式里作下标 */
	g[(s.n = 4) + 1] = 500;
	if (g[5] != 500)
		return 14;
	if (s.n != 4)
		return 15;

	/* 赋值作移位计数（消费者同样点名 rcx） */
	x = 1 << (s.n = 3);
	if (x != 8)
		return 16;

	return 0;
}
