/* `-Wimplicit-fallthrough` 与 gcc 逐条对齐的四种形状（期望值全部取 gcc 实测）：
 *  1) `case A: case B:`（共享体的空 case）⇒ **不报**：没有语句被跨过
 *  2) `case 1: int y = 2;`（只有一句声明）⇒ 报
 *  3) `case 1: { int y = 2; (void)y; }` ⇒ 报
 *  4) 真贯穿 ⇒ 报
 *  5) 以 `do { … __builtin_unreachable(); } while (0);` 收尾（内核 `BUG()` 的形状）⇒ 不报
 * 判据是「条数 + 每条落在哪个函数名行」，位置从诊断的 `--> 文件:行:列` 里取。 */

int empty_shared_case(int x)
{
	int r = 0;

	switch (x) {
	case 1:
	case 2:
		r = 1;
		break;
	default:
		break;
	}
	return r;
}

int decl_bare_case(int x)
{
	int r = 0;

	switch (x) {
	case 1:
		int y = 2;
		(void)y;
	case 2:
		r = 1;
		break;
	default:
		break;
	}
	return r;
}

int real_fallthrough(int x)
{
	int r = 0;

	switch (x) {
	case 1:
		r = 2;
	case 2:
		r = 1;
		break;
	default:
		break;
	}
	return r;
}

/* 内核 `BUG()` 的等价形状：`do { trap; unreachable; } while (0)` 必须算终止 */
void bug_like_case(int x)
{
	switch (x) {
	case 1:
		__builtin_trap();
		break;
	case 2:
		do {
			__builtin_unreachable();
		} while (0);
	case 3:
		break;
	default:
		break;
	}
}
