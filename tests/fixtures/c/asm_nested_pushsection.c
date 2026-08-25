/* Test fixture: nested .pushsection/.popsection handling */

/* Simulate kernel exception table with nested .fixup section */
void test_nested_pushsection_ex_table(int *p)
{
    int val;
    __asm__ __volatile__(
        "1: movl (%1), %0\n\t"
        ".pushsection __ex_table, \"a\"\n\t"
        ".long 1b - .\n\t"
        ".long 3f - .\n\t"
        ".long 0\n\t"
        ".pushsection .fixup, \"ax\"\n\t"
        "3: movl $-14, %0\n\t"
        "jmp 2f\n\t"
        ".popsection\n\t"
        ".popsection\n\t"
        "jmp 2f\n\t"
        "2:"
        : "=r"(val)
        : "r"(p)
    );
}

/* Test multiple levels of nesting */
void test_deeply_nested_pushsection(void)
{
    __asm__ __volatile__(
        "1: nop\n\t"
        ".pushsection .sec1, \"aw\"\n\t"
        ".long 1b\n\t"
        ".pushsection .sec2, \"aw\"\n\t"
        ".long 2f\n\t"
        ".pushsection .sec3, \"aw\"\n\t"
        "2: .long 0\n\t"
        ".popsection\n\t"
        ".popsection\n\t"
        ".popsection"
    );
}

/* Test nested exec pushsection */
void test_nested_exec_pushsection(void)
{
    __asm__ __volatile__(
        "1: nop\n\t"
        ".pushsection .fixup, \"ax\"\n\t"
        "movl $0, %eax\n\t"
        ".pushsection .altinstr_replacement, \"ax\"\n\t"
        "nop\n\t"
        ".popsection\n\t"
        "ret\n\t"
        ".popsection"
    );
}
