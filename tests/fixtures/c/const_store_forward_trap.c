/* 「常量存进局部 → 后面拿它当条件读」的**产物级**双向判据（§3.51）。
 *
 * 缺陷现场（`mm/shmem.c::shmem_alloc_and_add_folio`，THP 关闭）：内核先写成
 *     if (!IS_ENABLED(CONFIG_TRANSPARENT_HUGEPAGE))
 *             orders = 0;
 *     if (orders > 0) { … if (pages == HPAGE_PMD_NR) BUILD_BUG() … }
 * 预处理后第一个 `if` 是 `if (1)`；它被死分支折叠拍平成一行 `orders = 0;` 之后，
 * 第二条 `if (orders > 0)` 才是死块的门。可是 AST 层的两个常量道具（cprop 只认
 * **声明初值**、SSA 层的 SCCP 对**含 goto 的函数**只做 analysis-only 不重建）
 * 都看不到「先赋值再读」这条 —— 于是 `BUILD_BUG()` 那条**无条件**调用留在产物里，
 * 变成链接期 `undefined reference to __compiletime_assert_NNN`（实测 5 条）。
 *
 * 覆盖面：前三个函数要求陷阱引用**消失**，后五个要求引用**必须还在**。
 * 期望值全部先拿 `gcc -O2` 实测校准（不是推的）：gcc 侧 dead 三个 0 条，
 * keep 五个 5 条都在。
 *
 * 形状：
 *   P1 形参被常量钉死（`if (1) o = 0;`），后面凭它开死块
 *   P2 shmem 全形（含 `goto allocated;` 与 `else { pages = 1; }`）：两跳不动点才收敛
 *   P3 局部显式补一条与初值相同的 `x = 0;` 再读
 *   N1 `x = 0` 与读点之间有**被 goto 指向**的标号（那条路绕过赋值）
 *   N2 x 在后面被**别的常量**写过（分支里）
 *   N3 x 的地址逃逸（可能被别人改）
 *   N4 x 是 `++` 的目标
 *   N5 存储坐在**可能不执行**的嵌套块里
 */
typedef unsigned long size_t;

extern void trap_p1(void) __attribute__((noreturn));
extern void trap_p2_a(void) __attribute__((noreturn));
extern void trap_p2_b(void) __attribute__((noreturn));
extern void trap_p3(void) __attribute__((noreturn));

extern void trap_keep_label(void);
extern void trap_keep_reassign(void);
extern void trap_keep_addr(void);
extern void trap_keep_inc(void);
extern void trap_keep_nested(void);

extern void sink_touch(void *p);
extern long pick_order(long seed);

#define IS_ENABLED0 (0)

/* `BUILD_BUG()`：条件恒真 ⇒ 编译期报错（这里用「无定义的 extern 调用」代替
 * `__error__` 属性，判据落在重定位/UND 上）。*/
#define BUILD_BUG_A() do { if (!(!(1))) trap_p2_a(); } while (0)
#define BUILD_BUG_B() do { if (!(!(1))) trap_p2_b(); } while (0)
#define HPAGE_PMD_NR ({ BUILD_BUG_A(); 0; })

/* P1：形参先被常量钉死，再凭它开一个死块 */
long p1_param_pinned(long orders)
{
	if (!IS_ENABLED0)
		orders = 0;
	if (orders > 0)
		trap_p1();
	return orders;
}

/* P2：`mm/shmem.c::shmem_alloc_and_add_folio` 的形状。
 * 第一跳删掉 `if (orders > 0)` 之后，`else { pages = 1; }` 被拍平进函数体顶层，
 * `pages` 这时才有一条常量可代 ⇒ 第二跳才能把 `pages > 1` 判死。 */
long *p2_shmem_like_impl(long err, long seed)
{
	long pages;
	unsigned long orders = seed;
	long *folio = 0;

	if (!IS_ENABLED0)
		orders = 0;

	if (orders > 0) {
		long suitable = pick_order(seed);
		while (suitable > 0) {
			pages = 1UL << suitable;
			if (pages == 1024)
				folio = (long *)1;
			if (folio)
				goto allocated;
			if (pages == HPAGE_PMD_NR)
				trap_p2_a();
			suitable--;
		}
	} else {
		pages = 1;
		folio = (long *)2;
	}
	if (!folio)
		return (long *)0;
allocated:
	if (err) {
		if (pages > 1) {
			if (pages == HPAGE_PMD_NR)
				trap_p2_b();
		}
	}
	return folio;
}

/* P3：局部初值已经是 0，后面又显式写了一次同样的常量 */
long p3_store_then_read(long seed)
{
	long x = 0;

	x = 0;
	if (x > 0)
		trap_p3();
	return seed + x;
}

/* N1：`x = 0` 之后、读点之前有一个被 goto 指向的标号 —— 那条路径绕过了赋值 */
long n1_goto_label(long c)
{
	long x = 7;

	if (c)
		goto mid;
	x = 0;
mid:
	if (x > 0)
		trap_keep_label();
	return x;
}

/* N2：x 在后面被别的常量写过（在分支里，可能不执行） */
long n2_reassign(long c, long seed)
{
	long x = 0;

	x = 0;
	if (c)
		x = 3;
	if (x > 0)
		trap_keep_reassign();
	return seed + x;
}

/* N3：x 的地址逃逸 —— 别人可能改它 */
long n3_addr_escapes(long seed)
{
	long x = 0;

	x = 0;
	sink_touch(&x);
	if (x > 0)
		trap_keep_addr();
	return seed + x;
}

/* N4：x 是 `++` 的目标 */
long n4_inc_target(long seed)
{
	long x = 0;

	x = 0;
	x++;
	if (x > 0)
		trap_keep_inc();
	return seed + x;
}

/* N5：存储在可能不执行的嵌套块里 */
long n5_nested_store(long c, long seed)
{
	long x = 9;

	if (c) {
		x = 0;
	}
	if (x > 0)
		trap_keep_nested();
	return seed + x;
}
