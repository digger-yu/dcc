// Stage 2/4 交叉验证: **对象 cv 限定参与重载决议**。
// 三条链路都要选对候选:
// 1. `const M c; c.f()` -> `f() const` (隐式对象实参是 `&c`, 取地址不改 cv);
// 2. const 成员函数体内裸调 `g()` 与 `this->g()` -> 只能选 `g() const`;
// 3. 经由 const 对象的字段链 `outer.m.f()`。
// 早期 `CxxType`/`MangleType` 完全不携带对象 cv, 5 个调用全绑到第一个候选
// (实测 dcc 打 1 / g++ 打 2)。
#include <cstdio>

struct M {
    int v;
    M() : v(0) {}
    int which() { return 1; }
    int which() const { return 2; }
    int probe() { return 100 + which(); }
    int probe() const { return 200 + which(); }
    int via_this() { return this->which(); }
    int via_this() const { return 1000 + this->which(); }
};

struct Outer {
    M m;
    int read() const { return m.which(); }
};

int main() {
    M m;
    const M c;
    printf("%d\n", m.which());
    printf("%d\n", c.which());
    printf("%d\n", m.probe());
    printf("%d\n", c.probe());
    printf("%d\n", m.via_this());
    printf("%d\n", c.via_this());
    Outer o;
    printf("%d\n", o.read());
    return 0;
}
