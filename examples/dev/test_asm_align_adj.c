#define __stringify_1(x...) #x
#define __stringify(x...) __stringify_1(x)
#define __ASM_FORM(x, ...) " " __stringify(x,##__VA_ARGS__) " "
#define __ASM_SEL(a,b) __ASM_FORM(b)
#define _ASM_ALIGN __ASM_SEL(.balign 4, .balign 8)
const char *s = _ASM_ALIGN "\n\t";
