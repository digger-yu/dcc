/* Test __builtin_floor/ceil/round/trunc and f variants: SSE roundsd/roundss */
double test_floor(double x)  { return __builtin_floor(x); }
double test_ceil(double x)   { return __builtin_ceil(x); }
double test_round(double x)  { return __builtin_round(x); }
double test_trunc(double x)  { return __builtin_trunc(x); }

float test_floorf(float x)   { return __builtin_floorf(x); }
float test_ceilf(float x)    { return __builtin_ceilf(x); }
float test_roundf(float x)   { return __builtin_roundf(x); }
float test_truncf(float x)   { return __builtin_truncf(x); }

int approx_eq(double a, double b) {
    double d = a - b;
    return d > -0.001 && d < 0.001;
}

int main(void) {
    /* floor(2.7) == 2.0 */
    if (!approx_eq(test_floor(2.7), 2.0)) return 1;
    /* floor(-2.3) == -3.0 */
    if (!approx_eq(test_floor(-2.3), -3.0)) return 2;

    /* ceil(2.3) == 3.0 */
    if (!approx_eq(test_ceil(2.3), 3.0)) return 3;
    /* ceil(-2.7) == -2.0 */
    if (!approx_eq(test_ceil(-2.7), -2.0)) return 4;

    /* round(2.5) == 2.0 or 3.0 (nearest-even) */
    if (!approx_eq(test_round(2.3), 2.0)) return 5;
    if (!approx_eq(test_round(2.7), 3.0)) return 6;

    /* trunc(2.9) == 2.0 */
    if (!approx_eq(test_trunc(2.9), 2.0)) return 7;
    /* trunc(-2.9) == -2.0 */
    if (!approx_eq(test_trunc(-2.9), -2.0)) return 8;

    /* float variants */
    if (test_floorf(2.7f) < 1.999f || test_floorf(2.7f) > 2.001f) return 9;
    if (test_ceilf(2.3f) < 2.999f || test_ceilf(2.3f) > 3.001f) return 10;
    if (test_roundf(2.7f) < 2.999f || test_roundf(2.7f) > 3.001f) return 11;
    if (test_truncf(2.9f) < 1.999f || test_truncf(2.9f) > 2.001f) return 12;

    return 0;
}
