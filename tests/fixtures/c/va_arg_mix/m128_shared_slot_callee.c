typedef __builtin_va_list va_list;
typedef double v2df __attribute__((vector_size(16)));
int probe(int n, ...) {
    va_list ap; __builtin_va_start(ap, n);
    v2df a = __builtin_va_arg(ap, v2df);
    v2df b = __builtin_va_arg(ap, v2df);
    __builtin_va_end(ap);
    int r = 0;
    if (a[0] != 1.5 || a[1] != 2.5) r |= 1;
    if (b[0] != 3.5 || b[1] != 4.5) r |= 2;
    return r;
}
