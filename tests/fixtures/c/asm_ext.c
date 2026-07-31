/* mini-asm 扩展指令：位操作 / 扫描 / popcnt / bswap / movnt / lock / rep / 伪指令 */
int g_data[4] = {0xF0, 0x0F, 0xFF, 0x11};
int g_movnt_cell;

__asm__(
	".pushsection .test_asm,\"a\"\n\t"
	".balign 4\n\t"
	".long 0x12345678\n\t"
	".quad 0xdeadbeefcafebabe\n\t"
	".popsection\n"
);

unsigned long test_bit_ops(unsigned long x) {
	asm volatile(
		"btsq $5, %0\n\t"
		"btrq $3, %0\n\t"
		"btcq $7, %0\n\t"
		"btq $1, %0\n\t"
		"btsl $4, %0\n\t"
		"btrl $2, %0\n\t"
		"btcl $6, %0"
		: "+r"(x)
		:
		: "cc");
	return x;
}

unsigned long test_scan_bswap(unsigned long v) {
	unsigned long idx = 0;
	asm volatile(
		"bsfq %1, %0\n\t"
		"bsrq %1, %0\n\t"
		"popcntq %1, %0\n\t"
		"bswapq %1"
		: "=&r"(idx), "+r"(v)
		:
		: "cc");
	return idx ^ v;
}

int test_sbb_movnt(void) {
	int x = 10, y = 3;
	asm volatile(
		"sbbl %1, %0\n\t"
		"movntil %%eax, %2"
		: "+r"(x)
		: "r"(y), "m"(g_movnt_cell), "a"(0x42)
		: "memory");
	return x;
}

int test_lock_rep(int *dst, const char *src, int n) {
	int v = 0;
	asm volatile(
		"lock incl %0\n\t"
		"rep movsb"
		: "+m"(v), "+D"(dst), "+S"(src), "+c"(n)
		:
		: "memory");
	return v;
}

void test_virt(void) {
	asm volatile("vmcall");
	asm volatile("vmmcall");
}

int main(void) {
	unsigned long b = test_bit_ops(0);
	unsigned long s = test_scan_bswap(0x8000000000000001UL);
	int r = test_sbb_movnt();
	test_lock_rep((int *)g_data, (const char *)g_data, 4);
	test_virt();
	return (int)(b + s + r + g_data[0]) & 1;
}
