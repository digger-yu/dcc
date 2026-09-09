/* 含 int+double 的结构体 va_arg */
typedef __builtin_va_list va_list;

struct Mixed {
    int x;
    double y;
};

struct Mixed get_mixed(int n, ...) {
    va_list ap;
    __builtin_va_start(ap, n);
    struct Mixed s;
    s.x = __builtin_va_arg(ap, int);
    s.y = __builtin_va_arg(ap, double);
    __builtin_va_end(ap);
    return s;
}

int main(void) {
    struct Mixed r = get_mixed(2, 42, 3.14);
    /* 验证: x == 42, y 约等于 3.14 */
    int xi = r.x - 42;
    int yi = (int)(r.y * 100.0) - 314;
    if (yi < 0) yi = -yi;
    return xi + (yi > 1 ? 1 : 0);
}
