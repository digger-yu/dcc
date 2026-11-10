/* tests/fixtures/c/global_member_array_addr_init.c
 *
 * **数组成员表达式**作为指针字段的静态初始化式（内核 `LSM_HOOK_INIT` 形状）：
 *
 *   struct security_hook_list hooks[] = {
 *       { .scalls = static_calls_table.capable, .hook = { .capable = fn } },
 *   };
 *
 * `static_calls_table.capable` 的类型是结构体成员数组（`struct lsm_static_call[2]`）。
 * 按 C11 6.3.2.1p3 它退化为**成员地址**，静态初始化式必须发一条
 * R_X86_64_64 `符号 + 字段偏移` 重定位。旧实现里裸 Member 表达式没有退化臂：
 *   1. 求值落进兜底 → 不发任何重定位（指针字段恒 0）；
 *   2. 兜底按**表达式类型**（数组长度）返回零字节，`gen_global_init_list` 的
 *      字段写回把它当柔性数组扩容，整个元素被撑到数组长度 ——
 *      数组 stride 从 sizeof(struct) 变成 sizeof(成员数组)。
 * 内核现场（报告 §5.33）：`capability_hooks[]` 元素 stride 64（= [2] 数组），
 * 消费侧 `security_add_hooks` 按 24 走，`hl->scalls == NULL`，
 * boot 在 security_add_hooks+0xc5 空指针崩（CR2=0x10）。
 *
 * 失败码（成功返回 0；`assert_all_levels_match_gcc` 要求两侧退出码相等且为 0）：
 *   1  第一个元素的 .scalls 不是成员数组的地址
 *   2  第二个元素的 .scalls（偏移不同的成员）地址错
 *   3  hook 函数指针值错
 *   4  相邻元素间隔 != sizeof(struct)（stride 被成员数组长度撑大）
 *   5  整个数组大小 != N * sizeof(struct)
 *   6  extern 全局（定义在另一个 TU 的同一形状）成员地址
 *   7  成员数组元素地址 `&table.capable[2]` 回归（原本就支持的路径）
 */

struct slot {
	int pad0;
	int pad1;
	int pad2;
	int pad3;
};

struct calls_table {
	struct slot capable[3];
	struct slot settime[3];
};

struct hook_list {
	struct slot *scalls;
	int (*fn)(int);
	const struct slot *lsmid;
};

/* 定义为普通全局：初始化式只关心「符号 + 偏移」这一常量形态。 */
struct calls_table static_calls_table;

static int cap_capable(int x)
{
	return x + 1;
}

static int cap_settime(int x)
{
	return x + 2;
}

struct hook_list hooks[] = {
	{ .scalls = static_calls_table.capable, .fn = cap_capable },
	{ .scalls = static_calls_table.settime, .fn = cap_settime },
};

/* extern 版本：地址常量来自未定义符号（内核里 static_calls_table 定义在
 * security/security.c，各 LSM 的 TU 只看到 extern 声明）。 */
extern struct calls_table extern_calls_table;

struct hook_list ext_hooks[] = {
	{ .scalls = extern_calls_table.settime, .fn = cap_settime },
};

/* 单文件夹具里补上定义（否则运行期链接不过）；折叠路径与内核里
 * 「仅见 extern 声明」完全一致：常量求值只看名字与字段偏移。 */
struct calls_table extern_calls_table;

struct slot *third = &static_calls_table.capable[2];

int main(void)
{
	int fails = 0;

	if (hooks[0].scalls != static_calls_table.capable)
		fails |= 1;
	if (hooks[1].scalls != static_calls_table.settime)
		fails |= 2;
	if (hooks[0].fn(10) != 11 || hooks[1].fn(10) != 12)
		fails |= 4;
	if ((char *)&hooks[1] - (char *)&hooks[0] != (long)sizeof(struct hook_list))
		fails |= 8;
	if (sizeof(hooks) != 2 * sizeof(struct hook_list))
		fails |= 16;
	if (ext_hooks[0].scalls != extern_calls_table.settime)
		fails |= 32;
	if (third != &static_calls_table.capable[2])
		fails |= 64;
	return fails;
}
