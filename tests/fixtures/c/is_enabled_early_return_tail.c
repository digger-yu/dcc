/*
 * 形状：`if (!IS_ENABLED(CONFIG_X)) return;` 守住的整段设备代码。
 *
 * 内核里 `arch/x86/mm/ioremap.c:130` 就是这么写的（`CONFIG_EFI` 关掉时），而被守住
 * 的那一段里调用的 `efi_mem_type()` / `efi_mem_attributes()` 在 CONFIG_EFI=n 时
 * **全树没有定义**（gcc 侧连名字都不出现在任何 .o 里）。gcc 靠死代码消除把整段删掉，
 * 于是链接没问题；dcc 把引用留下就是链接期 `undefined reference`。
 *
 * 两个层次都要钉住：
 *   * 不内联的形状（`direct_goto_shapes` / `switch_break_tail`）：`if (1) return;` 折成
 *     `return;` 之后同一层级的语句不可达 ⇒ 必须删掉；
 *   * 内联后的形状（`probe_*` 里的 `check_*` 被折进调用方）：被调方的 `return;` 在
 *     调用方里变成 `goto __inl<N>__l__inl<N>__end;`，**不是** `Return` ⇒ 只认 `Return`
 *     的不可达截断对这一整族完全无效（实测 `/tmp/kwefi/t.c`：dcc 留 3 条，gcc 0 条）。
 *
 * 反向对照（必须留引用）：
 *   * `check_keep()` —— 调用在 `return` 之前，活路；
 *   * `enabled_keep()` —— 同一个宏但配置项是真的定义了的（`IS_ENABLED` 为 1），整段是活代码；
 *   * `nested_label_keep()` —— 无条件 `goto` 跳进**后面嵌套块**里的标号，那一整段尾部
 *     都不能删（删了跳转目标就没了）。这条是拦网本身的反向判据。
 *
 * `IS_ENABLED`/`__is_defined` 一段照抄 `include/linux/kconfig.h` 与 `include/linux/compiler.h`：
 * 判据用的就是内核的真实宏，不是自己写一个 `if (0)` 糊过去。
 */

#define IS_ENABLED(option) __or(IS_BUILTIN(option), IS_MODULE(option))
#define IS_BUILTIN(option) __is_defined(option)
#define IS_MODULE(option) __is_defined(option##_MODULE)

#define __ARG_PLACEHOLDER_1 0,
#define __take_second_arg(__ignored, val, ...) val

#define __is_defined(x) ___is_defined(x)
#define ___is_defined(val) ____is_defined(__ARG_PLACEHOLDER_##val)
#define ____is_defined(arg1_or_junk) __take_second_arg(arg1_or_junk 1, 0)

#define __or(a, b) __take_second_arg(__take_second_arg(a 1, 0) 1, __take_second_arg(b 1, 0))

/* 关掉的配置项：不定义 CONFIG_EFI、CONFIG_EFI_MODULE（内核 .config 里是 `# CONFIG_EFI is not set`）*/
/* 打开的配置项：定义成 1（`__ARG_PLACEHOLDER_1` 吃逗号那条路）*/
#define CONFIG_LIVE_FEATURE 1

#define EFI_RUNTIME_SERVICES_DATA 7
#define EFI_BOOT_SERVICES_DATA 3
#define EFI_MEMORY_RUNTIME 0x8000000000000000UL

/* 陷阱：一律 extern 只声明不定义 —— 同一 TU 里定义的陷阱，本地 call 不发重定位，
 * 「没有指向它的条目」这条断言就永远是空话（已实测）。 */
extern int efi_mem_type(unsigned long addr);
extern unsigned long efi_mem_attributes(unsigned long addr);
extern int live_touch(void *p);

struct ioremap_desc {
	unsigned int flags;
};

/* ---- 形状 1：内联后死尾段（ioremap.c:130 的原始形状） ---- */
static void check_other(unsigned long addr, struct ioremap_desc *desc)
{
	if (!IS_ENABLED(CONFIG_EFI))
		return;

	if (efi_mem_type(addr) == EFI_RUNTIME_SERVICES_DATA ||
	    (efi_mem_type(addr) == EFI_BOOT_SERVICES_DATA &&
	     efi_mem_attributes(addr) & EFI_MEMORY_RUNTIME))
		desc->flags |= 2;
}

int probe_other(unsigned long addr, struct ioremap_desc *desc)
{
	check_other(addr, desc);
	return (int)desc->flags;
}

/* ---- 形状 2：反向对照 —— 调用在 return 之前，活路 ---- */
static void check_keep(unsigned long addr, struct ioremap_desc *desc)
{
	live_touch(desc);
	if (!IS_ENABLED(CONFIG_EFI))
		return;
	(void)efi_mem_type(addr);
}

int probe_keep(unsigned long addr, struct ioremap_desc *desc)
{
	check_keep(addr, desc);
	return (int)desc->flags;
}

/* ---- 形状 3：反向对照 —— 配置项真的开着，整段是活代码 ---- */
static void check_enabled(unsigned long addr, struct ioremap_desc *desc)
{
	if (!IS_ENABLED(CONFIG_LIVE_FEATURE))
		return;
	(void)efi_mem_type(addr);
	desc->flags |= 4;
}

int probe_enabled(unsigned long addr, struct ioremap_desc *desc)
{
	check_enabled(addr, desc);
	return (int)desc->flags;
}

/* ---- 形状 4：不内联的 `goto` 形状（`if (1) goto out;` 后的尾段不可达） ---- */
int goto_tail(unsigned long addr, struct ioremap_desc *desc)
{
	if (!IS_ENABLED(CONFIG_EFI))
		goto out;

	(void)efi_mem_attributes(addr);
	desc->flags |= 8;
out:
	return (int)desc->flags;
}

/* ---- 形状 5：`break` 后的尾段不可达 ---- */
int break_tail(unsigned long addr)
{
	int i, n = 0;

	for (i = 0; i < 4; i++) {
		if (!IS_ENABLED(CONFIG_EFI))
			break;
		n += efi_mem_type(addr);
	}
	return n;
}

/* ---- 形状 6：无条件 `goto` 跳进后面嵌套块里的标号 ⇒ 整段尾部都不能删 ---- */
int nested_label_keep(unsigned long addr)
{
	goto inner;
	{
		int n;
	inner:
		n = efi_mem_type(addr);
		return n;
	}
}

/* ---- 形状 7：`continue` 后的尾段不可达（外层还有活路） ---- */
int continue_tail(unsigned long addr)
{
	int i, n = 0;

	for (i = 0; i < 4; i++) {
		n += live_touch(&n);
		if (!IS_ENABLED(CONFIG_EFI))
			continue;
		n += efi_mem_type(addr);
	}
	return n;
}
