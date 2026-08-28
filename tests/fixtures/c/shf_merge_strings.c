/* Test: SHF_MERGE / SHF_STRINGS section flags */
/* Tests that merge/string sections get correct ELF flags */

/* String merge section: should get SHF_ALLOC|SHF_MERGE|SHF_STRINGS */
__asm__(
    ".pushsection .rodata.str.merged,\"aMS\",@progbits,1\n"
    ".string \"hello\"\n"
    ".string \"world\"\n"
    ".popsection\n"
);

/* Regular rodata section: should get SHF_ALLOC only */
__asm__(
    ".pushsection .rodata.custom,\"a\",@progbits\n"
    ".quad 42\n"
    ".popsection\n"
);

int main(void) {
    return 0;
}
