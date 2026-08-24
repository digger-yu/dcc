/* Non-x86 inline asm constraint resolution test fixture */
/* Tests enhanced template substitution with real register names */

/* Global asm with no operands: should pass through unchanged */
__asm__(".globl __dcc_asm_constraint_test\n");

/* Simple output + input: %0 = output reg, %1 = input reg */
int test_simple_constraint(int x) {
    int result;
    __asm__("nop" : "=r"(result) : "r"(x));
    return result;
}

/* Named operands: %[out] and %[in] should resolve to register names */
int test_named_operands(int a, int b) {
    int out;
    __asm__("nop" : [out]"=r"(out) : [in1]"r"(a), [in2]"r"(b));
    return out;
}

/* Matched constraint: %0 and %1 should use same register */
int test_matched_constraint(int x) {
    __asm__("nop" : "+r"(x) : "0"(x));
    return x;
}

/* Memory clobber: should emit architecture barrier instruction */
void test_memory_clobber(int *p) {
    __asm__ volatile("nop" : "+r"(p) : : "memory");
}

/* asm goto with labels: %l0 should resolve to label name */
int test_asm_goto_labels(int x) {
    int result = 0;
    __asm__ goto("nop" : : "r"(x) : "memory" : done);
    result = 1;
done:
    return result;
}

/* Multiple outputs and inputs */
int test_multi_operands(int a, int b) {
    int out1, out2;
    __asm__("nop" : "=r"(out1), "=r"(out2) : "r"(a), "r"(b));
    return out1 + out2;
}
