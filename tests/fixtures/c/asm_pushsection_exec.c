/* Kernel asm fixture: executable .pushsection (.fixup / .altinstr_replacement)
 * code must be assembled into the custom section (not dropped, not leaked to
 * .text), with SHF_EXECINSTR ("ax") flags set on the section header. */

/* Kernel .fixup pattern: fixup code lives in its own executable section */
void test_fixup_section(long *p)
{
    long v;
    __asm__ __volatile__(
        "1: movq (%1), %0\n\t"
        ".pushsection .fixup, \"ax\"\n\t"
        "2: movq $-1, %0\n\t"
        "   jmp 3f\n\t"
        ".popsection\n\t"
        "3: nop\n\t"
        ".pushsection __ex_table, \"a\"\n\t"
        ".long 1b - .\n\t"
        ".long 2b - .\n\t"
        ".popsection\n\t"
        : "=r"(v)
        : "r"(p)
    );
    (void)v;
}

/* Kernel alternate-instruction pattern: replacement code in its own section */
void test_altinstr_replacement(void)
{
    __asm__ __volatile__(
        "1: nop\n\t"
        ".pushsection .altinstructions, \"a\"\n\t"
        ".quad 1b\n\t"
        ".quad 3f\n\t"
        ".word 1\n\t"
        ".byte 5\n\t"
        ".byte 0\n\t"
        ".popsection\n\t"
        ".pushsection .altinstr_replacement, \"ax\"\n\t"
        "3: nop\n\t"
        ".popsection"
    );
}

int main(void)
{
    test_fixup_section(0);
    test_altinstr_replacement();
    return 0;
}
