/* GNU asm goto and computed-goto regression. */
static int test_asm_goto(void) {
    int value = 1;
    asm goto("jmp %l0" : : : : target);
    value = 2;
target:
    return value == 1 ? 0 : 1;
}

static int test_computed_goto(void) {
    void *target = &&done;
    goto *target;
    return 2;
done:
    return 0;
}

int main(void) {
    return test_asm_goto() | test_computed_goto();
}
