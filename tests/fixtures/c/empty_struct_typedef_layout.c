/* 空 struct typedef 与匿名结构体定义共享 `struct_defs` 的空名键。
 *
 * 内核形状：include/net/net_trackers.h 里
 *   typedef struct {} netdevice_tracker;
 * 之后任何头文件再写一个匿名结构体（`struct { ... } foo;`、匿名成员等），
 * 都会把它的字段以**空串**为键写进 struct_defs。resolve_type_latest 若按
 * 空名回查，`netdevice_tracker` 就突然“长出”那一个匿名结构体的字段：
 * 同一类型在不同 TU 里 sizeof 各不相同，`struct inet6_dev` 之类整体后移，
 * 跨 TU 的字段偏移互不兼容（实测 idé6 refcnt 一边 0x268 一边 0x280）。
 *
 * 失败码（按位或）：
 *   1 = sizeof(empty_t) 非 0
 *   2 = sizeof(empty2_t) 非 0
 *   4 = sizeof(struct outer) 错
 *   8 = offsetof(struct outer, x) 错
 *  16 = sizeof(struct outer2) 错
 *  32 = offsetof(struct outer2, y) 错
 *  64 = 成员访问路径读到的偏移错（type_of_member 走 resolve_type_latest_deep）
 */
typedef struct {} empty_t;

struct { void *a, *b, *c; } g_anon;

typedef struct {} empty2_t;

struct { int i; } g_anon2;

struct outer {
	void *p;
	empty_t e;
	int x;
};

struct outer2 {
	char c;
	empty2_t e;
	int y;
};

struct holder {
	struct outer2 o2;
	struct outer o;
};

int main(void)
{
	int bad = 0;
	volatile int zero = 0;
	struct holder *h = (struct holder *)(long)zero;

	if (sizeof(empty_t) != 0)
		bad |= 1;
	if (sizeof(empty2_t) != 0)
		bad |= 2;
	if (sizeof(struct outer) != 16)
		bad |= 4;
	if (__builtin_offsetof(struct outer, x) != 8)
		bad |= 8;
	if (sizeof(struct outer2) != 8)
		bad |= 16;
	if (__builtin_offsetof(struct outer2, y) != 4)
		bad |= 32;
	if ((char *)&h->o2.y - (char *)h != 4)
		bad |= 64;
	return bad;
}
