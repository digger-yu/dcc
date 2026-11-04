/* 夹具：条件位上的**语句表达式**（`static __always_inline` 被内联后的形状）。
 *
 * 现场（§5.0o）：`drivers/iommu/generic_pt/fmt/iommu_amdv1.o` 的
 *   if (pts_feature(&pts, PT_FEAT_DMA_INCOHERENT) && !pt_test_sw_bit_acquire(&pts, …))
 *           flush_writes_item(&pts);
 * `pts_feature()`/`pt_feature()` 是「早退链 + 一条运行期返回」的 `__always_inline`：
 * 内联后条件左操作数成了 `({ unsigned __inl1$_n = n; …; __inl1$rv = 0; goto end; end: 0; })`，
 * 尾值已经是字面量 0。三格各自都会把这条 `&&` 钉住：
 *   1) 语句表达式求值器把「声明初值算不出常量」整块判非常量（`__inl1$_n = n` 里的 `n`
 *      是**外层**函数的变量）；
 *   2) `discarded_value_needs_code` 对 `StmtExpr` 一律答「需要代码」；
 *   3) `&&`/`||` 的丢弃判据两侧都问，右侧那个调用就把整条条件钉住。
 * 后果是只有声明、没有定义的陷阱函数活到链接期（`gcc -O2` 一条都不留）。
 *
 * 判据（`gone` 三条按 gcc 实测、`kept` 三条是刻意保守）：
 *   * `trap_se_and` / `trap_se_or` / `trap_se_pure`：常量早退链 ⇒ 引用必须消失；
 *   * `trap_se_live`：链子的尾值是运行期值 ⇒ 调用一定会发生，引用必须在；
 *   * `trap_se_outer`：语句表达式往**块外**的局部写、尾值读那个局部 —— 尾值因此是运行期
 *     值，`&&` 的右侧有可能被求值，调用必须在。求值器在这一格**必须**继续拒绝：
 *     `opt::fold_expr` 会把求值成功的语句表达式整个换成常量，那次外部存储会随替换一起
 *     消失（副作用丢了）。这一条同时是「顺手放开外部存储」那种改法的当场捕获点：
 *     放开了就会把 `trap_se_outer` 一起折掉，而 gcc 留着它。
 */
extern void trap_se_and(void);
extern void trap_se_or(void);
extern void trap_se_pure(void);
extern void trap_se_live(void);
extern void trap_se_outer(void);

struct holder {
	unsigned long f;
};

/* 常量早退链：两条守卫都是编译期可判的位运算，第三条返回运行期值 */
static __attribute__((__always_inline__)) int feat_true(struct holder *h, unsigned n)
{
	if ((1UL << 2) & (1UL << n))
		return 1;
	if (!((1UL << 1) & (1UL << n)))
		return 0;
	return (int)(h->f & (1UL << n));
}

/* 守卫直接写死成常量的形（`h` 一次都没用到 ⇒ 那条纯读拷贝必须能被跳过） */
static __attribute__((__always_inline__)) int feat_pure(int n)
{
	if (n == 9)
		return 7;
	return 0;
}

/* gone A) `&&`：左操作数是常量 0 的语句表达式 ⇒ 右侧不求生 */
int a_se_and(struct holder *h, int *p)
{
	if (feat_true(h, 8) && (trap_se_and(), 1))
		*p = 1;
	return *p;
}

/* gone B) `||`：左操作数是常量非 0 的语句表达式 ⇒ 右侧不求生 */
int b_se_or(struct holder *h, int *p)
{
	if (feat_true(h, 2) || (trap_se_or(), 1))
		*p = 2;
	return *p;
}

/* gone C) 被内联体里只剩纯读拷贝（`int m = n` 那种实体化形）⇒ 尾值照样是常量 */
int c_se_pure(int *p)
{
	if (feat_pure(3) && (trap_se_pure(), 1))
		*p = 3;
	return 0;
}

/* kept D) 尾值是运行期值 ⇒ `&&` 右侧一定会被求值 */
int d_se_live(struct holder *h, int *p)
{
	if (feat_true(h, 1) && (trap_se_live(), 1))
		*p = 4;
	return (int)h->f;
}

/* kept E) 反向对照：语句表达式写的是**块外**的局部，尾值再读它 */
int e_se_outer(struct holder *h, int *p)
{
	int x = 0;
	if (({ x = (int)h->f; x; }) && (trap_se_outer(), 1))
		*p = 5;
	return x;
}
