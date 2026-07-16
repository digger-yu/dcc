int main(void) {
    int r = 0;
    /* test_bswap */
    { unsigned int x = 0x01020304; unsigned int y = __builtin_bswap32(x); if (y != 0x04030201) r += 1; }
    /* test_clz */
    { unsigned int x = 0x00000001; int n = __builtin_clz(x); if (n != 31) r += 1; }
    /* test_ctz */
    { unsigned int x = 0x00000100; int n = __builtin_ctz(x); if (n != 8) r += 1; }
    /* test_popcount */
    { unsigned int x = 0xFF; int n = __builtin_popcount(x); if (n != 8) r += 1; }
    /* test_parity */
    { unsigned int x = 0xFF; int n = __builtin_parity(x); if (n != 0) r += 1; }
    return r;
}
