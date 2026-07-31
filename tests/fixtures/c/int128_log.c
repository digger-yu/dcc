/* __int128 上的 && / || 逻辑运算测试 */

typedef __int128 i128;

static i128 make_high_only(int high) {
    i128 v = (i128)high << 64;
    return v;
}

int test_int128_and_high_only(void) {
    i128 a = make_high_only(1);
    int r = (a && 1);
    if (r != 1) return 1;
    return 0;
}

int test_int128_or_high_only(void) {
    i128 a = make_high_only(1);
    int r = (a || 0);
    if (r != 1) return 2;
    i128 b = (i128)0;
    r = (b || 0);
    if (r != 0) return 3;
    return 0;
}

int test_int128_and_zero(void) {
    i128 a = (i128)0;
    int r = (a && 1);
    if (r != 0) return 4;
    return 0;
}

int test_int128_or_nonzero(void) {
    i128 a = (i128)5;
    int r = (a || 0);
    if (r != 1) return 5;
    return 0;
}

int test_int128_short_circuit(void) {
    i128 a = make_high_only(1);
    int r = (a && (1 / 1));
    if (r != 1) return 6;
    return 0;
}

int main(void) {
    int r = test_int128_and_high_only();
    if (r != 0) return 100 + r;
    r = test_int128_or_high_only();
    if (r != 0) return 200 + r;
    r = test_int128_and_zero();
    if (r != 0) return 300 + r;
    r = test_int128_or_nonzero();
    if (r != 0) return 400 + r;
    r = test_int128_short_circuit();
    if (r != 0) return 500 + r;
    return 0;
}
