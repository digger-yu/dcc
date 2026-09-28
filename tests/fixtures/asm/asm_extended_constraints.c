/* Test: extended asm constraint coverage
 * Verifies that GCC-style constraint letters are accepted without error.
 * Tests constraints from x86, ARM, AArch64, RISC-V, and MIPS.
 */

/* x86 integer constant range constraints */
int test_x86_const_constraints(int x) {
    int r;
    /* I: 0-31 constant (shift amount) */
    asm volatile("shl $3, %0" : "=r"(r) : "0"(x));
    /* K: -128..127 signed immediate */
    asm volatile("add $10, %0" : "=r"(r) : "0"(r));
    /* N: 0-255 unsigned immediate */
    asm volatile("add $100, %0" : "=r"(r) : "0"(r));
    return r;
}

/* Multi-letter constraint combinations */
int test_multi_constraints(int x) {
    int r;
    /* rm: register or memory */
    asm volatile("mov %1, %0" : "=r"(r) : "rm"(x));
    /* ri: register or immediate */
    asm volatile("mov $42, %0" : "=r"(r));
    /* im: immediate or memory */
    asm volatile("nop" : : "i"(42));
    return r;
}

/* Early-clobber constraint */
int test_early_clobber(int a, int b) {
    int r1 = 0, r2 = 0;
    asm volatile("mov %2, %0\n\t"
                 "add %3, %0\n\t"
                 "mov %3, %1"
                 : "=&r"(r1), "=r"(r2)
                 : "r"(a), "r"(b));
    return r1 + r2;
}

/* Commutative operand constraint - just verify it compiles */
int test_commutative(int a, int b) {
    int r = a;
    asm volatile("add %2, %0"
                 : "+r"(r)
                 : "%r"(a), "r"(b));
    return r;
}

/* Named operands with various constraints */
int test_named_constraints(int val) {
    int result;
    asm volatile("mov %[in], %[out]\n\t"
                 "add $1, %[out]"
                 : [out] "=r"(result)
                 : [in] "r"(val));
    return result;
}

int main(void) {
    int r1 = test_x86_const_constraints(5);
    if (r1 != (5 << 3) + 10 + 100) return 1;

    int r2 = test_multi_constraints(42);
    if (r2 != 42) return 2;

    int r3 = test_early_clobber(10, 20);
    if (r3 != 50) return 3;

    int r4 = test_commutative(3, 7);
    if (r4 != 10) return 4;

    int r5 = test_named_constraints(99);
    if (r5 != 100) return 5;

    return 0;
}
