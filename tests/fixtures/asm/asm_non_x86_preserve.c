/* Non-x86 inline asm preservation test fixture */
/* Tests that -S mode preserves asm text and -c mode encodes instructions */

/* Global asm: should be preserved in -S output for all targets */
__asm__(".globl __dcc_non_x86_asm_marker\n");
__asm__("__dcc_non_x86_asm_marker:\n");

/* Inline asm expression with output/input constraints */
int test_asm_expr(int x) {
    int result;
    result = __asm__("nop" : "=r"(x) : "0"(x));
    return x + result;
}

/* Inline asm statement with volatile and memory clobber */
void test_asm_stmt(int *p) {
    __asm__ volatile("nop" : "+r"(p) : : "memory");
}

/* asm goto: encoded by mini-assembler */
int test_asm_goto(int x) {
    int result = 0;
    __asm__ goto("nop" : : "r"(x) : "memory" : done);
    result = 1;
done:
    return result;
}

/* Named operand asm */
int test_named(int a, int b) {
    int out;
    __asm__("nop" : [out]"=r"(out) : [in1]"r"(a), [in2]"r"(b));
    return out;
}
