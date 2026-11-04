#include <stdio.h>
#include <stddef.h>
/* 具名 struct/union 之后的标识符是**声明符**，哪怕它正好等于一个可见 typedef 名
 * （C 的成员命名空间与 typedef 命名空间独立）。以前一条「看着像类型名就属于下一个
 * 字段」的启发式不分匿名/具名，于是 `struct mutex rl_t;` 的成员名被吞：字段丢名 +
 * 布局错位。这里钉住两件事：
 *   1) 具名情形 —— 每个成员的偏移与 sizeof 必须与 gcc 实测逐个相等；
 *   2) 反向对照（那条启发式真正服务的匿名内联体）—— `struct { int a; int b; };`
 *      仍然要展平进父结构，`after_anon` 的偏移仍是 8。
 * 退出码 = 第一条不匹配的形状编号。 */
typedef void (*rl_t)(void *);
typedef int anon_marker;

struct mutex {
	int owner;
};

struct named_case {
	int head;
	struct mutex rl_t; /* 成员名 = typedef 名 */
	int after_this;
	char tail[3];
	int last;
};

struct anon_case {
	int first;
	struct
	{
		int a;
		int b;
	};
	int after_anon;
};

static unsigned long g_off;

int main(void)
{
	/* 1) 具名：gcc 实测 size=20 head=0 rl=4 after=8 tail=12 last=16 */
	if (sizeof(struct named_case) != 20)
		return 1;
	if (offsetof(struct named_case, head) != 0)
		return 2;
	if (offsetof(struct named_case, rl_t) != 4)
		return 3;
	if (offsetof(struct named_case, after_this) != 8)
		return 4;
	if (offsetof(struct named_case, tail) != 12)
		return 5;
	if (offsetof(struct named_case, last) != 16)
		return 6;

	/* 成员真的可读写，且读写的是它自己那块存储 */
	struct named_case n = { 0 };
	n.rl_t.owner = 42;
	n.after_this = 7;
	if (n.head != 0 || n.rl_t.owner != 42 || n.after_this != 7)
		return 7;
	g_off = (unsigned long)&n.rl_t - (unsigned long)&n;
	if (g_off != 4)
		return 8;

	/* 2) 反向对照：匿名内联体仍被展平（gcc 实测 size=16、after_anon=12） */
	if (sizeof(struct anon_case) != 16)
		return 9;
	if (offsetof(struct anon_case, after_anon) != 12)
		return 10;

	struct anon_case a = { 0 };
	a.a = 1;
	a.b = 2;
	a.after_anon = 3;
	if (a.a != 1 || a.b != 2 || a.after_anon != 3)
		return 11;

	return 0;
}
