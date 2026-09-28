#define asm_volatile_goto(x...) asm goto(x)
void f(void) {
	asm_volatile_goto("1:"
		"jmp\n"
		: : "i" (0) : : lbl);
lbl: ;
}
