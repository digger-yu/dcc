#ifndef ASM_WARN_LOC_H
#define ASM_WARN_LOC_H
static inline void *dcc_asm_leaq_rip(void *p)
{
	void *r;
	asm("leaq %c1(%%rip), %0" : "=r"(r) : "i"(p));
	return r;
}
#endif
