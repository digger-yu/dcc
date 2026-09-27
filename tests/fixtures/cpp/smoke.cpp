// M17.0 smoke: 最小可工作 C++ 程序.
// 注意: `class` 成员默认 private, 原夹具把 ctor/get() 写在默认段里,
// g++ -std=c++17 直接拒绝 ('Foo::Foo()' is private within this context),
// 不能作为语义基准. 访问控制真正生效后必须显式 public.
class Foo {
public:
  int x;
  Foo() : x(0) {}
  int get() const { return x; }
};
int main() {
  Foo f;
  return f.get();
}
