/* pushfl/popfl/iret/iretl: flag register and interrupt return aliases */

unsigned long test_pushfl_popfl(void) {
	unsigned long flags = 0;
	asm volatile(
		"pushfl\n\t"
		"popq %0\n\t"
		"pushfl\n\t"
		"popfl"
		: "=r"(flags)
		:
		: "cc"
	);
	return flags;
}

void test_iret_iretl(void) {
	/* iret/iretl encode as CF (32-bit interrupt return) */
	asm volatile("iret");
	asm volatile("iretl");
}

int main(void) {
	(void)test_pushfl_popfl();
	/* iret/iretl are not callable in user space; compile-only test */
	return 0;
}
