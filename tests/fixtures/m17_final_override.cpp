// M17.0: final / override 联动 vtable + devirt (C++ 解析层)
// 覆盖: final / override 关键字被接受
struct Base {
  virtual void f() {}
  virtual void g() {}
};
struct Derived : Base {
  void f() override {}
  void g() final {}
};
int test_final_override() {
  Derived d;
  d.f();
  d.g();
  return 0;
}
