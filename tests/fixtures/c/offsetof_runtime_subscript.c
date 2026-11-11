/*
 * 报告 §5.59：`offsetof(struct tnode, kv[0].tnode[n])` 的成员路径里同时有
 * 「常量下标」「后续成员」「运行期下标」三段。旧 codegen 在 `kv[0]` 之后停在
 * 数组类型上，基础偏移只剩 `offsetof(kv)`；运行期那一段又用数组类型当 stride、
 * 把 `tnode[n]` 整个丢掉。内核里 `TNODE_SIZE(1ul << bits)` 因此恒为 32，
 * `tnode_new()` 只 kzalloc 32 字节，`tn = tnode->kv`（+0x20）落在对象外。
 *
 * 这里刻意复刻内核布局：key_vector 16 字节，kv 在 tnode 的 +32，tnode[] 在
 * kv 的 +8。断言全部用 sizeof 表达，不写死 40/8。
 */
#include <stddef.h>
#include <stdio.h>

struct key_vector {
	unsigned int key;
	unsigned char pos, bits, slen;
	union {
		struct { struct key_vector *next; } leaf;
		/* DECLARE_FLEX_ARRAY 的展开形：弹性数组必须在 struct 尾，
		 * 不能直接放进 union（C11 6.7.2.1p3）。 */
		struct {
			struct { } __empty_tnode;
			struct key_vector *tnode[];
		};
	};
};

struct tnode {
	void *rcu_next;
	void *rcu_func;
	unsigned int empty_children;
	unsigned int full_children;
	struct key_vector *parent;
	struct key_vector kv[1];
};

#define TNODE_SIZE(n) offsetof(struct tnode, kv[0].tnode[n])

static unsigned long var_size(unsigned long n)
{
	return TNODE_SIZE(n);
}

static unsigned long shift_size(unsigned int bits)
{
	return TNODE_SIZE(1ul << bits);
}

int main(void)
{
	unsigned long stride = sizeof(struct key_vector *);
	unsigned long base = offsetof(struct tnode, kv[0].tnode[0]);
	unsigned long c2 = TNODE_SIZE(2);
	unsigned long c4 = TNODE_SIZE(4);
	unsigned long v2 = var_size(2);
	unsigned long v4 = var_size(4);
	unsigned long sh1 = shift_size(1);
	unsigned long sh2 = shift_size(2);

	printf("c2=%lu c4=%lu v2=%lu v4=%lu sh1=%lu sh2=%lu base=%lu stride=%lu\n",
	       c2, c4, v2, v4, sh1, sh2, base, stride);

	if (c2 != base + 2 * stride)
		return 1;
	if (c4 != base + 4 * stride)
		return 2;
	if (v2 != base + 2 * stride)
		return 3;
	if (v4 != base + 4 * stride)
		return 4;
	if (sh1 != base + 2 * stride)
		return 5;
	if (sh2 != base + 4 * stride)
		return 6;
	return 0;
}
