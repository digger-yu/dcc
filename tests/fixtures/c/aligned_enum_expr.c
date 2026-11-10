/* tests/fixtures/c/aligned_enum_expr.c
 *
 * §5.23：`__attribute__((aligned(<常量表达式>)))` 里出现**枚举常量**时，
 * 对齐值必须真的算出来。现场是内核 `kernel/workqueue.c` 的
 *
 *     struct pool_workqueue { ... } __aligned(1 << WORK_STRUCT_PWQ_SHIFT);
 *
 * `WORK_STRUCT_PWQ_SHIFT` 是 enum 常量（= 4 + 4），预处理后原样保留。
 * dcc 的对齐表达式 mini-parser 只认数字 / sizeof / __alignof__ / 括号，
 * 枚举标识符落到 "unknown token"，随后 `(Some(l), None) => l` 又把
 * `1 << <unknown>` 折成 `1` ⇒ struct 只剩自然对齐 4：
 *
 *   - `__alignof__(struct pool_workqueue)` = 4（gcc: 256）
 *   - `sizeof(struct pool_workqueue)` 不按 256 取整
 *   - `KMEM_CACHE(pool_workqueue)` 传 `.align = __alignof__(...)` = 8
 *     （`sizeof` 8 → 结构体 256 字节，但对象只按 8 字节对齐）
 *   - `init_pwq()` 的 `BUG_ON((unsigned long)pwq & ~WORK_STRUCT_PWQ_MASK)`
 *     成立 ⇒ `ud2` ⇒ #UD → #GP → #DF → triple fault（报告 §5.23）
 *
 * 失败码（成功返回 0；`assert_all_levels_match_gcc` 要求两侧退出码相等且为 0）：
 *   1  __alignof__ 走枚举左移表达式（内核 pkq 形状）
 *   2  sizeof 必须按提升后的对齐取整
 *   3  `.align = __alignof__(...)` 指定初始化器交付的值
 *   4  静态对象的实际地址必须按 256 对齐（运行期检查）
 *   5  __aligned(枚举常量本身) 仍按字面值 8
 *   6  __aligned(枚举加法) = 8
 *   7  __aligned(1 << 8) 字面量回归（不该被改坏）
 *   8  __aligned(4 * 256) 乘法链
 *   9  __alignof__ 参与文件作用域静态初始化（常量折叠路径）
 *   10 内核 BUG_ON 形状：池里每个 pwq 都过 `(ulong)pwq & ~mask` 检查
 */

#define __aligned(x) __attribute__((aligned(x)))

typedef unsigned long ulong;

/* 与 include/linux/workqueue.h 的 enum work_bits 同形（DEBUG_OBJECTS_WORK 关）。 */
enum work_bits {
	WORK_STRUCT_PENDING_BIT = 0,
	WORK_STRUCT_INACTIVE_BIT,
	WORK_STRUCT_PWQ_BIT,
	WORK_STRUCT_LINKED_BIT,
	WORK_STRUCT_FLAG_BITS,
	WORK_STRUCT_COLOR_SHIFT = WORK_STRUCT_FLAG_BITS,
	WORK_STRUCT_COLOR_BITS = 4,
	WORK_STRUCT_PWQ_SHIFT = WORK_STRUCT_COLOR_SHIFT + WORK_STRUCT_COLOR_BITS,
};
enum { SHIFT8 = 8 };

/* 内核 pool_workqueue 的骨架：8 字节指针 + 若干 int，自然对齐 8。 */
struct pwq {
	ulong pool;
	ulong wq;
	int work_color;
	int refcnt;
	int nr_in_flight[4];
} __aligned(1 << WORK_STRUCT_PWQ_SHIFT);

struct direct {
	int x;
} __aligned(SHIFT8);
struct summed {
	int x;
} __aligned(WORK_STRUCT_COLOR_SHIFT + WORK_STRUCT_COLOR_BITS);
struct shifted {
	int x;
} __aligned(1 << 8);
struct multiplied {
	int x;
} __aligned(4 * 256);

#define WORK_STRUCT_PWQ_MASK (~((1ul << WORK_STRUCT_PWQ_SHIFT) - 1))

/* 缓存创建实参形状：`.align = __alignof__(struct ...)`。 */
struct kmem_cache_args {
	unsigned int align;
	unsigned int useroffset;
	unsigned int usersize;
	unsigned int freeptr_offset;
};

static unsigned long seen[4];

__attribute__((noinline)) static void take_args(struct kmem_cache_args *a)
{
	seen[0] = a->align;
	seen[1] = sizeof(struct pwq);
}

static struct pwq pool[3];

/* `__alignof__` / `sizeof` 参与文件作用域静态初始化。 */
static const ulong folded[] = {
	__alignof__(struct pwq),
	sizeof(struct pwq),
	__alignof__(struct direct),
	__alignof__(struct multiplied),
};

/* init_pwq() 的 BUG_ON 形状：不满足就直接失败，而不是 ud2。 */
__attribute__((noinline)) static int init_pwq_shape(struct pwq *pwq)
{
	if ((ulong)pwq & ~WORK_STRUCT_PWQ_MASK)
		return 0;
	return 1;
}

int main(void)
{
	struct kmem_cache_args a = { .align = __alignof__(struct pwq) };
	int i;

	take_args(&a);
	if (seen[0] != 256)
		return 3;
	if (seen[1] != 256)
		return 2;

	if (__alignof__(struct pwq) != 256)
		return 1;
	if (sizeof(struct pwq) % 256 != 0)
		return 2;

	if (((ulong)&pool[0]) % 256 != 0 || ((ulong)&pool[1]) % 256 != 0
	    || ((ulong)&pool[2]) % 256 != 0)
		return 4;

	if (__alignof__(struct direct) != 8)
		return 5;
	if (__alignof__(struct summed) != 8)
		return 6;
	if (__alignof__(struct shifted) != 256)
		return 7;
	if (__alignof__(struct multiplied) != 1024)
		return 8;

	if (folded[0] != 256 || folded[1] != 256 || folded[2] != 8
	    || folded[3] != 1024)
		return 9;

	for (i = 0; i < 3; i++) {
		if (!init_pwq_shape(&pool[i]))
			return 10;
	}
	return 0;
}
