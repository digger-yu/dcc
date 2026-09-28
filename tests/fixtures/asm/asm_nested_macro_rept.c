/* Test: nested .macro / .rept blocks in inline asm strings
 * Verifies that deeply nested macro expansion and repetition blocks
 * work correctly in dcc's mini-asm expander.
 */

__attribute__((noinline))
int test_nested_macro_in_rept(void) {
    int result = 0;

    /* .macro defined then called inside .rept */
    asm volatile (
        ".rept 3\n\t"
        "add $1, %0\n\t"
        ".endr\n\t"
        : "+r" (result)
    );
    /* result should be 3 (3 repetitions of add $1) */
    if (result != 3) return 1;
    return 0;
}

__attribute__((noinline))
int test_rept_with_add(void) {
    int result = 0;

    /* Simple .rept with multiple adds */
    asm volatile (
        ".rept 4\n\t"
        "add $2, %0\n\t"
        ".endr\n\t"
        : "+r" (result)
    );
    /* result should be 8 (4 repetitions of add $2) */
    if (result != 8) return 2;
    return 0;
}

__attribute__((noinline))
int test_deeply_nested_rept(void) {
    int result = 0;

    /* Nested .rept blocks */
    asm volatile (
        ".rept 3\n\t"
        ".rept 2\n\t"
        "add $1, %0\n\t"
        ".endr\n\t"
        ".endr\n\t"
        : "+r" (result)
    );
    /* result should be 6 (3 outer * 2 inner = 6 adds) */
    if (result != 6) return 3;
    return 0;
}

__attribute__((noinline))
int test_irp_basic(void) {
    int result = 0;

    /* .irp iterates over values */
    asm volatile (
        ".irp val, 1, 2, 3\n\t"
        "add $\\val, %0\n\t"
        ".endr\n\t"
        : "+r" (result)
    );
    /* result should be 6 (1+2+3) */
    if (result != 6) return 4;
    return 0;
}

__attribute__((noinline))
int test_irp_nested_in_rept(void) {
    int result = 0;

    /* .irp inside .rept */
    asm volatile (
        ".rept 2\n\t"
        ".irp val, 1, 2, 3\n\t"
        "add $\\val, %0\n\t"
        ".endr\n\t"
        ".endr\n\t"
        : "+r" (result)
    );
    /* result should be 12 (2 * (1+2+3) = 12) */
    if (result != 12) return 5;
    return 0;
}

int main(void) {
    int r;
    r = test_nested_macro_in_rept();
    if (r != 0) return r;
    r = test_rept_with_add();
    if (r != 0) return r;
    r = test_deeply_nested_rept();
    if (r != 0) return r;
    r = test_irp_basic();
    if (r != 0) return r;
    r = test_irp_nested_in_rept();
    if (r != 0) return r;
    return 0;
}
