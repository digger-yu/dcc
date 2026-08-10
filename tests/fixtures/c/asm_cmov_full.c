/* cmov full condition code coverage + width suffix aliases */

unsigned long test_cmov_new_conditions(unsigned long a, unsigned long b) {
	unsigned long r = a;
	/* cmovb/cmovae/cmovbe/cmovs/cmovo/cmovp + width suffix forms */
	asm volatile(
		"cmp %1, %0\n\t"
		"cmovb %1, %0\n\t"
		"cmovae %1, %0\n\t"
		"cmovbe %1, %0\n\t"
		"cmovs %1, %0\n\t"
		"cmovo %1, %0\n\t"
		"cmovp %1, %0\n\t"
		"cmovns %1, %0\n\t"
		"cmovno %1, %0\n\t"
		"cmovnp %1, %0"
		: "+r"(r)
		: "r"(b)
		: "cc"
	);
	return r;
}

unsigned long test_cmov_width_suffix(unsigned long a, unsigned long b) {
	unsigned long r = a;
	/* cmov with w/l/q width suffix aliases */
	asm volatile(
		"cmp %1, %0\n\t"
		"cmovzl %1, %0\n\t"
		"cmovzq %1, %0\n\t"
		"cmovael %1, %0\n\t"
		"cmovbl %1, %0\n\t"
		"cmovsl %1, %0\n\t"
		"cmovol %1, %0\n\t"
		"cmovpl %1, %0"
		: "+r"(r)
		: "r"(b)
		: "cc"
	);
	return r;
}

int main(void) {
	(void)test_cmov_new_conditions(1, 2);
	(void)test_cmov_width_suffix(1, 2);
	return 0;
}
