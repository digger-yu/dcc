/* va_arg(ap, struct S) 直接聚合读取 */
typedef __builtin_va_list va_list;

struct Mixed {
    int x;
    double y;
};

struct Pair {
    int a;
    int b;
};

/* 传入结构体实参, va_arg 以结构体类型读取 */
struct Pair get_pair(int n, ...) {
    va_list ap;
    __builtin_va_start(ap, n);
    struct Pair p = __builtin_va_arg(ap, struct Pair);
    __builtin_va_end(ap);
    return p;
}

struct Mixed get_mixed(int n, ...) {
    va_list ap;
    __builtin_va_start(ap, n);
    struct Mixed m = __builtin_va_arg(ap, struct Mixed);
    __builtin_va_end(ap);
    return m;
}

int main(void) {
    /* 测试 1: 传入 struct Pair 实参 */
    struct Pair arg1 = {100, 200};
    struct Pair r1 = get_pair(1, arg1);
    if (r1.a != 100 || r1.b != 200) return 1;

    /* 测试 2: 传入 struct Mixed 实参 */
    struct Mixed arg2 = {42, 3.14};
    struct Mixed r2 = get_mixed(1, arg2);
    if (r2.x != 42) return 2;
    int y_err = (int)(r2.y * 100.0) - 314;
    if (y_err < 0) y_err = -y_err;
    if (y_err > 1) return 3;

    return 0;
}
