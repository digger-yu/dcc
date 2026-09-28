/* Test: asm volatile semantics
 * Verifies that asm volatile is preserved while plain asm may be eliminated.
 * This test ensures volatile flag is correctly propagated from parser through
 * SSA to codegen.
 */

/* asm volatile with no outputs must be preserved (has side effects) */
__attribute__((noinline))
void test_volatile_preserved(void) {
    asm volatile("nop");
    asm volatile("" ::: "memory");
}

/* plain asm with no outputs - compiler may keep or remove */
__attribute__((noinline))
void test_plain_asm(void) {
    asm("nop");
}

/* asm volatile with output constraint */
__attribute__((noinline))
int test_volatile_with_output(void) {
    int x = 0;
    asm volatile("mov $42, %0" : "=r"(x));
    return x;
}

/* asm goto is always volatile per GCC semantics */
__attribute__((noinline))
int test_asm_goto_volatile(int x) {
    asm goto("test %0, %0\n\t"
             "jnz %l[positive]"
             :
             : "r"(x)
             : "cc"
             : positive);
    return -1;

positive:
    return x * 2;
}

int main(void) {
    test_volatile_preserved();
    test_plain_asm();

    int r = test_volatile_with_output();
    if (r != 42) return 1;

    int r2 = test_asm_goto_volatile(5);
    if (r2 != 10) return 2;

    int r3 = test_asm_goto_volatile(0);
    if (r3 != -1) return 3;

    return 0;
}
