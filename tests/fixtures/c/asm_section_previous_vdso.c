/* Test fixture: .section/.previous support for VDSO-like patterns.
 *
 * VDSO files use .section/.previous (not just .pushsection/.popsection)
 * to place data in custom sections.  Without .section support, dcc strips
 * the directive and emits data into .text with R_X86_64_32 absolute
 * relocations, which are PIC-incompatible and cause "bad or irreducible
 * absolute expression" errors at link time. */

/* Pattern 1: .section/.previous with data (like .altinstr_replacement) */
void test_section_previous_data(void)
{
    __asm__ __volatile__(
        "1: nop\n\t"
        ".section .altinstr_replacement, \"ax\"\n\t"
        "2: nop\n\t"
        ".previous\n\t"
        ".section .altinstructions, \"a\"\n\t"
        ".long 1b - .\n\t"
        ".long 2b - .\n\t"
        ".word 217\n\t"
        ".byte 5\n\t"
        ".byte 5\n\t"
        ".previous"
    );
}

/* Pattern 2: .section with symbol + N expression (PIC-compatible PC32) */
void test_section_symbol_plus_offset(void)
{
    __asm__ __volatile__(
        "1: ud2\n\t"
        ".section __bug_table, \"aw\"\n\t"
        ".align 4\n\t"
        ".long 1b - .\n\t"
        ".long 0\n\t"
        ".word 42\n\t"
        ".word 0\n\t"
        ".previous"
    );
}

/* Pattern 3: nested .pushsection inside .section context */
void test_nested_pushsection_in_section(void)
{
    __asm__ __volatile__(
        "1: movl $0, %eax\n\t"
        ".section .my_sec, \"aw\"\n\t"
        ".long 0xdeadbeef\n\t"
        ".pushsection __ex_table, \"a\"\n\t"
        ".long 1b - .\n\t"
        ".long 1b - .\n\t"
        ".popsection\n\t"
        ".previous"
    );
}

int main(void)
{
    test_section_previous_data();
    test_section_symbol_plus_offset();
    test_nested_pushsection_in_section();
    return 0;
}
