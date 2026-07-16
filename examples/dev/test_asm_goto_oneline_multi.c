void f(void) {
	asm goto("1:" "jmp\n" ".sec\n" : : "i" (0) : : l_yes);
l_yes:
	;
}
