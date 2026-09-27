// multi_declarator_static_local.cpp —— 函数内 `static` 声明里的多个 init-declarator
//
// `static int a = 1, b = 2;` 是一条声明的两个 declarator, 共享 decl-specifier 的
// `static` 存储期, 但**每个对象各自**参与线程安全初始化 ([stmt.dcl]/[dcl.stc]):
// 初始化只发生一次, 之后的调用保留各自的值.
//
// 验证点 (退出码逐条与 g++ -std=c++20 真值对照):
//   1) 两个静态局部各保留自己的累加值 (调用间不重置, 也不互相串味);
//   2) `static int *p, counter = 5;` —— 指针层级只属于 p, counter 是 int;
//   3) `static const int` 多 declarator 只初始化一次 (第二次调用值不变);
//   4) `static volatile int` 多 declarator 的读写跨调用保留;
//   5) 函数内 `const` / `volatile` / `constexpr` 多 declarator 的每个名字都可见.
int bump() {
    static int a = 1, b = 2;
    a = a + 10;
    b = b + 100;
    return a + b;
}

int mixed() {
    static int *p, counter = 5;
    p = &counter;
    counter = counter + 1;
    return *p;
}

int probe_const() {
    // 函数内 const 多 declarator: 两个名字都必须可见 (旧行为: 整条降级 OpaqueStmt)
    const int ca = 10, cb = 20;
    return ca + cb;
}

int probe_volatile() {
    volatile int va = 1, vb = 2;
    va = va + 1;
    vb = vb + 10;
    return va + vb;
}

int probe_constexpr() {
    constexpr int xa = 1, xb = 2;
    return xa * 10 + xb;
}

int table(int which) {
    static const int ta = 3, tb = 4;
    return which == 0 ? ta : tb;
}

int guarded(int which) {
    static volatile int v = 1, w = 2;
    if (which == 0) {
        w = w + v;
        return w + table(1);
    }
    v = v + 1;
    return v + table(0);
}

int main() {
    if (bump() != 113) return 1;
    if (bump() != 223) return 2;
    if (bump() != 333) return 3;
    if (mixed() != 6) return 4;
    if (mixed() != 7) return 5;
    if (table(0) != 3) return 6;
    if (table(1) != 4) return 7;
    if (guarded(1) != 5) return 8;
    if (guarded(0) != 8) return 9;
    if (guarded(1) != 6) return 10;
    if (probe_const() != 30) return 11;
    if (probe_volatile() != 14) return 12;
    if (probe_constexpr() != 12) return 13;
    return 0;
}
