/* 计划项 ①b：一般的「左值地址」形态 —— `&a[i][j]`（下标基址本身是计算出来的数组行）。
 *
 * 这类形状过去在 SSA 层表达不了，`Expr::AddrOf` 降级遇到内层基址仍是数组时整函数
 * 退回 AST 通道（正确但丢掉全部 SSA 优化）。现在由逐层 `InstKind::IndexAddr` 表达，
 * 且地址类临时值不被内联进取址操作数（否则重建出 `&(&m[i])[j]` 嵌套取址，历史上从
 * 错值恶化成段错误）。
 *
 * 覆盖轴：
 *   * 一维 `&a[i]`（含写入 + 指针差 p-a）；
 *   * 二维 `&m[i][j]`（行步长非 2 次幂：int[3][5] = 20 字节行）；
 *   * 三维 `&c[i][j][k]`（i 步长 12、j 步长 4）；
 *   * 常量 / 变量下标混用：`&m[i][1]`、`&m[2][j]`；
 *   * 8 字节元素、行步长 16：`long long e[10][2]`（正是历史段错误夹具的形状）；
 *   * 经 `(*p)[j]` 的取址（指针指数组形参的衰减基址）；
 *   * 把行地址 `int (*rp)[5] = &m[i]` 再解引用下标；
 *   * 全局二维数组 `&gm[i][j]`（基址是 SymAddr）。
 *
 * 只打印**整数**（元素值与以元素为单位的指针差），绝不打印裸地址：ASLR 下裸地址在
 * gcc 与 dcc 之间必然不同，会产生与编译器无关的假 MISMATCH。
 *
 * 期望值全部先经 gcc -O0 实测得到、再按 C 布局语义手算复核；自校验失败时打印完整
 * got/want，并只返回 0/1（不用 `return bad` 的位掩码 —— 退出码只保留低 8 位会掩盖
 * 第 8 位以上）。
 */
#include <stdio.h>

static int m35[3][5];
static int c234[2][3][4];
static long long e10x2[10][2];
static int gm[4][7];

static int bad;

#define CHK(label, got, want)                                                   \
    do {                                                                        \
        long long g_ = (long long)(got);                                        \
        long long w_ = (long long)(want);                                       \
        if (g_ != w_) {                                                         \
            printf("FAIL %-20s got=%lld want=%lld\n", (label), g_, w_);         \
            bad++;                                                              \
        } else {                                                                \
            printf("ok   %-20s = %lld\n", (label), g_);                         \
        }                                                                       \
    } while (0)

/* 一维：`&a[i]` 写入后读回，并把指针差（元素单位）带回来。 */
static int
one_d(int i)
{
	int a[6];
	int k;
	int *p;

	for (k = 0; k < 6; k++) {
		a[k] = k * 3;
	}
	p = &a[i];
	*p = 999;
	return a[i] + (int)(p - a);
}

/* 二维 + 行步长非 2 次幂：int[3][5]。q = &m35[i][j]，写入再读回，指针差 = i*5 + j。 */
static int
two_d(int i, int j)
{
	int x;
	int y;
	int *q;

	for (x = 0; x < 3; x++) {
		for (y = 0; y < 5; y++) {
			m35[x][y] = x * 10 + y;
		}
	}
	q = &m35[i][j];
	*q = 7;
	return m35[i][j] + (int)(q - &m35[0][0]);
}

/* 常量下标混用：外层变量、内层常量 `&m35[i][1]`。 */
static int
two_d_inner_const(int i)
{
	int *q = &m35[i][1];

	return (int)(q - &m35[0][0]);
}

/* 常量下标混用：外层常量、内层变量 `&m35[2][j]`。 */
static int
two_d_outer_const(int j)
{
	int *q = &m35[2][j];

	return (int)(q - &m35[0][0]);
}

/* 三维 int[2][3][4]：i 步长 12、j 步长 4、k 步长 1。 */
static int
three_d(int i, int j, int k)
{
	int *q = &c234[i][j][k];

	return (int)(q - &c234[0][0][0]);
}

/* 8 字节元素 + 行步长 16（历史段错误夹具形状）：`&e10x2[i][j]`。 */
static int
row_stride8(int i, int j)
{
	int x;
	long long *q;

	for (x = 0; x < 10; x++) {
		e10x2[x][0] = x;
		e10x2[x][1] = x;
	}
	q = &e10x2[i][j];
	*q = 4242;
	return (int)(q - &e10x2[0][0]) + (int)(e10x2[i][j] == 4242);
}

/* 经 `(*p)[j]` 的取址：p 是 int(*)[5]，`(*p)` 是数组类型 → 衰减基址。 */
static int
deref_decay(int (*p)[5], int j)
{
	int *q = &(*p)[j];

	return (int)(q - &p[0][0]);
}

/* 先把行地址存进 `int (*rp)[5] = &m35[i]`，再 `&(*rp)[j]` 下标。 */
static int
row_ptr_reindex(int i, int j)
{
	int (*rp)[5] = &m35[i];
	int *q = &(*rp)[j];

	return (int)(q - &m35[0][0]);
}

/* 全局二维数组：基址是全局符号（SymAddr），指针差仍是 i*7 + j。 */
static int
global_2d(int i, int j)
{
	int x;
	int y;
	int *q;

	for (x = 0; x < 4; x++) {
		for (y = 0; y < 7; y++) {
			gm[x][y] = x * 100 + y;
		}
	}
	q = &gm[i][j];
	*q = 55;
	return gm[i][j] * 100 + (int)(q - &gm[0][0]);
}

/* 循环里逐格取 `&m35[i][j]` 并累加指针差：i=0..2, j=0..4 → sum(5i+j)=105。 */
static int
two_d_loop_sum(void)
{
	int s = 0;

	for (int i = 0; i < 3; i++) {
		for (int j = 0; j < 5; j++) {
			int *q = &m35[i][j];
			s += (int)(q - &m35[0][0]);
		}
	}
	return s;
}

int
main(void)
{
	CHK("one_d(3)", one_d(3), 1002);
	CHK("two_d(1,2)", two_d(1, 2), 14);
	CHK("two_d_inner_const(2)", two_d_inner_const(2), 11);
	CHK("two_d_outer_const(3)", two_d_outer_const(3), 13);
	CHK("three_d(1,2,3)", three_d(1, 2, 3), 23);
	CHK("row_stride8(4,1)", row_stride8(4, 1), 10);
	CHK("deref_decay(,3)", deref_decay(m35, 3), 3);
	CHK("row_ptr_reindex(2,3)", row_ptr_reindex(2, 3), 13);
	CHK("global_2d(2,5)", global_2d(2, 5), 5519);
	CHK("two_d_loop_sum", two_d_loop_sum(), 105);

	printf("bad=%d\n", bad);
	return bad == 0 ? 0 : 1;
}
