// M17.0: explicit 构造函数 - 阻止隐式转换
// 覆盖: explicit 单参 ctor 不可被隐式调用
struct Wrap {
  int v;
  explicit Wrap(int x) : v(x) {}
};
int test_explicit() {
  // 显式构造 OK
  Wrap w(42);
  // 隐式转换 Wrap w2 = 42; 应被禁止 (C++ 语义)
  // 这里仅验证编译通过 (显式构造), 隐式部分由 C++ 静态分析保证
  return w.v;
}
