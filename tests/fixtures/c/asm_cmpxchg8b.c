/* cmpxchg8b: 32-bit atomic compare-and-exchange (64-bit operand) */

static unsigned long long g_val64 = 0x0000000100000001ULL;

unsigned long long test_cmpxchg8b(void) {
	/* EDX:EAX = expected (0x00000001:00000001)
	 * ECX:EBX = desired  (0x00000002:00000002)
	 * cmpxchg8b swaps if memory == EDX:EAX
	 */
	asm volatile(
		"mov $1, %%eax\n\t"
		"mov $1, %%edx\n\t"
		"mov $2, %%ebx\n\t"
		"mov $2, %%ecx\n\t"
		"lock cmpxchg8b %0\n\t"
		: "+m"(g_val64)
		:
		: "eax", "ebx", "ecx", "edx", "cc", "memory"
	);
	return g_val64;
}

int main(void) {
	unsigned long long r = test_cmpxchg8b();
	/* After cmpxchg8b: g_val64 should be 0x0000000200000002 */
	return (r == 0x0000000200000002ULL) ? 0 : 1;
}
