/* mini-asm 内核常见扩展：rol/prefetch/movss/xorps/setae 等 */
int g_cell;

unsigned long rol_test(unsigned long x) {
	asm volatile("rolq $4, %0" : "+r"(x));
	return x;
}

void prefetch_test(const void *p) {
	asm volatile("prefetcht0 (%0)" :: "r"(p) : "memory");
	asm volatile("prefetchnta (%0)" :: "r"(p) : "memory");
}

float sse_scalar(float a, float b) {
	float r;
	asm volatile(
		"movss %1, %%xmm0\n\t"
		"addss %2, %%xmm0\n\t"
		"movss %%xmm0, %0"
		: "=m"(r)
		: "m"(a), "m"(b));
	return r;
}

int main(void) {
	unsigned long v = rol_test(0xF0UL);
	prefetch_test(&g_cell);
	float f = sse_scalar(1.0f, 2.0f);
	return (int)((v & 0xFUL) + (f == 3.0f ? 0 : 1));
}
