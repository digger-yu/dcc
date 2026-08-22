/* Test: asm diagnostic validation
 * Covers: duplicate names
 * Expected: warnings only (W0008); compilation succeeds
 */

int main(void) {
    int x = 0, y = 0;

    /* W0008: duplicate operand name */
    asm (
        "mov %0, %0"
        : [val] "=r" (x)
        : [val] "r" (y)
    );

    return 0;
}
