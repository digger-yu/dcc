/* __builtin_fpclassify 测试 */
int test_fpclassify_nan(void) {
    double x = 0.0/0.0;
    int c = __builtin_fpclassify(0, 1, 2, 3, 4, x);
    /* NaN should return args[0] mapping = 0 */
    if (c != 0) return 1;
    return 0;
}

int test_fpclassify_inf(void) {
    double x = __builtin_huge_val();
    int c = __builtin_fpclassify(0, 1, 2, 3, 4, x);
    /* +Inf should return args[1] mapping = 1 */
    if (c != 1) return 2;
    return 0;
}

int test_fpclassify_zero(void) {
    double x = 0.0;
    int c = __builtin_fpclassify(0, 1, 2, 3, 4, x);
    /* 0 should return args[2] mapping = 2 */
    if (c != 2) return 3;
    return 0;
}

int test_fpclassify_normal(void) {
    double x = 1.5;
    int c = __builtin_fpclassify(0, 1, 2, 3, 4, x);
    /* Normal should return args[4] mapping = 4 */
    if (c != 4) return 4;
    return 0;
}

/* 测试常规 builtins 仍工作 */
int test_basic_builtins(void) {
    /* popcount */
    if (__builtin_popcount(0xFF) != 8) return 1;
    if (__builtin_popcountl(0xFFFFFFFF) != 32) return 2;
    /* clz */
    if (__builtin_clz(0x10000) != 15) return 3;
    /* ctz */
    if (__builtin_ctz(0x10000) != 16) return 4;
    /* parity */
    if (__builtin_parity(0xFF) != 0) return 5;  /* 8 ones = even */
    if (__builtin_parity(0x7F) != 1) return 6;  /* 7 ones = odd */
    /* bswap */
    if (__builtin_bswap32(0x12345678) != 0x78563412) return 7;
    if (__builtin_bswap16(0x1234) != 0x3412) return 8;
    /* signbit */
    if (__builtin_signbit(1.0)) return 9;
    if (!__builtin_signbit(-1.0)) return 10;
    /* constant_p */
    if (!__builtin_constant_p(42)) return 11;
    return 0;
}

int main(void) {
    int r;
    r = test_fpclassify_nan();
    if (r != 0) return 100 + r;
    r = test_fpclassify_inf();
    if (r != 0) return 200 + r;
    r = test_fpclassify_zero();
    if (r != 0) return 300 + r;
    r = test_fpclassify_normal();
    if (r != 0) return 400 + r;
    r = test_basic_builtins();
    if (r != 0) return 500 + r;
    return 0;
}
