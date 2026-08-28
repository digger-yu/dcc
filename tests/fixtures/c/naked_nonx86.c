/* Test: __attribute__((naked)) on non-x86 targets */
/* Tests that naked functions work with asm goto and statement expressions */

#if defined(__aarch64__) || defined(__riscv) || defined(__arm__) || defined(__mips__)
/* naked function with simple asm */
__attribute__((naked)) void naked_simple(void) {
    __asm__ volatile("nop");
}

/* naked function with asm goto */
__attribute__((naked)) int naked_goto(int x) {
    __asm__ goto(
        "nop\n"
        "%0:\n"
        : : "r"(x) : : done
    );
    return 0;
done:
    return 1;
}

/* naked function with multiple asm statements */
__attribute__((naked)) void naked_multi(void) {
    __asm__ volatile("nop");
    __asm__ volatile("nop");
}
#endif

int main(void) {
    return 0;
}
