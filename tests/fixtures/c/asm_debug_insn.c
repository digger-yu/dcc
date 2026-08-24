/* Test: per-instruction DWARF debug info for inline asm blocks
 * Verifies that -g produces per-instruction line entries within asm blocks,
 * enabling GDB to step into individual asm instructions (like GCC .debug_insn).
 *
 * The asm block below contains multiple instructions; each should get its
 * own .debug_line entry with is_stmt=false and a unique discriminator,
 * while the first instruction also gets is_stmt=true (statement boundary).
 */

__attribute__((noinline))
int asm_multi_insn_debug(int x) {
    int result = 0;
    int tmp = 0;
    /* Multi-instruction asm block: should generate per-insn debug entries */
    asm volatile (
        "mov %1, %0\n\t"
        "add $1, %0\n\t"
        "add $2, %0\n\t"
        "shl $1, %0"
        : "=r" (result)
        : "r" (x)
    );
    tmp = result;
    return tmp;
}

/* Single-instruction asm block: should still work correctly */
__attribute__((noinline))
int asm_single_insn_debug(int x) {
    int result = 0;
    asm volatile (
        "mov %1, %0"
        : "=r" (result)
        : "r" (x)
    );
    return result;
}

int main(void) {
    int r1 = asm_multi_insn_debug(42);
    int r2 = asm_single_insn_debug(10);
    /* r1 = (42 + 1 + 2) << 1 = 90 */
    /* r2 = 10 */
    return (r1 == 90 && r2 == 10) ? 0 : 1;
}
