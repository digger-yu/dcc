// E0311: unsupported inline asm instruction must be a hard error,
// not a silent NOP fallback (P0-5).
void test_unsupported_asm(void) {
    int x;
    __asm__ volatile("xyzzy_invalid_insn %0" : "=r"(x));
}
