/* 常量 switch 折叠后，臂体里**嵌在 if 里**的 break 必须仍然是「跳出 switch」。
 *
 * 内核形状（include/linux/rmap.h `__folio_try_dup_anon_rmap` 的
 * `case PGTABLE_LEVEL_PTE`）：
 *     case 3:
 *             if (cond) { r += 10; break; }
 *             r += 100;
 *             // fall through
 *     case 4: ...
 * 这个 switch 被 `__always_inline` 展开进 `copy_pte_range` 的 `do { … } while`
 * 之后，选择子是常量 ⇒ dcc 折掉 switch，但只摘掉了「臂顶层」与「块尾」的
 * break：if 里那条留在树里，codegen 把它绑到最近的外层循环 —— 实测
 * mm/memory.o 的 copy_p4d_range 里 5 处 `atomic_inc(&folio->_mapcount)`
 * （lock incl）后面直接 jmp 到循环出口的 _raw_spin_unlock，跳过 rmap 记账与
 * __copy_present_ptes，fork 出来的子进程页表没建立。
 *
 * 失败码（按位或）：
 *   1 = 第一次循环的 t 不是 11（break 提前跳出了 do-while，旧版实测 t=0）
 *   2 = cond=0 用例错（未命中 if 时 r 应为 3100）
 */
static inline __attribute__((always_inline)) int dup(int level, int cond)
{
	int r = 0;

	switch (level) {
	case 3:
		if (cond) {
			r += 10;
			break;
		}
		r += 3100;
		break;
	default:
		r = -1;
		break;
	}
	return r;
}

int main(void)
{
	volatile int zero = 0;
	int c = zero == 0;
	int t = 0;
	int bad = 0;

	do {
		t += dup(3, c);
		if (t > 100000)
			break;
		t += 1;
	} while (t < 6);
	if (t != 11)
		bad |= 1;

	if (dup(3, !c) != 3100)
		bad |= 2;

	return bad;
}
