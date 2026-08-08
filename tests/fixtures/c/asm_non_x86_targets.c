/* Test fixture: inline asm on non-x86_64 targets */
/* This file tests that inline asm constructs are accepted on all targets */
/* On non-x86_64 targets, -S mode preserves asm text, -c mode emits warnings */

/* Global asm - should be preserved in -S output for all targets */
__asm__(".globl __test_global_asm_marker\n");

int test_inline_asm_expr(int x) {
    int result;
    /* Inline asm expression - result is 0 on non-x86_64 */
    result = __asm__("nop" : "=r"(x) : "0"(x));
    return x + 1;
}

void test_inline_asm_stmt(int a, int b) {
    int sum = a + b;
    /* Inline asm statement */
    __asm__ volatile("nop" : "+r"(sum) : : "memory");
    (void)sum;
}

/* asm goto - should be accepted on all targets */
int test_asm_goto(int x) {
    int result = 0;
    __asm__ goto("nop" : : "r"(x) : "memory" : done);
    result = 1;
done:
    return result;
}

/* Named operand asm */
int test_named_operand(int a, int b) {
    int result;
    __asm__("nop" : [out]"=r"(result) : [in1]"r"(a), [in2]"r"(b));
    return result;
}
