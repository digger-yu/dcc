/* Test: asm dead code elimination
 * Pure asm (no volatile, no outputs, no memory clobber) can be
 * eliminated by the optimizer when results are unused.
 * Volatile asm and asm with side effects are preserved.
 */

int pure_asm_eliminated(int a) {
    int x = a + 1;
    /* This pure asm has no outputs, no volatile, no memory clobber.
     * At -O2, the DCE pass should remove it since x is not used
     * after the asm. */
    __asm__("nop");
    return a * 2;
}

int volatile_asm_preserved(int a) {
    /* volatile asm must never be eliminated */
    __asm__ volatile("nop");
    return a;
}

int memory_clobber_asm_preserved(int a) {
    /* asm with memory clobber has observable side effect */
    __asm__("nop" ::: "memory");
    return a;
}

int asm_with_output_used(int a) {
    int result;
    __asm__("mov %1, %0" : "=r"(result) : "r"(a));
    return result;
}

int asm_with_output_unused(int a) {
    int result;
    /* asm with output but result unused: DCE can remove */
    __asm__("mov %1, %0" : "=r"(result) : "r"(a));
    return a;
}

int main(void) {
    pure_asm_eliminated(42);
    volatile_asm_preserved(1);
    memory_clobber_asm_preserved(2);
    int r = asm_with_output_used(10);
    asm_with_output_unused(20);
    return r == 10 ? 0 : 1;
}
