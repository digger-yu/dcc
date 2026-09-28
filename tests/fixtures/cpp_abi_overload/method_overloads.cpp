// 同名重载成员函数 (含 const 限定重载) 必须各自可派发。
#include <cstdio>

struct M {
    int v;
    M() : v(0) {}
    int f() { return 1; }
    int f() const { return 2; }
    int f(int x) { return 3 + x; }
    int f(int x, int y) { return 4 + x + y; }
    int f(double d) { return 5 + (int)(d * 2); }
};

int main() {
    M m;
    m.v = 7;
    const M c = m;
    printf("%d\n", m.f());
    printf("%d\n", c.f());
    printf("%d\n", m.f(10));
    printf("%d\n", m.f(10, 20));
    printf("%d\n", m.f(2.5));
    return 0;
}
