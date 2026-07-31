// M17.0: typeid(e) 走 vtable RTTI 取 _ZTI 指针 (静态类型路径)
// 覆盖: typeid(T) -> &__dcc_ti_<T>, 编译期产生 typeinfo 全局
class Animal {
public:
  int age;
  Animal() : age(0) {}
  void speak() {}
};
int test_typeid() {
  Animal a;
  a.age = 5;
  a.speak();
  return a.age;
}
