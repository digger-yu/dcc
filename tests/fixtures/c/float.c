/* M2.5: float support - float/double/long double ABI */
#include <stdio.h>

static int fails = 0;

#define CHECK(cond, msg) do { \
    if (!(cond)) { \
        printf("FAIL: %s\n", msg); \
        fails++; \
    } \
} while (0)

#define CHECK_CLOSE(a, b, eps, msg) do { \
    double _a = (double)(a), _b = (double)(b); \
    if (_a - _b > (eps) || _b - _a > (eps)) { \
        printf("FAIL: %s (got %g, expected %g)\n", msg, _a, _b); \
        fails++; \
    } \
} while (0)

static float add_f(float a, float b) { return a + b; }
static double add_d(double a, double b) { return a + b; }

static double sum4(double a, double b, double c, double d) {
    return a + b + c + d;
}

static float sum4f(float a, float b, float c, float d) {
    return a + b + c + d;
}

int main(void) {
    /* sizeof checks */
    CHECK(sizeof(float) == 4, "sizeof(float) != 4");
    CHECK(sizeof(double) == 8, "sizeof(double) != 8");
    CHECK(sizeof(long double) == 16, "sizeof(long double) != 16");

    /* Basic float arithmetic */
    CHECK_CLOSE(add_f(1.5f, 2.5f), 4.0, 0.001, "float add");
    CHECK_CLOSE(add_d(1.5, 2.5), 4.0, 0.001, "double add");

    /* Float/int conversion */
    int i = (int)3.7;
    CHECK(i == 3, "double to int truncation");
    float f = (float)42;
    CHECK_CLOSE(f, 42.0, 0.001, "int to float");
    double d = (double)42;
    CHECK_CLOSE(d, 42.0, 0.001, "int to double");

    /* Multiple args ABI test */
    CHECK_CLOSE(sum4(1.0, 2.0, 3.0, 4.0), 10.0, 0.001, "double sum4");
    CHECK_CLOSE(sum4f(1.0f, 2.0f, 3.0f, 4.0f), 10.0f, 0.01, "float sum4f");

    /* Struct with mixed float/int */
    struct { double d; int i; } m;
    m.d = 3.14;
    m.i = 42;
    CHECK_CLOSE(m.d, 3.14, 0.001, "struct mixed double");
    CHECK(m.i == 42, "struct mixed int");

    /* Float comparisons */
    CHECK(1.0 < 2.0, "double less-than");
    CHECK(2.0 > 1.0, "double greater-than");
    CHECK(1.0 == 1.0, "double equality");
    CHECK(1.0 != 2.0, "double inequality");

    /* Float precision */
    float precise_f = 1.0f / 3.0f;
    CHECK_CLOSE(precise_f * 3.0f, 1.0f, 0.001, "float 1/3 * 3");

    /* Double precision */
    double pi = 3.14159265358979;
    CHECK_CLOSE(pi, 3.14159265358979, 0.0000001, "double pi precision");

    if (fails == 0) {
        printf("ALL FLOAT CHECKS PASSED\n");
    } else {
        printf("%d float checks FAILED\n", fails);
    }
    return fails;
}
