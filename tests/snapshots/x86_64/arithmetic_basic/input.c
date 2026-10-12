/* Daily hotspot: basic arithmetic (add/sub/mul/div/mod, shifts).
 *
 * Covers the integer arith dispatch path: three-operand forms,
 * immediate forms, and the signed/unsigned divide corner (idiv
 * vs div, which requires RDX zero/sign-extended first).
 */
int arith_basic(int a, int b, int c) {
    int x = a + b * c;
    int y = (a - b) / (c ? c : 1);
    int z = (a << 2) + (b >> 1);
    int w = x % 7;
    return x + y + z + w;
}

unsigned arith_unsigned(unsigned a, unsigned b) {
    return (a << 4) | (b >> 4);
}
