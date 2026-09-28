#define __stringify_1(x...) #x
#define __stringify(x...) __stringify_1(x)
#define __ASM_FORM(x, ...) " " __stringify(x,##__VA_ARGS__) " "
#define __ASM_SEL(a,b) __ASM_FORM(b)
#define _ASM_ALIGN __ASM_SEL(.balign 4, .balign 8)
#define _ASM_PTR __ASM_SEL(.long, .quad)
#define JUMP_TABLE_ENTRY \
	".pushsection __jump_table,  \"aw\" \n\t" \
	_ASM_ALIGN "\n\t" \
	".long 1b - . \n\t" \
	".long %l[l_yes] - . \n\t" \
	_ASM_PTR "%c0 + %c1 - .\n\t" \
	".popsection \n\t"
#define asm_volatile_goto(x...) asm goto(x)
void f(void) {
	asm_volatile_goto("1:"
		"jmp %l[l_yes]\n\t"
		JUMP_TABLE_ENTRY
		: : "i" (0) : : l_yes);
l_yes: ;
}
