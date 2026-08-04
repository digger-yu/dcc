/* mini-asm 内核扩展指令支持：orb/orw/cmova/clwb/invpcid/rdrand/rdseed/
 * fwait/fxsaveq/fxrstorq/xgetbv/xsetbv/monitor/mwait/monitorx/mwaitx/
 * clgi/stgi/vmsave/sldt/repe 前缀修复 */

static int g_cell;

/* orb / orw */
unsigned char orb_test(unsigned char a, unsigned char b) {
	asm volatile("orb %1, %0" : "+r"(a) : "r"(b));
	return a;
}
unsigned short orw_test(unsigned short a, unsigned short b) {
	asm volatile("orw %1, %0" : "+r"(a) : "r"(b));
	return a;
}

/* cmova */
unsigned long cmova_test(unsigned long a, unsigned long b) {
	unsigned long r = a;
	asm volatile("cmp %1, %0; cmova %2, %0" : "+r"(r) : "r"(a), "r"(b));
	return r;
}

/* clwb */
void clwb_test(void *p) {
	asm volatile("clwb (%0)" :: "r"(p) : "memory");
}

/* rdrand */
int rdrand_test(void) {
	unsigned int val;
	asm volatile("rdrand %0" : "=r"(val));
	return (int)val;
}

/* rdseed */
int rdseed_test(void) {
	unsigned int val;
	asm volatile("rdseed %0" : "=r"(val));
	return (int)val;
}

/* fwait */
void fwait_test(void) {
	asm volatile("fwait");
}

/* fxsaveq / fxrstorq */
char fxsave_area[512] __attribute__((aligned(16)));
void fxsaveq_test(void) {
	asm volatile("fxsaveq (%0)" :: "r"(fxsave_area) : "memory");
}
void fxrstorq_test(void) {
	asm volatile("fxrstorq (%0)" :: "r"(fxsave_area) : "memory");
}

/* xgetbv / xsetbv */
void xgetbv_test(void) {
	unsigned int eax, edx;
	asm volatile("xgetbv" : "=a"(eax), "=d"(edx) : "c"(0));
}
void xsetbv_test(void) {
	asm volatile("xsetbv" :: "a"(0), "d"(0), "c"(0));
}

/* monitor / mwait */
void monitor_test(void) {
	asm volatile("monitor" :: "a"(&g_cell), "c"(0), "d"(0));
}
void mwait_test(void) {
	asm volatile("mwait" :: "a"(0), "c"(0));
}

/* monitorx / mwaitx */
void monitorx_test(void) {
	asm volatile("monitorx" :: "a"(&g_cell), "c"(0), "d"(0));
}
void mwaitx_test(void) {
	asm volatile("mwaitx" :: "a"(0), "c"(0), "b"(0));
}

/* clgi / stgi */
void clgi_test(void) {
	asm volatile("clgi");
}
void stgi_test(void) {
	asm volatile("stgi");
}

/* vmsave */
void vmsave_test(void) {
	asm volatile("vmsave (%0)" :: "r"(&g_cell) : "memory");
}

/* sldt */
unsigned short sldt_test(void) {
	unsigned short val;
	asm volatile("sldt %0" : "=m"(val));
	return val;
}

/* invpcid */
void invpcid_test(void) {
	unsigned long desc[2] = {0, 0};
	asm volatile("invpcid %0, %1" :: "r"(0UL), "m"(desc[0]) : "memory");
}

/* repe 前缀 (repe scasb 等) */
void repe_test(void) {
	asm volatile("cld\nrepe\nscasb\n");
}

int main(void) {
	/* 编译通过即可，运行时行为依赖硬件 */
	(void)orb_test(1, 2);
	(void)orw_test(1, 2);
	(void)cmova_test(1, 2);
	clwb_test(&g_cell);
	(void)rdrand_test();
	(void)rdseed_test();
	fwait_test();
	monitor_test();
	mwait_test();
	monitorx_test();
	mwaitx_test();
	(void)sldt_test();
	repe_test();
	return 0;
}