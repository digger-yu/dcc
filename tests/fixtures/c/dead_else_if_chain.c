/* 链式 `if / else if / else` 里，**恒假的 `else if` 分支**必须是死码。
 * `fold_branches_block` 只能删「作为块内一项」的 `if`，而第三支是外层 `if` 的 `else_`
 * 操作数 —— 不塌缩就永远删不掉。现场后果（实测 `mm/rmap.o`）：THP 关时
 * `else if (IS_ENABLED(CONFIG_TRANSPARENT_HUGEPAGE))` 那一支调的是
 * `include/linux/pgtable.h:546` 的 `BUILD_BUG()` 占位实现，占位里的调用发进 `.text`
 * ⇒ 链接期 `undefined reference to 'pmdp_clear_flush_young'`。
 * 反向对照同一条里带上：恒真的 `else if (1)` 必须**选中**，不然就成了「一律删掉
 * 第三条」的假修复。 */
int rt;

extern void trap_never_called(void);

/* 1) 恒假的 else if ⇒ 那一支必须整条消失（连 trap 的引用都不留） */
int dead_else_if(void)
{
	int n = 0;

	if (rt) {
		n = 1;
	} else if (0) {
		trap_never_called();
		n = 2;
	} else {
		n = 3;
	}
	return n;
}

/* 2) 恒真的 else if ⇒ 必须选中这一支（反向对照） */
int taken_else_if(void)
{
	int n = 0;

	if (rt) {
		n = 1;
	} else if (1) {
		n = 2;
	} else {
		n = 3;
	}
	return n;
}

/* 3) 连着两跳：`else if (0)` 塌掉之后露出的 `else` 里还有一条恒假 if */
int two_hops(int *out)
{
	int n = 0;

	if (rt) {
		n = 1;
	} else if (0) {
		trap_never_called();
		n = 2;
	} else if (0) {
		trap_never_called();
		n = 4;
	} else {
		if (0)
			trap_never_called();
		n = 5;
	}
	*out = n;
	return n;
}

int main(void)
{
	int got = 0;

	if (dead_else_if() != 3)
		return 1;
	if (taken_else_if() != 2)
		return 2;
	if (two_hops(&got) != 5 || got != 5)
		return 3;
	return 0;
}
