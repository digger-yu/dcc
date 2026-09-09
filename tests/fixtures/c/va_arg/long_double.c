/* long double va_arg + 含 long double 的结构体 */
typedef __builtin_va_list va_list;

long double sum_ld(int n, ...) {
    va_list ap;
    __builtin_va_start(ap, n);
    long double s = 0.0L;
    for (int i = 0; i < n; i++)
        s += __builtin_va_arg(ap, long double);
    __builtin_va_end(ap);
    return s;
}

struct WithLD {
    int tag;
    long double val;
};

struct WithLD get_ld_struct(int n, ...) {
    va_list ap;
    __builtin_va_start(ap, n);
    struct WithLD r;
    r.tag = __builtin_va_arg(ap, int);
    r.val = __builtin_va_arg(ap, long double);
    __builtin_va_end(ap);
    return r;
}

int main(void) {
    /* long double 求和 */
    long double ld_sum = sum_ld(3, 1.0L, 2.5L, 3.5L);
    int sum_err = (int)(ld_sum * 10.0L) - 70;
    if (sum_err < 0) sum_err = -sum_err;

    /* 含 long double 的结构体 */
    struct WithLD w = get_ld_struct(2, 99, 7.5L);
    int tag_err = w.tag - 99;
    int val_err = (int)(w.val * 10.0L) - 75;
    if (val_err < 0) val_err = -val_err;

    return (sum_err > 1 ? 1 : 0) + (tag_err != 0 ? 1 : 0) + (val_err > 1 ? 1 : 0);
}
