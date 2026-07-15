/* Phase 3: freestanding integer builtin regression. */
int main(void) {
    if (__builtin_bswap16(0x1234) != 0x3412) return 1;
    if (__builtin_bswap32(0x01020304U) != 0x04030201U) return 2;
    if (__builtin_bswap64(0x0102030405060708UL) !=
        0x0807060504030201UL) return 3;

    if (__builtin_bitreverse8(0x12U) != 0x48U) return 4;
    if (__builtin_bitreverse32(1U) != 0x80000000U) return 5;
    if (__builtin_parity(0xffU) != 0) return 6;
    if (__builtin_parity(0x7fU) != 1) return 7;

    if (!__builtin_add_overflow_p(127, 1, (signed char)0)) return 8;
    if (__builtin_add_overflow_p(126, 1, (signed char)0)) return 9;
    return 0;
}
