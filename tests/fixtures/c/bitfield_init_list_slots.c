/* 位域初始化列表的 3 个重写槽必须住在位域 RMW 带里。
 *
 * 回归来源（gcc.c-torture execute/bf64-1.c 一族在 §5.42 收紧帧头后失败）：
 * `gen_init_list_at` 的位域分支把地址/容器/新值写死在 -24/-32/-40(%r13)，
 * 那三个格是帧头的 callee-saved 保存区。cs=4 恒存的年代，只有在函数真用到
 * 池寄存器时才会踩坏；帧头改成 `16 + 8*cs + 8*temps` 之后，cs=0 的函数
 * 帧头只剩 32B，三个格直接落进变量区 —— 同一函数里后面的结构体局部变量
 * 与前面的初始化临时量同格，两个结构体的初始化互相覆盖。
 *
 * 这里不打印，直接比对容器原始字：读到错值就返回 1，测试驱动只看退出码。
 */
#include <string.h>

struct tmp {
	long long pad : 12;
	long long field : 52;
};

struct tmp2 {
	long long field : 52;
	long long pad : 12;
};

int main(void)
{
	struct tmp a = {0x123, 0xFFF000FFF000FLL};
	struct tmp2 b = {0xFFF000FFF000FLL, 0x123};
	unsigned long long ra = 0, rb = 0;

	memcpy(&ra, &a, sizeof ra);
	memcpy(&rb, &b, sizeof rb);

	/* 容器原始字：pad 在低位 / field 在低位的两种排列。 */
	if (ra != 0xfff000fff000f123ULL)
		return 1;
	if (rb != 0x123fff000fff000fULL)
		return 2;

	/* 读路径：pad 无符号 12 位，field 有符号 52 位（位 51 已置位）。 */
	if ((unsigned long long)a.pad != 0x123ULL)
		return 3;
	if ((unsigned long long)b.pad != 0x123ULL)
		return 4;
	if ((unsigned long long)a.field != 0xffffff000fff000fULL)
		return 5;
	if ((unsigned long long)b.field != 0xffffff000fff000fULL)
		return 6;
	return 0;
}
