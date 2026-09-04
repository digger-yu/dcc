/* Test __builtin_fmin/fmax/fminf/fmaxf: inline SSE minsd/maxsd */
double test_fmin(double a, double b) { return __builtin_fmin(a, b); }
double test_fmax(double a, double b) { return __builtin_fmax(a, b); }
float  test_fminf(float a, float b)  { return __builtin_fminf(a, b); }
float  test_fmaxf(float a, float b)  { return __builtin_fmaxf(a, b); }

int approx_eq(double a, double b) {
    double d = a - b;
    return d > -0.001 && d < 0.001;
}

int main(void) {
    /* fmin(3.0, 5.0) == 3.0 */
    if (!approx_eq(test_fmin(3.0, 5.0), 3.0)) return 1;
    /* fmin(-1.0, 1.0) == -1.0 */
    if (!approx_eq(test_fmin(-1.0, 1.0), -1.0)) return 2;

    /* fmax(3.0, 5.0) == 5.0 */
    if (!approx_eq(test_fmax(3.0, 5.0), 5.0)) return 3;
    /* fmax(-1.0, 1.0) == 1.0 */
    if (!approx_eq(test_fmax(-1.0, 1.0), 1.0)) return 4;

    /* fminf(2.0f, 7.0f) == 2.0f */
    if (test_fminf(2.0f, 7.0f) < 1.999f || test_fminf(2.0f, 7.0f) > 2.001f) return 5;
    /* fmaxf(2.0f, 7.0f) == 7.0f */
    if (test_fmaxf(2.0f, 7.0f) < 6.999f || test_fmaxf(2.0f, 7.0f) > 7.001f) return 6;

    return 0;
}
