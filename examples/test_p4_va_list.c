/* Phase 4: simplified x86-64 GP va_list regression. */
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

int main(void) {
    if (sum_values(3, 10L, 20L, 30L) != 60L) return 1;
    if (sum_values(1, 42L) != 42L) return 2;
    if (copy_values(1, 99L) != 99L) return 3;
    return 0;
}
