/* Test: DWARF debug info for functions containing inline asm
 * Verifies that -g produces valid debug info even with asm statements.
 */

__attribute__((noinline))
int asm_with_debug(int x) {
    int result = 0;
    /* This asm block should not prevent debug info generation */
    asm volatile (
        "add %1, %0"
        : "=r" (result)
        : "r" (x), "0" (0)
    );
    return result;
}

int main(void) {
    int r = asm_with_debug(42);
    return (r == 42) ? 0 : 1;
}
