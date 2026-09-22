/* 变参探针: 只用 __builtin_va_* , 不 include <stdarg.h> (交叉时没有 sysroot) */
typedef __builtin_va_list va_list;
int va_first(int n, ...) {
    va_list ap;
    int v;
    __builtin_va_start(ap, n);
    v = __builtin_va_arg(ap, int);
    __builtin_va_end(ap);
    return v + n;
}
