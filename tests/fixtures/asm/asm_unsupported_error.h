/* Test: unsupported inline asm instructions produce errors, not NOP fallback */
#ifndef ASM_UNSUPPORTED_ERROR_H
#define ASM_UNSUPPORTED_ERROR_H

/* 三个助手必须带 `used`：`static inline` 没人引用时按 C 语义**不发射**副本
 * （gcc -O0 实测也不发射），不进 codegen 就没人编码 asm，断言会落空。
 * `used` 强制发射，与 gcc 同样会撞上这些错误。 */

/* Test 1: completely unrecognized instruction */
static inline __attribute__((__used__)) void dcc_asm_unrecognized_instr(void)
{
	asm("foobar %eax");
}

/* Test 2: recognized instruction with unsupported operand pattern */
static inline __attribute__((__used__)) int dcc_asm_unsupported_operand_pattern(void)
{
	int r;
	/* sha1rnds4 with non-xmm destination should error */
	asm("sha1rnds4 $0, %xmm0, %0" : "=r"(r));
	return r;
}

/* Test 3: pshufd with unsupported operand pattern */
static inline __attribute__((__used__)) void dcc_asm_pshufd_bad_operands(void)
{
	asm("pshufd $0, %eax, %xmm0");
}

#endif /* ASM_UNSUPPORTED_ERROR_H */
