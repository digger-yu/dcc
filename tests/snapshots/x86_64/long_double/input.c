/* Daily hotspot: long double.
 *
 * dcc stores LongDouble as a double (commit 668baa46) and routes
 * arithmetic through the SD opcode path. This case pins the
 * cvt / add / mul / div sequence and ensures no x87 FPU is used.
 */
long double ld_identity(long double x) {
    return x;
}

long double ld_add(long double a, long double b) {
    return a + b;
}

long double ld_mul(long double a, long double b) {
    return a * b;
}
