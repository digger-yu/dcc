/* 夹具：内联展开在**死臂**里留下的标号（尤其是 `asm goto` 的落点）不该挡住恒假支塌缩。
 *
 * 现场（§5.0o）：`include/linux/jump_label.h` 的 `static_branch_unlikely()` 写成
 *   if (__builtin_types_compatible_p(typeof(*x), struct static_key_true))
 *           branch = arch_static_branch_jump(&(x)->key, false);
 *   else if (…struct static_key_false…) branch = arch_static_branch(&(x)->key, false);
 *   else branch = ____wrong_branch_error();
 * `arch_static_branch_jump()` 是带 `asm goto` 的 `__always_inline`：一被内联，展开区里的
 * `l_yes` 就按 `__inl<N>__l` 前缀改名成 `__inl<N>__ll_yes`，落在**要丢弃**的那一臂里。
 * 拦网「被丢的臂含标号就不折」原先只豁免 `__inl<N>__end` 那一支文法，认不出改名后的
 * 落点 ⇒ 陷阱调用活到链接期。实测 `drivers/tty/serial/8250/8250_platform.o`（`DO_ONCE()`）、
 * `net/ipv4/route.o` 等 10 个对象各留 1 条 `undefined reference to ____wrong_branch_error`，
 * 而 `gcc -O2` 一条都不留。
 *
 * 四组判据（期望名单先拿 `gcc -O2` 校准，不是推的）：
 *   * `trap_dead_*`：恒假臂里的陷阱 ⇒ 引用必须消失（这一族缺陷的正判据）；
 *   * `trap_kept_asm`：活臂里的陷阱 ⇒ 引用必须在（防「一律删」）；
 *   * `trap_jumpin`：恒假臂里有**源码写的标签**、且函数别处 `goto` 跳得进去 ⇒ 引用必须在
 *     （把 `stmt_has_label` 整个删掉的改法能过前两条，但必挂这一条）。
 */
extern void trap_dead_asm(void);
extern void trap_dead_end(void);
extern void trap_kept_asm(void);
extern void trap_jumpin(void);

struct key {
	int enabled;
};
static struct key my_key = { .enabled = 1 };

/* 带 `asm goto` 的强制内联体：与 `arch_static_branch_jump()` 同形。
 * `"i"` 那一枚操作数必须是**装配期常量地址**（内核传的是 `&___once_key`），
 * 因此这里直接取文件作用域对象的地址，而不是把指针当形参传进去 —— 后者 gcc 与
 * dcc 都在编译期报 `impossible constraint in 'asm'`，那是另一条已知的正确行为。 */
static __attribute__((__always_inline__)) int probe_asm(void)
{
	asm goto("1: jmp %l[l_yes]\n\t" : : "i"(&my_key) : : l_yes);
	return 0;
l_yes:
	return 1;
}

/* 只有普通 return 的强制内联体：靠内联器自造的 `__inl<N>__end` 出口标签 */
static __attribute__((__always_inline__)) int probe_end(struct key *k)
{
	if (k->enabled)
		return 1;
	return 0;
}

/* 恒真的门卫，占 `__builtin_types_compatible_p(...)` 的位置 */
#define GUARD_TRUE (sizeof(int) >= 4)

/* A1) 恒假臂里坐一次 `asm goto` 展开 ⇒ 陷阱随臂一起消失 */
int dead_arm_asm_goto(struct key *k)
{
	int r;
	if (GUARD_TRUE)
		r = probe_asm();
	else {
		r = probe_asm();
		if (r == 3)
			trap_dead_asm();
	}
	return r;
}

/* A2) 恒假臂里坐一次「只有出口标签」的展开 ⇒ 同上（gdt_idt.c 的老现场，防退化） */
int dead_arm_inline_end(struct key *k)
{
	int r = probe_end(k);
	if (GUARD_TRUE)
		r += 1;
	else {
		r += probe_end(k);
		if (r == 99)
			trap_dead_end();
	}
	return r;
}

/* B) 活臂里的 `asm goto`：陷阱引用与展开体都不能被抹掉 */
int kept_arm_asm_goto(struct key *k, int c)
{
	int r = probe_asm();
	if (c)
		trap_kept_asm();
	return r;
}

/* C) 反向对照：恒假臂里有源码标签，别处 `goto` 跳得进去 ⇒ 拦网必须仍然生效 */
int jump_in_target_survives(struct key *k, int c)
{
	int r = probe_end(k);
	if (GUARD_TRUE)
		r += 1;
	else {
target_here:
		trap_jumpin();
		r = 7;
	}
	if (c == 1)
		goto target_here;
	return r;
}
