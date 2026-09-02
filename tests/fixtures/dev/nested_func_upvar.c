/* 嵌套函数 (GNU 扩展) 测试 */
int test_nested(void) {
    int x = 10;
    auto int get_x(void) {
        return x;
    }
    auto void set_x(int v) {
        x = v;
    }
    int a = get_x();
    if (a != 10) return 1;
    set_x(42);
    if (x != 42) return 2;
    if (get_x() != 42) return 3;
    return 0;
}

/* 嵌套函数调用: 内嵌调用 */
int test_call_nested(void) {
    int base = 100;
    auto int square(int v) {
        return v * v;
    }
    auto int sum_squares(int n) {
        int s = 0;
        for (int i = 1; i <= n; i++) {
            s = s + square(i);
        }
        return s;
    }
    int s = sum_squares(3);  /* 1+4+9 = 14 */
    if (s != 14) return 1;
    if (base != 100) return 2;
    return 0;
}

int main(void) {
    int r;
    r = test_nested();
    if (r != 0) return 100 + r;
    r = test_call_nested();
    if (r != 0) return 200 + r;
    return 0;
}
