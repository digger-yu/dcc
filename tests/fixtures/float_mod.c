/* 浮点取模 (fmod) 测试 — 软件 divss/divsd + trunc，无 libc */

int test_float_mod_basic(void) {
    float a = 7.0f, b = 3.0f;
    float r = a % b;
    if (r != 1.0f) return 1;
    return 0;
}

int test_double_mod_basic(void) {
    double a = 10.5, b = 3.0;
    double r = a % b;
    if (r != 1.5) return 2;
    return 0;
}

int test_float_mod_negative(void) {
    float a = -7.0f, b = 3.0f;
    float r = a % b;
    if (r != -1.0f) return 3;
    return 0;
}

int test_float_mod_exact(void) {
    float a = 6.0f, b = 3.0f;
    float r = a % b;
    if (r != 0.0f) return 4;
    return 0;
}

int test_double_mod_small(void) {
    double a = 1.5, b = 0.5;
    double r = a % b;
    if (r != 0.0) return 5;
    return 0;
}

int test_double_mod_expr(void) {
    double r = 10.5 % 3.0;
    if (r != 1.5) return 6;
    return 0;
}

int main(void) {
    int r = test_float_mod_basic();
    if (r != 0) return 100 + r;
    r = test_double_mod_basic();
    if (r != 0) return 200 + r;
    r = test_float_mod_negative();
    if (r != 0) return 300 + r;
    r = test_float_mod_exact();
    if (r != 0) return 400 + r;
    r = test_double_mod_small();
    if (r != 0) return 500 + r;
    r = test_double_mod_expr();
    if (r != 0) return 600 + r;
    return 0;
}
