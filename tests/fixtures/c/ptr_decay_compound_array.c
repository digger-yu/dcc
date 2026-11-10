/* §5.46: 指针目标的数组型复合字面量是**地址**（C11 6.3.2.1p3 退化），
 * 不是数组内容。内核形状（drivers/ata/libata-eh.c）：
 *
 *   #define CMDS(cmds...) (const u8 []){ cmds, 0 }
 *   static const struct ata_eh_cmd_timeout_ent table[] = {
 *       { .commands = CMDS(ATA_CMD_ID_ATA, ATA_CMD_ID_ATAPI), ... },
 *   };
 *
 * 旧行为把 `{0xec, 0xa1}` 这两字节当指针值 0xa1ec 写进 `.data` 且**不发
 * 重定位**，kbuild23 首次 ata_lookup_timeout_table() 解引用即 #PF。
 * 失败码：1/2/3/4=查表命中、5=顶层指针、6/7=指针数组元素。
 *
 * 不包含两种 GCC 本身就拒绝的写法（不是退化）：
 *   - `struct S *p = (struct S){…}`（左值不退化，必须写 `&`）
 *   - 函数内 `static T *p = (T []){…}`（块作用域复合字面量是自动
 *     存储期，地址不是地址常量） */
#define CMDS(cmds...) (const unsigned char []){ cmds, 0 }

struct ent {
	const unsigned char *commands;
	const unsigned int *timeouts;
};

static const unsigned int t_ident[] = { 1000, 2000 };
static const unsigned int t_log[] = { 3000 };

static const struct ent tab[3] = {
	{ .commands = CMDS(0xec, 0xa1), .timeouts = t_ident },
	{ .commands = CMDS(0x2f), .timeouts = t_log },
	{ .commands = 0, .timeouts = 0 },
};

static const unsigned char *const head = (const unsigned char []){ 0x11, 0x22, 0 };

static const unsigned char *const rows[2] = {
	(const unsigned char []){ 'a', 'b', 0 },
	CMDS('z'),
};

static int lookup(unsigned char c)
{
	int i;

	for (i = 0; i < 3; i++) {
		const unsigned char *cur;

		if (!tab[i].commands)
			continue;
		for (cur = tab[i].commands; *cur; cur++)
			if (*cur == c)
				return i;
	}
	return -1;
}

int main(void)
{
	if (lookup(0xec) != 0)
		return 1;
	if (lookup(0xa1) != 0)
		return 2;
	if (lookup(0x2f) != 1)
		return 3;
	if (lookup(0x99) != -1)
		return 4;
	if (head[0] != 0x11 || head[1] != 0x22 || head[2] != 0)
		return 5;
	if (rows[0][0] != 'a' || rows[0][1] != 'b' || rows[0][2] != 0)
		return 6;
	if (rows[1][0] != 'z' || rows[1][1] != 0)
		return 7;
	return 0;
}
