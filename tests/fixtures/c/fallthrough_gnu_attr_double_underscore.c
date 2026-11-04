/* `-Wimplicit-fallthrough` 的标记形态。内核的伪关键字 `fallthrough;` 经
 * include/linux/compiler_attributes.h:214 展开成 `__attribute__((__fallthrough__))`
 * —— **双侧下划线**，与裸名等价（GCC 的保留标识符规则）。只比裸名的话内核每一处
 * 标记都不认，实测 localmodconfig 全树刷出 44 185 条 W0501。
 * 判据取 gcc -O2 -Wall -Wimplicit-fallthrough 实测：下面四个「已标记」的函数都不该报，
 * 最后一个「没标记」的必须照报（反向对照）。 */
int f_attr_us(int x)
{
	switch (x) {
	case 1:
		x++;
		__attribute__((__fallthrough__));
	case 2:
		return x;
	default:
		return 0;
	}
}

int f_attr_plain(int x)
{
	switch (x) {
	case 1:
		x++;
		__attribute__((fallthrough));
	case 2:
		return x;
	default:
		return 0;
	}
}

int f_cpp11(int x)
{
	switch (x) {
	case 1:
		x++;
		[[fallthrough]];
	case 2:
		return x;
	default:
		return 0;
	}
}

/* 宏展开后的实际写法：`fallthrough;` 是一个标识符 + ';'，由头文件定义成属性 */
#define fallthrough __attribute__((__fallthrough__))

int f_kernel_macro(int x)
{
	switch (x) {
	case 1:
		x++;
		fallthrough;
	case 2:
		x++;
		fallthrough;
	case 3:
		return x;
	default:
		return 0;
	}
}

/* 反向对照：真的没有任何标记的贯穿，必须仍然报 */
int f_real_bug(int x)
{
	switch (x) {
	case 1:
		x++;
	case 2:
		return x;
	default:
		return 0;
	}
}
