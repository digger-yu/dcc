/* Test fixture: .pushsection non-text section data collection */

/* Simulate kernel BUG() macro pattern */
void test_bug_table_pushsection(void)
{
    __asm__ __volatile__(
        "1: ud2\n\t"
        ".pushsection __bug_table, \"aw\"\n\t"
        ".align 4\n\t"
        ".long 1b - .\n\t"
        ".long 0\n\t"
        ".word 42\n\t"
        ".word 0\n\t"
        ".popsection"
    );
}

/* Simulate kernel exception table pattern */
void test_ex_table_pushsection(int *p)
{
    int val;
    __asm__ __volatile__(
        "1: movl (%1), %0\n\t"
        ".pushsection __ex_table, \"a\"\n\t"
        ".long 1b - .\n\t"
        ".long 3f - .\n\t"
        ".popsection\n\t"
        "jmp 2f\n\t"
        "3: movl $-14, %0\n\t"
        "2:"
        : "=r"(val)
        : "r"(p)
    );
}

/* Test .byte and .quad directives */
void test_misc_data_directives(void)
{
    __asm__ __volatile__(
        ".pushsection .my_custom_sec, \"aw\"\n\t"
        ".byte 0xff\n\t"
        ".word 0x1234\n\t"
        ".long 0xdeadbeef\n\t"
        ".quad 0\n\t"
        ".zero 8\n\t"
        ".asciz \"test\"\n\t"
        ".popsection"
    );
}
