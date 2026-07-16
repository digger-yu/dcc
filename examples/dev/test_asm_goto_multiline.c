void f(void) {
	asm goto("1:"
		"jmp %l[l_yes]\n\t"
		".pushsection x\n\t"
		: : "i" (0) : : l_yes);
l_yes:
	;
}
