// tests/fixtures/c/c23_constexpr.c
// C23 constexpr 函数: 解析为声明说明符, 等价 inline + always_inline,
// 无强制编译期求值.

/* ---- constexpr 函数声明 ---- */
constexpr int square(int x) {
    return x * x;
}

constexpr int add(int a, int b) {
    return a + b;
}

/* ---- constexpr 与返回类型组合 ---- */
constexpr long long mul(long long a, long long b) {
    return a * b;
}

/* ---- constexpr 前置声明 ---- */
constexpr void do_nothing(void);

constexpr void do_nothing(void) {
    (void)0;
}

int main(void) {
    int s = square(5);
    int sum = add(3, 4);
    long long product = mul(100LL, 200LL);
    do_nothing();
    return (s == 25 && sum == 7 && product == 20000LL) ? 0 : 1;
}
