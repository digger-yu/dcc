// M17.0: using Base::Base; 继承构造 (C++ 解析层)
// 覆盖: using 声明在类内被 lower 接受 (不强求链接通过)
struct Base1 {
  int v;
  Base1() : v(0) {}
  Base1(int x) : v(x) {}
};
struct Derived1 {
  using Base1::Base1;
  int extra;
};
int test_inheriting_ctor() {
  Base1 b(7);
  return b.v;
}
