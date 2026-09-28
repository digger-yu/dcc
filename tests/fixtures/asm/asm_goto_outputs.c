/* asm goto output constraint write-back semantics:
 *   fall-through path: outputs are written back
 *   jump path: outputs retain original values
 */

int test_asm_goto_basic(void) {
    int x = 0;
    /* always jumps to label_done, x should NOT be written back */
    asm goto (
        "jmp %l0"
        : "=r"(x)
        : "i"(42)
        :
        : label_done
    );
    return -1;  /* not reached */
label_done:
    return x;  /* 0: output not written back on jump path */
}

int test_asm_goto_fallthrough(void) {
    int x = 0;
    int result = 0;
    /* writes 42 to output and falls through, result should be 42 */
    asm goto (
        "mov $42, %0"
        : "=r"(result)
        : "r"(x)
        : "cc"
        : label_unused
    );
    if (result != 42) return 1;
    if (x != 0) return 2;
    return 0;
label_unused:
    return 3;  /* not reached */
}

int test_asm_goto_taken(void) {
    int x = 1;  /* non-zero, jne will jump */
    int result = 0;
    /* writes 42 to output, then jumps; output should NOT be written back */
    asm goto (
        "mov $42, %0\n\t"
        "test %1, %1\n\t"
        "jne %l0"
        : "=r"(result)
        : "r"(x)
        : "cc"
        : label_zero
    );
    return 4;  /* not reached */
label_zero:
    return result;  /* 0: output not written back on jump path */
}

int main(void) {
    int r = test_asm_goto_basic();
    if (r != 0) return 100 + r;
    r = test_asm_goto_fallthrough();
    if (r != 0) return 200 + r;
    r = test_asm_goto_taken();
    if (r != 0) return 300 + r;
    return 0;
}
