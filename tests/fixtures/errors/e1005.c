#include <stdarg.h>

int f(int count, ...) {
    int x = count;
    return __builtin_va_arg(&x, int);
}

int main(void) { return f(1, 42); }
