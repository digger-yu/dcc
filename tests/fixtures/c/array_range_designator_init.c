/*
 * GNU range designator (`[first ... last] = value`) in *static* initializers.
 *
 * 报告 §5.37：`init_list.rs` 的数组分支给范围设计符单独写了一份内联编码，
 * 只认标量，聚合值被 `eval_const_expr` 当成不支持的表达式写成全 0；链式
 * 形式 `[f ... l].field` 更是直接交给 `resolve_designator`（那里 Range 固定
 * 返回 None）而被整项静默丢弃。内核 `kernel/ucount.c` 的
 *
 *     static struct hlist_nulls_head ucounts_hashtable[1024] = {
 *         [0 ... 1023] = HLIST_NULLS_HEAD_INIT(0)
 *     };
 *
 * 正是这个形状：NULLS 哨兵（1）退化成 NULL 后，空链表不再"空"，
 * `hlist_nulls_for_each_entry_rcu` 把 NULL 当节点去读 `iter->uid`（+0x18），
 * boot 在 devtmpfs 处 #PF。
 *
 * 失败码：
 *   1   = 顶层 range + 聚合花括号（哨兵值丢失）
 *   2   = 链式 range `[f ... l].field`
 *   4   = 标量 range（回归）
 *   8   = 部分 range `[1 ... 2]`
 *   16  = range + 位域元素
 *   32  = range + char 数组元素（字符串整行填充）
 *   64  = range + 指针元素（字符串指针）
 *   128 = range + 复合字面量元素
 *   256 = range 与普通项混合 + 前跳后回退
 *   512 = 内核 hlist_nulls 语义（空表遍历不得崩，且必须判为"空"）
 */
#include <stddef.h>

/* ---- 1. 顶层 range + 聚合花括号 ------------------------------------- */
struct head {
	void *first;
};
struct head table[8] = { [0 ... 7] = { .first = (void *)1 } };

/* ---- 2. 链式 range `[f ... l].field` -------------------------------- */
struct pair {
	int a;
	int b;
};
struct pair chain[4] = { [0 ... 3].b = 7 };

/* ---- 4. 标量 range（回归） ------------------------------------------ */
int scalars[4] = { [0 ... 3] = 5 };
int partial[4] = { [1 ... 2] = 9 };

/* ---- 16. range + 位域元素 ------------------------------------------- */
struct bits {
	unsigned lo : 3;
	unsigned hi : 5;
	unsigned rest : 8;
};
struct bits bitfields[4] = { [0 ... 3] = { .hi = 7 } };

/* ---- 32. range + char 数组元素 -------------------------------------- */
char rows[3][6] = { [0 ... 2] = "hi" };

/* ---- 64. range + 指针元素 ------------------------------------------- */
const char *names[3] = { [0 ... 2] = "z" };

/* ---- 128. range + 复合字面量元素 ------------------------------------ */
struct head lis[2] = { [0 ... 1] = (struct head){ .first = (void *)2 } };

/* ---- 256. 与普通项混合 / 前跳后回退 --------------------------------- */
struct head mixed[6] = {
	[4] = { .first = (void *)3 },
	[1 ... 2] = { .first = (void *)4 },
	[5] = { .first = (void *)5 },
};

/* ---- 512. 内核 hlist_nulls 语义 ------------------------------------- */
struct nulls_node {
	struct nulls_node *next;
	struct nulls_node **pprev;
};
struct ucounts {
	struct nulls_node node; /* offset 0 */
	void *ns;
	unsigned int uid;
};
#define NULLS_MARKER(v) (1UL | (((unsigned long)(v)) << 1))
static struct nulls_node ucounts_table[8] = {
	[0 ... 7] = { .next = (void *)NULLS_MARKER(0) },
};

static int is_nulls(const struct nulls_node *p)
{
	return (unsigned long)p & 1;
}

/* 空表必须一次都不进循环体；旧产物 head->next == 0 ⇒ is_nulls 假 ⇒
 * 拿 NULL 当节点读 node 之后的成员，当场段错误。 */
static int lookup_uid(struct nulls_node *head, unsigned int uid)
{
	const struct ucounts *it;

	for (const struct nulls_node *pos = head->next; !is_nulls(pos);
	     pos = pos->next) {
		it = (const struct ucounts *)((const char *)pos -
					      offsetof(struct ucounts, node));
		if (it->uid == uid)
			return 1;
	}
	return 0;
}

int main(void)
{
	int bad = 0;

	for (int i = 0; i < 8; i++)
		if (table[i].first != (void *)1)
			bad |= 1;

	for (int i = 0; i < 4; i++)
		if (chain[i].a != 0 || chain[i].b != 7)
			bad |= 2;

	for (int i = 0; i < 4; i++)
		if (scalars[i] != 5)
			bad |= 4;

	for (int i = 0; i < 4; i++)
		if (partial[i] != (i == 1 || i == 2 ? 9 : 0))
			bad |= 8;

	for (int i = 0; i < 4; i++)
		if (bitfields[i].lo != 0 || bitfields[i].hi != 7 ||
		    bitfields[i].rest != 0)
			bad |= 16;

	for (int i = 0; i < 3; i++)
		if (rows[i][0] != 'h' || rows[i][1] != 'i' || rows[i][2] != 0)
			bad |= 32;

	for (int i = 0; i < 3; i++)
		if (names[i][0] != 'z' || names[i][1] != 0)
			bad |= 64;

	for (int i = 0; i < 2; i++)
		if (lis[i].first != (void *)2)
			bad |= 128;

	for (int i = 0; i < 6; i++) {
		const void *want = (i == 4) ? (void *)3
				 : (i == 1 || i == 2) ? (void *)4
				 : (i == 5) ? (void *)5
					    : (void *)0;
		if (mixed[i].first != want)
			bad |= 256;
	}

	for (int i = 0; i < 8; i++)
		if (ucounts_table[i].next != (void *)NULLS_MARKER(0))
			bad |= 512;
	/* 走到这里说明空表遍历没有解引用 NULL */
	if (lookup_uid(&ucounts_table[0], 0) != 0)
		bad |= 512;
	if (lookup_uid(&ucounts_table[7], 1234) != 0)
		bad |= 512;

	return bad;
}
