/* 夹具：`asm goto` 的落点**定义**在 `if (0)` 死块里，而引用它的 `asm goto` 在活代码里。
 *
 * 现场（build13，`arch/x86/kernel/signal_64.c`）：
 *   error[E1103]: relocation site '.rela<custom section>' references internal label
 *   '.Ll_x64_setup_rt_frame___inl9261__llocal_label' which has no entry in this TU's symtab
 * 形状来自 `include/linux/uaccess.h` 的 `unsafe_get_user()`：
 *   do {
 *           __label__ local_label;
 *           arch_unsafe_get_user(x, ptr, local_label);   // asm goto(... : : : : local_label)
 *           if (0) {
 *           local_label:
 *                   goto label;
 *           }
 *   } while (0)
 * 这段代码坐在 `__always_inline` 函数里，内联进 caller 时 `local_label` 按 `__inl<N>__l`
 * 前缀改名成 `__inl9261__llocal_label`。死分支折叠的拦网用 `is_inliner_label` 判「是不是
 * 内联器自造标号」：前缀 ⇒ 自造 ⇒ 可删，于是 `if (0)` 整块被删，定义消失而 `asm goto`
 * 的落点还在 ⇒ 发射期 E1103。
 *
 * 本夹具有两条判据：
 *   * 编译必须过（回归现场就是编译失败），且 `-O2` 产物里 `local_label` 必须是**有定义**的
 *     本地符号 —— 定义还在，才说明死块没被删；
 *   * 运行结果必须与 gcc 逐档一致：`g_fault = 1` 时 `asm goto` 真的跳进那个死块走
 *     `return -1`，所以「保留死块」不只是别让编译器报错，它还是活语义的一部分。
 *
 * 反向对照在 `asm_goto_label_in_dead_branch.c`（jump_label 族：`asm goto` 与落点同坐一条
 * 被丢的臂，必须仍然能删）。那一条改了这里的判据就会红，两条互为约束。
 */
extern int printf(const char *, ...);

int g_fault;
int g_src;
int g_dst;

/* 与 `arch_unsafe_get_user()` 的展开同形：`__label__` 声明、`asm goto` 在活代码里、
 * 落点定义在 `if (0)` 里。 */
static __attribute__((__always_inline__)) int load_or_fail(void)
{
	__label__ local_label;

	asm goto("testl %0, %0\n\t"
		 "jnz %l[bad]\n\t"
		 "movl %1, %%eax\n\t"
		 "movl %%eax, %2\n"
		 :
		 : "r"(g_fault), "m"(g_src), "m"(g_dst)
		 : "eax", "cc", "memory"
		 : bad);
	if (0) {
	bad:
		return -1;
	}
	return 0;
}

int read_or_default(void)
{
	if (load_or_fail())
		return -7;
	return g_dst;
}

/* 第二个调用点：同一份 `__always_inline` 体内联两次，各拿一套 `__inl<N>__l` 前缀标号。 */
int read_plus_one(void)
{
	int v = 0;

	if (!load_or_fail())
		v = g_dst;
	return v + 1;
}

int main(void)
{
	int a, d1, b, d2;

	g_src = 1234;
	g_fault = 0;
	a = read_or_default();
	d1 = g_dst;
	g_fault = 1;
	g_dst = 0;
	b = read_or_default();
	d2 = g_dst;
	printf("ok=%d dst=%d bad=%d dst2=%d\n", a, d1, b, d2);

	g_fault = 0;
	g_src = 40;
	printf("plus=%d\n", read_plus_one());
	g_fault = 1;
	printf("plus_bad=%d\n", read_plus_one());
	return 0;
}
