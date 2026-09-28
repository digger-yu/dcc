// Stage 4 交叉验证: ctor-initializer 的**完整实参列表**。
// 早期 parser 只取括号里的第一个表达式, `D() : B(1, 2)` 会退化成
// `B(1)` (逗号表达式取尾值), 选不到 `B(int, int)` 这个重载, 链接期报
// undefined `B_ctor_base`; 命名空间限定的基类名 `: ns::Base()` 同理匹配不上。
#include <cstdio>

namespace ns {
struct Base {
    int x;
    int y;
    Base(int a, int b) : x(a), y(b) {}
};
} // namespace ns

struct B {
    int a;
    int b;
    B(int x, int y) : a(x), b(y) {}
};

struct D : B, ns::Base {
    int c;
    D() : B(1, 2), ns::Base(3, 4), c(5) {}
};

int main() {
    D d;
    printf("%d %d %d %d %d\n", d.a, d.b, d.x, d.y, d.c);
    return 0;
}
