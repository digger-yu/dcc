void test_local_label(void) {
	int x = 0;
	asm volatile(
		"jmp 6f\n\t"
		"movl $1, %0\n\t"
		"6:\n\t"
		"movl $2, %0\n"
		: "+r"(x)
		:
		: "memory");
}

int main(void) { test_local_label(); return 0; }
