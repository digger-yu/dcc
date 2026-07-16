void f(void) {
	asm goto("x" : : : : lbl);
lbl:
	;
}
