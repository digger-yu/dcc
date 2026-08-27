// P2.2: typeid(e) 真 RTTI 取类型
// M27: 多态表达式通过 vtable RTTI slot 获取真实类型
// 非多态表达式仍用静态类 RTTI
struct Animal {
    int age;
};

struct PolyAnimal {
    int age;
    PolyAnimal() {}
    virtual void speak() {}
};

struct Dog : PolyAnimal {
    void speak() override {}
};

int test_typeid_static() {
    // typeid(Animal) 走 TypeidType 静态路径, 生成 &__dcc_ti_Animal
    return (typeid(Animal) == typeid(Animal)) ? 1 : 0;
}

int test_typeid_value() {
    Animal a;
    a.age = 5;
    // typeid(a) 走 Typeid 静态路径 (Animal 非多态)
    return (typeid(a) == typeid(Animal)) ? a.age : 0;
}

int test_typeid_polymorphic() {
    PolyAnimal p;
    // typeid(p) 走 Typeid 动态路径 (PolyAnimal 是多态类)
    // 通过 __vptr[0] 读取运行时 typeinfo
    void* ti = (void*)typeid(p);
    void* ti_expected = (void*)typeid(PolyAnimal);
    return (ti == ti_expected) ? 1 : 0;
}

int test_typeid_derived() {
    Dog d;
    PolyAnimal* p = &d;
    // typeid(*p) 应该返回 Dog 的 typeinfo (动态 RTTI)
    // 因为 p 指向 Dog 对象, vtable 是 Dog 的
    void* ti = (void*)typeid(*p);
    void* ti_dog = (void*)typeid(Dog);
    return (ti == ti_dog) ? 1 : 0;
}
