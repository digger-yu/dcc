/* 夹具：短路运算符**不被求值的那一侧**，以及「常量存进局部 → 跨过有 goto 指向的标号后代回」。
 *
 * 两条独立现场（§5.0o）：
 *   1) `include/linux/jump_label.h` 的 `static_key_enabled()` 写成
 *      `if (!types_compatible_p(…) && !types_compatible_p(…) && !types_compatible_p(…))
 *              ____wrong_branch_error();` —— 第一个 `!` 折成 0 之后，按 C11 6.5.13 右侧
 *      那一侧**不被求值**。旧规则问的是「两侧有没有副作用」，`Call` 一律答「有」 ⇒
 *      常量条件塌缩后仍把整条表达式当语句发出去，链接期留
 *      `undefined reference to ____wrong_branch_error`。`?:`（6.5.15p2）同一条道理。
 *   2) `mm/shmem.c`/`generic_pt` 那一族「常量先存进局部、跨过**有 goto 指向的标号**后再读
 *      它」：cprop 的代回器缺 `Expr::Ident` 叶臂，纯拷贝 `r1 = r2` 从来代不回 ⇒ 恒 0 的
 *      条件读不出来，死臂里的陷阱调用留在产物里（实测
 *      `drivers/iommu/generic_pt/fmt/iommu_amdv1.o` 引 `__pt_no_sw_bit`）。
 *
 * 期望名单是拿 `gcc -O2`/`-O3` 实测的：`trap_*` 五条 0 引用，`keep_*` 四条全在。
 * `keep_*` 那一半是防过修的：把「短路右侧」当成「永远可丢」的改法会一并抹掉
 * `keep_f_and`（`1 && 调用`）与 `keep_g_or`（`0 || 调用`）—— 那两侧的调用是真要执行的。
 */
/* 与内核 `extern bool ____wrong_branch_error(void);` 同位：`&&`/`||`/`?:` 的操作数
 * 必须是标量，返回 void 的陷阱函数在那三个位置上本身就是**非法 C**
 * （`gcc -O2` 报 `void value not ignored as it ought to be`）。 */
extern int trap_a_and(void);
extern int trap_b_or(void);
extern int trap_c_cond(void);
extern void trap_d_xlabel(void);
extern int trap_e_nested(void);

extern int keep_f_and(void);
extern int keep_g_or(void);
extern int keep_h_cond(void);
extern int keep_i_and(void);

static __attribute__((__always_inline__)) int inner(int c)
{
	if (c)
		trap_e_nested();
	return 0;
}

static __attribute__((__always_inline__)) int outer(int c)
{
	return inner(c);
}

/* gone A) `&&` 左侧恒 0 ⇒ 右侧那次调用不求生 */
int a_and_dead(int *p)
{
	if (0 && trap_a_and())
		*p = 1;
	*p += 2;
	return *p;
}

/* gone B) `||` 左侧恒非 0 ⇒ 右侧不求生 */
int b_or_dead(int *p)
{
	if (1 || trap_b_or())
		*p = 2;
	return *p;
}

/* gone C) `?:` 条件恒假 ⇒ 未取用的那一臂不求生（丢弃值，不走常量折叠顶替） */
int c_cond_dead(int *p)
{
	(void)(0 ? trap_c_cond() : 7);
	return ++*p;
}

/* gone D) 常量跨过「有 goto 指向的标号」代回（s11 形） */
int d_xlabel_dead(int *p)
{
	int r1, r2;
	if (0) {
		r2 = 1;
		goto e;
	} else {
		r2 = 0;
		goto e;
	}
e:
	;
	r1 = r2;
	if (r1)
		trap_d_xlabel();
	return r1;
}

/* gone E) 两层 `always_inline` 链上的常量条件 */
int e_nested_dead(int *p)
{
	if (outer(0))
		*p = 4;
	return 0;
}

/* kept F) `1 && 调用`：求值次序决定右侧一定会被执行 */
int f_and_live(int *p)
{
	if (1 && keep_f_and())
		*p = 1;
	return 0;
}

/* kept G) `0 || 调用`：同上 */
int g_or_live(int *p)
{
	if (0 || keep_g_or())
		*p = 2;
	return 0;
}

/* kept H) `?:` 条件不是常量 ⇒ 两臂都可能被求值 */
int h_cond_live(int *p, int c)
{
	(void)(c ? keep_h_cond() : 0);
	return 0;
}

/* kept I) 左侧是运行期值 ⇒ 短路位置定不下来，右侧的调用必须在 */
int i_runtime_and(int *p, int k)
{
	if (k && keep_i_and())
		*p = 5;
	return 0;
}
