int main(void) {
    double x = __builtin_huge_val();
    float y = __builtin_huge_valf();
    /* long double: skip, may not be fully supported */
    if (x != 1.0/0.0) return 1;
    if (y != 1.0f/0.0f) return 2;
    if (!__builtin_isinf(x)) return 4;
    if (!__builtin_isinff(y)) return 5;
    if (__builtin_isnan(x)) return 7;
    if (__builtin_isnan(y)) return 8;
    if (__builtin_isunordered(x, y)) return 10;
    if (!__builtin_finite(1.0)) return 11;
    if (__builtin_finite(x)) return 12;
    if (__builtin_finite(0.0/0.0)) return 13;
    return 0;
}
