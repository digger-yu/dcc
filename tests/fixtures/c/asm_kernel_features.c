/* Kernel asm features: .byte multi-value, .pushsection skip, popcnt REX fix */

/* .byte directive with multiple comma-separated values */
void test_byte_multi(void) {
    asm volatile(".byte 0xc4, 0xe2, 0x78, 0x49, 0xc0");
}

/* .pushsection/.popsection: non-text section content must not leak to .text */
void test_pushsection_skip(void) {
    asm volatile(
        "nop\n"
        ".pushsection .rodata\n"
        ".long 42\n"
        ".popsection\n"
        "nop\n"
    );
}

/* popcntl: 32-bit popcnt must NOT emit spurious REX prefix (0x40) */
unsigned int test_popcntl(unsigned int x) {
    unsigned int result;
    asm volatile("popcntl %1, %0" : "=r"(result) : "r"(x));
    return result;
}

/* push $label: push address of local forward label */
void test_push_local_label(void) {
    asm volatile(
        "pushq $1f\n"
        "popq %rax\n"
        "1:\n"
    );
}

int main(void) {
    test_byte_multi();
    test_pushsection_skip();
    unsigned int c = test_popcntl(0xFF);
    test_push_local_label();
    return (c == 8) ? 0 : 1;
}
