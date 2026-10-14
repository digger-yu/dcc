/* Daily hotspot: va_arg (variable arguments).
 *
 * Pin the System V AMD64 va_list handling: register save area on
 * stack, overflow_area pointer, and the va_arg() advancement for
 * both integer and floating slots. dcc recently unified long-double
 * va_arg to the xmm0 double convention (commit 71815369).
 */
#include <stdarg.h>

int va_sum_int(int n, ...) {
    va_list ap;
    va_start(ap, n);
    int s = 0;
    for (int i = 0; i < n; i++) {
        s += va_arg(ap, int);
    }
    va_end(ap);
    return s;
}

double va_sum_dbl(int n, ...) {
    va_list ap;
    va_start(ap, n);
    double s = 0.0;
    for (int i = 0; i < n; i++) {
        s += va_arg(ap, double);
    }
    va_end(ap);
    return s;
}
