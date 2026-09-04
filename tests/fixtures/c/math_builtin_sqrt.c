/* Test __builtin_sqrt / sqrtf: inline SSE sqrtsd/sqrtss */
double test_sqrt_double(double x) {
    return __builtin_sqrt(x);
}

float test_sqrt_float(float x) {
    return __builtin_sqrtf(x);
}

int main(void) {
    /* sqrt(4.0) == 2.0 */
    double d = test_sqrt_double(4.0);
    if (d < 1.999 || d > 2.001) return 1;

    /* sqrt(2.0) ~= 1.4142 */
    d = test_sqrt_double(2.0);
    if (d < 1.414 || d > 1.415) return 2;

    /* sqrt(0.0) == 0.0 */
    d = test_sqrt_double(0.0);
    if (d != 0.0) return 3;

    /* sqrtf(9.0f) == 3.0f */
    float f = test_sqrt_float(9.0f);
    if (f < 2.999f || f > 3.001f) return 4;

    /* sqrtf(1.0f) == 1.0f */
    f = test_sqrt_float(1.0f);
    if (f < 0.999f || f > 1.001f) return 5;

    return 0;
}
