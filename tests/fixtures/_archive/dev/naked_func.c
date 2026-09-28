/* Test __attribute__((naked)): function body consists entirely of inline asm.
 * The compiler must NOT generate prologue/epilogue for naked functions.
 * This is used by the Linux kernel for low-level entry points
 * (exception vectors, syscall entries, CPU init code).
 */

/* Basic naked function: just an asm ret */
__attribute__((naked)) int naked_ret(void) {
    __asm__("ret");
}

/* Naked function with asm operations before ret */
__attribute__((naked)) int naked_add(int a, int b) {
    __asm__(
        "mov %edi, %eax\n"
        "add %esi, %eax\n"
        "ret"
    );
}

/* Naked function with multiple asm statements */
__attribute__((naked)) long naked_get_sp(void) {
    __asm__("mov %rsp, %rax");
    __asm__("ret");
}

/* Naked function combined with section attribute (kernel pattern) */
__attribute__((naked, section(".text.naked"))) void naked_in_section(void) {
    __asm__("nop");
    __asm__("ret");
}

/* Naked function combined with other attributes */
__attribute__((naked, unused)) int naked_unused(void) {
    __asm__("ret");
}

/* Normal function for comparison - should have prologue/epilogue */
int normal_func(int x) {
    return x + 1;
}

int main(void) {
    /* Call the naked add function to verify it works at runtime */
    int result = naked_add(20, 22);
    if (result != 42) return 1;

    /* Normal function should still work */
    if (normal_func(41) != 42) return 2;

    return 0;
}
