// M17.0: auto x = expr; 真类型推导
// 覆盖: int / const char* / class 类型推导
class Widget {
public:
  int v;
  Widget(int x) : v(x) {}
};
int test() {
  auto i = 42;          // int
  auto p = "hello";     // const char* (StringLit)
  auto w = Widget(7);   // Widget (值类型, 通过类 ctor 推断)
  int s = i + 1;
  s += w.v;
  (void)p;
  return s;
}
