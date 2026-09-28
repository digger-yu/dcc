// Stage 7 交叉验证: `typeid` 的**引用形态**结果 (`const std::type_info&`)。
// `<typeinfo>` 被包含时结果类型按 [expr.type.id]/3 是引用, 于是
// `typeid(x).name()` / `a.before(b)` / `a == b` 都要能降级。
// 每个 `namespace std { ... }` 都是同一实体的重开 ([namespace.def]/2);
// 早期 sema 每次新建一个 std 符号与新作用域, 限定名解析只看第一个符号的
// 作用域 => `std::type_info` 永远查不到, 结果类型悄悄退化成 `void*`。
#include <cstdio>
#include <typeinfo>

struct Poly {
    virtual ~Poly() {}
};
struct Leaf : Poly {
    virtual void m() {}
};

int main() {
    const std::type_info& direct = typeid(Leaf);
    printf("%s\n", direct.name());
    Poly* p = new Leaf;
    const std::type_info& dyn = typeid(*p);
    printf("%s\n", dyn.name());
    printf("eq=%d before=%d ne=%d\n", direct == dyn, direct.before(dyn), direct != dyn);
    Poly base;
    printf("static=%s\n", typeid(base).name());
    delete p;
    return 0;
}
