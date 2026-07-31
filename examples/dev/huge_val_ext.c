int main(void) {
    double x = __builtin_huge_val();
    float y = __builtin_huge_valf();
    long double z = __builtin_huge_vall();
    if (x != 1.0/0.0) return 1;
    if (y != 1.0f/0.0f) return 2;
    if (z != 1.0L/0.0L) return 3;
    if (!__builtin_isinf(x)) return 4;
    if (!__builtin_isinff(y)) return 5;
    if (!__builtin_isinfl(z)) return 6;
    /* +Inf must not be classified as NaN. */
    if (__builtin_isnan(x)) return 7;
    if (__builtin_isnan(y)) return 8;
    if (__builtin_isnan(z)) return 9;
    if (__builtin_isunordered(x, y)) return 10;
    return 0;
}
