/* Test: unsupported inline asm instructions produce errors, not NOP fallback */
#ifndef ASM_UNSUPPORTED_ERROR_H
#define ASM_UNSUPPORTED_ERROR_H

/* Test 1: completely unrecognized instruction */
static inline void dcc_asm_unrecognized_instr(void)
{
	asm("foobar %eax");
}

/* Test 2: recognized instruction with unsupported operand pattern */
static inline int dcc_asm_unsupported_operand_pattern(void)
{
	int r;
	/* sha1rnds4 with non-xmm destination should error */
	asm("sha1rnds4 $0, %xmm0, %0" : "=r"(r));
	return r;
}

/* Test 3: pshufd with unsupported operand pattern */
static inline void dcc_asm_pshufd_bad_operands(void)
{
	asm("pshufd $0, %eax, %xmm0");
}

#endif /* ASM_UNSUPPORTED_ERROR_H */
