/* Test: .incbin binary embedding directive */
/* Tests that .incbin reads a file and embeds its binary content */

/* Create a small test: embed this file's first few bytes */
__asm__(
    ".section .rodata.incbin_test,\"aM\",@progbits,1\n"
    ".incbin \"tests/fixtures/c/incbin_data.bin\"\n"
    ".previous\n"
);

/* Test with skip and count parameters */
__asm__(
    ".section .rodata.incbin_skip,\"aM\",@progbits,1\n"
    ".incbin \"tests/fixtures/c/incbin_data.bin\", 4, 8\n"
    ".previous\n"
);

int main(void) {
    return 0;
}
