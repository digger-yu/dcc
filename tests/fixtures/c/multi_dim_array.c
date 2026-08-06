/* 多维数组维度语义: C11 6.7.6.2p4 从内向外结合, 最内层元素与最近的
 * [N] 结合. 修复前 3 处维度解析从左到右包裹, char buf[2][8] 被解析为
 * 8 个 char[2], 导致 sizeof buf[0]==2 (应为 8), 元素 stride 错,
 * words[1] 按 1 字节步进读标量, 运行时越界/崩溃.
 */
#include <stdio.h>

char buf[2][8];
int g[2][3];
int cube[2][3][4];
char words[][6] = {"hello", "world"};

_Static_assert(sizeof buf == 16, "buf total");
_Static_assert(sizeof buf[0] == 8, "buf row");
_Static_assert(sizeof g[0] == 12, "g row");
_Static_assert(sizeof g[0][0] == 4, "g elem");
_Static_assert(sizeof cube[0][0] == 16, "cube plane");
_Static_assert(sizeof cube[0] == 48, "cube slab");
_Static_assert(sizeof words[0] == 6, "words row");
_Static_assert(sizeof(int[2][3]) == 24, "anon 2d");
_Static_assert(sizeof(int[2][3][4]) == 96, "anon 3d");

/* 参数多维数组: int a[][3] 退化为 int (*a)[3], 只剥最外层 (C11 6.7.6.3p8) */
static int sum_rows(int a[][3], int n)
{
	int s = 0;
	for (int i = 0; i < n; i++)
		for (int j = 0; j < 3; j++)
			s += a[i][j];
	return s;
}

static int sum_flat(int a[][2][3], int n)
{
	int s = 0;
	for (int i = 0; i < n; i++)
		for (int j = 0; j < 2; j++)
			for (int k = 0; k < 3; k++)
				s += a[i][j][k];
	return s;
}

int main(void)
{
	int m[2][3] = {{1, 2, 3}, {4, 5, 6}};
	int cube2[2][2][3] = {{{1, 2, 3}, {4, 5, 6}}, {{7, 8, 9}, {10, 11, 12}}};
	if (sizeof m[0] != 12)
		return 1;
	if (m[1][2] != 6)
		return 2;
	if (cube2[1][1][2] != 12)
		return 3;
	if (sum_rows(m, 2) != 21)
		return 4;
	if (sum_flat(cube2, 2) != 78)
		return 5;
	/* words[1] 类型为 char[6], 作为值使用退化为 &words[1] 地址
	 * (C11 6.3.2.1p3). 修复前按 stride=1 标量加载, 指针指向错误. */
	char *p = words[1];
	if (p[0] != 'w' || p[4] != 'd')
		return 6;
	if (words[1][1] != 'o')
		return 7;
	printf("%s %s\n", words[0], words[1]);
	return 0;
}
