#ifndef ASM_WARN_LOC_H
#define ASM_WARN_LOC_H
static inline void *dcc_asm_unsupported_lea(void *p)
{
	void *r;
	asm("leaq $42, %0" : "=r"(r));
	return r;
}
#endif
