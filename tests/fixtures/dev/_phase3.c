int test_bswap(void) {
    unsigned int x = 0x01020304;
    unsigned int y = __builtin_bswap32(x);
    return (y == 0x04030201) ? 0 : 1;
}
int test_clz(void) {
    unsigned int x = 0x00000001;
    int n = __builtin_clz(x);
    return (n == 31) ? 0 : 1;
}
int test_ctz(void) {
    unsigned int x = 0x00000100;
    int n = __builtin_ctz(x);
    return (n == 8) ? 0 : 1;
}
int test_popcount(void) {
    unsigned int x = 0xFF;
    int n = __builtin_popcount(x);
    return (n == 8) ? 0 : 1;
}
int test_parity(void) {
    unsigned int x = 0xFF;
    int n = __builtin_parity(x);
    return (n == 0) ? 0 : 1;
}
int main(void) {
    int r = 0;
    r += test_bswap();
    r += test_clz();
    r += test_ctz();
    r += test_popcount();
    r += test_parity();
    return r;
}
