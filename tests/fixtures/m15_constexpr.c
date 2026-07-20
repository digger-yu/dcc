// C23 constexpr 函数 (M15): 仅解析, 不强制编译期求值
extern int printf(const char *, ...);

constexpr int square(int n) { return n * n; }

int main(void) {
    int r = square(5);
    printf("%d\n", r);
    return 0;
}
