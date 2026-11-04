/* 块级 `const` 声明不得在出块后继续遮蔽同名变量。
 * 现场（实测 `mm/compaction.c:643`）：
 *   unsigned int order;                       ← 函数作用域，可变
 *   if (PageCompound(page)) {
 *           const unsigned int order = compound_order(page);   ← 块作用域
 *           ...
 *   }
 *   order = buddy_order(page);                ← 该写外层，dcc 却报 read-only 并拒编
 * `var_quals` 以前只按名字建表、只在换函数时清，所以出块后那条 const 还挂着。
 * 主程序 rc=0 表示两处赋值都落到了正确的对象上；反向对照在下面的测试里单独编
 * （真 const 赋值必须仍然是错误），不混在这份能编过的夹具里。 */
int outer_assign(void)
{
	int order = 0;

	if (order) {
		const unsigned int order = 3;
		if (order != 3) {
			return 1;
		}
	}
	order = 5;
	if (order != 5) {
		return 2;
	}
	return 0;
}

/* 兄弟块：同一深度上的两次同名声明也必须各归各的 */
int sibling_blocks(void)
{
	int total = 0;

	{
		int v = 7;
		total += v;
	}
	{
		int v = 11;
		total += v;
	}
	return total == 18 ? 0 : 3;
}

/* 嵌套块里读外层可变对象，写回也要落在外层 */
int nested_write_outer(void)
{
	int acc = 1;

	for (int i = 0; i < 2; i++) {
		int step = i + 1;
		acc += step * 3;
	}
	/* 1 + (1*3) + (2*3) = 10 */
	return acc == 10 ? 0 : 4;
}

int main(void)
{
	int rc = outer_assign();

	if (rc == 0) {
		rc = sibling_blocks();
	}
	if (rc == 0) {
		rc = nested_write_outer();
	}
	return rc;
}
