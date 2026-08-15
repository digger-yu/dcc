/* Test: asm diagnostic validation
 * Covers: output prefix, duplicate names, type mismatch, placeholder count
 * Expected: multiple warnings/errors from the semantic validator
 */

int main(void) {
    int x = 0, y = 0;
    float f = 1.0f;

    /* E0016: duplicate operand name */
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

    /* E0015: placeholder index out of range */
    asm (
        "mov %0, %2"
        : "=r" (x)
        : "r" (y)
    );

    return 0;
}
