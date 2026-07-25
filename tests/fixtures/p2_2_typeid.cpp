// P2.2: typeid(e) 真 RTTI 取类型 (Section 6).
// 验证 dcc lower 阶段为 typeid(e) 生成
// `(&__dcc_ti_<class>)` 路径, 取静态类对应的 typeinfo
// 符号 (M17.0 阶段实装). 跨多态指针的动态 RTTI 走 vtable
// typeinfo 字段 (Section 9 follow-up).
struct Animal {
    int age;
};
int test_typeid_static() {
    // typeid(Animal) 走 TypeidType 静态路径, 生成 &__dcc_ti_Animal
    return (typeid(Animal) == typeid(Animal)) ? 1 : 0;
}
int test_typeid_value() {
    Animal a;
    a.age = 5;
    // typeid(a) 走 Typeid 动态路径, expr_static_class 推断
    // 出 Animal 类, 生成 &__dcc_ti_Animal
    return (typeid(a) == typeid(Animal)) ? a.age : 0;
}
