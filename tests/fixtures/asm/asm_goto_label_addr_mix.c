/* Test: asm goto mixed with &&label (computed goto / label addresses)
 * Verifies that computed goto label addresses (&&label) can coexist
 * with asm goto in the same compilation unit.
 */

__attribute__((noinline))
int test_asm_goto_simple(int x) {
    asm goto (
        "test %0, %0\n\t"
        "jnz %l[positive]"
        :
        : "r" (x)
        : "cc"
        : positive
    );
    return -1;

positive:
    return x * 2;
}

__attribute__((noinline))
long test_label_addr_value(void) {
    void *target = &&here;
    long addr = (long)target;
    /* Label address should be non-zero */
    return addr;

here:
    return 0;
}

int main(void) {
    int r1 = test_asm_goto_simple(5);
    if (r1 != 10) return 1;

    int r2 = test_asm_goto_simple(0);
    if (r2 != -1) return 2;

    long addr = test_label_addr_value();
    if (addr == 0) return 3;

    return 0;
}
