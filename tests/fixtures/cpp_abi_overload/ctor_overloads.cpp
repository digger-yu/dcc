// 重载构造函数的链接名必须互不相同, 否则链接器把两个定义合并成一个,
// 另一个静默消失 (实测 `S s; S t(2);` 曾打出 `491541736 2` 而不是 `1 2`)。
#include <cstdio>

struct S {
    int a;
    int b;
    S() : a(1), b(1) {}
    S(int x) : a(x), b(x) {}
    S(int x, int y) : a(x), b(y) {}
    S(double d) : a((int)(d * 10)), b((int)(d * 10)) {}
};

struct Wrap {
    S s;
    Wrap() : s() {}
    Wrap(int x) : s(x) {}
};

int main() {
    S s0;
    S s1(2);
    S s2(3, 4);
    S s3(1.5);
    printf("%d %d\n", s0.a, s0.b);
    printf("%d %d\n", s1.a, s1.b);
    printf("%d %d\n", s2.a, s2.b);
    printf("%d %d\n", s3.a, s3.b);
    Wrap w1;
    Wrap w2(9);
    printf("wrap=%d %d\n", w1.s.a, w2.s.a);
    return 0;
}
