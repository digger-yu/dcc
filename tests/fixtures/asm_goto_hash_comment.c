#define asm_volatile_goto(x...) asm goto(x)

int main(void) {
	asm_volatile_goto("1:"
		"jmp %l[l_yes] # objtool NOPs this \n\t"
		: : "i" (0) : : l_yes);
l_yes:
	return 0;
}
