// namespace_attributes.cpp — 带属性说明符的 namespace 声明正向夹具
//
// glibc++ 头几乎每个顶层块都写成 `namespace std _GLIBCXX_VISIBILITY(default)
// { ... }` (宏展开为 `__attribute__((__visibility__("default")))`)。之前 native
// parser 只接受 `namespace 名字 {`, 名字与 `{` 之间夹属性时整条 namespace 被顶层
// 容错降级成 Opaque, native 随即硬错 (libcpp 统一首错链上的一站)。
//
// 覆盖 g++ 接受的两种位置: GNU `__attribute__` 在名字之后、C++11 `[[...]]` 在
// 名字之后; 命名空间里放一个真实可用的类, 通过成员函数产生可观察输出。
#include <cstdio>

namespace wrapped __attribute__((__visibility__("default"))) {
struct Counter {
    int n;
    int inc() {
        ++n;
        return n;
    }
};
}  // namespace wrapped

namespace tagged {
struct Flag {
    int v;
};
}  // namespace tagged

int main() {
    wrapped::Counter c;
    c.n = 0;
    c.inc();
    c.inc();
    tagged::Flag f;
    f.v = 42;
    printf("count=%d flag=%d\n", c.n, f.v);
    return 0;
}
