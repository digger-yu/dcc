/* 小尺寸 struct（<= 8 字节）在**块内**用复合字面量初始化。
 *
 * 复合字面量的值通道是「缓冲地址」：rax 里是对象地址，不是数据。块内声明
 * （`gen_block_item`）与语句表达式（`({ ... })`）两条路径曾只看 `size > 8`
 * 决定是否整块拷贝，于是 8 字节结构体把**地址**存进了变量槽。
 *
 * 内核中的真实爆点：`arch/x86/kernel/apic/apic.c` 的
 * `set_fixmap_nocache(FIX_APIC_BASE, mp_lapic_addr)` 展开为
 * `__set_fixmap(..., (pgprot_t){ ... })`，`pgprot_t` 是 `struct { u64 pgprot; }`。
 * inline 展开把形参实体化成块内的 `__inlN$_flags = (pgprot_t){...}`，地址被
 * 当作 pte 值传给 `pfn_pte()`，APIC fixmap 的 PTE 写成 `0xFFFFFFFF87DFF398`，
 * 首次读 APIC 就 #PF → #GP → #DF（`PANIC: early exception 0x08`）。
 *
 * 本件用运行期取值把三条路径各钉一遍：if 块、while 块、语句表达式。值若被
 * 地址覆盖，`v`/`a`/`b` 与期望不符，退出码非 0。
 */
typedef struct {
	unsigned long v;
} one_t;

typedef struct {
	unsigned int a;
	unsigned int b;
} pair_t;

static int one_ok(one_t x, unsigned long expect)
{
	return x.v == expect;
}

static int pair_ok(pair_t p)
{
	return p.a == 0x11223344u && p.b == 0x55667788u;
}

static unsigned long run_if(int c)
{
	if (c) {
		one_t x = (one_t){ 0x0123456789abcdefUL };
		if (!one_ok(x, 0x0123456789abcdefUL))
			return 1;
		pair_t p = (pair_t){ 0x11223344u, 0x55667788u };
		if (!pair_ok(p))
			return 2;
	}
	return 0;
}

static unsigned long run_while(int c)
{
	unsigned long fails = 0;

	while (c > 0) {
		one_t y = (one_t){ 0xfedcba9876543210UL };
		if (!one_ok(y, 0xfedcba9876543210UL))
			fails++;
		c--;
	}
	return fails;
}

int main(void)
{
	if (run_if(1) != 0)
		return 1;
	if (run_while(3) != 0)
		return 2;

	/* 语句表达式路径：dispatch.rs 的声明分支 */
	unsigned long r = ({ one_t t = (one_t){ 0x55aa55aa55aa55aaUL }; t.v; });
	if (r != 0x55aa55aa55aa55aaUL)
		return 3;

	/* 嵌套块中的结构体 -> 结构体初始化（同形） */
	one_t src = (one_t){ 0x0f0f0f0f0f0f0f0fUL };
	{
		one_t dst = src;
		if (!one_ok(dst, 0x0f0f0f0f0f0f0f0fUL))
			return 4;
	}
	return 0;
}
