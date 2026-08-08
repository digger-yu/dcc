void f(void) {
	asm goto("jmp %l[missing_label]" : : : : real_label);
real_label:
	;
}
