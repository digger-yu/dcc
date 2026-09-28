// Stage 4 交叉验证: **命名空间作用域对象的动态初始化**。
// g++ 为每个需要运行期初始化的全局对象发一个 `__cxx_global_var_init.N`,
// 并把地址放进 `.init_array`; 退出时由 `__cxa_atexit` 登记的析构按构造逆序执行。
// dcc 缺这一步时全局对象的构造根本不跑 (实测 `global ctor=0` / g++ `1`)。
#include <cstdio>

static int n = 0;

struct A {
    A() { printf("A ctor %d\n", ++n); }
    ~A() { printf("A dtor %d\n", n--); }
};

struct B {
    A a;          // 未写初始化器, 仍要默认构造
    int tag;
    B() : tag(1) { printf("B ctor\n"); }
};

A ga;
B gb;
// const 全局对象同样要构造
const A gc;

int main() {
    printf("main n=%d\n", n);
    return 0;
}
