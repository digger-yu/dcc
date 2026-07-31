// 测试预处理器第二轮增强 (期望返回 30)

// 1. __attribute__ 剥离
__attribute__((unused)) int foo(void) {
    return 10;
}

// 2. __has_attribute (应返回 0)
#if __has_attribute(packed)
int bad1 = 999;
#endif

// 3. likely 展开为 (x)
int bar(int x) {
    if (likely(x)) return x;
    return 0;
}

int main(void) {
    int r = 0;
    r += foo();
    r += bar(10);
    r += bar(10);
    return r;
}
