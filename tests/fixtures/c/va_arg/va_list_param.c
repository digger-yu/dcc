/* va_list 作为函数参数传递 */
typedef __builtin_va_list va_list;

int sum_via_ap(int n, va_list ap) {
    int s = 0;
    for (int i = 0; i < n; i++)
        s += __builtin_va_arg(ap, int);
    return s;
}

int caller(int n, ...) {
    va_list ap;
    __builtin_va_start(ap, n);
    int r = sum_via_ap(n, ap);
    __builtin_va_end(ap);
    return r;
}

int main(void) {
    /* caller(4, 10, 20, 30, 40) = 100 */
    return caller(4, 10, 20, 30, 40) - 100;
}
