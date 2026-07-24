/* complex.h 函数 pass-through (Section 11.2)
 * 验证 cproj / cabs / creal / cimag / conj / csqrt / cexp 等被
 * dcc 发射为外部符号 (走 libm).
 * 编译为 -c 后, 用 nm 检查符号表中包含这些函数名. */
#include <complex.h>

double test_cabs(double complex z) { return cabs(z); }
double complex test_cproj(double complex z) { return cproj(z); }
double test_creal(double complex z) { return creal(z); }
double test_cimag(double complex z) { return cimag(z); }
double complex test_conj(double complex z) { return conj(z); }
double complex test_csqrt(double complex z) { return csqrt(z); }
double complex test_cexp(double complex z) { return cexp(z); }
double complex test_clog(double complex z) { return clog(z); }
double complex test_csin(double complex z) { return csin(z); }
double complex test_ccos(double complex z) { return ccos(z); }
double complex test_cpow(double complex z, double complex w) { return cpow(z, w); }
double complex test_carg(double complex z) { return carg(z); }
double complex test_csinh(double complex z) { return csinh(z); }
double complex test_ccosh(double complex z) { return ccosh(z); }
double complex test_ctanh(double complex z) { return ctanh(z); }
