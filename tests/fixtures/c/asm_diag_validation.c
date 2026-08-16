/* Test: asm diagnostic validation
 * Covers: duplicate names, type mismatch, missing/invalid placeholders
 * Expected: warnings only; unsupported mini-asm lines become nop
 */

int main(void) {
    int x = 0, y = 0;
    float f = 1.0f;

    /* W0008: duplicate operand name */
    asm (
        "mov %0, %0"
        : [val] "=r" (x)
        : [val] "r" (y)
    );

    /* W0005: float with 'r' constraint */
    asm (
        "mov %0, %1"
        : "=r" (x)
        : "r" (f)
    );

    /* W0008: placeholder index out of range */
    asm (
        "mov %0, %2"
        : "=r" (x)
        : "r" (y)
    );

    /* W0008: missing named placeholder */
    asm (
        "mov %[missing], %0"
        : "=r" (x)
    );

    /* W0008: placeholder index out of range */
    asm (
        "mov %2, %0"
        : "=r" (x)
        : "r" (y)
    );

    /* recognized integer mnemonic with illegal XMM operand: warning + nop */
    asm volatile("mov %xmm0, %rax");

    return 0;
}
