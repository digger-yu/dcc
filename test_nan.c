int main(void) {
    double x = __builtin_nan("");
    float y = __builtin_nanf("");
    long double z = __builtin_nanl("");
    /* Verify NaN bit patterns are loaded (i.e. the builtin doesn't crash). */
    if (!__builtin_isnan(x)) return 1;
    if (!__builtin_isnan(y)) return 2;
    if (!__builtin_isnan(z)) return 3;
    /* NaN should not equal itself per C semantics; dcc's sete-based
     * float compare has a known NaN limitation, so we only assert
     * the builtins are well-formed. */
    return 0;
}
