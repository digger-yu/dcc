/* 内存左值的「读—改—写」（自增/自减/复合赋值）在 -O2/-O3 不得静默丢访问。
 *
 * 这条夹具守的是第 26 轮查出的一处 P0 静默误编译（不是丢优化，是**错值**）：
 *   * `Expr::PostInc/PreInc` 的 SSA 降级只认 `Expr::Ident` 操作数，其余形态直接
 *     `return Operand::Int(one)` —— 既不读也不写，连 `unsupported` 都不标，函数照样
 *     进 SSA。实测 -O2 下逐条与 gcc 对照：`int r = ++*p;` 得到常量 1 且对象**没被写**、
 *     `int r = a[3]++;` 也得到 1（应为旧值）、`++s.f` / `++q->g` / `++p[i]` 同构。
 *   * 复合赋值那一半：parser 把 `LV op= R` 展开成 `Assign{l: LV, r: Binop{op, LV拷贝, R}}`
 *     （`BinOp` 里没有 `Set` 变体），旧降级把整棵 `r` 降下来再写 `l` ⇒ LV 被求值两遍。
 *     无副作用时两个地址碰巧相同，有副作用时（`a[i++] += 5`、`*--p += 1`）地址不同 ⇒
 *     写错位置；而赋值结果那个临时值连定义都没有 —— 实测 `int r = a[i] += 7;` 的 `r`
 *     是未初始化垃圾。
 *
 * 修法是两侧共用一条机器：地址只求一次的读—改—写（`ssa::Builder::lower_mem_rmw`），
 * 判据（结构相等、是否有副作用）与 codegen 共用 `crate::semantics` 里的同一份实现。
 *
 * 每一项都**同时**检查「表达式的值」与「对象被写后的值」：只看前者会放过「值恰好算对
 * 但内存没动」这一类，只看后者会放过「内存动了但返回值错」这一类。
 *
 * 通过: exit 0。期望值全部取自 gcc -O0 参照，再按 C 语义逐项复核。
 */

struct S {
	int f;
	int g;
	int arr[4];
};

static int ga[8];

#define CHK(cond, code)                                                         \
	do {                                                                        \
		if (!(cond)) {                                                          \
			return (code);                                                      \
		}                                                                       \
	} while (0)

/* 1..2: `++*p` / `(*p)++` —— 历史上返回常量、且不写内存 */
static int inc_deref(void)
{
	int v = 41;
	int *p = &v;
	int r = ++*p;

	CHK(r == 42, 1);
	CHK(v == 42, 2);

	v = 41;
	r = (*p)++;
	CHK(r == 41, 3);
	CHK(v == 42, 4);

	v = 41;
	r = --*p;
	CHK(r == 40, 5);
	CHK(v == 40, 6);

	v = 41;
	r = (*p)--;
	CHK(r == 41, 7);
	CHK(v == 40, 8);
	return 0;
}

/* 9..12: 下标与指针下标的自增自减 */
static int inc_index(void)
{
	int a[4] = { 1, 2, 3, 4 };
	int r;

	r = ++a[1];
	CHK(r == 3, 9);
	CHK(a[1] == 3, 10);

	r = a[2]++;
	CHK(r == 3, 11);
	CHK(a[2] == 4, 12);

	int *p = a + 3;

	r = ++p[0];
	CHK(r == 5, 13);
	CHK(a[3] == 5, 14);

	r = p[0]--;
	CHK(r == 5, 15);
	CHK(a[3] == 4, 16);

	/* 变量下标（历史上也一起错，因为退到同一条 fallthrough） */
	int i = 0;

	r = ++a[i];
	CHK(r == 2, 17);
	CHK(a[0] == 2, 18);
	return 0;
}

/* 19..22: 成员与箭头成员的自增自减 */
static int inc_member(void)
{
	struct S s = { 5, 6, { 1, 2, 3, 4 } };
	struct S *q = &s;
	int r;

	r = ++s.f;
	CHK(r == 6, 19);
	CHK(s.f == 6, 20);

	r = q->g++;
	CHK(r == 6, 21);
	CHK(s.g == 7, 22);

	r = ++s.arr[1];
	CHK(r == 3, 23);
	CHK(s.arr[1] == 3, 24);

	r = ++q->arr[2];
	CHK(r == 4, 25);
	CHK(s.arr[2] == 4, 26);
	return 0;
}

