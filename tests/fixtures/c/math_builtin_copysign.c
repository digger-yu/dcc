/* Test __builtin_copysign / copysignf: libc forwarding verification */
double test_copysign(double mag, double sgn) {
    return __builtin_copysign(mag, sgn);
}

float test_copysignf(float mag, float sgn) {
    return __builtin_copysignf(mag, sgn);
}

int main(void) {
    /* copysign(3.0, -1.0) == -3.0 */
    double d = test_copysign(3.0, -1.0);
    if (d < -3.001 || d > -2.999) return 1;

    /* copysign(3.0, 1.0) == 3.0 */
    d = test_copysign(3.0, 1.0);
    if (d < 2.999 || d > 3.001) return 2;

    /* copysign(-5.0, 1.0) == 5.0 */
    d = test_copysign(-5.0, 1.0);
    if (d < 4.999 || d > 5.001) return 3;

    /* copysignf(2.0f, -1.0f) == -2.0f */
    float f = test_copysignf(2.0f, -1.0f);
    if (f < -2.001f || f > -1.999f) return 4;

    return 0;
}
