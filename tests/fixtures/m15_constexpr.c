// C23 constexpr 函数 (M15): 仅解析, 不强制编译期求值
constexpr int square(int n) { return n * n; }

int main(void) {
    int r = square(5);
    return r == 25 ? 0 : 1;
}
