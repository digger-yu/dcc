/* GNU as 同行局部标签 N:insn */
void test_label_insn(void) {
	int x = 0;
	asm volatile(
		"1:jmp 2f\n\t"
		"movl $1, %0\n\t"
		"2:\n\t"
		"movl $2, %0\n"
		: "+r"(x)
		:
		: "memory");
}

int main(void) { test_label_insn(); return 0; }
