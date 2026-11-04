/* 内核取分支的开关本身：include/linux/compiler_attributes.h:213 用
 *   #if __has_attribute(__fallthrough__)
 * 选 `__attribute__((__fallthrough__))`，否则退到注释形态的 `do {} while (0)` 一支。
 * GCC 承认两侧下划线的保留标识符写法与裸名等价，dcc 也必须答 1 —— 答 0 会让整个内核
 * 走到注释那一支，而注释形态的 -Wimplicit-fallthrough 识别是另一套机制，实测
 * localmodconfig 全树因此刷出 44 185 条 W0501（gcc 同配置只有个位数）。
 * 判据取 gcc -E 实测：yes_us 存在、no_us 不存在、mismatch 不存在。 */
#if __has_attribute(__fallthrough__)
int yes_us;
#else
int no_us;
#endif

/* 裸名与下划线名必须给出同一个答案（同一属性的两种写法） */
#if __has_attribute(fallthrough) != __has_attribute(__fallthrough__)
int mismatch;
#endif

/* 真用一次宏体里的 fallthrough;（内核 _SIG_SET_OP 的形状），把「属性被认下」钉住 */
#define fallthrough __attribute__((__fallthrough__))

int marked_switch(int x, int *acc)
{
	switch (x) {
	case 3:
		*acc += 1;
		fallthrough;
	case 2:
		*acc += 2;
		fallthrough;
	case 1:
		*acc += 4;
		break;
	default:
		return -1;
	}
	return 0;
}
