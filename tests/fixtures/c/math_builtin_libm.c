/* Test __builtin_pow/log/exp/sin: libm forwarding */
double test_pow(double x, double y)   { return __builtin_pow(x, y); }
double test_log(double x)             { return __builtin_log(x); }
double test_exp(double x)             { return __builtin_exp(x); }
double test_sin(double x)             { return __builtin_sin(x); }
float  test_powf(float x, float y)    { return __builtin_powf(x, y); }
float  test_logf(float x)             { return __builtin_logf(x); }
float  test_expf(float x)             { return __builtin_expf(x); }
float  test_sinf(float x)             { return __builtin_sinf(x); }

int approx_eq(double a, double b) {
    double d = a - b;
    return d > -0.01 && d < 0.01;
}

int main(void) {
    /* pow(2.0, 10.0) == 1024.0 */
    if (!approx_eq(test_pow(2.0, 10.0), 1024.0)) return 1;

    /* log(1.0) == 0.0 */
    if (!approx_eq(test_log(1.0), 0.0)) return 2;

    /* exp(0.0) == 1.0 */
    if (!approx_eq(test_exp(0.0), 1.0)) return 3;

    /* sin(0.0) == 0.0 */
    if (!approx_eq(test_sin(0.0), 0.0)) return 4;

    /* powf(3.0f, 2.0f) ~= 9.0f */
    float f = test_powf(3.0f, 2.0f);
    if (f < 8.99f || f > 9.01f) return 5;

    /* logf(1.0f) ~= 0.0f */
    f = test_logf(1.0f);
    if (f < -0.01f || f > 0.01f) return 6;

    /* expf(0.0f) ~= 1.0f */
    f = test_expf(0.0f);
    if (f < 0.99f || f > 1.01f) return 7;

    /* sinf(0.0f) ~= 0.0f */
    f = test_sinf(0.0f);
    if (f < -0.01f || f > 0.01f) return 8;

    return 0;
}
