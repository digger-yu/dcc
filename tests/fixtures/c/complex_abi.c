/* double _Complex SysV AMD64 by-value ABI coverage. */
#include <complex.h>

static double _Complex mk(double r, double i) {
    double _Complex z;
    __real__ z = r;
    __imag__ z = i;
    return z;
}

static double real_part(double _Complex z) {
    return __real__ z;
}

static double imag_part(double _Complex z) {
    return __imag__ z;
}

static double _Complex add_dc(double _Complex a, double _Complex b) {
    return a + b;
}

static double _Complex id_dc(double _Complex z) {
    return z;
}

static double _Complex forward_dc(double r, double i) {
    return id_dc(mk(r, i));
}

static double _Complex mix9(
    double a0, double a1, double a2, double a3,
    double a4, double a5, double a6,
    double _Complex z)
{
    (void)a0;
    (void)a1;
    (void)a2;
    (void)a3;
    (void)a4;
    (void)a5;
    (void)a6;
    return z;
}

int main(void) {
    double _Complex z = mk(2.0, 3.0);
    if (real_part(z) != 2.0) return 1;
    if (imag_part(z) != 3.0) return 2;

    double _Complex rhs = mk(4.0, 5.0);
    double _Complex w = add_dc(z, rhs);
    if (__real__ w != 6.0) return 3;
    if (__imag__ w != 8.0) return 4;

    double _Complex stack_arg = forward_dc(8.0, 9.0);
    double _Complex s = mix9(1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0,
                             stack_arg);
    if (__real__ s != 8.0) return 5;
    if (__imag__ s != 9.0) return 6;

    double _Complex direct = id_dc(mk(10.0, 11.0));
    if (__real__ direct != 10.0) return 7;
    if (__imag__ direct != 11.0) return 8;

    return 0;
}
