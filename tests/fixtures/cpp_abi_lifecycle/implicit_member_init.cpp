// Stage 4 交叉验证: **隐式成员初始化器**与空实参初始化列表。
// 三条容易漏掉的规则:
// 1. 未出现在初始化列表里的类类型成员也要默认构造 ([class.base.init]/8);
// 2. `a()` (空实参) 对标量成员是**值初始化** = 0, 不是「没有初始化器」;
//    早期 parser 把 `s()` 和 `: x` 都编码成 `IntLit(0)`, 于是类类型成员
//    `s()` 被当成「用 0 赋值」, 构造调用整个丢失 (实测 wrap=0 / g++ wrap=1);
// 3. 花括号形式 `b{7}` 与圆括号等价 (列表初始化)。
#include <cstdio>

static int s_ctor = 0;

struct S {
    int a;
    int b;
    S() : a(1), b(2) { s_ctor++; }
    S(int x) : a(x), b(x) { s_ctor += 10; }
};

struct Wrap {
    S s;      // 未写初始化器 -> 仍要默认构造
    S t;      // 写 `t()`   -> 默认构造
    S u;      // 写 `u(9)`  -> 带参构造
    int zero; // 写 `zero()` -> 值初始化 = 0
    int seven; // 写 `seven{7}` -> 列表初始化 = 7
    Wrap() : t(), u(9), zero(), seven{7} {}
};

struct Two {
    int lo;
    int hi;
    Two(int x, int y) : lo(x), hi(y) {}
};

struct HoldsTwo {
    Two w;
    HoldsTwo() : w(4, 5) {}
};

int main() {
    Wrap w;
    printf("%d %d %d %d %d %d\n", w.s.a, w.s.b, w.t.a, w.u.a, w.zero, w.seven);
    HoldsTwo h;
    printf("%d %d\n", h.w.lo, h.w.hi);
    printf("s_ctor=%d\n", s_ctor);
    return 0;
}