/* 27..30: 复合赋值，含「左值里有副作用」这一类 */
static int compound_assign(void)
{
	int a[8] = { 0 };
	int i;
	int r;

	i = 1;
	a[i] += 7;
	CHK(a[1] == 7, 27);

	i = 2;
	r = a[i] += 7; /* 赋值本身也有值: 必须等于写回后的对象 */
	CHK(r == 7, 28);
	CHK(a[2] == 7, 29);

	/* 关键一项: 下标带副作用 ⇒ 左值只能求值一次。
	 * `a[i++] += 5` 展开成 `a[i++] = a[i++] + 5` 之后如果两侧都求值,
	 * 读的是 a[0]、写的却是 a[1]（实测 -O2 得到 a[1]=5 而 gcc 是 a[0]=5）。 */
	i = 0;
	a[i] += 3;
	a[i++] += 5;
	CHK(a[0] == 8, 30);
	CHK(i == 1, 31);
	/* a[1] 仍是上面 `i = 1; a[i] += 7;` 留下的 7 —— 本项要的是「i++ 只跑一遍」，
	 * 所以这里断言的是「没有多写一笔到 a[1]」。 */
	CHK(a[1] == 7, 32);

	/* 指针侧带副作用：`*--p += 1`（先把两个格子归零，期望值不被上面的干扰） */
	int *p = a + 2;

	a[1] = 0;
	a[2] = 0;
	*p = 10;
	*--p += 1;
	CHK(a[1] == 1, 33);
	CHK(a[2] == 10, 34);
	CHK(p == a + 1, 35);

	/* 成员/箭头复合赋值 */
	struct S s = { 1, 2, { 0 } };

	s.f += 4;
	CHK(s.f == 5, 36);
	s.arr[1] -= 3;
	CHK(s.arr[1] == -3, 37);
	(&s)->g *= 5;
	CHK(s.g == 10, 38);

	/* 全局数组（走的是「文件作用域对象」那条读改写路径） */
	ga[3] += 6;
	ga[3] += 6;
	CHK(ga[3] == 12, 39);
	return 0;
}

/* 40..42: volatile 的读—改—写必须留在内存里, 且各只发生一次 */
static int volatile_rmw(void)
{
	volatile int v = 7;
	volatile int arr[4] = { 0 };
	volatile int *p = &v;
	int r;

	r = ++v;
	CHK(r == 8, 40);
	CHK(v == 8, 41);

	*p -= 3;
	CHK(v == 5, 42);

	arr[1] += 4;
	CHK(arr[1] == 4, 43);
	r = arr[1]++;
	CHK(r == 4, 44);
	CHK(arr[1] == 5, 45);
	return 0;
}

/* 46..48: 循环体里的读—改—写（LICM/ivop/GVN 都要在改写过的区域上保持语义） */
static int loop_rmw(int n)
{
	int a[8] = { 0 };
	int s = 0;

	for (int i = 0; i < n; i++) {
		a[i] += i + 1;
		s += a[i];
		++a[i];
		s += a[i];
	}
	/* n=4: 每轮 a[i] = (i+1) 再加 1 ⇒ {2,3,4,5};
	 * s = (1+2)+(2+3)+(3+4)+(4+5) = 3+5+7+9 = 24 */
	CHK(a[0] == 2, 46);
	CHK(a[3] == 5, 47);
	CHK(s == 24, 48);
	return 0;
}

/* 49: 一个表达式里同时有前/后自增的内存左值 */
static int mixed_rmw(void)
{
	int a[4] = { 10, 20, 0, 0 };
	int *p = a;
	int *q = a + 1;

	a[2] = ++*p + (*q)++;
	/* ++*p ⇒ a[0]=11（值 11）; (*q)++ ⇒ 值 20、a[1]=21 ⇒ a[2] = 31 */
	CHK(a[0] == 11, 49);
	CHK(a[1] == 21, 50);
	CHK(a[2] == 31, 51);
	CHK(*p + *q == 32, 52);
	return 0;
}

int main(void)
{
	int rc;

	rc = inc_deref();
	if (rc != 0)
		return rc;
	rc = inc_index();
	if (rc != 0)
		return rc;
	rc = inc_member();
	if (rc != 0)
		return rc;
	rc = compound_assign();
	if (rc != 0)
		return rc;
	rc = volatile_rmw();
	if (rc != 0)
		return rc;
	rc = loop_rmw(4);
	if (rc != 0)
		return rc;
	rc = mixed_rmw();
	if (rc != 0)
		return rc;
	return 0;
}
