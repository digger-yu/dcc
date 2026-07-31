/* Phase 4: SysV AMD64 va_list — GP + SSE 浮点参数区 */
static long sum_values(int count, ...) {
    __builtin_va_list ap;
    long total = 0;
    int i;

    __builtin_va_start(ap, count);
    for (i = 0; i < count; i = i + 1)
        total = total + __builtin_va_arg(ap, long);
    __builtin_va_end(ap);
    return total;
}

static long copy_values(int count, ...) {
    __builtin_va_list ap;
    __builtin_va_list copy;
    long value;

    __builtin_va_start(ap, count);
    __builtin_va_copy(copy, ap);
    value = __builtin_va_arg(copy, long);
    __builtin_va_end(copy);
    __builtin_va_end(ap);
    return value;
}

static double sum_doubles(int count, ...) {
    __builtin_va_list ap;
    double total = 0.0;
    int i;

    __builtin_va_start(ap, count);
    for (i = 0; i < count; i = i + 1)
        total = total + __builtin_va_arg(ap, double);
    __builtin_va_end(ap);
    return total;
}

static double mixed_sum(int count, double first, ...) {
    __builtin_va_list ap;
    double total = first;
    int i;

    __builtin_va_start(ap, first);
    for (i = 0; i < count; i = i + 1)
        total = total + __builtin_va_arg(ap, double);
    __builtin_va_end(ap);
    return total;
}

int main(void) {
    if (sum_values(3, 10L, 20L, 30L) != 60L) return 1;
    if (sum_values(1, 42L) != 42L) return 2;
    if (copy_values(1, 99L) != 99L) return 3;
    if (sum_doubles(3, 1.0, 2.0, 3.0) != 6.0) return 4;
    if (mixed_sum(2, 10.0, 0.5, 1.5) != 12.0) return 5;
    return 0;
}
