/* Test: asm goto output constraints across fall-through and jump paths
 * Verifies that:
 * - fall-through path: output writeback occurs (GCC semantics)
 * - jump path: output retains initial value (not written back)
 *
 * NOTE: Each function uses unique label names to avoid asm label collision.
 */

__attribute__((noinline))
int test_asm_goto_output_jump(void) {
    int result = 77;  /* initial value */
    asm goto (
        "test %0, %0\n\t"       /* test result (will be non-zero) */
        "jnz %l[jump_target1]"  /* jump if not zero -> will jump */
        :
        : "r" (result)
        : "cc"
        : jump_target1
    );
    /* fall-through: should not reach here */
    return 0;

jump_target1:
    /* jump path: result retains initial value 77 */
    return result;
}

__attribute__((noinline))
int test_asm_goto_output_fallthrough(void) {
    int x = 0;  /* zero -> test will set ZF */
    asm goto (
        "test %0, %0\n\t"       /* test x (will be zero) */
        "jnz %l[jump_target2]"  /* jump if not zero -> won't jump */
        :
        : "r" (x)
        : "cc"
        : jump_target2
    );
    /* fall-through path: x == 0 */
    return 42;

jump_target2:
    /* should not reach here */
    return -1;
}

int main(void) {
    int r1 = test_asm_goto_output_jump();
    /* jump path: result retains initial value 77 */
    if (r1 != 77) return 1;

    int r2 = test_asm_goto_output_fallthrough();
    /* fall-through path returns 42 */
    if (r2 != 42) return 2;

    return 0;
}
