// Stage 4 交叉验证: **隐式析构函数**。
// 类自己没写 `~T()`，但成员 (或基类) 需要析构时，编译器必须合成 D1/D2 并在
// 作用域退出时调用；漏掉时 GCC 计数 1 而 dcc 计数 0 (实测 `struct S { P p; };`)。
// `ClassInfo.fields` 曾经是一个从不填充的副本，让「是否需要析构」的判据
// 永远看不到成员，现在成员枚举统一走 `Sema::class_field_symbols`。
#include <cstdio>

static int ctor_n = 0;
static int dtor_n = 0;

struct P {
    P() { ctor_n++; }
    ~P() { dtor_n++; }
};

// 无用户析构、但持有需析构成员
struct S {
    P p;
    int v;
};

// 继承链中间层无用户析构: Leaf 析构要能链到 S 的合成析构
struct Q : S {
    P q;
    ~Q() { dtor_n += 100; }
};

static int g_ctor = 0;

struct G {
    P p;
    G() { g_ctor++; }
};

G globalsample;

int main() {
    { S s; s.v = 1; }
    printf("after-S ctor=%d dtor=%d\n", ctor_n, dtor_n);
    { Q q; }
    printf("after-Q ctor=%d dtor=%d\n", ctor_n, dtor_n);
    printf("global ctor=%d\n", g_ctor);
    return 0;
}
