/* 内核 DO_ONCE() 的两条必经形状（include/linux/once.h + jump_label.h），去头文件依赖：
 *   A) 语句表达式里的 `static` 键必须真的发出定义（mangled 名），而且
 *      inline asm 的 `"i"` 立即数（`__jump_table` 条目）必须指向**那个 mangled 名**，
 *      不能指向源码名 —— 后者在 symtab 里根本不存在，链接期就是
 *      `undefined reference to '___once_key'`（实测全树 47 个对象）。
 *   B) static_key_enabled() 的 `__builtin_types_compatible_p` 守卫必须折成假，
 *      死支里那条 `trap_wrong_branch()` 不许留下引用。 */
typedef _Bool bool;

struct static_key {
	int enabled;
};
struct static_key_true {
	struct static_key key;
};

extern bool trap_wrong_branch(void);
extern int static_key_count(struct static_key *s);

#define DEFINE_STATIC_KEY_TRUE(name) \
	struct static_key_true name = { .key = { .enabled = 1 } }

#define static_key_enabled(x)						\
({									\
	if (!__builtin_types_compatible_p(typeof(*x), struct static_key) && \
	    !__builtin_types_compatible_p(typeof(*x), struct static_key_true)) \
		trap_wrong_branch();					\
	static_key_count((struct static_key *)x) > 0;			\
})

/* A：把键的地址写进自定义节的一条 `.quad`（这正是 jump table 条目的做法） */
int use_once_key(void)
{
	static DEFINE_STATIC_KEY_TRUE(___once_key);

	__asm__ volatile(
		".pushsection __jump_table, \"aw\"\n\t"
		".align 8\n\t"
		".quad %[key]\n\t"
		".popsection\n"
		:
		: [key] "i" (&___once_key)
		: "memory");
	return static_key_enabled(&___once_key);
}

/* B：普通全局键，同一套判据 */
static DEFINE_STATIC_KEY_TRUE(global_key);

int use_global_key(void)
{
	return static_key_enabled(&global_key);
}
