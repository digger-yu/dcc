/* Test: optimizer preserves asm statements (GCC-compatible DCE behavior)
 * Even asm without outputs or clobbers must not be eliminated.
 */

__attribute__((noinline))
int test_asm_nop_preserved(void) {
    int x = 0;
    /* This asm has an output, so it must be preserved. */
    asm volatile (
        "mov $42, %0"
        : "=r" (x)
    );
    return x;
}

__attribute__((noinline))
int test_asm_no_output_preserved(void) {
    /* This asm has no outputs and no clobbers, but must still be preserved
     * because GCC treats all inline asm as potentially side-effecting. */
    asm volatile (
        "nop"
    );
    return 0;
}

int main(void) {
    int r1 = test_asm_nop_preserved();
    if (r1 != 42) return 1;
    int r2 = test_asm_no_output_preserved();
    if (r2 != 0) return 2;
    return 0;
}
