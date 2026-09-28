int test_va_int(int n, ...) {
    __builtin_va_list ap;
    __builtin_va_start(ap, n);
    int sum = 0;
    int i;
    for (i = 0; i < n; i = i + 1) {
        sum = sum + __builtin_va_arg(ap, int);
    }
    __builtin_va_end(ap);
    return sum;
}

int test_va_long(int n, ...) {
    __builtin_va_list ap;
    __builtin_va_start(ap, n);
    long sum = 0;
    int i;
    for (i = 0; i < n; i = i + 1) {
        sum = sum + __builtin_va_arg(ap, long);
    }
    __builtin_va_end(ap);
    return sum;
}

int main(void) {
    int r = 0;
    /* test_va_int(3, 10, 20, 30) should return 60 */
    if (test_va_int(3, 10, 20, 30) != 60) r = r + 1;
    /* test_va_int(1, 42) should return 42 */
    if (test_va_int(1, 42) != 42) r = r + 2;
    /* test_va_long(2, 100L, 200L) should return 300 */
    if (test_va_long(2, 100L, 200L) != 300) r = r + 4;
    return r;
}
